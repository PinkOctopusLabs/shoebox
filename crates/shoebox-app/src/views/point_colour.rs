//! Develop's Point Color (EDITING-SPEC, Tools beyond PhotoLab): pick a
//! colour on the photo, then shift its hue, saturation and luminance
//! within a range of it (`shoebox_develop::cpu::look::Point`). "Show
//! affected" greys everything the selected point doesn't reach.

use eframe::egui::{self, Color32, Rect, Sense, vec2};
use shoebox_develop::cpu;
use shoebox_develop::edit::{COLOUR_POINTS, ColourPoint, PointColour};

use super::develop::{edit, slider};
use crate::app::ShoeboxApp;

/// Point Color's state in Develop.
#[derive(Debug, Default)]
pub struct Tool {
    /// The next click on the photo adds a point there.
    pub picking: bool,
    pub selected: Option<usize>,
    /// Show what the selected point reaches.
    pub show: bool,
}

const KEYS: [&str; 6] = [
    "pc-hue", "pc-sat", "pc-lum", "pc-hue-r", "pc-sat-r", "pc-lum-r",
];

/// The Point Color section.
pub fn section(app: &mut ShoeboxApp, ui: &mut egui::Ui, pc: &PointColour) {
    let n = pc.points.len();
    if app.develop.points.selected.is_some_and(|i| i >= n) {
        app.develop.points.selected = n.checked_sub(1);
    }
    let changed = *pc != PointColour::default();
    let reset = super::look::section(ui, "Point colour", changed, false, |ui| {
        ui.horizontal(|ui| {
            let mut picking = app.develop.points.picking;
            if ui
                .add_enabled(
                    n < COLOUR_POINTS,
                    egui::Button::selectable(picking, "Pick colour"),
                )
                .on_hover_text("Click a colour on the photo to change it")
                .clicked()
            {
                picking = !picking;
                app.develop.points.picking = picking;
            }
            let mut show = app.develop.points.show;
            if ui
                .add_enabled(
                    app.develop.points.selected.is_some(),
                    egui::Button::selectable(show, "Show affected"),
                )
                .on_hover_text("Grey out what the selected colour doesn't reach")
                .clicked()
            {
                show = !show;
                app.develop.points.show = show;
            }
        });
        // The points as swatches of the colours picked.
        let mut select = None;
        ui.horizontal_wrapped(|ui| {
            for (i, p) in pc.points.iter().enumerate() {
                let on = app.develop.points.selected == Some(i);
                let (rect, r) = ui.allocate_exact_size(vec2(26.0, 20.0), Sense::click());
                ui.painter().rect_filled(rect, 3.0, swatch(app, p));
                if on {
                    ui.painter().rect_stroke(
                        rect.expand(2.0),
                        4.0,
                        egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                        egui::StrokeKind::Outside,
                    );
                }
                let r = r.on_hover_text(format!("Colour {}", i + 1));
                r.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::Role::Button,
                        true,
                        on,
                        format!("Colour {}", i + 1),
                    )
                });
                if r.clicked() {
                    select = Some(i);
                }
            }
        });
        if let Some(i) = select {
            app.develop.points.selected = Some(i);
            app.develop.last_step_done();
        }
        let Some(i) = app.develop.points.selected else {
            if n == 0 {
                ui.weak("No colours picked.");
            }
            return;
        };
        let p = pc.points[i].clone();
        let mut remove = false;
        egui::Grid::new("point-colour")
            .num_columns(2)
            .show(ui, |ui| {
                let rows = [
                    ("Hue", p.hue_shift, -100.0, 0.0),
                    ("Saturation", p.saturation, -100.0, 0.0),
                    ("Luminance", p.luminance, -100.0, 0.0),
                    ("Hue range", p.hue_range, 0.0, 50.0),
                    ("Saturation range", p.saturation_range, 0.0, 50.0),
                    ("Luminance range", p.luminance_range, 0.0, 50.0),
                ];
                for (k, (name, v, lo, default)) in rows.into_iter().enumerate() {
                    if let Some(v) = slider(ui, name, v, lo..=100.0, default, 0) {
                        let label = if k < 3 {
                            format!("Colour {} {} {v:+.0}", i + 1, name.to_lowercase())
                        } else {
                            format!("Colour {} {} {v:.0}", i + 1, name.to_lowercase())
                        };
                        edit(app, KEYS[k], label, |s| {
                            if let Some(p) = s.modules.point_colour.points.get_mut(i) {
                                let f = match k {
                                    0 => &mut p.hue_shift,
                                    1 => &mut p.saturation,
                                    2 => &mut p.luminance,
                                    3 => &mut p.hue_range,
                                    4 => &mut p.saturation_range,
                                    _ => &mut p.luminance_range,
                                };
                                *f = v;
                            }
                        });
                    }
                }
            });
        if ui.button("Remove colour").clicked() {
            remove = true;
        }
        if remove {
            app.develop.last_step_done();
            edit(app, "pc-remove", format!("Remove colour {}", i + 1), |s| {
                if i < s.modules.point_colour.points.len() {
                    s.modules.point_colour.points.remove(i);
                }
            });
            app.develop.points.selected = i.checked_sub(1).or((n > 1).then_some(0));
        }
    });
    if reset {
        app.develop.points.selected = None;
        edit(app, "pc-remove", "Reset point colour".into(), |s| {
            s.modules.point_colour = PointColour::default();
        });
    }
}

/// A point's picked colour as the screen shows it.
fn swatch(app: &ShoeboxApp, p: &ColourPoint) -> Color32 {
    let Some(f) = &app.develop.frame else {
        return Color32::GRAY;
    };
    let lin = cpu::look::from_oklch(&f.look, [p.lightness as f32, p.chroma as f32, p.hue as f32]);
    let d = cpu::display_pixel(&f.display, lin);
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Color32::from_rgb(q(d[0]), q(d[1]), q(d[2]))
}

/// Picking: a click on the photo adds a point with the colour there.
pub fn on_photo(app: &mut ShoeboxApp, ui: &mut egui::Ui, photo: &egui::Response, target: Rect) {
    if !app.develop.points.picking || app.develop.read_only {
        return;
    }
    if photo.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if !photo.clicked() {
        return;
    }
    let Some(pos) = photo.interact_pointer_pos().filter(|p| target.contains(*p)) else {
        return;
    };
    app.develop.points.picking = false;
    let (Some(f), Some(ready)) = (&app.develop.frame, &app.develop.ready) else {
        return;
    };
    // The colour stage's input there, averaged over 3×3 output pixels.
    let x = (pos.x - target.left()) / target.width() * f.width as f32 - 0.5;
    let y = (pos.y - target.top()) / target.height() * f.height as f32 - 0.5;
    let mut sum = [0.0f32; 3];
    for dy in -1..=1 {
        for dx in -1..=1 {
            let p = cpu::look_input(&ready.src.rgb, f, x + dx as f32, y + dy as f32);
            for c in 0..3 {
                sum[c] += p[c] / 9.0;
            }
        }
    }
    let [l, c, h] = cpu::look::oklch(&f.look, sum);
    let point = ColourPoint {
        lightness: f64::from((l * 1000.0).round() / 1000.0),
        chroma: f64::from((c * 1000.0).round() / 1000.0),
        hue: f64::from(h.round()),
        ..Default::default()
    };
    let n = app
        .develop
        .doc
        .as_ref()
        .map_or(0, |d| d.settings().modules.point_colour.points.len());
    if n >= COLOUR_POINTS {
        return;
    }
    app.develop.last_step_done();
    edit(app, "pc-add", "Colour picked".into(), |s| {
        s.modules.point_colour.points.push(point)
    });
    app.develop.points.selected = Some(n);
}
