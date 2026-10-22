//! Develop's Lens Blur (EDITING-SPEC, Phase 4 additions;
//! `shoebox_develop::cpu::lens_blur`): on and off, the depth model's
//! download, Amount, the focus (a click on the photo picks the depth
//! there; without one, the nearest things), the focus range, the bokeh's
//! shape, highlight boost, and the depth map shown.

use eframe::egui::{self, Color32, Rect};
use shoebox_develop::edit::{Bokeh, LensBlur};

use super::develop::{edit, slider};
use crate::app::ShoeboxApp;

/// Lens Blur's state in Develop.
#[derive(Debug, Default)]
pub struct Tool {
    /// The next click on the photo sets the focus there.
    pub picking: bool,
    /// The depth map shown instead of the photo (the focus range tinted).
    pub show_depth: bool,
}

/// Wait for a click on the photo to set the focus, or stop.
pub fn set_picking(app: &mut ShoeboxApp, on: bool) {
    app.develop.lens_blur.picking = on && !app.develop.read_only;
    if app.develop.lens_blur.picking {
        // One tool on the photo at a time.
        super::spots::set_active(app, false);
        let st = &mut app.develop;
        st.local.selected = None;
        st.local.placing = false;
        st.local.picking = false;
        st.points.picking = false;
        st.picking = false;
    }
}

/// The Lens Blur section.
pub fn section(app: &mut ShoeboxApp, ui: &mut egui::Ui, lb: &LensBlur) {
    let changed = *lb != LensBlur::default();
    let reset = super::look::section(ui, "Lens Blur", changed, false, |ui| {
        let mut on = lb.enabled;
        if ui
            .checkbox(&mut on, "Apply")
            .on_hover_text(
                "A shallow depth of field from a depth map of the photo \
                 (Depth Anything V2 Small, Apache 2.0, run on this Mac)",
            )
            .changed()
        {
            edit(
                app,
                "lb-on",
                format!("Lens Blur {}", if on { "on" } else { "off" }),
                |s| s.modules.lens_blur.enabled = on,
            );
        }
        if !lb.enabled {
            return;
        }
        let model = app.depth.clone();
        if !model.ready() {
            egui::Grid::new("lb-dl").num_columns(1).show(ui, |ui| {
                crate::models::download_ui(
                    ui,
                    &model,
                    "the depth model",
                    "Depth Anything V2 Small, once, from its authors' Hugging Face page",
                );
            });
        } else if let Some(ready) = &app.develop.ready {
            let (finding, failed) = ready.src.cache.depthing();
            if finding {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Finding depth…");
                });
            } else if let Some(e) = failed {
                ui.weak(format!("Couldn't find the depth: {e}"));
            }
        }
        egui::Grid::new("lb-grid").num_columns(2).show(ui, |ui| {
            if let Some(v) = slider(ui, "Amount", lb.amount, 0.0..=100.0, 50.0, 0) {
                edit(app, "lb-amount", format!("Lens Blur amount {v:.0}"), |s| {
                    s.modules.lens_blur.amount = v;
                });
            }
            if let Some(v) = slider(ui, "Focus range", lb.range, 0.0..=100.0, 10.0, 0) {
                edit(app, "lb-range", format!("Focus range {v:.0}"), |s| {
                    s.modules.lens_blur.range = v;
                });
            }
            if let Some(v) = slider(ui, "Highlight boost", lb.boost, 0.0..=100.0, 0.0, 0) {
                edit(app, "lb-boost", format!("Highlight boost {v:.0}"), |s| {
                    s.modules.lens_blur.boost = v;
                });
            }
            ui.label("Bokeh");
            let mut bokeh = lb.bokeh;
            egui::ComboBox::from_id_salt("lb-bokeh")
                .selected_text(name(bokeh))
                .show_ui(ui, |ui| {
                    for b in [Bokeh::Circle, Bokeh::Blades] {
                        ui.selectable_value(&mut bokeh, b, name(b));
                    }
                });
            ui.end_row();
            if bokeh != lb.bokeh {
                edit(app, "lb-bokeh", format!("Bokeh {}", name(bokeh)), |s| {
                    s.modules.lens_blur.bokeh = bokeh;
                });
            }
        });
        ui.horizontal(|ui| {
            let picking = app.develop.lens_blur.picking;
            if ui
                .add(egui::Button::selectable(picking, "Set focus"))
                .on_hover_text("Click the photo where it should be sharp")
                .clicked()
            {
                set_picking(app, !picking);
            }
            if ui
                .add_enabled(lb.focus.is_some(), egui::Button::new("Nearest"))
                .on_hover_text("Focus on the nearest things again")
                .clicked()
            {
                edit(app, "lb-focus", "Focus on the nearest".into(), |s| {
                    s.modules.lens_blur.focus = None;
                });
            }
            let mut show = app.develop.lens_blur.show_depth;
            if ui
                .add(egui::Button::selectable(show, "Show depth"))
                .on_hover_text("Show the depth map: near light, far dark, in focus tinted")
                .clicked()
            {
                show = !show;
                app.develop.lens_blur.show_depth = show;
            }
        });
        ui.weak(
            "A local adjustment's Depth slider moves what it covers nearer or farther, \
             where the depth map is wrong.",
        );
    });
    if reset {
        app.develop.lens_blur = Tool::default();
        edit(app, "lens-blur", "Reset Lens Blur".into(), |s| {
            s.modules.lens_blur = LensBlur::default();
        });
    }
}

fn name(b: Bokeh) -> &'static str {
    match b {
        Bokeh::Circle => "Circle",
        Bokeh::Blades => "Six blades",
    }
}

/// On the photo: the focus point, and (while picking) a click sets it.
pub fn on_photo(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    photo: &egui::Response,
    painter: &egui::Painter,
    target: Rect,
) {
    let Some(doc) = &app.develop.doc else { return };
    let lb = doc.settings().modules.lens_blur.clone();
    if !lb.enabled {
        return;
    }
    let (Some(frame), Some(ready)) = (app.develop.frame.clone(), &app.develop.ready) else {
        return;
    };
    let src = (ready.src.rgb.width, ready.src.rgb.height);
    let Some(map) = super::local::Mapping::new(&frame, target, src) else {
        return;
    };
    let t = &app.develop.lens_blur;
    // The focus point, while choosing it or looking at the depth.
    if (t.picking || t.show_depth)
        && let Some(at) = lb.focus
    {
        let p = map.to_screen(at);
        if target.contains(p) {
            painter.circle_stroke(
                p,
                9.0,
                egui::Stroke::new(3.0, Color32::from_black_alpha(140)),
            );
            painter.circle_stroke(p, 9.0, egui::Stroke::new(1.5, Color32::WHITE));
            painter.circle_filled(p, 2.0, Color32::WHITE);
        }
    }
    if !t.picking || app.develop.read_only {
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
    app.develop.lens_blur.picking = false;
    let u = map.to_units(pos);
    let at = [(u[0] * 1e4).round() / 1e4, (u[1] * 1e4).round() / 1e4];
    app.develop.last_step_done();
    edit(app, "lb-focus", "Focus set".into(), |s| {
        s.modules.lens_blur.focus = Some(at);
    });
}
