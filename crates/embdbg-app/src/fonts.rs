//! The fonts the app embeds, so it looks the same on every machine.

use std::sync::Arc;

use egui::epaint::text::{LayoutJob, TextFormat};
use egui::{FontData, FontDefinitions, FontFamily, FontId, FontTweak, RichText};

pub const NOTO_FONT: &str = "noto-sans-sc";
pub const ICON_OUTLINE_FONT: &str = "tabler-icons";
pub const ICON_FILLED_FONT: &str = "tabler-icons-filled";
pub const ICON_FAMILY: &str = "embdbg-icons";
pub const ICON_FILLED_FAMILY: &str = "embdbg-icons-filled";

static NOTO_SANS_SC: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSC-Regular.subset.ttf");
static TABLER_OUTLINE: &[u8] =
    include_bytes!("../../../assets/fonts/TablerIcons-Outline.subset.ttf");
static TABLER_FILLED: &[u8] = include_bytes!("../../../assets/fonts/TablerIcons-Filled.subset.ttf");

/// Tabler's glyphs occupy 80% of the em box; scaling them up makes an icon line up with text of the same size.
const ICON_SCALE: f32 = 1.25;

/// Installs the embedded fonts: Noto Sans SC (OFL 1.1) for text, Tabler Icons (MIT) for icons.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        NOTO_FONT.to_owned(),
        Arc::new(FontData::from_static(NOTO_SANS_SC)),
    );
    // First for proportional text, so Latin letters and hanzi come from the same typeface.
    // After Hack for monospace, which keeps code and hex dumps in a coding font and only borrows hanzi.
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .expect("egui ships a proportional family")
        .insert(0, NOTO_FONT.to_owned());
    fonts
        .families
        .get_mut(&FontFamily::Monospace)
        .expect("egui ships a monospace family")
        .push(NOTO_FONT.to_owned());

    let tweak = || FontTweak {
        scale: ICON_SCALE,
        ..Default::default()
    };
    fonts.font_data.insert(
        ICON_OUTLINE_FONT.to_owned(),
        Arc::new(FontData::from_static(TABLER_OUTLINE).tweak(tweak())),
    );
    fonts.font_data.insert(
        ICON_FILLED_FONT.to_owned(),
        Arc::new(FontData::from_static(TABLER_FILLED).tweak(tweak())),
    );
    // Icon fonts get families of their own: their codepoints overlap, and text must never hit them by accident.
    fonts.families.insert(
        FontFamily::Name(ICON_FAMILY.into()),
        vec![ICON_OUTLINE_FONT.to_owned()],
    );
    fonts.families.insert(
        FontFamily::Name(ICON_FILLED_FAMILY.into()),
        vec![ICON_FILLED_FONT.to_owned()],
    );

    ctx.set_fonts(fonts);
}

/// An outline icon (see [`crate::icons::outline`]) drawn at `size` points, like text.
pub fn icon(code: &str, size: f32) -> RichText {
    RichText::new(code)
        .family(FontFamily::Name(ICON_FAMILY.into()))
        .size(size)
}

/// A label made of an outline icon followed by text, for buttons and menu items.
///
/// Both parts are laid out as one run of text, so the label sizes itself like any other and
/// the icon inherits the text color of the widget that shows it.
pub fn icon_label(icon_code: &str, text: impl Into<String>, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        icon_code,
        // Icons carry a little empty space on their right; close part of it.
        size * 0.1,
        TextFormat {
            font_id: FontId::new(size, FontFamily::Name(ICON_FAMILY.into())),
            ..Default::default()
        },
    );
    job.append(
        &text.into(),
        0.0,
        TextFormat {
            font_id: FontId::proportional(size),
            ..Default::default()
        },
    );
    job
}
