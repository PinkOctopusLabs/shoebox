//! Develop's Guided upright (EDITING-SPEC, Geometry: the person draws two
//! to four lines that must become straight; Lightroom's Guided). While it's
//! on, the photo shows without rotation or perspective (as cropping shows
//! the whole frame), so the screen maps straight onto the upright frame the
//! guides live in. Drag on the photo to draw a guide (up to four), drag an
//! end to move it, click one to select it; Backspace or Remove guide takes
//! it away. Each change solves the rotation and perspective again
//! (`shoebox_develop::upright::solve_guided`) as one step, a drag folding
//! into it. Done, Enter or Escape leave it.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, vec2};
use shoebox_develop::cpu;
use shoebox_develop::upright::{self, Correction, Lines, Segment};

use super::develop::{chip, edit, options};
use crate::app::ShoeboxApp;

/// A guide's ends, as fractions of the upright frame.
type Guide = [[f64; 2]; 2];

/// The guided tool's state (the guides themselves are in the edit).
#[derive(Debug, Clone, Copy, Default)]
pub struct Tool {
    pub active: bool,
    pub selected: Option<usize>,
    /// The guide being drawn: its far end follows the pointer.
    drawing: Option<usize>,
}

/// An end's knob (points).
const KNOB: f32 = 5.0;

/// Guided upright on or off.
pub fn set_active(app: &mut ShoeboxApp, on: bool) {
    if on {
        if app.develop.read_only || app.develop.doc.is_none() {
            return;
        }
        // One tool on the photo at a time.
        super::develop::finish_crop(app, true);
        super::spots::set_active(app, false);
        let st = &mut app.develop;
        st.local.selected = None;
        st.points.picking = false;
        st.picking = false;
        st.zoom = None;
    }
    app.develop.guided = Tool {
        active: on,
        ..Default::default()
    };
    app.develop.last_step_done();
}

/// The upright frame's size (the proxy's, before rotation and perspective).
fn frame_size(app: &ShoeboxApp) -> Option<(f64, f64)> {
    let o = options(app);
    let st = &app.develop;
    let (ready, doc) = (st.ready.as_ref()?, st.doc.as_ref()?);
    let g = &doc.settings().geometry;
    let (w, h) = cpu::frame_size(ready.src.rgb.width, ready.src.rgb.height, o.orientation, g);
    Some((w as f64, h as f64))
}

/// The guides as the solver takes them: pixels from the frame's centre.
fn lines(guides: &[Guide], (w, h): (f64, f64)) -> Lines {
    let px = |u: [f64; 2]| [(u[0] - 0.5) * w, (u[1] - 0.5) * h];
    Lines {
        segments: guides
            .iter()
            .map(|g| Segment {
                a: px(g[0]),
                b: px(g[1]),
            })
            .collect(),
        width: w,
        height: h,
    }
}

/// Change the guides, and with them the rotation (with the largest crop
/// that fits, as the Angle slider does) and perspective, as one step;
/// changes under the same `key` fold into it until `last_step_done`.
fn change(app: &mut ShoeboxApp, key: &'static str, label: &str, f: impl FnOnce(&mut Vec<Guide>)) {
    let (Some(size), Some(doc)) = (frame_size(app), &app.develop.doc) else {
        return;
    };
    let mut guides = doc.settings().geometry.guides.clone();
    let before = upright::solve_guided(&lines(&guides, size));
    f(&mut guides);
    // Under two guides, no correction: none at all if the guides gave the
    // one there was, else the photo stays as it is.
    let c = upright::solve_guided(&lines(&guides, size))
        .or_else(|| before.map(|_| Correction::default()));
    let crop = c.map(|c| cpu::max_crop(size.0, size.1, c.rotate));
    edit(app, key, label.into(), |s| {
        let g = &mut s.geometry;
        g.guides = guides;
        if let (Some(c), Some(crop)) = (c, crop) {
            g.rotate = c.rotate;
            g.crop = crop;
            g.transform.vertical = c.vertical;
            g.transform.horizontal = c.horizontal;
            g.transform.constrain = true;
        }
    });
}

/// Remove guide `i` (Backspace, or Remove guide).
pub fn remove(app: &mut ShoeboxApp, i: usize) {
    app.develop.last_step_done();
    change(app, "guide-remove", "Remove guide", |g| {
        if i < g.len() {
            g.remove(i);
        }
    });
    app.develop.last_step_done();
    app.develop.guided.selected = None;
}

/// Remove every guide (and the correction they gave).
fn clear(app: &mut ShoeboxApp) {
    app.develop.last_step_done();
    change(app, "guide-remove", "Clear guides", Vec::clear);
    app.develop.last_step_done();
    app.develop.guided.selected = None;
}

/// How far `p` is from the segment `a`–`b` (points).
fn distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let d = b - a;
    let t = ((p - a).dot(d) / d.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + d * t).distance(p)
}

/// The guides over the photo (`target`: where the upright frame is drawn),
/// and drawing, dragging and clicking them. `photo` is the view's own
/// response (a drag there draws a new guide).
pub fn on_photo(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    photo: &egui::Response,
    painter: &egui::Painter,
    target: Rect,
) {
    if !app.develop.guided.active || app.develop.read_only {
        return;
    }
    let Some(doc) = &app.develop.doc else { return };
    let guides = doc.settings().geometry.guides.clone();
    let t = &mut app.develop.guided;
    if t.selected.is_some_and(|i| i >= guides.len()) {
        t.selected = None;
    }
    let t = *t;
    let to_screen = |u: [f64; 2]| {
        target.min + vec2(u[0] as f32 * target.width(), u[1] as f32 * target.height())
    };
    let to_frame = |p: Pos2| {
        [
            f64::from(((p.x - target.left()) / target.width()).clamp(0.0, 1.0)),
            f64::from(((p.y - target.top()) / target.height()).clamp(0.0, 1.0)),
        ]
    };
    for (i, g) in guides.iter().enumerate() {
        let ends = [to_screen(g[0]), to_screen(g[1])];
        let colour = if t.selected == Some(i) {
            Color32::from_rgb(70, 140, 255)
        } else {
            Color32::WHITE
        };
        painter.line_segment(ends, Stroke::new(4.0, Color32::from_black_alpha(140)));
        painter.line_segment(ends, Stroke::new(2.0, colour));
    }
    // The ends: drag to move.
    let mut moved: Option<(usize, usize, [f64; 2])> = None;
    let mut select = None;
    for (i, g) in guides.iter().enumerate() {
        for (end, u) in g.iter().enumerate() {
            let at = to_screen(*u);
            let rect = Rect::from_center_size(at, vec2(KNOB * 3.5, KNOB * 3.5));
            let r = ui
                .interact(
                    rect,
                    ui.scope_id().with(("guide-end", i, end)),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab);
            painter.circle(
                at,
                KNOB,
                if t.selected == Some(i) {
                    Color32::from_rgb(70, 140, 255)
                } else {
                    Color32::WHITE
                },
                Stroke::new(1.5, Color32::from_black_alpha(180)),
            );
            if r.clicked() || r.drag_started() {
                select = Some(i);
            }
            if r.drag_started() {
                app.develop.last_step_done();
            }
            if r.dragged()
                && let Some(p) = r.interact_pointer_pos()
            {
                moved = Some((i, end, to_frame(p)));
            }
        }
    }
    if let Some(i) = select {
        app.develop.guided.selected = Some(i);
    }
    if let Some((i, end, u)) = moved {
        change(app, "guide", "Move guide", |g| {
            if let Some(x) = g.get_mut(i) {
                x[end] = u;
            }
        });
        return;
    }
    if select.is_some() {
        return;
    }
    // A drag elsewhere draws a new guide.
    if let Some(n) = t.drawing {
        if photo.dragged()
            && let Some(p) = photo.interact_pointer_pos()
        {
            let u = to_frame(p);
            change(app, "guide", "Draw guide", |g| {
                if let Some(x) = g.get_mut(n) {
                    x[1] = u;
                }
            });
        }
        if !photo.dragged() {
            // Let go: a guide too short to mean anything goes again (the
            // step folds back to where it began).
            app.develop.guided.drawing = None;
            let short = guides
                .get(n)
                .is_none_or(|g| to_screen(g[0]).distance(to_screen(g[1])) < 8.0);
            if short {
                change(app, "guide", "Draw guide", |g| {
                    if n < g.len() {
                        g.remove(n);
                    }
                });
                app.develop.guided.selected = None;
            }
            app.develop.last_step_done();
        }
    } else if photo.drag_started()
        && let Some(from) = ui
            .input(|i| i.pointer.press_origin())
            .filter(|p| target.contains(*p))
    {
        if guides.len() < upright::GUIDES {
            let a = to_frame(from);
            let b = photo.interact_pointer_pos().map_or(a, to_frame);
            let n = guides.len();
            app.develop.last_step_done();
            app.develop.guided.drawing = Some(n);
            app.develop.guided.selected = Some(n);
            change(app, "guide", "Draw guide", |g| g.push([a, b]));
        } else {
            app.notice = Some("Four guides at most: remove one to draw another".into());
        }
    } else if photo.clicked()
        && let Some(p) = photo.interact_pointer_pos()
    {
        // A click on a guide selects it; elsewhere, nothing is selected.
        app.develop.guided.selected = guides
            .iter()
            .position(|g| distance(p, to_screen(g[0]), to_screen(g[1])) < 6.0);
    }
    chip(
        painter,
        target.left_bottom() + vec2(10.0, -30.0),
        "Draw two to four lines along things that should be vertical or level",
    );
}

/// The bar over the photo while guiding: remove, clear, done.
pub fn bar(app: &mut ShoeboxApp, ui: &mut egui::Ui, at: Rect) {
    let n = app
        .develop
        .doc
        .as_ref()
        .map_or(0, |d| d.settings().geometry.guides.len());
    let selected = app.develop.guided.selected;
    ui.scope_builder(egui::UiBuilder::new().max_rect(at), |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Guided upright").color(Color32::WHITE));
            if ui
                .add_enabled(selected.is_some(), egui::Button::new("Remove guide"))
                .on_hover_text("Take away the selected guide (Backspace)")
                .clicked()
                && let Some(i) = selected
            {
                remove(app, i);
            }
            if ui
                .add_enabled(n > 0, egui::Button::new("Clear"))
                .on_hover_text("Take away every guide, and the correction they gave")
                .clicked()
            {
                clear(app);
            }
            if ui
                .button("Done")
                .on_hover_text("Leave Guided upright (Enter or Escape)")
                .clicked()
            {
                set_active(app, false);
            }
        });
    });
}
