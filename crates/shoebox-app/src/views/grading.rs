//! Develop's colour grading (EDITING-SPEC, module 10): one wheel at a time
//! for shadows, midtones, highlights or the whole photo (drag for hue and
//! saturation; double-click clears it), its luminance, and how the ranges
//! blend and balance (`shoebox_develop::cpu::look::Grading`).

use eframe::egui::{self, Color32, Pos2, Sense, Stroke, vec2};
use shoebox_develop::edit::{ColourGrading, Wheel};

use super::develop::{edit, slider};
use crate::app::ShoeboxApp;

const RANGES: [(&str, &str); 4] = [
    ("Shadows", "cg-s"),
    ("Midtones", "cg-m"),
    ("Highlights", "cg-h"),
    ("Global", "cg-g"),
];

fn wheel_mut(g: &mut ColourGrading, i: usize) -> &mut Wheel {
    match i {
        0 => &mut g.shadows,
        1 => &mut g.midtones,
        2 => &mut g.highlights,
        _ => &mut g.global,
    }
}

fn wheel(g: &ColourGrading, i: usize) -> Wheel {
    match i {
        0 => g.shadows,
        1 => g.midtones,
        2 => g.highlights,
        _ => g.global,
    }
}

/// An Oklab colour as an 8-bit sRGB colour for the screen (Björn
/// Ottosson's matrices), clipped.
fn oklab(l: f32, a: f32, b: f32) -> Color32 {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bl = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    let enc = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (e * 255.0 + 0.5) as u8
    };
    Color32::from_rgb(enc(r), enc(g), enc(bl))
}

/// The Colour grading section.
pub fn section(app: &mut ShoeboxApp, ui: &mut egui::Ui, g: &ColourGrading) {
    let changed = *g != ColourGrading::default();
    let reset = super::look::section(ui, "Colour grading", changed, false, |ui| {
        let id = ui.make_persistent_id("grading-range");
        let mut range: usize = ui.data(|d| d.get_temp(id)).unwrap_or(1);
        ui.horizontal(|ui| {
            for (i, (name, _)) in RANGES.iter().enumerate() {
                let w = wheel(g, i);
                let mut text = egui::RichText::new(*name);
                if w.saturation > 0.0 || w.luminance != 0.0 {
                    text = text.strong();
                }
                if ui.selectable_label(range == i, text).clicked() {
                    range = i;
                }
            }
        });
        ui.data_mut(|d| d.insert_temp(id, range));
        let w = wheel(g, range);
        let (name, key) = RANGES[range];
        if let Some(nw) = draw_wheel(ui, w) {
            edit(
                app,
                key,
                format!("{name} {:.0}° {:.0}", nw.hue, nw.saturation),
                |s| *wheel_mut(&mut s.modules.colour_grading, range) = nw,
            );
        }
        egui::Grid::new("grading-grid")
            .num_columns(2)
            .show(ui, |ui| {
                if let Some(v) = slider(ui, "Hue", w.hue, 0.0..=360.0, 0.0, 0) {
                    edit(app, key, format!("{name} hue {v:.0}°"), |s| {
                        wheel_mut(&mut s.modules.colour_grading, range).hue = v
                    });
                }
                if let Some(v) = slider(ui, "Saturation", w.saturation, 0.0..=100.0, 0.0, 0) {
                    edit(app, key, format!("{name} saturation {v:.0}"), |s| {
                        wheel_mut(&mut s.modules.colour_grading, range).saturation = v
                    });
                }
                if let Some(v) = slider(ui, "Luminance", w.luminance, -100.0..=100.0, 0.0, 0) {
                    edit(app, key, format!("{name} luminance {v:+.0}"), |s| {
                        wheel_mut(&mut s.modules.colour_grading, range).luminance = v
                    });
                }
                if let Some(v) = slider(ui, "Blending", g.blending, 0.0..=100.0, 50.0, 0) {
                    edit(app, "cg-blend", format!("Grading blending {v:.0}"), |s| {
                        s.modules.colour_grading.blending = v
                    });
                }
                if let Some(v) = slider(ui, "Balance", g.balance, -100.0..=100.0, 0.0, 0) {
                    edit(app, "cg-balance", format!("Grading balance {v:+.0}"), |s| {
                        s.modules.colour_grading.balance = v
                    });
                }
            });
    });
    if reset {
        edit(app, "cg", "Reset colour grading".into(), |s| {
            s.modules.colour_grading = ColourGrading::default()
        });
    }
}

/// The wheel: hue round it (Oklab's, as the grading uses), saturation
/// out from the middle. Returns the wheel moved to where it's dragged or
/// clicked; a double-click takes its colour away.
fn draw_wheel(ui: &mut egui::Ui, w: Wheel) -> Option<Wheel> {
    let d = ui.available_width().clamp(120.0, 180.0);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), d), Sense::click_and_drag());
    let c = rect.center();
    let r = d / 2.0 - 6.0;
    let painter = ui.painter_at(rect);
    // A fan of triangles from a grey middle to the hues round the rim.
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(c, oklab(0.72, 0.0, 0.0));
    let n = 72;
    for i in 0..=n {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        let at = c + vec2(t.cos(), -t.sin()) * r;
        mesh.colored_vertex(at, oklab(0.72, 0.13 * t.cos(), 0.13 * t.sin()));
        if i > 0 {
            mesh.add_triangle(0, i as u32, i as u32 + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
    painter.circle_stroke(c, r, Stroke::new(1.0, Color32::from_black_alpha(120)));
    let t = (w.hue as f32).to_radians();
    let at: Pos2 = c + vec2(t.cos(), -t.sin()) * (r * (w.saturation as f32 / 100.0));
    painter.circle(
        at,
        6.0,
        Color32::TRANSPARENT,
        Stroke::new(2.0, Color32::WHITE),
    );
    painter.circle_stroke(at, 7.5, Stroke::new(1.0, Color32::from_black_alpha(160)));
    if resp.double_clicked() {
        return Some(Wheel {
            saturation: 0.0,
            ..w
        });
    }
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let v = p - c;
        let sat = (v.length() / r * 100.0).clamp(0.0, 100.0).round();
        let hue = (-v.y).atan2(v.x).to_degrees().rem_euclid(360.0).round();
        let nw = Wheel {
            hue: f64::from(hue),
            saturation: f64::from(sat),
            ..w
        };
        return (nw != w).then_some(nw);
    }
    None
}
