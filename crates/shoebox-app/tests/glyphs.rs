//! Every non-ASCII character in the app's string literals must exist in the
//! bundled fonts: egui panics on a missing glyph, so a stray arrow in a
//! button label would crash the app the first time it's drawn.

use eframe::egui;

const SOURCES: &[(&str, &str)] = &[
    ("app.rs", include_str!("../src/app.rs")),
    ("panels.rs", include_str!("../src/panels.rs")),
    ("views/mod.rs", include_str!("../src/views/mod.rs")),
    ("views/grid.rs", include_str!("../src/views/grid.rs")),
    ("views/masonry.rs", include_str!("../src/views/masonry.rs")),
    ("views/loupe.rs", include_str!("../src/views/loupe.rs")),
    (
        "views/filmstrip.rs",
        include_str!("../src/views/filmstrip.rs"),
    ),
];

/// Non-ASCII characters inside "…" on non-comment lines.
fn literal_chars(src: &str) -> Vec<char> {
    let mut out = Vec::new();
    for line in src.lines().filter(|l| !l.trim_start().starts_with("//")) {
        let mut inside = false;
        let mut prev = ' ';
        for ch in line.chars() {
            if ch == '"' && prev != '\\' {
                inside = !inside;
            } else if inside && !ch.is_ascii() && !out.contains(&ch) {
                out.push(ch);
            }
            prev = ch;
        }
    }
    out
}

#[test]
fn every_ui_glyph_is_in_the_fonts() {
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(Default::default(), |_| {}); // load fonts
    out.textures_delta.clear();
    let mut missing = Vec::new();
    for (file, src) in SOURCES {
        for ch in literal_chars(src) {
            for font in [
                egui::FontId::proportional(12.0),
                egui::FontId::monospace(12.0),
            ] {
                if !ctx.fonts_mut(|f| f.has_glyph(&font, ch)) {
                    missing.push(format!(
                        "{file}: {ch:?} (U+{:04X}) in {:?}",
                        ch as u32, font.family
                    ));
                }
            }
        }
    }
    missing.dedup();
    assert!(
        missing.is_empty(),
        "glyphs missing from egui's fonts:\n{}",
        missing.join("\n")
    );
}
