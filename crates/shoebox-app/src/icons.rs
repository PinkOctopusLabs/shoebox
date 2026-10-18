//! Icons for the bars: Phosphor (regular weight, MIT; licence in
//! `assets/Phosphor-LICENSE.txt`), bundled as a font so they draw as text.

use eframe::egui::{self, RichText};

const FONT: &[u8] = include_bytes!("../assets/Phosphor.ttf");

pub const SIDEBAR: &str = "\u{EC24}";
pub const UNDO: &str = "\u{E08A}";
pub const REDO: &str = "\u{E08C}";
pub const SEARCH: &str = "\u{E30C}";
pub const FILTER: &str = "\u{E266}";
pub const CONTENT: &str = "\u{E6A2}";
pub const INFO: &str = "\u{E2CE}";
pub const SETTINGS: &str = "\u{E272}";
pub const GRID: &str = "\u{E464}";
pub const MASONRY: &str = "\u{E6D6}";
pub const LOUPE: &str = "\u{E2CC}";
pub const COMPARE: &str = "\u{E870}";
pub const SURVEY: &str = "\u{E296}";
pub const TIMELINE: &str = "\u{E7B4}";
pub const MAP: &str = "\u{E31A}";
pub const SORT_UP: &str = "\u{E444}";
pub const SORT_DOWN: &str = "\u{E446}";
pub const PICK: &str = "\u{E244}";
pub const REJECT: &str = "\u{E4F8}";
pub const FACES: &str = "\u{E6FC}";

/// Every icon (the glyph test checks each is in the font).
pub const ALL: [&str; 20] = [
    SIDEBAR, UNDO, REDO, SEARCH, FILTER, CONTENT, INFO, SETTINGS, GRID, MASONRY, LOUPE, COMPARE,
    SURVEY, TIMELINE, MAP, SORT_UP, SORT_DOWN, PICK, REJECT, FACES,
];

/// Add the icon font after the text font, so ordinary text keeps its own
/// glyphs and the icons (private-use code points) come from Phosphor.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("phosphor".into(), egui::FontData::from_static(FONT).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(keys) = fonts.families.get_mut(&family) {
            keys.insert(keys.len().min(1), "phosphor".into());
        }
    }
    ctx.set_fonts(fonts);
}

/// A frameless icon button, highlighted when `selected`. `name` is what
/// VoiceOver (and the UI tests) call it; `hint` shows on hover.
pub fn button(
    ui: &mut egui::Ui,
    icon: &str,
    name: &str,
    hint: &str,
    selected: bool,
) -> egui::Response {
    let enabled = ui.is_enabled();
    let r = ui
        .add(
            egui::Button::new(RichText::new(icon).size(17.0))
                .selected(selected)
                .frame_when_inactive(selected)
                .min_size(egui::vec2(28.0, 24.0)),
        )
        .on_hover_text(hint);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, name));
    r
}
