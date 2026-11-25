//! Real-time waveform: every visible channel against time.
//!
//! The widget never scans raw samples itself: each frame it asks every channel for a min/max
//! decimation sized to the pixel width of the plot, so the cost of a frame depends on the
//! window, not on how much data is behind it.

use std::sync::Arc;

use egui::epaint::{CornerRadius, Galley, PathStroke};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use embdbg_core::pipeline::{ChannelSet, ColumnDetail, DecPoint};

use super::area::{AxisStyle, PlotArea, draw_axes, time_label, value_label};
use crate::theme::{self, SPACE_1, SPACE_2, SPACE_3, SPACE_4};
use crate::util::format_sig;

const HEADER_HEIGHT: f32 = 22.0;
const LEFT_MARGIN: f32 = 62.0;
const RIGHT_MARGIN: f32 = SPACE_2;
/// Room for the tick labels and, below them, the hint line.
const BOTTOM_MARGIN: f32 = 36.0;
const TOP_MARGIN: f32 = SPACE_1;

/// Widest and narrowest time window, in seconds.
const MAX_WINDOW: f64 = 3600.0;
const MIN_WINDOW: f64 = 2e-6;
/// Zoom exponent per point of scroll: one wheel notch (about 50 points) zooms by a factor of 1.65.
const ZOOM_PER_SCROLL_POINT: f64 = 0.01;

const LINE_WIDTH: f32 = 1.4;
const DOT_RADIUS: f32 = 4.0;
const READOUT_LINE_HEIGHT: f32 = 16.0;

/// What the waveform shows, and how the user has moved it.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveView {
    /// Visible time span, in seconds.
    pub window: f64,
    /// Keep the newest sample at the right edge. Panning in time turns this off.
    pub follow: bool,
    /// Time at the right edge while not following.
    pub t_end: f64,
    /// Fit the value axis to the visible data.
    pub y_auto: bool,
    pub y_min: f64,
    pub y_max: f64,
    /// Times of the measurement cursors A and B.
    pub cursors: [Option<f64>; 2],
    /// Cursor the next click replaces.
    next_cursor: usize,
}

impl Default for WaveView {
    fn default() -> Self {
        Self {
            window: 0.01,
            follow: true,
            t_end: 0.0,
            y_auto: true,
            y_min: -1.0,
            y_max: 1.0,
            cursors: [None, None],
            next_cursor: 0,
        }
    }
}

impl WaveView {
    /// Back to following the data, fitted to it, at the default zoom, without cursors.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Zooms the time axis by `factor`. While following, the newest sample stays where it is;
    /// otherwise the moment under the pointer stays put. `anchor` is `(fraction across the plot, time there)`.
    pub fn zoom_time(&mut self, factor: f64, anchor: Option<(f64, f64)>) {
        let window = (self.window * factor).clamp(MIN_WINDOW, MAX_WINDOW);
        if !self.follow
            && let Some((frac, time)) = anchor
        {
            self.t_end = time + (1.0 - frac) * window;
        }
        self.window = window;
    }

    /// Zooms the value axis by `factor` around a screen position, and stops fitting it to the data.
    /// `anchor` is `(fraction up the plot, value there)`.
    pub fn zoom_value(&mut self, factor: f64, anchor: (f64, f64)) {
        let (frac, value) = anchor;
        let span = ((self.y_max - self.y_min) * factor).clamp(1e-12, 1e12);
        self.y_auto = false;
        self.y_min = value - frac * span;
        self.y_max = self.y_min + span;
    }

    /// Slides the visible time by `dt` seconds. Any real pan stops the following.
    pub fn pan_time(&mut self, dt: f64) {
        if dt != 0.0 {
            self.follow = false;
            self.t_end += dt;
        }
    }

    /// Slides the value axis by `dv`, unless it is fitted to the data.
    pub fn pan_value(&mut self, dv: f64) {
        if !self.y_auto {
            self.y_min += dv;
            self.y_max += dv;
        }
    }

    /// Puts a measurement cursor at `t`, replacing the one placed longest ago.
    pub fn place_cursor(&mut self, t: f64) {
        let slot = match self.cursors {
            [None, _] => 0,
            [_, None] => 1,
            _ => self.next_cursor,
        };
        self.cursors[slot] = Some(t);
        self.next_cursor = (slot + 1) % self.cursors.len();
    }
}

/// Buffers reused from frame to frame, so drawing does not allocate.
#[derive(Debug, Default)]
pub struct WaveScratch {
    series: Vec<Vec<DecPoint>>,
    points: Vec<Pos2>,
}

/// Draws the waveform of `data` into all the space `ui` has left.
///
/// `visible` says which channels to draw; missing entries count as visible.
pub fn show(
    ui: &mut Ui,
    data: &ChannelSet,
    view: &mut WaveView,
    visible: &[bool],
    scratch: &mut WaveScratch,
) -> egui::Response {
    let (outer, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let plot_rect = Rect::from_min_max(
        outer.min + vec2(LEFT_MARGIN, HEADER_HEIGHT + TOP_MARGIN),
        outer.max - vec2(RIGHT_MARGIN, BOTTOM_MARGIN),
    );
    if plot_rect.width() < 16.0 || plot_rect.height() < 16.0 {
        return response;
    }

    let axis = data.axis();
    let Some(plottable) = data.plottable_range() else {
        draw_empty(ui, outer);
        return response;
    };

    // --- which frames are on screen, and what they hold ---
    // Leave a little room after the newest sample, so the line does not touch the frame.
    let x_max = if view.follow {
        axis.last_time().unwrap_or(0.0) + view.window * 0.02
    } else {
        view.t_end
    };
    let x_min = x_max - view.window;
    // `index_at_time` counts the samples that arrived by a time, so it ends a range.
    // Step one back to include the sample straddling the left edge.
    let start = axis
        .index_at_time(x_min)
        .unwrap_or(plottable.start)
        .saturating_sub(1)
        .max(plottable.start);
    let end = axis
        .index_at_time(x_max)
        .unwrap_or(plottable.end)
        .clamp(start, plottable.end);

    let columns = (plot_rect.width().ceil() as usize).max(1);
    // A column this narrow cannot show where it starts and ends, so only its extremes are drawn,
    // which halves the points there are to tessellate.
    let detail = if end - start >= 16 * columns as u64 {
        ColumnDetail::Extremes
    } else {
        ColumnDetail::ExtremesAndEdges
    };
    scratch.series.resize_with(data.channels().len(), Vec::new);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (index, channel) in data.channels().iter().enumerate() {
        let points = &mut scratch.series[index];
        points.clear();
        if visible.get(index).copied().unwrap_or(true) {
            channel.decimate(axis, start..end, columns, detail, points);
        }
        for point in points.iter() {
            let value = f64::from(point.v);
            lo = lo.min(value);
            hi = hi.max(value);
        }
    }
    if view.y_auto {
        if lo <= hi {
            let pad = (hi - lo).max(1e-9) * 0.08;
            view.y_min = lo - pad;
            view.y_max = hi + pad;
        } else {
            view.y_min = -1.0;
            view.y_max = 1.0;
        }
    }
    let area = PlotArea::new(plot_rect, (x_min, x_max), (view.y_min, view.y_max));

    handle_input(ui, &response, &area, view);

    // --- paint ---
    let painter = ui.painter();
    draw_axes(
        painter,
        &area,
        &AxisStyle::default(),
        time_label,
        value_label,
    );

    let plot_painter = painter.with_clip_rect(plot_rect);
    for (index, points) in scratch.series.iter().enumerate() {
        if points.len() < 2 {
            continue;
        }
        scratch.points.clear();
        scratch
            .points
            .extend(points.iter().map(|p| area.screen_pos(p.t, f64::from(p.v))));
        plot_painter.add(Shape::line(
            scratch.points.clone(),
            PathStroke::new(LINE_WIDTH, theme::series_color(index)),
        ));
    }

    draw_header(painter, outer, data, visible);
    draw_cursors(painter, &area, view);
    if response.hovered()
        && let Some(pos) = response.hover_pos()
        && area.rect.contains(pos)
    {
        draw_crosshair(painter, &area, data, visible, pos);
    }
    draw_hints(painter, outer);
    response
}

fn handle_input(ui: &Ui, response: &egui::Response, area: &PlotArea, view: &mut WaveView) {
    if response.double_clicked() {
        view.reset();
        return;
    }
    if response.dragged() {
        let delta = response.drag_delta();
        view.pan_time(-f64::from(delta.x) * area.x_span() / f64::from(area.rect.width()));
        view.pan_value(-f64::from(delta.y) * area.y_span() / f64::from(area.rect.height()));
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        view.place_cursor(area.data_at(pos).0);
    }
    if response.hovered() {
        let (shift, ctrl, scroll) =
            ui.input(|i| (i.modifiers.shift, i.modifiers.ctrl, i.smooth_scroll_delta));
        if scroll.y != 0.0 {
            let factor = (-f64::from(scroll.y) * ZOOM_PER_SCROLL_POINT).exp();
            let pos = response.hover_pos().unwrap_or(area.rect.center());
            let (data_x, data_y) = area.data_at(pos);
            if shift || ctrl {
                let frac_y = f64::from((area.rect.bottom() - pos.y) / area.rect.height());
                view.zoom_value(factor, (frac_y, data_y));
            } else {
                view.zoom_time(factor, Some((area.x_frac(data_x), data_x)));
            }
        }
    }
}

fn draw_empty(ui: &Ui, outer: Rect) {
    let painter = ui.painter();
    painter.rect_filled(outer, CornerRadius::ZERO, theme::BG_WIDGET);
    painter.text(
        outer.center() - vec2(0.0, 11.0),
        Align2::CENTER_CENTER,
        "暂无数据",
        FontId::proportional(16.0),
        theme::FG,
    );
    painter.text(
        outer.center() + vec2(0.0, 12.0),
        Align2::CENTER_CENTER,
        "在左侧「连接」里启动模拟信号源，或打开一个数据源",
        FontId::proportional(13.0),
        theme::FG_DIM,
    );
}

/// Channel names and their latest values, above the plot.
fn draw_header(painter: &egui::Painter, outer: Rect, data: &ChannelSet, visible: &[bool]) {
    let name_font = FontId::proportional(12.5);
    let value_font = FontId::monospace(12.0);
    let y = outer.top() + HEADER_HEIGHT / 2.0;
    let mut x = outer.left() + SPACE_2;
    let limit = outer.right() - SPACE_4;

    for (index, channel) in data.channels().iter().enumerate() {
        let shown = visible.get(index).copied().unwrap_or(true);
        let text_color = if shown { theme::FG } else { theme::FG_DIM };
        let name = painter.layout_no_wrap(channel.name().to_owned(), name_font.clone(), text_color);
        let last = channel
            .stats()
            .last
            .map(|v| format_sig(f64::from(v), 5))
            .unwrap_or_else(|| "—".to_owned());
        let value = painter.layout_no_wrap(last, value_font.clone(), text_color);
        let (name_width, value_width) = (name.size().x, value.size().x);
        let width = SPACE_2 + 2.0 * DOT_RADIUS + SPACE_1 + name_width + SPACE_2 + value_width;

        // Out of room: say how many channels are not listed, if even that fits.
        if x + width > limit {
            let hidden = data.channels().len() - index;
            let text = format!("还有 {hidden} 个通道未显示");
            let fits = x + painter
                .layout_no_wrap(text.clone(), name_font.clone(), theme::FG_DIM)
                .size()
                .x
                <= limit;
            if fits {
                painter.text(
                    pos2(x, y),
                    Align2::LEFT_CENTER,
                    text,
                    name_font,
                    theme::FG_DIM,
                );
            }
            break;
        }

        painter.circle_filled(
            pos2(x + SPACE_2 + DOT_RADIUS, y),
            DOT_RADIUS,
            if shown {
                theme::series_color(index)
            } else {
                theme::BORDER
            },
        );
        let name_x = x + SPACE_2 + 2.0 * DOT_RADIUS + SPACE_1;
        painter.galley(pos2(name_x, y - name.size().y / 2.0), name, text_color);
        let value_x = name_x + name_width + SPACE_2;
        painter.galley(pos2(value_x, y - value.size().y / 2.0), value, text_color);
        x = value_x + value_width + SPACE_4;
    }
}

/// The measurement cursors, and what lies between them.
fn draw_cursors(painter: &egui::Painter, area: &PlotArea, view: &WaveView) {
    const COLORS: [Color32; 2] = [theme::CURSOR_A, theme::CURSOR_B];
    const NAMES: [&str; 2] = ["A", "B"];
    for (index, cursor) in view.cursors.iter().enumerate() {
        let Some(t) = *cursor else { continue };
        let x = area.screen_pos(t, 0.0).x;
        if !area.rect.x_range().contains(x) {
            continue;
        }
        painter.with_clip_rect(area.rect).vline(
            x,
            area.rect.y_range(),
            Stroke::new(1.0, COLORS[index]),
        );
        let tag = pos2(x, area.rect.top() + 10.0);
        painter.rect_filled(
            Rect::from_center_size(tag, vec2(14.0, 14.0)),
            CornerRadius::same(3),
            COLORS[index],
        );
        painter.text(
            tag,
            Align2::CENTER_CENTER,
            NAMES[index],
            FontId::monospace(10.0),
            theme::BG_WINDOW,
        );
    }

    if let [Some(a), Some(b)] = view.cursors {
        let dt = (b - a).abs();
        let text = if dt > 0.0 {
            format!(
                "Δt = {}    1/Δt = {}",
                format_sig(dt, 4),
                format_sig(1.0 / dt, 4)
            )
        } else {
            "Δt = 0".to_owned()
        };
        let font = FontId::monospace(12.0);
        let galley = painter.layout_no_wrap(text, font, theme::FG);
        let size = galley.size() + vec2(2.0 * SPACE_2, 2.0 * SPACE_1);
        let origin = pos2(
            area.rect.right() - size.x - SPACE_2,
            area.rect.top() + SPACE_2,
        );
        painter.rect_filled(
            Rect::from_min_size(origin, size),
            CornerRadius::same(4),
            theme::BG_WINDOW.gamma_multiply(0.9),
        );
        painter.galley(origin + vec2(SPACE_2, SPACE_1), galley, theme::FG);
    }
}

/// Crosshair through the pointer, with the time and every visible channel's value there.
fn draw_crosshair(
    painter: &egui::Painter,
    area: &PlotArea,
    data: &ChannelSet,
    visible: &[bool],
    pos: Pos2,
) {
    let (t, _) = area.data_at(pos);
    // The last sample that had arrived by `t`.
    let frame = data
        .axis()
        .index_at_time(t)
        .and_then(|next| next.checked_sub(1));
    let clipped = painter.with_clip_rect(area.rect);
    clipped.vline(pos.x, area.rect.y_range(), Stroke::new(1.0, theme::FG_DIM));
    clipped.hline(area.rect.x_range(), pos.y, Stroke::new(1.0, theme::FG_DIM));

    let font = FontId::monospace(12.0);
    let mut lines = vec![format!("t = {}", format_sig(t, 6))];
    for (index, channel) in data.channels().iter().enumerate() {
        if !visible.get(index).copied().unwrap_or(true) {
            continue;
        }
        let value = frame
            .and_then(|f| channel.value_at(f))
            .map_or_else(|| "—".to_owned(), |v| format_sig(f64::from(v), 6));
        lines.push(format!("{}  {}", channel.name(), value));
    }
    let galleys: Vec<Arc<Galley>> = lines
        .iter()
        .map(|line| painter.layout_no_wrap(line.clone(), font.clone(), theme::FG))
        .collect();
    let width = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 2.0 * SPACE_2;
    let height = galleys.len() as f32 * READOUT_LINE_HEIGHT + 2.0 * SPACE_1;
    // Stay inside the plot: move to the other side of the pointer near an edge.
    let mut origin = pos + vec2(SPACE_3, SPACE_3);
    if origin.x + width > area.rect.right() {
        origin.x = pos.x - SPACE_3 - width;
    }
    if origin.y + height > area.rect.bottom() {
        origin.y = pos.y - SPACE_3 - height;
    }
    painter.rect_filled(
        Rect::from_min_size(origin, vec2(width, height)),
        CornerRadius::same(4),
        theme::BG_WINDOW.gamma_multiply(0.92),
    );
    for (index, galley) in galleys.iter().enumerate() {
        painter.galley(
            pos2(
                origin.x + SPACE_2,
                origin.y + SPACE_1 + index as f32 * READOUT_LINE_HEIGHT,
            ),
            galley.clone(),
            theme::FG,
        );
    }
}

fn draw_hints(painter: &egui::Painter, outer: Rect) {
    painter.text(
        pos2(outer.right() - SPACE_2, outer.bottom() - 9.0),
        Align2::RIGHT_CENTER,
        "滚轮 缩放时间 · Shift/Ctrl+滚轮 缩放幅值 · 拖动 平移 · 单击 放游标 · 双击 复位",
        FontId::proportional(11.0),
        theme::FG_DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_moment_under_the_pointer_while_panning() {
        // The view shows 0.99..1.0 s.
        let mut view = WaveView {
            follow: false,
            t_end: 1.0,
            ..WaveView::default()
        };

        // Zoom in 10× with the pointer a quarter of the way across, which is t = 0.9925.
        view.zoom_time(0.1, Some((0.25, 0.992_5)));
        assert!((view.window - 0.001).abs() < 1e-15);
        assert!((view.t_end - (0.992_5 + 0.75 * 0.001)).abs() < 1e-15);
    }

    #[test]
    fn zooming_while_following_only_changes_the_window() {
        let mut view = WaveView::default();
        view.zoom_time(0.5, Some((0.25, 123.0)));
        assert!((view.window - 0.005).abs() < 1e-15);
        assert!(view.follow);
    }

    #[test]
    fn a_factor_of_one_changes_nothing() {
        let mut view = WaveView::default();
        view.zoom_time(1.0, Some((0.5, 0.5)));
        assert_eq!(view, WaveView::default());
    }

    #[test]
    fn zoom_is_bounded() {
        let mut view = WaveView::default();
        for _ in 0..40 {
            view.zoom_time(0.5, None);
        }
        assert_eq!(view.window, MIN_WINDOW);
        for _ in 0..80 {
            view.zoom_time(2.0, None);
        }
        assert_eq!(view.window, MAX_WINDOW);
    }

    #[test]
    fn panning_in_time_stops_following_and_a_fitted_value_axis_ignores_vertical_pans() {
        let mut view = WaveView::default();
        view.pan_value(5.0);
        assert_eq!(view, WaveView::default());
        view.pan_time(0.0);
        assert!(view.follow);
        view.pan_time(0.25);
        assert!(!view.follow);
        assert!((view.t_end - 0.25).abs() < 1e-15);

        // Once the value axis was touched, panning it works: a zoom by 1 only leaves the fitted mode.
        view.zoom_value(1.0, (0.5, 0.0));
        view.pan_value(5.0);
        assert!((view.y_min - 4.0).abs() < 1e-12 && (view.y_max - 6.0).abs() < 1e-12);
    }

    #[test]
    fn value_zoom_anchors_the_value_under_the_pointer() {
        let mut view = WaveView::default(); // -1..1
        view.zoom_value(0.5, (0.75, 0.5));
        // Half the span, with 0.5 still three quarters of the way up: -0.25..0.75.
        assert!((view.y_min + 0.25).abs() < 1e-12 && (view.y_max - 0.75).abs() < 1e-12);
        assert!(!view.y_auto);
    }

    #[test]
    fn cursors_fill_up_then_alternate() {
        let mut view = WaveView::default();
        view.place_cursor(1.0);
        assert_eq!(view.cursors, [Some(1.0), None]);
        view.place_cursor(2.0);
        assert_eq!(view.cursors, [Some(1.0), Some(2.0)]);
        view.place_cursor(3.0);
        assert_eq!(view.cursors, [Some(3.0), Some(2.0)]);
        view.place_cursor(4.0);
        assert_eq!(view.cursors, [Some(3.0), Some(4.0)]);

        view.reset();
        assert_eq!(view, WaveView::default());
    }
}
