//! Shared coordinate system and axes of the hand-drawn plots.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, pos2};

use crate::theme;

/// The screen rect of a plot's data area, and the data ranges it shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlotArea {
    pub rect: Rect,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
}

impl PlotArea {
    /// Both spans must be positive.
    pub fn new(rect: Rect, x: (f64, f64), y: (f64, f64)) -> Self {
        Self {
            rect,
            x_min: x.0,
            x_max: x.1,
            y_min: y.0,
            y_max: y.1,
        }
    }

    pub fn x_span(&self) -> f64 {
        self.x_max - self.x_min
    }

    pub fn y_span(&self) -> f64 {
        self.y_max - self.y_min
    }

    /// Where a data point lies on screen. Points outside the ranges are extrapolated, not clamped.
    pub fn screen_pos(self, x: f64, y: f64) -> Pos2 {
        pos2(
            self.rect.left() + (self.x_frac(x) * f64::from(self.rect.width())) as f32,
            self.rect.bottom() - (self.y_frac(y) * f64::from(self.rect.height())) as f32,
        )
    }

    /// The data point shown at a screen position.
    pub fn data_at(self, pos: Pos2) -> (f64, f64) {
        let fx = f64::from((pos.x - self.rect.left()) / self.rect.width());
        let fy = f64::from((self.rect.bottom() - pos.y) / self.rect.height());
        (
            self.x_min + fx * self.x_span(),
            self.y_min + fy * self.y_span(),
        )
    }

    /// Fraction of the horizontal span lying left of `x`.
    pub fn x_frac(self, x: f64) -> f64 {
        (x - self.x_min) / self.x_span()
    }

    fn y_frac(self, y: f64) -> f64 {
        (y - self.y_min) / self.y_span()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AxisStyle {
    pub background: Color32,
    pub grid: Color32,
    pub frame: Stroke,
    pub text: Color32,
    pub text_size: f32,
}

impl Default for AxisStyle {
    fn default() -> Self {
        Self {
            background: theme::BG_WIDGET,
            grid: theme::GRID,
            frame: Stroke::new(1.0, theme::BORDER),
            text: theme::FG_DIM,
            text_size: 11.5,
        }
    }
}

/// The smallest "nice" step (1, 2 or 5 times a power of ten) that splits the range into at
/// most `max_intervals` intervals.
pub fn nice_step(min: f64, max: f64, max_intervals: usize) -> f64 {
    let span = (max - min).max(f64::EPSILON);
    let raw = span / max_intervals.max(1) as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    let norm = raw / magnitude;
    let factor = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    factor * magnitude
}

/// Multiples of `step` inside `min..=max`, in ascending order.
pub fn linear_ticks(min: f64, max: f64, step: f64) -> Vec<f64> {
    if !step.is_finite() || step <= 0.0 || !min.is_finite() || !max.is_finite() || min > max {
        return Vec::new();
    }
    let first = (min / step).ceil();
    let last = (min / step).ceil().max((max / step).floor());
    // The step is chosen from the pixel width, so a handful of ticks is expected; the cap only guards against runaway.
    let count = (last - first).max(0.0) as usize;
    (0..=count)
        .take(512)
        .map(|k| (first + k as f64) * step)
        .collect()
}

/// Decimals that tell neighbouring ticks apart.
fn decimals_for(step: f64) -> usize {
    if step >= 1.0 {
        0
    } else {
        (-step.log10()).ceil() as usize
    }
}

const SI_PREFIXES: [(&str, f64); 7] = [
    ("G", 1e9),
    ("M", 1e6),
    ("k", 1e3),
    ("", 1.0),
    ("m", 1e-3),
    ("µ", 1e-6),
    ("n", 1e-9),
];

/// Tick label for a time in seconds. The unit follows the tick spacing.
pub fn time_label(t: f64, step: f64) -> String {
    let (unit, scale) = if step >= 0.9995 {
        ("s", 1.0)
    } else if step >= 0.9995e-3 {
        ("ms", 1e-3)
    } else if step >= 0.9995e-6 {
        ("µs", 1e-6)
    } else {
        ("ns", 1e-9)
    };
    let decimals = decimals_for(step / scale);
    format!("{:.*} {unit}", decimals, t / scale)
}

/// Tick label for a plain value, with an SI prefix when it is very large or very small.
pub fn value_label(v: f64, step: f64) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    let abs = v.abs();
    let (prefix, scale) = if abs >= 1e5 || abs < 1e-3 {
        let (prefix, scale) = SI_PREFIXES
            .iter()
            .find(|(_, scale)| abs >= *scale)
            .unwrap_or(SI_PREFIXES.last().expect("the prefix table is not empty"));
        (*prefix, *scale)
    } else {
        ("", 1.0)
    };
    format!("{:.*}{prefix}", decimals_for(step / scale), v / scale)
}

/// Draws background, grid, frame and tick labels around `area`. Returns nothing to paint by.
pub fn draw_axes(
    painter: &egui::Painter,
    area: &PlotArea,
    style: &AxisStyle,
    x_label: impl Fn(f64, f64) -> String,
    y_label: impl Fn(f64, f64) -> String,
) {
    let font = FontId::proportional(style.text_size);
    painter.rect_filled(area.rect, CornerRadius::ZERO, style.background);

    let grid = Stroke::new(1.0, style.grid);
    // About one x label per 90 points and one y label per 44 points of height.
    let x_step = nice_step(area.x_min, area.x_max, (area.rect.width() / 90.0) as usize);
    for t in linear_ticks(area.x_min, area.x_max, x_step) {
        let x = area.screen_pos(t, 0.0).x;
        painter.vline(x, area.rect.y_range(), grid);
        painter.text(
            pos2(x, area.rect.bottom() + 3.0),
            Align2::CENTER_TOP,
            x_label(t, x_step),
            font.clone(),
            style.text,
        );
    }

    let y_step = nice_step(area.y_min, area.y_max, (area.rect.height() / 44.0) as usize);
    for v in linear_ticks(area.y_min, area.y_max, y_step) {
        let y = area.screen_pos(0.0, v).y;
        painter.hline(area.rect.x_range(), y, grid);
        painter.text(
            pos2(area.rect.left() - 5.0, y),
            Align2::RIGHT_CENTER,
            y_label(v, y_step),
            font.clone(),
            style.text,
        );
    }

    painter.rect_stroke(
        area.rect,
        CornerRadius::ZERO,
        style.frame,
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_steps_are_one_two_five_times_a_power_of_ten() {
        assert_eq!(nice_step(0.0, 10.0, 10), 1.0);
        assert_eq!(nice_step(0.0, 10.0, 5), 2.0);
        assert_eq!(nice_step(0.0, 10.0, 2), 5.0);
        assert_eq!(nice_step(-0.003, 0.003, 4), 0.002);
        // An empty range still yields a usable step.
        assert!(nice_step(0.0, 0.0, 5) > 0.0);
    }

    #[test]
    fn ticks_stay_inside_their_range() {
        assert_eq!(linear_ticks(-1.0, 1.0, 0.5), [-1.0, -0.5, 0.0, 0.5, 1.0]);
        assert_eq!(linear_ticks(0.1, 0.9, 0.25), [0.25, 0.5, 0.75]);
        assert!(linear_ticks(1.0, 2.0, 0.0).is_empty());
    }

    #[test]
    fn time_labels_follow_the_spacing() {
        assert_eq!(time_label(3.0, 2.0), "3 s");
        // Half-second ticks read as milliseconds.
        assert_eq!(time_label(1.5, 0.5), "1500 ms");
        assert_eq!(time_label(-0.002, 0.001), "-2 ms");
        assert_eq!(time_label(1.23e-6, 1e-6), "1 µs");
        assert_eq!(time_label(0.0, 20.0), "0 s");
    }

    #[test]
    fn value_labels_switch_to_si_prefixes_for_extremes() {
        assert_eq!(value_label(0.0, 0.5), "0");
        assert_eq!(value_label(2500.0, 500.0), "2500");
        assert_eq!(value_label(250_000.0, 50_000.0), "250k");
        assert_eq!(value_label(0.00025, 0.0001), "250µ");
    }

    #[test]
    fn screen_and_data_coordinates_round_trip() {
        let area = PlotArea::new(
            Rect::from_min_size(pos2(10.0, 20.0), egui::vec2(200.0, 100.0)),
            (-1.0, 3.0),
            (0.0, 10.0),
        );
        let pos = area.screen_pos(1.0, 5.0);
        assert_eq!(pos, pos2(110.0, 70.0));
        let (x, y) = area.data_at(pos);
        assert!((x - 1.0).abs() < 1e-9 && (y - 5.0).abs() < 1e-9);
    }
}
