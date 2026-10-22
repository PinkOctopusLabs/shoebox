//! Develop's spot removal (EDITING-SPEC, Healing; `shoebox_develop::heal`):
//! with the tool on (Q), a click on the photo heals or clones a spot there
//! from a source found automatically; drag the spot or its source to move
//! it. Backspace removes the selected spot.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, vec2};
use shoebox_develop::edit::{Heal, Settings, Spot, SpotKind};

use super::develop::{edit, slider};
use super::local::Mapping;
use crate::app::ShoeboxApp;

/// The spot tool's state.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    pub active: bool,
    pub selected: Option<usize>,
    /// For new spots (and the selected one): kind, radius (long edges),
    /// feather (0–1) and opacity (0–100).
    pub kind: SpotKind,
    pub radius: f64,
    pub feather: f64,
    pub opacity: f64,
}

impl Default for Tool {
    fn default() -> Self {
        let s = Spot::default();
        Self {
            active: false,
            selected: None,
            kind: s.kind,
            radius: s.radius,
            feather: s.feather,
            opacity: s.opacity,
        }
    }
}

impl Tool {
    /// The tool's settings, but none of its place (kept from photo to
    /// photo).
    pub fn carried(&self) -> Self {
        Self {
            active: self.active,
            selected: None,
            ..*self
        }
    }
}

/// The Spot removal section.
pub fn section(app: &mut ShoeboxApp, ui: &mut egui::Ui, h: &Heal) {
    let n = h.spots.len();
    if app.develop.spots.selected.is_some_and(|i| i >= n) {
        app.develop.spots.selected = None;
    }
    let reset = super::look::section(ui, "Spot removal", !h.spots.is_empty(), false, |ui| {
        ui.horizontal(|ui| {
            let t = &mut app.develop.spots;
            let mut on = t.active;
            if ui
                .toggle_value(&mut on, "Spot tool")
                .on_hover_text("Click blemishes on the photo to remove them (Q)")
                .changed()
            {
                set_active(app, on);
            }
            let t = &mut app.develop.spots;
            ui.selectable_value(&mut t.kind, SpotKind::Heal, "Heal")
                .on_hover_text("Blend the copied texture into the surroundings");
            ui.selectable_value(&mut t.kind, SpotKind::Clone, "Clone")
                .on_hover_text("Copy it exactly");
        });
        let sel = app.develop.spots.selected;
        // A kind chosen with a spot selected changes that spot.
        if let Some(i) = sel
            && h.spots[i].kind != app.develop.spots.kind
        {
            let kind = app.develop.spots.kind;
            edit(app, "spot-kind", format!("Spot {}: {kind:?}", i + 1), |s| {
                if let Some(p) = s.modules.heal.spots.get_mut(i) {
                    p.kind = kind;
                }
            });
        }
        egui::Grid::new("spot-grid").num_columns(2).show(ui, |ui| {
            let t = app.develop.spots;
            let shown = sel.map(|i| &h.spots[i]);
            let radius = shown.map_or(t.radius, |s| s.radius);
            let feather = shown.map_or(t.feather, |s| s.feather);
            let opacity = shown.map_or(t.opacity, |s| s.opacity);
            if let Some(v) = slider(ui, "Size", radius * 200.0, 0.5..=20.0, 2.0, 1) {
                app.develop.spots.radius = v / 200.0;
                change(app, sel, "spot-size", format!("Spot size {v:.1}"), |s| {
                    s.radius = v / 200.0
                });
            }
            if let Some(v) = slider(ui, "Feather", feather * 100.0, 0.0..=100.0, 30.0, 0) {
                app.develop.spots.feather = v / 100.0;
                change(
                    app,
                    sel,
                    "spot-feather",
                    format!("Spot feather {v:.0}"),
                    |s| s.feather = v / 100.0,
                );
            }
            if let Some(v) = slider(ui, "Opacity", opacity, 0.0..=100.0, 100.0, 0) {
                app.develop.spots.opacity = v;
                change(
                    app,
                    sel,
                    "spot-opacity",
                    format!("Spot opacity {v:.0}"),
                    |s| s.opacity = v,
                );
            }
        });
        ui.horizontal(|ui| {
            ui.weak(match n {
                0 => "No spots".to_owned(),
                1 => "1 spot".to_owned(),
                n => format!("{n} spots"),
            });
            if let Some(i) = sel
                && ui.button("Remove spot").clicked()
            {
                remove(app, i);
            }
        });
    });
    if reset {
        app.develop.spots.selected = None;
        edit(app, "spot-remove", "Reset spots".into(), |s| {
            s.modules.heal = Heal::default()
        });
    }
}

/// Change the selected spot (if one is).
fn change(
    app: &mut ShoeboxApp,
    sel: Option<usize>,
    key: &'static str,
    label: String,
    f: impl FnOnce(&mut Spot),
) {
    if let Some(i) = sel {
        edit(app, key, label, |s| {
            if let Some(p) = s.modules.heal.spots.get_mut(i) {
                f(p);
            }
        });
    }
}

/// Turn the tool on or off (Q).
pub fn set_active(app: &mut ShoeboxApp, on: bool) {
    let t = &mut app.develop.spots;
    t.active = on;
    if !on {
        t.selected = None;
    } else {
        // One tool on the photo at a time.
        app.develop.local.selected = None;
        app.develop.points.picking = false;
        app.develop.picking = false;
    }
}

/// Remove spot `i` (Backspace, or Remove spot).
pub fn remove(app: &mut ShoeboxApp, i: usize) {
    app.develop.last_step_done();
    edit(app, "spot-remove", format!("Remove spot {}", i + 1), |s| {
        if i < s.modules.heal.spots.len() {
            s.modules.heal.spots.remove(i);
        }
    });
    app.develop.spots.selected = None;
}

#[derive(Clone, Copy, PartialEq)]
enum End {
    Dest,
    Source,
}

/// The spots over the photo, and clicks and drags on them.
pub fn on_photo(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    photo: &egui::Response,
    painter: &egui::Painter,
    target: Rect,
) {
    if !app.develop.spots.active || app.develop.read_only {
        return;
    }
    let Some(doc) = &app.develop.doc else { return };
    let s: Settings = doc.settings().clone();
    let (Some(frame), Some(ready)) = (app.develop.frame.clone(), &app.develop.ready) else {
        return;
    };
    let src = (ready.src.rgb.width, ready.src.rgb.height);
    let Some(map) = Mapping::new(&frame, target, src) else {
        return;
    };
    let spots = &s.modules.heal.spots;
    let sel = app.develop.spots.selected;
    let mut moved: Option<(usize, End, [f64; 2])> = None;
    let mut select = None;
    for (i, sp) in spots.iter().enumerate() {
        let on = sel == Some(i);
        let c = map.to_screen(sp.dest);
        let r = (sp.radius as f32 * map.scale_at(sp.dest)).max(3.0);
        let ends: &[End] = if on {
            &[End::Dest, End::Source]
        } else {
            &[End::Dest]
        };
        if on {
            let sc = map.to_screen(sp.source);
            ring(painter, sc, r, 1.0);
            let d = c - sc;
            if d.length() > 2.0 * r {
                let dir = d.normalized();
                let line = [sc + dir * r, c - dir * r];
                painter.line_segment(line, Stroke::new(3.0, Color32::from_black_alpha(120)));
                painter.line_segment(line, Stroke::new(1.0, Color32::WHITE));
            }
        }
        ring(painter, c, r, if on { 2.0 } else { 1.0 });
        for &end in ends {
            let at = if end == End::Dest {
                c
            } else {
                map.to_screen(sp.source)
            };
            let rect = Rect::from_center_size(at, vec2(2.0 * r, 2.0 * r));
            let id = ui.scope_id().with(("spot", i, end == End::Source));
            let resp = ui
                .interact(rect, id, egui::Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::Grab);
            if resp.clicked() || resp.drag_started() {
                select = Some(i);
            }
            if resp.drag_started() {
                app.develop.last_step_done();
            }
            if resp.dragged()
                && let Some(p) = resp.interact_pointer_pos()
            {
                // The point under the pointer, less where it was grabbed.
                let grab = resp.drag_delta();
                let now = map.to_units(p);
                let before = map.to_units(p - grab);
                let was = if end == End::Dest { sp.dest } else { sp.source };
                moved = Some((
                    i,
                    end,
                    [was[0] + now[0] - before[0], was[1] + now[1] - before[1]],
                ));
            }
        }
    }
    if let Some(i) = select {
        app.develop.spots.selected = Some(i);
        app.develop.spots.kind = spots[i].kind;
    }
    if let Some((i, end, to)) = moved {
        let label = format!(
            "Move spot {}{}",
            i + 1,
            if end == End::Source { " source" } else { "" }
        );
        edit(app, "spot-move", label, |s| {
            if let Some(p) = s.modules.heal.spots.get_mut(i) {
                match end {
                    End::Dest => p.dest = to,
                    End::Source => p.source = to,
                }
            }
        });
        return;
    }
    // The tool's circle where a click would put a spot.
    if let Some(p) = photo.hover_pos().filter(|p| target.contains(*p)) {
        let u = map.to_units(p);
        let r = (app.develop.spots.radius as f32 * map.scale_at(u)).max(3.0);
        painter.circle_stroke(p, r, Stroke::new(1.0, Color32::from_white_alpha(180)));
    }
    if select.is_none()
        && photo.clicked()
        && let Some(p) = photo.interact_pointer_pos().filter(|p| target.contains(*p))
    {
        add(app, &map, p, &s);
    }
}

fn ring(painter: &egui::Painter, c: Pos2, r: f32, width: f32) {
    painter.circle_stroke(
        c,
        r,
        Stroke::new(width + 2.0, Color32::from_black_alpha(140)),
    );
    painter.circle_stroke(c, r, Stroke::new(width, Color32::WHITE));
}

/// A new spot at `p`, its source found automatically.
fn add(app: &mut ShoeboxApp, map: &Mapping<'_>, p: Pos2, s: &Settings) {
    let Some(ready) = &app.develop.ready else {
        return;
    };
    let t = app.develop.spots;
    let dest = map.to_units(p);
    // Found on the photo as healed so far, so it doesn't copy a blemish
    // already dealt with.
    let healed = ready.src.prepared(s);
    let img = healed.as_deref().unwrap_or(&ready.src.rgb);
    let source = shoebox_develop::heal::find_source(img, dest, t.radius);
    let spot = Spot {
        kind: t.kind,
        dest,
        source,
        radius: t.radius,
        feather: t.feather,
        opacity: t.opacity,
    };
    let n = s.modules.heal.spots.len();
    app.develop.last_step_done();
    let label = match t.kind {
        SpotKind::Heal => "Heal spot",
        SpotKind::Clone => "Clone spot",
    };
    edit(app, "spot-add", label.into(), |s| {
        s.modules.heal.spots.push(spot)
    });
    app.develop.spots.selected = Some(n);
}
