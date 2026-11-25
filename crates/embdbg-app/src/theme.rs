//! Colors, spacing and type sizes of the UI.

use std::collections::BTreeMap;

use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle, ThemePreference, Visuals};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Window background.
pub const BG_WINDOW: Color32 = rgb(0x13, 0x15, 0x1a);
/// Panels and tab bodies.
pub const BG_PANEL: Color32 = rgb(0x1a, 0x1d, 0x23);
/// Inputs and plot backgrounds.
pub const BG_WIDGET: Color32 = rgb(0x21, 0x25, 0x2c);
/// Hover highlights.
pub const BG_HOVER: Color32 = rgb(0x2b, 0x30, 0x39);
pub const BORDER: Color32 = rgb(0x30, 0x36, 0x41);

pub const FG: Color32 = rgb(0xd6, 0xdb, 0xe3);
pub const FG_DIM: Color32 = rgb(0x8a, 0x92, 0xa0);
pub const ACCENT: Color32 = rgb(0x4f, 0x8f, 0xff);
pub const SUCCESS: Color32 = rgb(0x3f, 0xb9, 0x7f);
pub const WARNING: Color32 = rgb(0xe6, 0xb3, 0x4c);
pub const DANGER: Color32 = rgb(0xe5, 0x62, 0x5e);

/// Grid lines of the plots.
pub const GRID: Color32 = rgb(0x2a, 0x2f, 0x38);
/// Measurement cursors.
pub const CURSOR_A: Color32 = rgb(0xff, 0xb0, 0x4c);
pub const CURSOR_B: Color32 = rgb(0x5c, 0xe0, 0xd8);

/// The classic sixteen ANSI colours, tuned to read on a dark terminal.
pub const ANSI: [Color32; 16] = [
    rgb(0x00, 0x00, 0x00),
    rgb(0xcd, 0x31, 0x31),
    rgb(0x0d, 0xbc, 0x79),
    rgb(0xe5, 0xe5, 0x10),
    rgb(0x24, 0x72, 0xc8),
    rgb(0xbc, 0x3f, 0xbc),
    rgb(0x11, 0xa8, 0xcd),
    rgb(0xe5, 0xe5, 0xe5),
    rgb(0x66, 0x66, 0x66),
    rgb(0xf1, 0x4c, 0x4c),
    rgb(0x23, 0xd1, 0x8b),
    rgb(0xf5, 0xf5, 0x43),
    rgb(0x3b, 0x8e, 0xea),
    rgb(0xd6, 0x70, 0xd6),
    rgb(0x29, 0xb8, 0xdb),
    rgb(0xff, 0xff, 0xff),
];

/// Channel colors: the Okabe–Ito color-blind-safe palette. Its black is replaced by a light
/// gray, which stays visible on a dark background.
pub const SERIES: [Color32; 8] = [
    rgb(0x56, 0xb4, 0xe9),
    rgb(0xe6, 0x9f, 0x00),
    rgb(0x00, 0x9e, 0x73),
    rgb(0xd5, 0x5e, 0x00),
    rgb(0xcc, 0x79, 0xa7),
    rgb(0xf0, 0xe4, 0x42),
    rgb(0x00, 0x72, 0xb2),
    rgb(0xe6, 0xe6, 0xe6),
];

/// Color of channel `index`; channels beyond the palette repeat it.
pub fn series_color(index: usize) -> Color32 {
    SERIES[index % SERIES.len()]
}

/// The spacing scale is a 4 point grid.
pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;

/// Corner radius of windows, menus and buttons, in whole points.
pub const RADIUS: u8 = 6;

pub fn install(ctx: &egui::Context) {
    ctx.set_theme(ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(SPACE_2, SPACE_1 + 2.0);
        style.spacing.button_padding = egui::vec2(10.0, SPACE_1);
        style.spacing.interact_size.y = 24.0;
        style.visuals = visuals();
        style.text_styles = text_styles();
        style.animation_time = 0.1;
    });
}

fn visuals() -> Visuals {
    let mut visuals = Visuals::dark();
    visuals.override_text_color = Some(FG);
    visuals.hyperlink_color = ACCENT;
    visuals.faint_bg_color = BG_PANEL;
    visuals.extreme_bg_color = BG_WIDGET;
    visuals.window_fill = BG_PANEL;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(RADIUS);
    visuals.panel_fill = BG_WINDOW;
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = DANGER;

    visuals.widgets.noninteractive.weak_bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, FG);

    visuals.widgets.inactive.weak_bg_fill = BG_WIDGET;
    visuals.widgets.inactive.bg_fill = BG_WIDGET;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, FG);

    visuals.widgets.hovered.weak_bg_fill = BG_HOVER;
    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, FG);

    visuals.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.25);
    visuals.widgets.active.bg_fill = ACCENT.gamma_multiply(0.25);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, FG);

    let radius = CornerRadius::same(RADIUS - 2);
    visuals.widgets.noninteractive.corner_radius = radius;
    visuals.widgets.inactive.corner_radius = radius;
    visuals.widgets.hovered.corner_radius = radius;
    visuals.widgets.active.corner_radius = radius;
    visuals
}

fn text_styles() -> BTreeMap<TextStyle, FontId> {
    [
        (TextStyle::Small, FontId::proportional(11.5)),
        (TextStyle::Body, FontId::proportional(13.5)),
        (TextStyle::Button, FontId::proportional(13.5)),
        (TextStyle::Heading, FontId::proportional(16.0)),
        (TextStyle::Monospace, FontId::monospace(12.5)),
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_colors_repeat_after_the_palette() {
        assert_eq!(series_color(0), SERIES[0]);
        assert_eq!(series_color(8), SERIES[0]);
        assert_eq!(series_color(9), SERIES[1]);
    }

    #[test]
    fn foreground_is_readable_on_every_background() {
        // Relative luminance, as egui's default dark theme roughly has it (about 0.77 vs 0.02).
        let luminance = |c: Color32| {
            let linear = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        };
        for background in [BG_WINDOW, BG_PANEL, BG_WIDGET, BG_HOVER] {
            let contrast = (luminance(FG) + 0.05) / (luminance(background) + 0.05);
            assert!(contrast >= 7.0, "contrast {contrast} on {background:?}");
        }
    }
}
