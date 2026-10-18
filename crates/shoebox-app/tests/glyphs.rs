//! Every non-ASCII character in the app's string literals must exist in the
//! bundled fonts: egui panics on a missing glyph, so a stray arrow in a
//! button label would crash the app the first time it's drawn.

use eframe::egui;

/// Every `.rs` file under `src/`, so new views are covered automatically.
fn sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs")
                && let Ok(src) = std::fs::read_to_string(&p)
            {
                out.push((p.display().to_string(), src));
            }
        }
    }
    assert!(out.len() > 5, "found the sources");
    out
}

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
    shoebox_app::icons::install(&ctx);
    let mut out = ctx.run_ui(Default::default(), |_| {}); // load fonts
    out.textures_delta.clear();
    let mut missing = Vec::new();
    for (file, src) in sources() {
        for ch in literal_chars(&src) {
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

/// The bars' icons are in the bundled icon font.
#[test]
fn every_icon_is_in_the_font() {
    let ctx = egui::Context::default();
    shoebox_app::icons::install(&ctx);
    let mut out = ctx.run_ui(Default::default(), |_| {});
    out.textures_delta.clear();
    for icon in shoebox_app::icons::ALL {
        for ch in icon.chars() {
            assert!(
                ctx.fonts_mut(|f| f.has_glyph(&egui::FontId::proportional(12.0), ch)),
                "U+{:04X} missing",
                ch as u32
            );
        }
    }
}
