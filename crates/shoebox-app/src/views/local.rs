//! Develop's local adjustments (EDITING-SPEC, Local adjustments): the
//! panel's list of masks and the selected one's parts and sliders, and the
//! handles drawn over the photo (gradients, ellipses, the brush, range
//! pickers), and control points (K: one click makes a mask of a circle and
//! the colour and brightness there, `mask::control_point`). Positions are kept as the edit stores them: in the stored
//! image, in units of its long edge; the render's frame maps them to the
//! screen and back.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke as Line, pos2, vec2};
use shoebox_develop::cpu::{self, Frame};
use shoebox_develop::edit::{Local, LocalAdjust, MaskPart, Op, Part, Settings, Shape, Stroke};

use super::develop::{edit, slider};
use crate::app::ShoeboxApp;

/// The local adjustment tool's state.
#[derive(Debug, Default)]
pub struct Tool {
    /// The mask being edited (its id) and its part.
    pub selected: Option<String>,
    pub part: usize,
    /// Show its mask over the photo (O), or alone.
    pub show_mask: bool,
    pub mask_only: bool,
    /// The brush: radius (long edges), feather and flow; Alt erases.
    pub brush: Brush,
    /// A handle being dragged.
    pub(super) drag: Option<Handle>,
    /// The next click on the photo picks a range part's colour or
    /// brightness.
    pub picking: bool,
    /// The next click on the photo places a control point (K).
    pub placing: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Brush {
    pub radius: f64,
    pub feather: f64,
    pub flow: f64,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            radius: 0.03,
            feather: 0.5,
            flow: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Handle {
    From,
    To,
    Line,
    Centre,
    AxisX,
    AxisY,
    Paint,
}

/// The kinds of part, as the menus offer them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Subject,
    Background,
    Person,
    Face(shoebox_develop::edit::FacePart),
    Object,
    Linear,
    Radial,
    Brush,
    Luminance,
    Colour,
}

impl Kind {
    pub const ALL: [Kind; 14] = [
        Kind::Subject,
        Kind::Background,
        Kind::Person,
        Kind::Face(shoebox_develop::edit::FacePart::Skin),
        Kind::Face(shoebox_develop::edit::FacePart::Eyes),
        Kind::Face(shoebox_develop::edit::FacePart::Eyebrows),
        Kind::Face(shoebox_develop::edit::FacePart::Lips),
        Kind::Face(shoebox_develop::edit::FacePart::Teeth),
        Kind::Object,
        Kind::Linear,
        Kind::Radial,
        Kind::Brush,
        Kind::Luminance,
        Kind::Colour,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Linear => "Linear gradient",
            Kind::Radial => "Radial gradient",
            Kind::Brush => "Brush",
            Kind::Luminance => "Luminance range",
            Kind::Colour => "Colour range",
            Kind::Subject => "Subject",
            Kind::Background => "Background",
            Kind::Person => "People",
            Kind::Face(p) => match p {
                shoebox_develop::edit::FacePart::Skin => "Face skin",
                shoebox_develop::edit::FacePart::Eyes => "Eyes",
                shoebox_develop::edit::FacePart::Eyebrows => "Eyebrows",
                shoebox_develop::edit::FacePart::Lips => "Lips",
                shoebox_develop::edit::FacePart::Teeth => "Teeth",
            },
            Kind::Object => "Object (click)",
        }
    }

    /// A new part of this kind on a `units`-sized image (long edges).
    fn shape(self, units: [f64; 2]) -> Shape {
        let [w, h] = units;
        match self {
            Kind::Linear => Shape::Linear {
                from: [w / 2.0, h * 0.2],
                to: [w / 2.0, h * 0.55],
            },
            Kind::Radial => Shape::Radial {
                centre: [w / 2.0, h / 2.0],
                radii: [w * 0.25, h * 0.25],
                angle: 0.0,
                feather: 0.5,
            },
            Kind::Brush => Shape::Brush {
                strokes: Vec::new(),
            },
            Kind::Luminance => Shape::Luminance {
                range: [0.5, 1.0],
                smooth: 0.1,
            },
            Kind::Colour => Shape::Colour {
                hue: 30.0,
                width: 20.0,
            },
            Kind::Subject | Kind::Background => Shape::Subject,
            Kind::Person => Shape::Person,
            Kind::Face(part) => Shape::Face { part },
            Kind::Object => Shape::Object { clicks: Vec::new() },
        }
    }
}

/// The kinds of mask part as a menu: the ones found in the photo first
/// (faces' parts in a submenu), then the drawn ones. The kind clicked.
fn kind_menu(ui: &mut egui::Ui) -> Option<Kind> {
    let mut chosen = None;
    let mut item = |ui: &mut egui::Ui, k: Kind| {
        if ui.button(k.label()).clicked() {
            chosen = Some(k);
            ui.close();
        }
    };
    for k in [Kind::Subject, Kind::Background, Kind::Person] {
        item(ui, k);
    }
    ui.menu_button("Face", |ui| {
        for k in Kind::ALL {
            if matches!(k, Kind::Face(_)) {
                item(ui, k);
            }
        }
    });
    item(ui, Kind::Object);
    ui.separator();
    for k in [
        Kind::Linear,
        Kind::Radial,
        Kind::Brush,
        Kind::Luminance,
        Kind::Colour,
    ] {
        item(ui, k);
    }
    chosen
}

fn kind_of(s: &Shape) -> Kind {
    match s {
        Shape::Linear { .. } => Kind::Linear,
        Shape::Radial { .. } => Kind::Radial,
        Shape::Brush { .. } => Kind::Brush,
        Shape::Luminance { .. } => Kind::Luminance,
        Shape::Colour { .. } => Kind::Colour,
        Shape::Subject => Kind::Subject,
        Shape::Person => Kind::Person,
        Shape::Face { part } => Kind::Face(*part),
        Shape::Object { .. } => Kind::Object,
    }
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

/// The selected mask, if it's still there (undo may have removed it).
fn selected<'a>(app: &ShoeboxApp, s: &'a Settings) -> Option<(usize, &'a Local)> {
    let id = app.develop.local.selected.as_ref()?;
    s.locals.iter().enumerate().find(|(_, l)| &l.id == id)
}

/// Change mask `id` as one step (or folded into the last, as sliders are).
fn edit_layer(
    app: &mut ShoeboxApp,
    key: &'static str,
    label: String,
    id: &str,
    f: impl FnOnce(&mut Local),
) {
    edit(app, key, label, |s| {
        if let Some(l) = s.locals.iter_mut().find(|l| l.id == id) {
            f(l);
        }
    });
}

/// The next mask's number and id ("l1", "l2", …).
fn next_id(app: &ShoeboxApp) -> Option<(u32, String)> {
    let doc = app.develop.doc.as_ref()?;
    let n = doc
        .settings()
        .locals
        .iter()
        .filter_map(|l| l.id.strip_prefix('l')?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    Some((n, format!("l{n}")))
}

/// A new mask with one part of `kind`, selected.
pub fn new_mask(app: &mut ShoeboxApp, kind: Kind) {
    let Some((n, id)) = next_id(app) else { return };
    let shape = kind.shape(units(app));
    let layer = Local {
        id: id.clone(),
        name: match kind {
            Kind::Linear | Kind::Radial | Kind::Brush | Kind::Luminance | Kind::Colour => {
                format!("Mask {n}")
            }
            k => k.label().to_owned(),
        },
        mask: shoebox_develop::edit::Mask {
            parts: vec![Part::Known(MaskPart {
                op: Op::Add,
                // The background: everything but the subject.
                invert: kind == Kind::Background,
                shape,
            })],
        },
        ..Default::default()
    };
    app.develop.last_step_done();
    edit(
        app,
        "mask-new",
        format!("New mask ({})", kind.label()),
        |s| s.locals.push(layer),
    );
    let t = &mut app.develop.local;
    t.selected = Some(id);
    t.part = 0;
    t.picking = kind == Kind::Colour;
    t.placing = false;
}

/// Wait for a click on the photo to place a control point (K), or stop.
pub fn set_placing(app: &mut ShoeboxApp, on: bool) {
    app.develop.local.placing = on && !app.develop.read_only;
    if app.develop.local.placing {
        // One tool on the photo at a time.
        super::spots::set_active(app, false);
        app.develop.local.picking = false;
        app.develop.points.picking = false;
        app.develop.picking = false;
        app.develop.guided = Default::default();
    }
}

/// A control point at `pos`: a new mask of a circle there, intersected
/// with the colour and brightness there; selected with its circle's
/// handles, to size it.
fn place_point(app: &mut ShoeboxApp, map: &Mapping<'_>, pos: Pos2) {
    app.develop.local.placing = false;
    let Some(ready) = &app.develop.ready else {
        return;
    };
    let u = map.to_units(pos);
    let Some(tone) = shoebox_develop::mask::tone_at(&ready.src.rgb, map.frame, u) else {
        return;
    };
    let Some((n, id)) = next_id(app) else { return };
    let layer = Local {
        id: id.clone(),
        name: format!("Control point {n}"),
        mask: shoebox_develop::mask::control_point(u, tone),
        ..Default::default()
    };
    app.develop.last_step_done();
    edit(app, "mask-new", "New control point".into(), |s| {
        s.locals.push(layer)
    });
    let t = &mut app.develop.local;
    t.selected = Some(id);
    t.part = 0;
    t.picking = false;
}

/// Add a part of `kind` to the selected mask with `op`.
fn add_part(app: &mut ShoeboxApp, id: &str, kind: Kind, op: Op) {
    let shape = kind.shape(units(app));
    let mut at = 0;
    app.develop.last_step_done();
    edit_layer(
        app,
        "mask-part",
        format!("{op:?} {}", kind.label()),
        id,
        |l| {
            l.mask.parts.push(Part::Known(MaskPart {
                op,
                invert: kind == Kind::Background,
                shape,
            }));
            at = l.mask.parts.len() - 1;
        },
    );
    app.develop.local.part = at;
    app.develop.local.picking = kind == Kind::Colour;
}

/// The Local adjustments section of Develop's panel.
pub fn section(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &Settings) {
    let any = !s.locals.is_empty();
    let reset = super::look::section(ui, "Local adjustments", any, true, |ui| {
        ui.horizontal(|ui| {
            ui.menu_button("New mask", |ui| {
                if let Some(k) = kind_menu(ui) {
                    new_mask(app, k);
                }
            });
            let placing = app.develop.local.placing;
            if ui
                .add_enabled(
                    !app.develop.read_only,
                    egui::Button::selectable(placing, "Control point"),
                )
                .on_hover_text(
                    "Click the photo to place a control point: a circle that takes the \
                     colour and brightness there (K)",
                )
                .clicked()
            {
                set_placing(app, !placing);
            }
            let sel = selected(app, s).is_some();
            let mut show = app.develop.local.show_mask;
            if ui
                .add_enabled(sel, egui::Button::selectable(show, "Show mask"))
                .on_hover_text("The selected mask over the photo in red (O)")
                .clicked()
            {
                show = !show;
                app.develop.local.show_mask = show;
            }
        });
        // The masks, newest last.
        let mut select = None;
        let mut remove = None;
        let mut toggle = None;
        for l in &s.locals {
            let on = app.develop.local.selected.as_deref() == Some(l.id.as_str());
            ui.horizontal(|ui| {
                let mut enabled = l.enabled;
                if ui
                    .checkbox(&mut enabled, "")
                    .on_hover_text("Turn this mask's adjustment on or off")
                    .changed()
                {
                    toggle = Some((l.id.clone(), enabled));
                }
                if ui.selectable_label(on, &l.name).clicked() {
                    select = Some((!on).then(|| l.id.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("✖")
                        .on_hover_text("Delete this mask")
                        .clicked()
                    {
                        remove = Some(l.id.clone());
                    }
                });
            });
        }
        if let Some(sel) = select {
            let t = &mut app.develop.local;
            t.selected = sel;
            t.part = 0;
            t.picking = false;
            t.placing = false;
            app.develop.last_step_done();
        }
        if let Some((id, on)) = toggle {
            let label = format!("Mask {}", if on { "on" } else { "off" });
            edit_layer(app, "mask-on", label, &id, |l| l.enabled = on);
        }
        if let Some(id) = remove {
            app.develop.last_step_done();
            edit(app, "mask-remove", "Delete mask".into(), |s| {
                s.locals.retain(|l| l.id != id)
            });
        }
        if let Some((_, l)) = selected(app, s) {
            ui.separator();
            layer_controls(app, ui, l);
        } else if any {
            ui.weak("Select a mask to change it.");
        }
    });
    if reset {
        app.develop.local.selected = None;
        edit(app, "mask-remove", "Delete all masks".into(), |s| {
            s.locals.clear()
        });
    }
}

const ADJUST_KEYS: [&str; 11] = [
    "l-exposure",
    "l-contrast",
    "l-highlights",
    "l-shadows",
    "l-temperature",
    "l-tint",
    "l-saturation",
    "l-clarity",
    "l-dehaze",
    "l-sharpness",
    "l-denoise-ai",
];

fn layer_controls(app: &mut ShoeboxApp, ui: &mut egui::Ui, l: &Local) {
    let id = l.id.clone();
    let mut name = l.name.clone();
    ui.horizontal(|ui| {
        ui.label("Name");
        if ui
            .add(egui::TextEdit::singleline(&mut name).desired_width(ui.available_width() - 8.0))
            .lost_focus()
            && name != l.name
            && !name.is_empty()
        {
            edit_layer(app, "mask-name", format!("Rename mask {name}"), &id, |l| {
                l.name = name
            });
        }
    });
    // The parts.
    let mut chosen = None;
    let mut change: Option<(usize, MaskPart)> = None;
    let mut remove = None;
    for (i, part) in l.mask.parts.iter().enumerate() {
        ui.horizontal(|ui| {
            let Part::Known(p) = part else {
                ui.weak("(from a newer Shoebox)");
                return;
            };
            if i > 0 {
                let mut op = p.op;
                egui::ComboBox::from_id_salt(("mask-op", &id, i))
                    .width(80.0)
                    .selected_text(op_label(op))
                    .show_ui(ui, |ui| {
                        for o in [Op::Add, Op::Subtract, Op::Intersect] {
                            ui.selectable_value(&mut op, o, op_label(o));
                        }
                    });
                if op != p.op {
                    change = Some((i, MaskPart { op, ..p.clone() }));
                }
            }
            let on = app.develop.local.part == i;
            if ui.selectable_label(on, kind_of(&p.shape).label()).clicked() {
                chosen = Some(i);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if l.mask.parts.len() > 1
                    && ui
                        .small_button("✖")
                        .on_hover_text("Remove this part")
                        .clicked()
                {
                    remove = Some(i);
                }
                let mut inv = p.invert;
                if ui
                    .toggle_value(&mut inv, "Invert")
                    .on_hover_text("Everything but this part")
                    .changed()
                {
                    change = Some((
                        i,
                        MaskPart {
                            invert: inv,
                            ..p.clone()
                        },
                    ));
                }
            });
        });
    }
    if let Some(i) = chosen {
        app.develop.local.part = i;
        app.develop.local.picking = false;
    }
    if let Some((i, p)) = change {
        edit_layer(app, "mask-op", "Mask part".into(), &id, |l| {
            l.mask.parts[i] = Part::Known(p)
        });
    }
    if let Some(i) = remove {
        app.develop.last_step_done();
        edit_layer(app, "mask-part", "Remove mask part".into(), &id, |l| {
            l.mask.parts.remove(i);
        });
        app.develop.local.part = 0;
    }
    ui.horizontal(|ui| {
        for op in [Op::Add, Op::Subtract, Op::Intersect] {
            ui.menu_button(op_label(op), |ui| {
                if let Some(k) = kind_menu(ui) {
                    add_part(app, &id, k, op);
                }
            });
        }
    });
    // The selected part's own settings.
    let part = app
        .develop
        .local
        .part
        .min(l.mask.parts.len().saturating_sub(1));
    if let Some(Part::Known(p)) = l.mask.parts.get(part) {
        part_controls(app, ui, &id, part, p);
    }
    let mut only = app.develop.local.mask_only;
    if ui
        .checkbox(&mut only, "Mask only")
        .on_hover_text("Show the mask alone, white where it applies")
        .changed()
    {
        app.develop.local.mask_only = only;
        if only {
            app.develop.local.show_mask = true;
        }
    }
    // The sliders (AI denoise strength for raw photos only).
    let a = &l.adjust;
    let raw = app
        .develop
        .ready
        .as_ref()
        .is_some_and(|r| r.src.referred == cpu::Referred::Scene);
    let model = app.denoiser.ready();
    egui::Grid::new(("mask-sliders", &id))
        .num_columns(2)
        .show(ui, |ui| {
            if let Some(v) = slider(ui, "Amount", l.amount, 0.0..=100.0, 100.0, 0) {
                edit_layer(app, "l-amount", format!("Mask amount {v:.0}"), &id, |l| {
                    l.amount = v
                });
            }
            let rows: [(&str, f64, f64, usize); 11] = [
                ("Exposure", a.exposure, 4.0, 2),
                ("Contrast", a.contrast, 100.0, 0),
                ("Highlights", a.highlights, 100.0, 0),
                ("Shadows", a.shadows, 100.0, 0),
                ("Temperature", a.temperature, 100.0, 0),
                ("Tint", a.tint, 100.0, 0),
                ("Saturation", a.saturation, 100.0, 0),
                ("Clarity", a.clarity, 100.0, 0),
                ("Dehaze", a.dehaze, 100.0, 0),
                ("Sharpness", a.sharpness, 100.0, 0),
                ("AI denoise", a.denoise_ai, 100.0, 0),
            ];
            for (k, (name, v, max, dp)) in rows.into_iter().enumerate() {
                if k == 10 && !raw {
                    continue;
                }
                if let Some(v) = slider(ui, name, v, -max..=max, 0.0, dp) {
                    let what = if k == 10 {
                        name.to_owned()
                    } else {
                        name.to_lowercase()
                    };
                    let label = if dp > 0 {
                        format!("Mask {what} {v:+.2}")
                    } else {
                        format!("Mask {what} {v:+.0}")
                    };
                    edit_layer(app, ADJUST_KEYS[k], label, &id, |l| {
                        set_adjust(&mut l.adjust, k, v)
                    });
                }
            }
        });
    if raw && !model && a.denoise_ai != 0.0 {
        ui.label(
            egui::RichText::new(
                "AI denoise needs its model: download it under Detail, AI denoise.",
            )
            .small()
            .weak(),
        );
    }
}

fn set_adjust(a: &mut LocalAdjust, k: usize, v: f64) {
    let f = match k {
        0 => &mut a.exposure,
        1 => &mut a.contrast,
        2 => &mut a.highlights,
        3 => &mut a.shadows,
        4 => &mut a.temperature,
        5 => &mut a.tint,
        6 => &mut a.saturation,
        7 => &mut a.clarity,
        8 => &mut a.dehaze,
        9 => &mut a.sharpness,
        _ => &mut a.denoise_ai,
    };
    *f = v;
}

fn op_label(op: Op) -> &'static str {
    match op {
        Op::Add => "Add",
        Op::Subtract => "Subtract",
        Op::Intersect => "Intersect",
    }
}

/// Settings of the selected part that aren't handles on the photo.
fn part_controls(app: &mut ShoeboxApp, ui: &mut egui::Ui, id: &str, i: usize, p: &MaskPart) {
    let set = |app: &mut ShoeboxApp, key, label: String, shape: Shape| {
        let p = MaskPart { shape, ..p.clone() };
        edit_layer(app, key, label, id, |l| l.mask.parts[i] = Part::Known(p));
    };
    egui::Grid::new(("mask-part", id, i))
        .num_columns(2)
        .show(ui, |ui| match &p.shape {
            Shape::Radial {
                centre,
                radii,
                angle,
                feather,
            } => {
                if let Some(v) = slider(ui, "Feather", feather * 100.0, 0.0..=100.0, 50.0, 0) {
                    let shape = Shape::Radial {
                        centre: *centre,
                        radii: *radii,
                        angle: *angle,
                        feather: v / 100.0,
                    };
                    set(app, "m-feather", format!("Feather {v:.0}"), shape);
                }
            }
            Shape::Brush { .. } => {
                let b = &mut app.develop.local.brush;
                if let Some(v) = slider(ui, "Size", b.radius * 200.0, 0.5..=40.0, 6.0, 1) {
                    b.radius = v / 200.0;
                }
                let b = &mut app.develop.local.brush;
                if let Some(v) = slider(ui, "Feather", b.feather * 100.0, 0.0..=100.0, 50.0, 0) {
                    b.feather = v / 100.0;
                }
                let b = &mut app.develop.local.brush;
                if let Some(v) = slider(ui, "Flow", b.flow * 100.0, 1.0..=100.0, 100.0, 0) {
                    b.flow = v / 100.0;
                }
                ui.weak("Paint on the photo;");
                ui.weak("hold Option to erase.");
                ui.end_row();
            }
            Shape::Luminance { range, smooth } => {
                let (lo, hi, sm) = (range[0], range[1], *smooth);
                if let Some(v) = slider(ui, "From", lo * 100.0, 0.0..=100.0, 50.0, 0) {
                    let shape = Shape::Luminance {
                        range: [(v / 100.0).min(hi), hi],
                        smooth: sm,
                    };
                    set(app, "m-lo", format!("Luminance from {v:.0}"), shape);
                }
                if let Some(v) = slider(ui, "To", hi * 100.0, 0.0..=100.0, 100.0, 0) {
                    let shape = Shape::Luminance {
                        range: [lo, (v / 100.0).max(lo)],
                        smooth: sm,
                    };
                    set(app, "m-hi", format!("Luminance to {v:.0}"), shape);
                }
                if let Some(v) = slider(ui, "Smoothness", sm * 100.0, 0.0..=50.0, 10.0, 0) {
                    let shape = Shape::Luminance {
                        range: [lo, hi],
                        smooth: v / 100.0,
                    };
                    set(app, "m-smooth", format!("Smoothness {v:.0}"), shape);
                }
                picker(app, ui, "Click the photo to pick a brightness");
            }
            Shape::Colour { hue, width } => {
                let h = *hue;
                if let Some(v) = slider(ui, "Range", *width, 5.0..=90.0, 20.0, 0) {
                    let shape = Shape::Colour { hue: h, width: v };
                    set(app, "m-width", format!("Colour range {v:.0}°"), shape);
                }
                picker(app, ui, "Click the photo to pick a colour");
            }
            Shape::Linear { .. } => {
                ui.weak("Drag the lines on the photo.");
                ui.end_row();
            }
            Shape::Object { clicks } => {
                let model = app.objects.model.clone();
                if !model.ready() {
                    crate::models::download_ui(
                        ui,
                        &model,
                        "the object model",
                        "Meta's SAM 2.1, once, from Hugging Face; it runs on this Mac",
                    );
                } else {
                    let (finding, failed) = app
                        .develop
                        .ready
                        .as_ref()
                        .map_or((false, None), |r| r.src.cache.finding());
                    if finding {
                        ui.weak("Finding it…");
                    } else if let Some(e) = failed {
                        ui.weak(format!("Couldn't: {e}"));
                    } else if clicks.is_empty() {
                        ui.weak("Click the object on the photo.");
                    } else {
                        ui.weak("Click to add; Option-click to take away.");
                    }
                    ui.end_row();
                    if !clicks.is_empty() && ui.button("Clear clicks").clicked() {
                        let shape = Shape::Object { clicks: Vec::new() };
                        set(app, "m-clicks", "Clear object clicks".into(), shape);
                    }
                    ui.end_row();
                }
            }
            Shape::Subject | Shape::Person | Shape::Face { .. } => {
                let (finding, failed) = app
                    .develop
                    .ready
                    .as_ref()
                    .map_or((false, None), |r| r.src.cache.finding());
                if finding {
                    ui.weak("Finding it…");
                } else if let Some(e) = failed {
                    ui.weak(format!("Couldn't: {e}"));
                } else {
                    ui.weak("Found by Apple's Vision.");
                }
                ui.end_row();
            }
        });
}

fn picker(app: &mut ShoeboxApp, ui: &mut egui::Ui, hint: &str) {
    ui.label("");
    let mut picking = app.develop.local.picking;
    if ui
        .toggle_value(&mut picking, "Pick")
        .on_hover_text(hint)
        .changed()
    {
        app.develop.local.picking = picking;
    }
    ui.end_row();
}

/// Screen ↔ stored-image units for the photo drawn at `target`.
pub(super) struct Mapping<'a> {
    frame: &'a Frame,
    inv: [[f64; 3]; 3],
    target: Rect,
    /// The source's long edge (pixels).
    pub(super) long: f64,
}

impl<'a> Mapping<'a> {
    pub(super) fn new(frame: &'a Frame, target: Rect, src: (usize, usize)) -> Option<Self> {
        let m = frame.map.map(|r| r.map(f64::from));
        Some(Self {
            frame,
            inv: shoebox_develop::colour::inv(&m)?,
            target,
            long: src.0.max(src.1) as f64,
        })
    }

    pub(super) fn to_units(&self, p: Pos2) -> [f64; 2] {
        let f = self.frame;
        let ox = (p.x - self.target.left()) / self.target.width() * f.width as f32 - 0.5;
        let oy = (p.y - self.target.top()) / self.target.height() * f.height as f32 - 0.5;
        let (sx, sy) = f.source_at(ox, oy);
        [
            (f64::from(sx) + 0.5) / self.long,
            (f64::from(sy) + 0.5) / self.long,
        ]
    }

    pub(super) fn to_screen(&self, u: [f64; 2]) -> Pos2 {
        let s = [u[0] * self.long - 0.5, u[1] * self.long - 0.5, 1.0];
        let o = shoebox_develop::colour::mulv(&self.inv, s);
        let (ox, oy) = (o[0] / o[2], o[1] / o[2]);
        let f = self.frame;
        pos2(
            self.target.left() + ((ox + 0.5) / f.width as f64) as f32 * self.target.width(),
            self.target.top() + ((oy + 0.5) / f.height as f64) as f32 * self.target.height(),
        )
    }

    /// Screen points per unit near `u` (for the brush's circle).
    pub(super) fn scale_at(&self, u: [f64; 2]) -> f32 {
        let a = self.to_screen(u);
        let b = self.to_screen([u[0] + 0.01, u[1]]);
        a.distance(b) / 0.01
    }
}

pub(super) const HANDLE: f32 = 6.0;

pub(super) fn outline(painter: &egui::Painter, pts: &[Pos2], width: f32) {
    painter.add(egui::Shape::line(
        pts.to_vec(),
        Line::new(width + 2.0, Color32::from_black_alpha(140)),
    ));
    painter.add(egui::Shape::line(
        pts.to_vec(),
        Line::new(width, Color32::WHITE),
    ));
}

pub(super) fn knob(painter: &egui::Painter, at: Pos2, active: bool) {
    let fill = if active {
        Color32::from_rgb(70, 140, 255)
    } else {
        Color32::WHITE
    };
    painter.circle(
        at,
        HANDLE,
        fill,
        Line::new(1.5, Color32::from_black_alpha(180)),
    );
}

/// The selected mask's handles over the photo, and what dragging or
/// clicking on them does. `photo` is the view's own response (clicks for
/// pickers).
pub fn on_photo(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    photo: &egui::Response,
    painter: &egui::Painter,
    target: Rect,
) {
    if app.develop.spots.active {
        return;
    }
    if app.develop.local.placing {
        if photo.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if photo.clicked()
            && let Some(pos) = photo.interact_pointer_pos()
            && target.contains(pos)
            && let (Some(frame), Some(ready)) = (app.develop.frame.clone(), &app.develop.ready)
            && let Some(map) =
                Mapping::new(&frame, target, (ready.src.rgb.width, ready.src.rgb.height))
        {
            place_point(app, &map, pos);
        }
        return;
    }
    let Some(doc) = &app.develop.doc else { return };
    let s = doc.settings().clone();
    let Some((_, layer)) = selected(app, &s) else {
        return;
    };
    let (Some(frame), Some(ready)) = (app.develop.frame.clone(), &app.develop.ready) else {
        return;
    };
    let src = (ready.src.rgb.width, ready.src.rgb.height);
    let Some(map) = Mapping::new(&frame, target, src) else {
        return;
    };
    let id = layer.id.clone();
    let i = app
        .develop
        .local
        .part
        .min(layer.mask.parts.len().saturating_sub(1));
    // The other geometric parts, thin.
    for (k, part) in layer.mask.parts.iter().enumerate() {
        if k == i {
            continue;
        }
        if let Part::Known(p) = part {
            draw_shape(painter, &map, &p.shape, 1.0);
        }
    }
    let Some(Part::Known(p)) = layer.mask.parts.get(i).cloned() else {
        return;
    };
    draw_shape(painter, &map, &p.shape, 2.0);
    let read_only = app.develop.read_only;
    let mut new: Option<Shape> = None;
    let drag_started = |r: &egui::Response| r.drag_started();
    match &p.shape {
        Shape::Linear { from, to } => {
            let (a, b) = (map.to_screen(*from), map.to_screen(*to));
            let mid = a + (b - a) / 2.0;
            for (h, at) in [(Handle::From, a), (Handle::To, b), (Handle::Line, mid)] {
                let r = handle(ui, &id, h, at);
                knob(painter, at, app.develop.local.drag == Some(h));
                if read_only {
                    continue;
                }
                if drag_started(&r) {
                    app.develop.local.drag = Some(h);
                    app.develop.last_step_done();
                }
                if r.dragged()
                    && let Some(pos) = r.interact_pointer_pos()
                {
                    let u = map.to_units(pos);
                    new = Some(match h {
                        Handle::From => Shape::Linear { from: u, to: *to },
                        Handle::To => Shape::Linear { from: *from, to: u },
                        _ => {
                            let m = map.to_units(mid);
                            let d = [u[0] - m[0], u[1] - m[1]];
                            Shape::Linear {
                                from: [from[0] + d[0], from[1] + d[1]],
                                to: [to[0] + d[0], to[1] + d[1]],
                            }
                        }
                    });
                }
            }
        }
        Shape::Radial {
            centre,
            radii,
            angle,
            feather,
        } => {
            let (sin, cos) = angle.to_radians().sin_cos();
            let ax = [centre[0] + cos * radii[0], centre[1] + sin * radii[0]];
            let ay = [centre[0] - sin * radii[1], centre[1] + cos * radii[1]];
            for (h, u) in [
                (Handle::Centre, *centre),
                (Handle::AxisX, ax),
                (Handle::AxisY, ay),
            ] {
                let at = map.to_screen(u);
                let r = handle(ui, &id, h, at);
                knob(painter, at, app.develop.local.drag == Some(h));
                if read_only {
                    continue;
                }
                if drag_started(&r) {
                    app.develop.local.drag = Some(h);
                    app.develop.last_step_done();
                }
                if r.dragged()
                    && let Some(pos) = r.interact_pointer_pos()
                {
                    let u = map.to_units(pos);
                    let d = [u[0] - centre[0], u[1] - centre[1]];
                    let (c, rr, a) = match h {
                        Handle::Centre => (u, *radii, *angle),
                        // The long axis's handle sizes and turns it.
                        Handle::AxisX => (
                            *centre,
                            [d[0].hypot(d[1]).max(1e-3), radii[1]],
                            d[1].atan2(d[0]).to_degrees(),
                        ),
                        _ => (
                            *centre,
                            [radii[0], (-sin * d[0] + cos * d[1]).abs().max(1e-3)],
                            *angle,
                        ),
                    };
                    new = Some(Shape::Radial {
                        centre: c,
                        radii: rr,
                        angle: a,
                        feather: *feather,
                    });
                }
            }
        }
        Shape::Brush { .. } if !read_only => {
            brush(app, ui, painter, &map, &id, i, target);
        }
        Shape::Object { clicks } if !read_only => {
            for c in clicks {
                let at = map.to_screen(c.at);
                let colour = if c.add {
                    Color32::from_rgb(60, 200, 90)
                } else {
                    Color32::from_rgb(230, 70, 70)
                };
                painter.circle(at, 5.0, colour, Line::new(1.5, Color32::WHITE));
            }
            if photo.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if photo.clicked()
                && let Some(pos) = photo.interact_pointer_pos()
                && target.contains(pos)
            {
                let add = !ui.input(|i| i.modifiers.alt);
                let mut list = clicks.clone();
                list.push(shoebox_develop::edit::ObjectClick {
                    at: map.to_units(pos),
                    add,
                });
                new = Some(Shape::Object { clicks: list });
                app.develop.last_step_done();
            }
        }
        Shape::Luminance { .. } | Shape::Colour { .. } if app.develop.local.picking => {
            if photo.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if photo.clicked()
                && let Some(pos) = photo.interact_pointer_pos()
                && target.contains(pos)
            {
                app.develop.local.picking = false;
                if let Some(shape) = pick(app, &map, pos, &p.shape) {
                    new = Some(shape);
                    app.develop.last_step_done();
                }
            }
        }
        _ => {}
    }
    if !ui.input(|i| i.pointer.any_down()) {
        app.develop.local.drag = None;
    }
    if let Some(shape) = new {
        let label = match &shape {
            Shape::Object { clicks } => match clicks.last() {
                Some(c) if !c.add => "Object: take away".to_owned(),
                _ => "Object: add".to_owned(),
            },
            s => format!("Move {}", kind_of(s).label().to_lowercase()),
        };
        let part = MaskPart { shape, ..p };
        edit_layer(app, "mask-drag", label, &id, |l| {
            if i < l.mask.parts.len() {
                l.mask.parts[i] = Part::Known(part);
            }
        });
    }
}

/// An invisible drag target for a handle.
fn handle(ui: &mut egui::Ui, id: &str, h: Handle, at: Pos2) -> egui::Response {
    let rect = Rect::from_center_size(at, vec2(HANDLE * 3.0, HANDLE * 3.0));
    ui.interact(
        rect,
        ui.scope_id().with(("mask-handle", id, h as u8)),
        egui::Sense::drag(),
    )
    .on_hover_cursor(egui::CursorIcon::Grab)
}

fn draw_shape(painter: &egui::Painter, map: &Mapping<'_>, s: &Shape, width: f32) {
    match s {
        Shape::Linear { from, to } => {
            let (a, b) = (map.to_screen(*from), map.to_screen(*to));
            let d = b - a;
            let n = vec2(-d.y, d.x).normalized() * 4000.0;
            for at in [a, b] {
                outline(painter, &[at - n, at + n], width);
            }
            let mid = a + d / 2.0;
            outline(painter, &[mid - n, mid + n], width * 0.5);
        }
        Shape::Radial {
            centre,
            radii,
            angle,
            feather,
        } => {
            let (sin, cos) = angle.to_radians().sin_cos();
            let ellipse = |k: f64| -> Vec<Pos2> {
                (0..=72)
                    .map(|i| {
                        let t = f64::from(i) / 72.0 * std::f64::consts::TAU;
                        let (x, y) = (radii[0] * k * t.cos(), radii[1] * k * t.sin());
                        map.to_screen([
                            centre[0] + cos * x - sin * y,
                            centre[1] + sin * x + cos * y,
                        ])
                    })
                    .collect()
            };
            outline(painter, &ellipse(1.0), width);
            if *feather > 0.01 {
                outline(
                    painter,
                    &ellipse(1.0 - feather.clamp(0.0, 1.0)),
                    width * 0.5,
                );
            }
        }
        _ => {}
    }
}

/// Painting with the brush: a drag is one stroke (one history step);
/// holding ⌥ erases.
fn brush(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    map: &Mapping<'_>,
    id: &str,
    i: usize,
    target: Rect,
) {
    let r = ui.interact(
        target,
        ui.scope_id().with(("mask-brush", id)),
        egui::Sense::drag(),
    );
    let b = app.develop.local.brush;
    let erase = ui.input(|i| i.modifiers.alt);
    if let Some(pos) = r.hover_pos().or(r.interact_pointer_pos()) {
        let u = map.to_units(pos);
        let radius = (b.radius as f32 * map.scale_at(u)).max(2.0);
        let colour = if erase {
            Color32::from_rgb(255, 120, 120)
        } else {
            Color32::WHITE
        };
        painter.circle_stroke(pos, radius, Line::new(1.5, colour));
        painter.circle_stroke(
            pos,
            radius * (1.0 - b.feather as f32).max(0.05),
            Line::new(1.0, colour.gamma_multiply(0.6)),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    }
    let Some(pos) = r.interact_pointer_pos() else {
        return;
    };
    let u = map.to_units(pos);
    if r.drag_started() {
        app.develop.local.drag = Some(Handle::Paint);
        app.develop.last_step_done();
        let stroke = Stroke {
            points: vec![u],
            radius: b.radius,
            feather: b.feather,
            flow: b.flow,
            erase,
        };
        let label = if erase { "Erase" } else { "Brush" };
        edit_layer(app, "mask-paint", label.into(), id, |l| {
            if let Some(Part::Known(MaskPart {
                shape: Shape::Brush { strokes },
                ..
            })) = l.mask.parts.get_mut(i)
            {
                strokes.push(stroke);
            }
        });
    } else if r.dragged() && app.develop.local.drag == Some(Handle::Paint) {
        // A point every quarter of the radius.
        let label = if erase { "Erase" } else { "Brush" };
        let step = b.radius * 0.25;
        edit_layer(app, "mask-paint", label.into(), id, |l| {
            if let Some(Part::Known(MaskPart {
                shape: Shape::Brush { strokes },
                ..
            })) = l.mask.parts.get_mut(i)
                && let Some(s) = strokes.last_mut()
                && s.points
                    .last()
                    .is_none_or(|p| (p[0] - u[0]).hypot(p[1] - u[1]) >= step)
            {
                s.points.push(u);
            }
        });
    }
}

/// A range part picked at `pos`: the colour's hue, or a band of
/// brightness around the one there.
fn pick(app: &ShoeboxApp, map: &Mapping<'_>, pos: Pos2, s: &Shape) -> Option<Shape> {
    let ready = app.develop.ready.as_ref()?;
    let [shown, hue, _] =
        shoebox_develop::mask::tone_at(&ready.src.rgb, map.frame, map.to_units(pos))?;
    Some(match s {
        Shape::Luminance { smooth, .. } => shoebox_develop::mask::picked_luminance(shown, *smooth),
        Shape::Colour { width, .. } => Shape::Colour {
            hue: f64::from(hue).round(),
            width: *width,
        },
        other => other.clone(),
    })
}
