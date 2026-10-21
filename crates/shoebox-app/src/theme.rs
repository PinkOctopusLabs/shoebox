//! The app's look in one place: text sizes and spacing (egui's defaults are
//! tight for a desktop app: 12.5 pt text on 18 px rows), and the Text size
//! setting, a zoom on all of it.

use eframe::egui::{self, Margin, TextStyle, vec2};
use shoebox_core::settings::TextSize;

/// Set the text sizes and spacing (both themes).
pub fn install(ctx: &egui::Context) {
    // ⌘+ / ⌘− / ⌘0 change the saved setting instead (`size_key`).
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    ctx.all_styles_mut(|s| {
        for (style, size) in [
            (TextStyle::Small, 11.5),
            (TextStyle::Body, 13.5),
            (TextStyle::Button, 13.5),
            (TextStyle::Monospace, 12.5),
            (TextStyle::Heading, 19.0),
        ] {
            if let Some(f) = s.text_styles.get_mut(&style) {
                f.size = size;
            }
        }
        let sp = &mut s.spacing;
        sp.item_spacing = vec2(8.0, 6.0);
        sp.button_padding = vec2(7.0, 3.0);
        sp.interact_size = vec2(44.0, 22.0);
        sp.icon_width = 16.0;
        sp.icon_width_inner = 9.0;
        sp.icon_spacing = 6.0;
        sp.window_margin = Margin::same(12);
        sp.menu_margin = Margin::same(8);
        sp.combo_height = 260.0;
    });
}

/// The zoom for a text size.
pub fn zoom(size: TextSize) -> f32 {
    match size {
        TextSize::Small => 0.9,
        TextSize::Medium => 1.0,
        TextSize::Large => 1.15,
        TextSize::Larger => 1.3,
    }
}

pub fn label(size: TextSize) -> &'static str {
    match size {
        TextSize::Small => "Small",
        TextSize::Medium => "Medium",
        TextSize::Large => "Large",
        TextSize::Larger => "Larger",
    }
}

/// ⌘+ / ⌘− / ⌘0 this frame: the size they ask for, from `size`.
pub fn size_key(ctx: &egui::Context, size: TextSize) -> Option<TextSize> {
    ctx.input_mut(|i| {
        let cmd = egui::Modifiers::COMMAND;
        if i.consume_key(cmd, egui::Key::Plus) || i.consume_key(cmd, egui::Key::Equals) {
            Some(size.step(true))
        } else if i.consume_key(cmd, egui::Key::Minus) {
            Some(size.step(false))
        } else if i.consume_key(cmd, egui::Key::Num0) {
            Some(TextSize::Medium)
        } else {
            None
        }
    })
    .filter(|&s| s != size)
}

/// Keep the zoom in step with the setting.
pub fn apply(ctx: &egui::Context, size: TextSize) {
    let z = zoom(size);
    if (ctx.zoom_factor() - z).abs() > f32::EPSILON {
        ctx.set_zoom_factor(z);
    }
}
