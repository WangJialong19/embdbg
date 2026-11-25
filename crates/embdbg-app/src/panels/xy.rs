//! The X-Y view: two channels plotted against each other, the way an oscilloscope shows a
//! Lissajous figure or a phase portrait.

use egui::{ComboBox, RichText, Ui};
use embdbg_core::pipeline::SessionShared;

use crate::plots::area::{AxisStyle, PlotArea, draw_axes, value_label};
use crate::theme;

/// How many samples to draw at most; more only thickens the line.
const POINT_CHOICES: [usize; 4] = [2_000, 5_000, 10_000, 20_000];

#[derive(Debug)]
pub struct XyState {
    pub x_channel: usize,
    pub y_channel: usize,
    max_points: usize,
    points: Vec<[f32; 2]>,
}

impl Default for XyState {
    fn default() -> Self {
        Self {
            x_channel: 0,
            y_channel: 1,
            max_points: 5_000,
            points: Vec::new(),
        }
    }
}

pub fn ui(ui: &mut Ui, shared: &SessionShared, state: &mut XyState) {
    let (channel_count, frames) = {
        let data = shared.channels.read();
        (data.channels().len(), data.frames())
    };
    if channel_count < 2 {
        ui.label(RichText::new("至少需要两个通道才能画 X-Y 图。").color(theme::FG_DIM));
        return;
    }
    state.x_channel = state.x_channel.min(channel_count - 1);
    state.y_channel = state.y_channel.min(channel_count - 1);

    ui.horizontal(|ui| {
        ui.label(RichText::new("横轴").small().color(theme::FG_DIM));
        channel_combo(ui, shared, "xy-x", &mut state.x_channel, channel_count);
        ui.label(RichText::new("纵轴").small().color(theme::FG_DIM));
        channel_combo(ui, shared, "xy-y", &mut state.y_channel, channel_count);
        ComboBox::from_id_salt("xy-points")
            .selected_text(format!("最多 {} 点", state.max_points))
            .show_ui(ui, |ui| {
                for points in POINT_CHOICES {
                    ui.selectable_value(&mut state.max_points, points, format!("最多 {points} 点"));
                }
            });
    });

    let (x_name, y_name) = (
        channel_name(shared, state.x_channel),
        channel_name(shared, state.y_channel),
    );

    state.points.clear();
    {
        let data = shared.channels.read();
        // The newest stretch of samples, so the figure follows the data.
        let span = state.max_points as u64 * 4;
        let range = frames.saturating_sub(span)..frames;
        data.xy_points(
            state.x_channel,
            state.y_channel,
            range,
            state.max_points,
            &mut state.points,
        );
    }

    if state.points.len() < 2 {
        ui.label(RichText::new("还没有足够的成对样本").color(theme::FG_DIM));
        return;
    }

    draw(
        ui,
        &state.points,
        state.y_channel,
        &format!("{x_name} → 横轴，{y_name} → 纵轴"),
    );
}

fn channel_combo(
    ui: &mut Ui,
    shared: &SessionShared,
    salt: &str,
    channel: &mut usize,
    count: usize,
) {
    ComboBox::from_id_salt(salt)
        .selected_text(channel_name(shared, *channel))
        .show_ui(ui, |ui| {
            for index in 0..count {
                ui.selectable_value(channel, index, channel_name(shared, index));
            }
        });
}

fn channel_name(shared: &SessionShared, index: usize) -> String {
    shared.channels.read().channel(index).map_or_else(
        || format!("通道 {}", index + 1),
        |channel| channel.name().to_owned(),
    )
}

fn draw(ui: &mut Ui, points: &[[f32; 2]], colour_by: usize, note: &str) {
    // Leave a line under the plot for the axis note.
    let height = (ui.available_height() - 18.0).max(80.0);
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let plot = rect.shrink2(egui::vec2(64.0, 12.0));
    if plot.width() < 32.0 || plot.height() < 32.0 {
        return;
    }

    let (mut x_min, mut x_max) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut y_min, mut y_max) = (f64::INFINITY, f64::NEG_INFINITY);
    for [x, y] in points {
        x_min = x_min.min(f64::from(*x));
        x_max = x_max.max(f64::from(*x));
        y_min = y_min.min(f64::from(*y));
        y_max = y_max.max(f64::from(*y));
    }
    let pad_x = ((x_max - x_min) * 0.05).max(1e-9);
    let pad_y = ((y_max - y_min) * 0.05).max(1e-9);
    let area = PlotArea::new(
        plot,
        (x_min - pad_x, x_max + pad_x),
        (y_min - pad_y, y_max + pad_y),
    );
    draw_axes(
        ui.painter(),
        &area,
        &AxisStyle::default(),
        value_label,
        value_label,
    );

    let screen: Vec<egui::Pos2> = points
        .iter()
        .map(|[x, y]| area.screen_pos(f64::from(*x), f64::from(*y)))
        .collect();
    let painter = ui.painter().with_clip_rect(area.rect);
    painter.add(egui::Shape::line(
        screen,
        egui::epaint::PathStroke::new(1.2, theme::series_color(colour_by)),
    ));
    painter.text(
        egui::pos2(plot.right() - 4.0, plot.bottom() + 3.0),
        egui::Align2::RIGHT_TOP,
        note,
        egui::FontId::proportional(11.5),
        theme::FG_DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_pairs_the_first_two_channels() {
        let state = XyState::default();
        assert_eq!((state.x_channel, state.y_channel), (0, 1));
        assert_eq!(state.max_points, 5_000);
    }
}
