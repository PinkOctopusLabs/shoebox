//! Smart Lighting's spot weighted mode (EDITING-SPEC, Auto: "biases it
//! towards a box you drag or a detected face"): Uniform or Spot weighted
//! under the Tone section's sliders; in Spot weighted, the box drawn over
//! the photo with a handle at each corner (drag inside it to move it,
//! outside it to draw a new one), and Face, which puts it on the largest
//! face Vision finds. The box is kept as the edit stores it: in the stored
//! image, in units of its long edge.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Pos2, Rect, vec2};
use shoebox_develop::edit::SmartLighting;

use super::develop::edit;
use super::local::{Mapping, knob, outline};
use crate::app::ShoeboxApp;
use crate::jobs::PhotoId;

/// A face search: the photo, and its answer once it's there.
type Search = (PhotoId, Arc<Mutex<Option<Result<Vec<[f64; 4]>, String>>>>);

/// The spot's state.
#[derive(Debug, Default)]
pub struct Tool {
    /// The box shown over the photo, to drag.
    pub editing: bool,
    /// What's being dragged: a corner (0 top left, 1 top right, 2 bottom
    /// left, 3 bottom right, in the stored image), the whole box, or a new
    /// one from where the drag began.
    drag: Option<Drag>,
    /// A face search running.
    search: Option<Search>,
    /// What the last search found, if not a face.
    pub message: Option<String>,
}

impl Tool {
    /// A face search is running.
    pub fn searching(&self) -> bool {
        self.search.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Corner(usize),
    Move([f64; 2]),
    New([f64; 2]),
}

/// Corner `k` of box `b`.
fn corner(b: &[f64; 4], k: usize) -> [f64; 2] {
    [
        if k & 1 == 0 { b[0] } else { b[2] },
        if k & 2 == 0 { b[1] } else { b[3] },
    ]
}

/// The photo's size in long edges (`[w, h]`, the longer one 1), in the
/// stored image.
fn units(app: &ShoeboxApp) -> [f64; 2] {
    let Some(r) = &app.develop.ready else {
        return [1.0, 2.0 / 3.0];
    };
    let (w, h) = (r.src.rgb.width as f64, r.src.rgb.height as f64);
    let long = w.max(h).max(1.0);
    [w / long, h / long]
}

/// A new spot: the middle of the photo, 30% of its long edge across.
fn middle(units: [f64; 2]) -> [f64; 4] {
    let (cx, cy, r) = (units[0] / 2.0, units[1] / 2.0, 0.15);
    [cx - r, (cy - r).max(0.0), cx + r, (cy + r).min(units[1])]
}

fn set_spot(app: &mut ShoeboxApp, label: &str, spot: Option<[f64; 4]>) {
    edit(app, "smart-spot", label.to_owned(), |s| {
        s.modules.smart_lighting.spot = spot;
    });
}

/// Uniform or Spot weighted, and Face, under the Smart Lighting sliders.
pub fn controls(app: &mut ShoeboxApp, ui: &mut egui::Ui, sl: &SmartLighting) {
    ui.horizontal(|ui| {
        let spot = sl.spot.is_some();
        if ui
            .selectable_label(!spot, "Uniform")
            .on_hover_text("Even all over the photo")
            .clicked()
            && spot
        {
            app.develop.smart.editing = false;
            app.develop.last_step_done();
            set_spot(app, "Smart Lighting uniform", None);
        }
        if ui
            .selectable_label(spot, "Spot weighted")
            .on_hover_text("Favour a box you drag on the photo (a face, say)")
            .clicked()
            && !spot
        {
            app.develop.smart.editing = true;
            app.develop.local.selected = None;
            app.develop.last_step_done();
            let b = middle(units(app));
            set_spot(app, "Smart Lighting spot weighted", Some(b));
        }
        if spot {
            let t = &mut app.develop.smart;
            ui.toggle_value(&mut t.editing, "Edit box")
                .on_hover_text("Show the box on the photo to drag it");
            let searching = t.search.is_some();
            let face = ui
                .add_enabled(!searching, egui::Button::new("Face"))
                .on_hover_text("Put the box on the largest face");
            if face.clicked() {
                find_face(app, ui.ctx());
            }
            if searching {
                ui.spinner();
            }
        }
    });
    if let Some(m) = &app.develop.smart.message {
        ui.weak(m);
    }
}

/// Look for faces in the photo as the frame shows it, on a thread of its
/// own.
fn find_face(app: &mut ShoeboxApp, ctx: &egui::Context) {
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
    st.smart.search = Some((photo, slot.clone()));
    st.smart.message = None;
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("faces".into())
        .spawn(move || {
            let found = shoebox_develop::cpu::layers::faces(&src, &colour, &display, orientation);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        st.smart.search = None;
        st.smart.message = Some(format!("Couldn't look for faces: {e}"));
    }
}

/// Take a face search's answer, when it's there (every frame in Develop).
pub fn poll(app: &mut ShoeboxApp) {
    let Some((photo, slot)) = &app.develop.smart.search else {
        return;
    };
    let Some(found) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let photo = *photo;
    app.develop.smart.search = None;
    if app.develop.photo != Some(photo) {
        return;
    }
    match found {
        Ok(faces) => match faces.first() {
            Some(b) => {
                app.develop.smart.message = None;
                app.develop.smart.editing = true;
                app.develop.last_step_done();
                set_spot(app, "Smart Lighting spot on a face", Some(*b));
            }
            None => app.develop.smart.message = Some("No face found".into()),
        },
        Err(e) => {
            tracing::debug!("face search: {e}");
            app.develop.smart.message = Some("Faces can't be found on this system".into());
        }
    }
}

/// The spot's box over the photo, and what dragging does.
pub fn on_photo(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    photo: &egui::Response,
    painter: &egui::Painter,
    target: Rect,
) {
    let t = &app.develop.smart;
    if !t.editing || app.develop.spots.active || app.develop.local.selected.is_some() {
        return;
    }
    let Some(doc) = &app.develop.doc else { return };
    let Some(b) = doc.settings().modules.smart_lighting.spot else {
        return;
    };
    let (Some(frame), Some(ready)) = (app.develop.frame.clone(), &app.develop.ready) else {
        return;
    };
    let src = (ready.src.rgb.width, ready.src.rgb.height);
    let Some(map) = Mapping::new(&frame, target, src) else {
        return;
    };
    let pts: Vec<Pos2> = [0, 1, 3, 2, 0]
        .iter()
        .map(|&k| map.to_screen(corner(&b, k)))
        .collect();
    outline(painter, &pts, 1.5);
    let read_only = app.develop.read_only;
    let mut new: Option<[f64; 4]> = None;
    // Inside the box moves it (under the corners, which come after).
    let inside = Rect::from_points(&pts[..4]).shrink(super::local::HANDLE * 1.5);
    if !read_only && inside.is_positive() {
        let r = ui
            .interact(
                inside,
                ui.scope_id().with("smart-spot-move"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Move);
        if r.drag_started()
            && let Some(pos) = r.interact_pointer_pos()
        {
            app.develop.smart.drag = Some(Drag::Move(map.to_units(pos)));
            app.develop.last_step_done();
        }
        if r.dragged()
            && let Some(pos) = r.interact_pointer_pos()
            && let Some(Drag::Move(from)) = app.develop.smart.drag
        {
            // Moved whole, kept on the photo.
            let (u, un) = (map.to_units(pos), units(app));
            let d = [
                (u[0] - from[0]).max(-b[0]).min(un[0] - b[2]),
                (u[1] - from[1]).max(-b[1]).min(un[1] - b[3]),
            ];
            new = Some([b[0] + d[0], b[1] + d[1], b[2] + d[0], b[3] + d[1]]);
            app.develop.smart.drag = Some(Drag::Move(u));
        }
    }
    for k in 0..4 {
        let at = map.to_screen(corner(&b, k));
        knob(painter, at, app.develop.smart.drag == Some(Drag::Corner(k)));
        if read_only {
            continue;
        }
        let size = vec2(super::local::HANDLE * 3.0, super::local::HANDLE * 3.0);
        let r = ui
            .interact(
                Rect::from_center_size(at, size),
                ui.scope_id().with(("smart-spot", k)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab);
        if r.drag_started() {
            app.develop.smart.drag = Some(Drag::Corner(k));
            app.develop.last_step_done();
        }
        if r.dragged()
            && let Some(pos) = r.interact_pointer_pos()
        {
            // The corner it is now (see below).
            let k = match app.develop.smart.drag {
                Some(Drag::Corner(c)) => c,
                _ => k,
            };
            let u = map.to_units(pos);
            let mut nb = b;
            nb[if k & 1 == 0 { 0 } else { 2 }] = u[0];
            nb[if k & 2 == 0 { 1 } else { 3 }] = u[1];
            new = Some(nb);
            // Dragged past the opposite corner, it becomes the other one
            // (the box is kept with its corners in order).
            let mut k2 = k;
            if (k & 1 == 0 && u[0] > b[2]) || (k & 1 == 1 && u[0] < b[0]) {
                k2 ^= 1;
            }
            if (k & 2 == 0 && u[1] > b[3]) || (k & 2 == 2 && u[1] < b[1]) {
                k2 ^= 2;
            }
            app.develop.smart.drag = Some(Drag::Corner(k2));
        }
    }
    // Outside it (fitted, not zoomed: a drag there pans), a new box.
    if !read_only && app.develop.zoom.is_none() {
        if photo.drag_started()
            && let Some(pos) = photo.interact_pointer_pos()
            && target.contains(pos)
        {
            app.develop.smart.drag = Some(Drag::New(map.to_units(pos)));
            app.develop.last_step_done();
        }
        if photo.dragged()
            && let Some(pos) = photo.interact_pointer_pos()
            && let Some(Drag::New(from)) = app.develop.smart.drag
        {
            let u = map.to_units(pos);
            new = Some([from[0], from[1], u[0], u[1]]);
        }
    }
    if !ui.input(|i| i.pointer.any_down()) {
        app.develop.smart.drag = None;
    }
    if let Some(nb) = new {
        // Kept to the photo, corners in order.
        let un = units(app);
        let (x0, x1) = (nb[0].min(nb[2]), nb[0].max(nb[2]));
        let (y0, y1) = (nb[1].min(nb[3]), nb[1].max(nb[3]));
        let nb = [
            x0.clamp(0.0, un[0]),
            y0.clamp(0.0, un[1]),
            x1.clamp(0.0, un[0]),
            y1.clamp(0.0, un[1]),
        ];
        set_spot(app, "Move Smart Lighting spot", Some(nb));
    }
}
