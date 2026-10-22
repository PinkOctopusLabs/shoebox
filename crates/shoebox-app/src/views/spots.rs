//! Develop's spot removal (EDITING-SPEC, Healing; `shoebox_develop::heal`),
//! red-eye and object removal: with the tool on (Q), a click on the photo
//! heals or clones a spot there from a source found automatically, or
//! corrects a red (or pet) eye; drag the spot or its source to move it.
//! Remove paints over an object (one drag, one removal; ⌥ with a removal
//! selected erases from it) and the inpainting model fills it. Backspace
//! removes the selected spot.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, vec2};
use shoebox_develop::edit::{Heal, Settings, Spot, SpotKind};
use shoebox_develop::heal::remove;

use super::develop::{edit, slider};
use super::local::Mapping;
use crate::app::ShoeboxApp;

/// The spot tool's state.
#[derive(Debug, Clone)]
pub struct Tool {
    pub active: bool,
    pub selected: Option<usize>,
    /// For new spots (and the selected one): kind, radius (long edges),
    /// feather (0–1) and opacity (0–100).
    pub kind: SpotKind,
    pub radius: f64,
    pub feather: f64,
    pub opacity: f64,
    pub darken: f64,
    /// Removing: the brush's radius (long edges; larger than a spot's).
    pub brush: f64,
    /// Removing: the stroke being painted (units), and whether it erases
    /// from the selected removal.
    pub painting: Vec<[f64; 2]>,
    pub erasing: bool,
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
            darken: s.darken,
            brush: BRUSH,
            painting: Vec::new(),
            erasing: false,
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
            painting: Vec::new(),
            erasing: false,
            ..self.clone()
        }
    }
}

/// The removal brush's radius to begin with (long edges).
const BRUSH: f64 = 0.025;

/// Which kinds a selected spot can turn into each other.
fn family(k: SpotKind) -> u8 {
    match k {
        SpotKind::Heal | SpotKind::Clone => 0,
        SpotKind::RedEye | SpotKind::PetEye => 1,
        SpotKind::Remove => 2,
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
        ui.horizontal(|ui| {
            let t = &mut app.develop.spots;
            ui.selectable_value(&mut t.kind, SpotKind::RedEye, "Red eye")
                .on_hover_text("Click an eye: the red goes from its pupil");
            ui.selectable_value(&mut t.kind, SpotKind::PetEye, "Pet eye")
                .on_hover_text("Click an animal's eye: its glowing pupil darkens");
            ui.selectable_value(&mut t.kind, SpotKind::Remove, "Remove")
                .on_hover_text(
                    "Paint over an object (a person, a power line) and it's filled \
                     from its surroundings",
                );
        });
        // A kind chosen with a spot selected changes that spot (heal and
        // clone into each other, the eyes likewise); otherwise it's for
        // the next one.
        let mut sel = app.develop.spots.selected;
        if let Some(i) = sel
            && family(h.spots[i].kind) != family(app.develop.spots.kind)
        {
            app.develop.spots.selected = None;
            sel = None;
        }
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
            let t = app.develop.spots.clone();
            let shown = sel.map(|i| &h.spots[i]);
            let removing = t.kind == SpotKind::Remove;
            let own = if removing { t.brush } else { t.radius };
            let radius = shown.map_or(own, |s| s.radius);
            let feather = shown.map_or(t.feather, |s| s.feather);
            let opacity = shown.map_or(t.opacity, |s| s.opacity);
            let darken = shown.map_or(t.darken, |s| s.darken);
            if t.kind.is_eye()
                && let Some(v) = slider(ui, "Darken", darken, 0.0..=100.0, 50.0, 0)
            {
                app.develop.spots.darken = v;
                change(app, sel, "spot-darken", format!("Darken {v:.0}"), |s| {
                    s.darken = v
                });
            }
            let (size, default) = if removing {
                ("Brush size", BRUSH * 200.0)
            } else {
                ("Size", 2.0)
            };
            if let Some(v) = slider(ui, size, radius * 200.0, 0.5..=20.0, default, 1) {
                if removing {
                    app.develop.spots.brush = v / 200.0;
                } else {
                    app.develop.spots.radius = v / 200.0;
                }
                change(app, sel, "spot-size", format!("Spot size {v:.1}"), |s| {
                    s.radius = v / 200.0;
                    for k in &mut s.strokes {
                        k.radius = v / 200.0;
                    }
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
        if app.develop.spots.kind == SpotKind::Remove {
            removal_status(app, ui);
        }
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

/// Object removal: the model's download, or how filling is going.
fn removal_status(app: &ShoeboxApp, ui: &mut egui::Ui) {
    let model = app.inpainter.clone();
    if !model.ready() {
        crate::models::download_ui(
            ui,
            &model,
            "the object removal model",
            "Picsart's MI-GAN, once, from its authors; it runs on this Mac",
        );
        return;
    }
    let (removing, failed) = app
        .develop
        .ready
        .as_ref()
        .map_or((false, None), |r| r.src.cache.removing());
    if removing {
        ui.weak("Removing…");
    } else if let Some(e) = failed {
        ui.weak(format!("Couldn't: {e}"));
    } else {
        ui.weak("Paint over what to remove; Option-drag erases from the selected one.");
    }
    ui.end_row();
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
    t.painting.clear();
    if !on {
        t.selected = None;
    } else {
        // One tool on the photo at a time.
        app.develop.local.selected = None;
        app.develop.points.picking = false;
        app.develop.local.placing = false;
        app.develop.picking = false;
        app.develop.lens_blur.picking = false;
        app.develop.guided = Default::default();
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
    let removing = app.develop.spots.kind == SpotKind::Remove;
    // Removing: the photo is painted on (under the spots' own handles,
    // which come after it).
    let paint = removing.then(|| {
        ui.interact(
            target,
            ui.scope_id().with("remove-paint"),
            egui::Sense::click_and_drag(),
        )
    });
    let mut moved: Option<(usize, End, [f64; 2])> = None;
    let mut select = None;
    for (i, sp) in spots.iter().enumerate() {
        let on = sel == Some(i);
        let c = map.to_screen(sp.dest);
        let r = (sp.radius as f32 * map.scale_at(sp.dest)).max(3.0);
        let ends: &[End] = if on && sp.kind.has_source() {
            &[End::Dest, End::Source]
        } else {
            &[End::Dest]
        };
        if on && sp.kind == SpotKind::Remove {
            for k in &sp.strokes {
                let pts: Vec<Pos2> = k.points.iter().map(|&p| map.to_screen(p)).collect();
                let colour = if k.erase {
                    Color32::from_rgb(255, 120, 120)
                } else {
                    Color32::WHITE
                };
                trace(painter, &pts, colour);
            }
        }
        if on && sp.kind.has_source() {
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
                    End::Dest => {
                        // A removal's strokes go with it.
                        let d = [to[0] - p.dest[0], to[1] - p.dest[1]];
                        for k in &mut p.strokes {
                            for q in &mut k.points {
                                *q = [q[0] + d[0], q[1] + d[1]];
                            }
                        }
                        p.dest = to;
                    }
                    End::Source => p.source = to,
                }
            }
        });
        return;
    }
    if let Some(paint) = paint {
        if select.is_none() {
            painting(app, ui, &paint, painter, &map, &s);
        }
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

/// A path along a removal's stroke, shown while it's selected.
fn trace(painter: &egui::Painter, pts: &[Pos2], colour: Color32) {
    if pts.len() == 1 {
        painter.circle_filled(pts[0], 2.0, colour);
        return;
    }
    painter.add(egui::Shape::line(
        pts.to_vec(),
        Stroke::new(3.0, Color32::from_black_alpha(120)),
    ));
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(1.0, colour)));
}

/// Removing: a drag paints a stroke (shown as it goes); letting go makes
/// it a removal, or with ⌥ and a removal selected, erases from that one.
/// A click is a stroke of one dab.
fn painting(
    app: &mut ShoeboxApp,
    ui: &egui::Ui,
    r: &egui::Response,
    painter: &egui::Painter,
    map: &Mapping<'_>,
    s: &Settings,
) {
    let radius = app.develop.spots.brush;
    let erase_from = app.develop.spots.selected.filter(|&i| {
        s.modules
            .heal
            .spots
            .get(i)
            .is_some_and(|p| p.kind == SpotKind::Remove)
    });
    let alt = ui.input(|i| i.modifiers.alt);
    if let Some(pos) = r.hover_pos().or(r.interact_pointer_pos()) {
        let u = map.to_units(pos);
        let rr = (radius as f32 * map.scale_at(u)).max(2.0);
        let colour = if alt && erase_from.is_some() {
            Color32::from_rgb(255, 120, 120)
        } else {
            Color32::WHITE
        };
        painter.circle_stroke(pos, rr, Stroke::new(1.5, colour));
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    }
    let t = &mut app.develop.spots;
    if r.drag_started()
        && let Some(pos) = r.interact_pointer_pos()
    {
        // From where the button went down (a drag starts a few points
        // on), to here.
        let from = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
        t.painting = vec![map.to_units(from)];
        let u = map.to_units(pos);
        if u != t.painting[0] {
            t.painting.push(u);
        }
        t.erasing = alt && erase_from.is_some();
    } else if r.dragged()
        && !t.painting.is_empty()
        && let Some(pos) = r.interact_pointer_pos()
    {
        // A point every quarter of the radius.
        let u = map.to_units(pos);
        if t.painting
            .last()
            .is_none_or(|p| (p[0] - u[0]).hypot(p[1] - u[1]) >= radius * 0.25)
        {
            t.painting.push(u);
        }
    }
    // The stroke so far, as wide as it paints.
    if !t.painting.is_empty() {
        let pts: Vec<Pos2> = t.painting.iter().map(|&p| map.to_screen(p)).collect();
        let w = 2.0 * (radius as f32 * map.scale_at(t.painting[0])).max(2.0);
        let colour = if t.erasing {
            Color32::from_rgba_unmultiplied(255, 255, 255, 90)
        } else {
            Color32::from_rgba_unmultiplied(255, 60, 60, 110)
        };
        if pts.len() == 1 {
            painter.circle_filled(pts[0], w * 0.5, colour);
        } else {
            painter.add(egui::Shape::line(pts, Stroke::new(w, colour)));
        }
    }
    let done = if r.drag_stopped() {
        std::mem::take(&mut t.painting)
    } else if r.clicked()
        && let Some(pos) = r.interact_pointer_pos()
    {
        t.erasing = alt && erase_from.is_some();
        vec![map.to_units(pos)]
    } else {
        return;
    };
    if done.is_empty() {
        return;
    }
    let erasing = std::mem::take(&mut t.erasing);
    let stroke = shoebox_develop::edit::Stroke {
        points: done,
        radius,
        feather: 0.0,
        flow: 1.0,
        erase: erasing,
    };
    app.develop.last_step_done();
    if let Some(i) = erase_from.filter(|_| erasing) {
        edit(app, "spot-erase", "Erase from removal".into(), |s| {
            if let Some(p) = s.modules.heal.spots.get_mut(i) {
                p.strokes.push(stroke);
            }
        });
        return;
    }
    let strokes = vec![stroke];
    let t = &app.develop.spots;
    let spot = Spot {
        kind: SpotKind::Remove,
        dest: remove::middle(&strokes).unwrap_or_default(),
        source: remove::middle(&strokes).unwrap_or_default(),
        radius,
        feather: t.feather,
        opacity: t.opacity,
        darken: t.darken,
        strokes,
    };
    let n = s.modules.heal.spots.len();
    edit(app, "spot-add", "Remove object".into(), |s| {
        s.modules.heal.spots.push(spot)
    });
    app.develop.spots.selected = Some(n);
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
    let t = app.develop.spots.clone();
    let dest = map.to_units(p);
    // Found on the photo as healed so far, so it doesn't copy a blemish
    // already dealt with.
    let source = if t.kind.has_source() {
        let healed = ready.src.prepared(s);
        let img = healed.as_deref().unwrap_or(&ready.src.rgb);
        shoebox_develop::heal::find_source(img, dest, t.radius)
    } else {
        dest
    };
    let spot = Spot {
        kind: t.kind,
        dest,
        source,
        radius: t.radius,
        feather: t.feather,
        opacity: t.opacity,
        darken: t.darken,
        strokes: Vec::new(),
    };
    let n = s.modules.heal.spots.len();
    app.develop.last_step_done();
    let label = match t.kind {
        SpotKind::Heal => "Heal spot",
        SpotKind::Clone => "Clone spot",
        SpotKind::RedEye => "Red eye",
        SpotKind::PetEye => "Pet eye",
        SpotKind::Remove => "Remove object",
    };
    edit(app, "spot-add", label.into(), |s| {
        s.modules.heal.spots.push(spot)
    });
    app.develop.spots.selected = Some(n);
}
