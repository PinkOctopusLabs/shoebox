//! Develop's volume deformation (EDITING-SPEC, Optics and geometry:
//! ViewPoint's wide-angle fix; `shoebox_develop::volume`): a projection
//! (Off, Cylindrical across or down, Spherical) with its Amount, and Faces,
//! which looks for faces on a thread of its own (as Smart Lighting's Face
//! does), keeps their boxes in the edit and makes them round by Face fix.
//! The boxes show on the photo while the section's Show faces is on.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Pos2, Rect};
use shoebox_develop::edit::{Volume, VolumeMode};

use super::develop::{edit, slider};
use super::local::{Mapping, outline};
use crate::app::ShoeboxApp;
use crate::jobs::PhotoId;

/// A face search: the photo, and its answer once it's there.
type Search = (PhotoId, Arc<Mutex<Option<Result<Vec<[f64; 4]>, String>>>>);

#[derive(Debug, Default)]
pub struct Tool {
    /// The faces' boxes drawn on the photo.
    pub show: bool,
    search: Option<Search>,
    /// What the last search found, if not faces.
    pub message: Option<String>,
}

impl Tool {
    /// A face search is running.
    pub fn searching(&self) -> bool {
        self.search.is_some()
    }
}

/// The section, after Transform.
pub fn section(app: &mut ShoeboxApp, ui: &mut egui::Ui, v: &Volume) {
    let reset = super::look::section(
        ui,
        "Volume deformation",
        *v != Volume::default(),
        false,
        |ui| body(app, ui, v),
    );
    if reset {
        app.develop.volume.show = false;
        edit(app, "volume", "Reset volume deformation".into(), |s| {
            s.geometry.volume = Volume::default();
        });
    }
}

fn body(app: &mut ShoeboxApp, ui: &mut egui::Ui, v: &Volume) {
    // How wide the lens is, which decides how much there is to fix.
    if let Some(r) = &app.develop.ready {
        let (w, h) = if r.meta.width > 0 {
            (r.meta.width, r.meta.height)
        } else {
            (r.src.rgb.width, r.src.rgb.height)
        };
        let shot = r.src.lens.as_deref().cloned().unwrap_or_default();
        let (_, fov) = shoebox_develop::volume::focal_px(&shot, w, h);
        let from = if shot.focal > 0.0 {
            format!("{:.0} mm", shot.focal)
        } else {
            "no focal length: taken as 24 mm".into()
        };
        ui.weak(format!("Field of view {fov:.0}° ({from})"));
    }
    ui.horizontal_wrapped(|ui| {
        for m in VolumeMode::ALL {
            let hint = match m {
                VolumeMode::Off => "Rectilinear, as the lens drew it",
                VolumeMode::Horizontal => {
                    "What's at the left and right keeps its width; verticals stay vertical"
                }
                VolumeMode::Vertical => {
                    "What's at the top and bottom keeps its height; horizontals stay level"
                }
                VolumeMode::Spherical => {
                    "Round everywhere, corners too; lines through the centre stay straight"
                }
            };
            if ui
                .selectable_label(v.mode == m, m.label())
                .on_hover_text(hint)
                .clicked()
                && v.mode != m
            {
                edit(
                    app,
                    "volume-mode",
                    format!("Volume deformation: {}", m.label()),
                    |s| s.geometry.volume.mode = m,
                );
            }
        }
    });
    egui::Grid::new("volume-grid")
        .num_columns(2)
        .show(ui, |ui| {
            if v.mode != VolumeMode::Off
                && let Some(a) = slider(ui, "Amount", v.amount, 0.0..=100.0, 100.0, 0)
            {
                edit(
                    app,
                    "volume-amount",
                    format!("Volume amount {a:.0}%"),
                    |s| {
                        s.geometry.volume.amount = a;
                    },
                );
            }
            if v.faces
                && let Some(a) = slider(ui, "Face fix", v.face_amount, 0.0..=100.0, 100.0, 0)
            {
                edit(
                    app,
                    "volume-faces-amount",
                    format!("Face fix {a:.0}%"),
                    |s| {
                        s.geometry.volume.face_amount = a;
                    },
                );
            }
        });
    ui.horizontal(|ui| {
        let mut on = v.faces;
        if ui
            .checkbox(&mut on, "Fix faces")
            .on_hover_text("Find the faces and make them round where they are")
            .changed()
        {
            let label = if on { "Fix faces on" } else { "Fix faces off" };
            edit(app, "volume-faces", label.into(), |s| {
                s.geometry.volume.faces = on;
            });
            app.develop.volume.show = on;
            // Looked for once, when first ticked.
            if on && v.boxes.is_empty() {
                find_faces(app, ui.ctx());
            }
        }
        if v.faces {
            let searching = app.develop.volume.searching();
            if ui
                .add_enabled(!searching, egui::Button::new("Find again"))
                .on_hover_text("Look for faces again")
                .clicked()
            {
                find_faces(app, ui.ctx());
            }
            ui.toggle_value(&mut app.develop.volume.show, "Show faces")
                .on_hover_text("Show the faces found on the photo");
            if searching {
                ui.spinner();
            }
        }
    });
    if let Some(m) = &app.develop.volume.message {
        ui.weak(m);
    } else if v.faces && !app.develop.volume.searching() {
        ui.weak(match v.boxes.len() {
            0 => "No faces".to_owned(),
            1 => "1 face".to_owned(),
            n => format!("{n} faces"),
        });
    }
}

/// Look for faces in the photo as the frame shows it, on a thread of its
/// own.
fn find_faces(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let Some(g) = app
        .develop
        .doc
        .as_ref()
        .map(|d| d.settings().geometry.clone())
    else {
        return;
    };
    let orientation = shoebox_develop::cpu::compose_orientation(
        super::develop::options(app).orientation,
        g.quarter,
        g.flip,
    );
    let st = &mut app.develop;
    let (Some(ready), Some(frame), Some(photo)) = (&st.ready, &st.frame, st.photo) else {
        return;
    };
    let (src, colour, display) = (ready.src.clone(), frame.colour, frame.display.clone());
    let slot = Arc::new(Mutex::new(None));
    st.volume.search = Some((photo, slot.clone()));
    st.volume.message = None;
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("volume-faces".into())
        .spawn(move || {
            let found = shoebox_develop::cpu::layers::faces(&src, &colour, &display, orientation);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        st.volume.search = None;
        st.volume.message = Some(format!("Couldn't look for faces: {e}"));
    }
}

/// Take a face search's answer, when it's there (every frame in Develop).
pub fn poll(app: &mut ShoeboxApp) {
    let Some((photo, slot)) = &app.develop.volume.search else {
        return;
    };
    let Some(found) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let photo = *photo;
    app.develop.volume.search = None;
    if app.develop.photo != Some(photo) {
        return;
    }
    match found {
        Ok(faces) => {
            let n = faces.len().min(shoebox_develop::volume::MAX_FACES);
            app.develop.volume.message = (n == 0).then(|| "No face found".to_owned());
            app.develop.last_step_done();
            let label = match n {
                1 => "Faces found: 1".to_owned(),
                n => format!("Faces found: {n}"),
            };
            edit(app, "volume-boxes", label, |s| {
                s.geometry.volume.boxes = faces.into_iter().take(n).collect();
            });
        }
        Err(e) => {
            tracing::debug!("face search: {e}");
            app.develop.volume.message = Some("Faces can't be found on this system".into());
        }
    }
}

/// The faces' boxes over the photo, where they show (through the lens
/// corrections and the deformation itself).
pub fn on_photo(app: &ShoeboxApp, painter: &egui::Painter, target: Rect) {
    if !app.develop.volume.show {
        return;
    }
    let Some(doc) = &app.develop.doc else { return };
    let v = &doc.settings().geometry.volume;
    if !v.faces || v.boxes.is_empty() {
        return;
    }
    let (Some(frame), Some(ready)) = (&app.develop.frame, &app.develop.ready) else {
        return;
    };
    let (w, h) = (ready.src.rgb.width, ready.src.rgb.height);
    let Some(map) = Mapping::new(frame, target, (w, h)) else {
        return;
    };
    let long = w.max(h).max(1) as f64;
    // A point found in the source → where it shows.
    let shown = |u: [f64; 2]| -> Pos2 {
        let (sx, sy) = ((u[0] * long - 0.5) as f32, (u[1] * long - 0.5) as f32);
        let (px, py) = frame.unlensed(w, h, sx, sy);
        map.to_screen([(f64::from(px) + 0.5) / long, (f64::from(py) + 0.5) / long])
    };
    for b in &v.boxes {
        // Each side in a few pieces, so a bent box shows bent.
        let corners = [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]];
        let mut pts = Vec::with_capacity(17);
        for k in 0..4 {
            let (p, q) = (corners[k], corners[(k + 1) % 4]);
            for t in 0..4 {
                let t = f64::from(t) / 4.0;
                pts.push(shown([p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]));
            }
        }
        pts.push(pts[0]);
        outline(painter, &pts, 1.5);
    }
}
