//! Develop's look modules: tone curve, colour (HSL), black and white and
//! calibration (`shoebox_develop::cpu::look`), each a collapsible section.

use eframe::egui::{self, Color32, RichText, Sense, pos2, vec2};
use shoebox_develop::edit::{
    BlackAndWhite, Calibration, Clarity, Colour, Dehaze, HUES, Settings, ToneCurve,
};

use super::develop::{edit, slider};
use crate::app::ShoeboxApp;

/// A collapsible module: its title, a Reset button (enabled when `changed`)
/// and `body`. True if Reset was clicked.
pub fn section(
    ui: &mut egui::Ui,
    title: &str,
    changed: bool,
    open: bool,
    body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut reset = false;
    let mut title_clicked = false;
    let id = ui.make_persistent_id(("module", title));
    let mut header =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open)
            .show_header(ui, |ui| {
                // The title opens and closes it too, not just the arrow.
                title_clicked = ui
                    .add(egui::Label::new(RichText::new(title).strong()).sense(Sense::click()))
                    .clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    reset = ui
                        .add_enabled(changed, egui::Button::new("Reset").small())
                        .on_hover_text("Back to the default")
                        .clicked();
                });
            });
    if title_clicked {
        header.toggle();
    }
    header.body(body);
    ui.separator();
    reset
}

/// Slider keys (one drag of one slider is one history step).
const HUE_KEYS: [&str; 8] = ["h0", "h1", "h2", "h3", "h4", "h5", "h6", "h7"];
const SAT_KEYS: [&str; 8] = ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7"];
const LUM_KEYS: [&str; 8] = ["l0", "l1", "l2", "l3", "l4", "l5", "l6", "l7"];
const MIX_KEYS: [&str; 8] = ["m0", "m1", "m2", "m3", "m4", "m5", "m6", "m7"];

/// The look modules, after tone.
pub fn modules(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &Settings) {
    presence(app, ui, s);
    tone_curve(app, ui, &s.modules.tone_curve);
    colour(app, ui, &s.modules.colour);
    black_and_white(app, ui, &s.modules.black_and_white);
    calibration(app, ui, &s.modules.calibration);
}

/// Clarity and dehaze.
fn presence(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &Settings) {
    let (c, d) = (&s.modules.clarity, &s.modules.dehaze);
    let changed = *c != Clarity::default() || *d != Dehaze::default();
    let reset = section(ui, "Presence", changed, true, |ui| {
        egui::Grid::new("presence-grid")
            .num_columns(2)
            .show(ui, |ui| {
                if let Some((v, dr)) = slider(ui, "Clarity", c.amount, -100.0..=100.0, 0.0, 0) {
                    edit(app, "clarity", format!("Clarity {v:+.0}"), dr, |s| {
                        s.modules.clarity.amount = v;
                    });
                }
                if let Some((v, dr)) = slider(ui, "Dehaze", d.amount, -100.0..=100.0, 0.0, 0) {
                    edit(app, "dehaze", format!("Dehaze {v:+.0}"), dr, |s| {
                        s.modules.dehaze.amount = v;
                    });
                }
            });
    });
    if reset {
        edit(app, "presence", "Reset presence".into(), false, |s| {
            s.modules.clarity = Clarity::default();
            s.modules.dehaze = Dehaze::default();
        });
    }
}

fn tone_curve(app: &mut ShoeboxApp, ui: &mut egui::Ui, t: &ToneCurve) {
    let changed = *t != ToneCurve::default();
    let reset = section(ui, "Tone curve", changed, true, |ui| {
        curve_editor(app, ui, t);
        egui::Grid::new("curve-grid").num_columns(2).show(ui, |ui| {
            // Top to bottom as on the curve.
            for (i, name, key) in [
                (3, "Highlights", "c3"),
                (2, "Lights", "c2"),
                (1, "Darks", "c1"),
                (0, "Shadows", "c0"),
            ] {
                if let Some((v, d)) = slider(ui, name, t.regions[i], -100.0..=100.0, 0.0, 0) {
                    edit(
                        app,
                        key,
                        format!("Curve {} {v:+.0}", name.to_lowercase()),
                        d,
                        |s| {
                            s.modules.tone_curve.regions[i] = v;
                        },
                    );
                }
            }
        });
    });
    if reset {
        edit(app, "curve", "Reset tone curve".into(), false, |s| {
            s.modules.tone_curve = ToneCurve::default();
        });
    }
}

const CHANNELS: [(&str, Color32); 4] = [
    ("RGB", Color32::from_gray(225)),
    ("Red", Color32::from_rgb(235, 90, 90)),
    ("Green", Color32::from_rgb(100, 210, 110)),
    ("Blue", Color32::from_rgb(100, 150, 245)),
];

fn points_of(t: &ToneCurve, channel: usize) -> &Vec<[f64; 2]> {
    match channel {
        1 => &t.red,
        2 => &t.green,
        3 => &t.blue,
        _ => &t.rgb,
    }
}

fn points_mut(t: &mut ToneCurve, channel: usize) -> &mut Vec<[f64; 2]> {
    match channel {
        1 => &mut t.red,
        2 => &mut t.green,
        3 => &mut t.blue,
        _ => &mut t.rgb,
    }
}

/// The point curve: drag a point to move it, drag elsewhere to add one,
/// double-click (or right-click) a point to remove it. RGB shows the parametric curve too.
fn curve_editor(app: &mut ShoeboxApp, ui: &mut egui::Ui, t: &ToneCurve) {
    let mut channel = app.develop.curve_channel;
    ui.horizontal(|ui| {
        for (i, (name, _)) in CHANNELS.iter().enumerate() {
            ui.selectable_value(&mut channel, i, *name);
        }
    });
    app.develop.curve_channel = channel;
    let side = (ui.available_width() - 14.0).clamp(120.0, 260.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let resp = resp.on_hover_text(
        "Drag to add or move a point; double-click or right-click a point to remove it",
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(24));
    let grid = egui::Stroke::new(1.0, Color32::from_gray(48));
    for k in 1..4 {
        let f = k as f32 / 4.0;
        painter.vline(rect.left() + f * rect.width(), rect.y_range(), grid);
        painter.hline(rect.x_range(), rect.top() + f * rect.height(), grid);
    }
    if let Some(h) = &app.develop.histogram {
        let lum = &h[3];
        let max = lum.iter().copied().max().unwrap_or(1).max(1) as f32;
        for (i, &n) in lum.iter().enumerate() {
            let x = rect.left() + (i as f32 + 0.5) / 256.0 * rect.width();
            let hgt = (n as f32 / max).sqrt() * rect.height() * 0.9;
            painter.vline(
                x,
                (rect.bottom() - hgt)..=rect.bottom(),
                egui::Stroke::new(rect.width() / 256.0 + 0.5, Color32::from_gray(40)),
            );
        }
    }
    let to_screen = |x: f64, y: f64| {
        pos2(
            rect.left() + x as f32 * rect.width(),
            rect.bottom() - y as f32 * rect.height(),
        )
    };
    let from_screen = |p: egui::Pos2| {
        (
            f64::from(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0)),
            f64::from(((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0)),
        )
    };
    painter.line_segment(
        [to_screen(0.0, 0.0), to_screen(1.0, 1.0)],
        egui::Stroke::new(1.0, Color32::from_gray(70)),
    );
    // This channel's own curve (RGB: with the parametric regions).
    let mut own = ToneCurve::default();
    if channel == 0 {
        own.regions = t.regions;
        own.splits = t.splits;
    }
    *points_mut(&mut own, 0) = points_of(t, channel).clone();
    let table = shoebox_develop::cpu::look::curve_table(&own);
    let colour = CHANNELS[channel].1;
    let line: Vec<egui::Pos2> = (0..=128)
        .map(|i| {
            let x = i as f32 / 128.0;
            let y = table
                .as_ref()
                .map_or(x, |tb| shoebox_develop::cpu::look::curve_at(tb, 0, x));
            to_screen(f64::from(x), f64::from(y))
        })
        .collect();
    painter.add(egui::Shape::line(line, egui::Stroke::new(1.5, colour)));
    let mut points = points_of(t, channel).clone();
    for p in &points {
        painter.circle_filled(to_screen(p[0], p[1]), 4.0, colour);
    }
    let near = |at: egui::Pos2, points: &[[f64; 2]]| {
        points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(p[0], p[1]).distance(at)))
            .filter(|(_, d)| *d <= 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    if app.develop.read_only {
        return;
    }
    let mut set = None;
    if (resp.double_clicked() || resp.secondary_clicked())
        && let Some(at) = resp.interact_pointer_pos().or(resp.hover_pos())
        && let Some(i) = near(at, &points)
    {
        points.remove(i);
        set = Some((points.clone(), false));
    } else if resp.drag_started()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        app.develop.curve_drag = match near(origin, &points) {
            Some(i) => Some(i),
            None => {
                let (x, y) = from_screen(origin);
                if x > 0.01 && x < 0.99 {
                    let i = points.partition_point(|p| p[0] < x);
                    points.insert(i, [x, y]);
                    set = Some((points.clone(), true));
                    Some(i)
                } else {
                    None
                }
            }
        };
    } else if resp.dragged()
        && let (Some(i), Some(at)) = (app.develop.curve_drag, resp.interact_pointer_pos())
        && i < points.len()
    {
        let (x, y) = from_screen(at);
        // Between its neighbours, never past them.
        let lo = if i == 0 {
            0.01
        } else {
            points[i - 1][0] + 0.01
        };
        let hi = if i + 1 == points.len() {
            0.99
        } else {
            points[i + 1][0] - 0.01
        };
        points[i] = [x.clamp(lo, hi.max(lo)), y];
        set = Some((points.clone(), true));
    }
    if resp.drag_stopped() {
        app.develop.curve_drag = None;
    }
    if let Some((pts, dragging)) = set {
        let name = CHANNELS[channel].0;
        edit(app, "curve", format!("Curve {name}"), dragging, |s| {
            *points_mut(&mut s.modules.tone_curve, channel) = pts;
        });
    }
}

fn colour(app: &mut ShoeboxApp, ui: &mut egui::Ui, c: &Colour) {
    let reset = section(ui, "Colour", *c != Colour::default(), true, |ui| {
        egui::Grid::new("colour-grid")
            .num_columns(2)
            .show(ui, |ui| {
                if let Some((v, d)) = slider(ui, "Vibrance", c.vibrance, -100.0..=100.0, 0.0, 0) {
                    edit(app, "vibrance", format!("Vibrance {v:+.0}"), d, |s| {
                        s.modules.colour.vibrance = v;
                    });
                }
                if let Some((v, d)) = slider(ui, "Saturation", c.saturation, -100.0..=100.0, 0.0, 0)
                {
                    edit(app, "saturation", format!("Saturation {v:+.0}"), d, |s| {
                        s.modules.colour.saturation = v;
                    });
                }
            });
        for (title, values, keys, which) in [
            ("Hue", c.hue, HUE_KEYS, 0),
            ("Saturation", c.hsl_saturation, SAT_KEYS, 1),
            ("Luminance", c.luminance, LUM_KEYS, 2),
        ] {
            egui::CollapsingHeader::new(title)
                .id_salt(("hsl", which))
                .show(ui, |ui| {
                    egui::Grid::new(("hsl-grid", which))
                        .num_columns(2)
                        .show(ui, |ui| {
                            for (i, name) in HUES.iter().enumerate() {
                                let Some((v, d)) =
                                    slider(ui, name, values[i], -100.0..=100.0, 0.0, 0)
                                else {
                                    continue;
                                };
                                let label = format!("{name} {} {v:+.0}", title.to_lowercase());
                                edit(app, keys[i], label, d, |s| {
                                    let m = &mut s.modules.colour;
                                    match which {
                                        0 => m.hue[i] = v,
                                        1 => m.hsl_saturation[i] = v,
                                        _ => m.luminance[i] = v,
                                    }
                                });
                            }
                        });
                });
        }
    });
    if reset {
        edit(app, "colour", "Reset colour".into(), false, |s| {
            s.modules.colour = Colour::default();
        });
    }
}

fn black_and_white(app: &mut ShoeboxApp, ui: &mut egui::Ui, b: &BlackAndWhite) {
    let reset = section(
        ui,
        "Black & white",
        *b != BlackAndWhite::default(),
        false,
        |ui| {
            let mut on = b.enabled;
            if ui.checkbox(&mut on, "Black and white").changed() {
                let label = if on { "Black and white" } else { "Colour" };
                edit(app, "bw", label.into(), false, |s| {
                    s.modules.black_and_white.enabled = on;
                });
            }
            ui.add_enabled_ui(b.enabled, |ui| {
                egui::Grid::new("bw-grid").num_columns(2).show(ui, |ui| {
                    for (i, name) in HUES.iter().enumerate() {
                        if let Some((v, d)) = slider(ui, name, b.mix[i], -100.0..=100.0, 0.0, 0) {
                            edit(app, MIX_KEYS[i], format!("B&W {name} {v:+.0}"), d, |s| {
                                s.modules.black_and_white.mix[i] = v;
                            });
                        }
                    }
                });
            });
        },
    );
    if reset {
        edit(app, "bw", "Reset black & white".into(), false, |s| {
            s.modules.black_and_white = BlackAndWhite::default();
        });
    }
}

/// A calibration slider: name, history key, value, and its field.
type CalRow = (
    &'static str,
    &'static str,
    f64,
    fn(&mut Calibration) -> &mut f64,
);

fn calibration(app: &mut ShoeboxApp, ui: &mut egui::Ui, c: &Calibration) {
    let reset = section(
        ui,
        "Calibration",
        *c != Calibration::default(),
        false,
        |ui| {
            egui::Grid::new("cal-grid").num_columns(2).show(ui, |ui| {
                let rows: [CalRow; 7] = [
                    ("Shadows tint", "cal0", c.shadows_tint, |c| {
                        &mut c.shadows_tint
                    }),
                    ("Red hue", "cal1", c.red_hue, |c| &mut c.red_hue),
                    ("Red saturation", "cal2", c.red_saturation, |c| {
                        &mut c.red_saturation
                    }),
                    ("Green hue", "cal3", c.green_hue, |c| &mut c.green_hue),
                    ("Green saturation", "cal4", c.green_saturation, |c| {
                        &mut c.green_saturation
                    }),
                    ("Blue hue", "cal5", c.blue_hue, |c| &mut c.blue_hue),
                    ("Blue saturation", "cal6", c.blue_saturation, |c| {
                        &mut c.blue_saturation
                    }),
                ];
                for (name, key, value, field) in rows {
                    if let Some((v, d)) = slider(ui, name, value, -100.0..=100.0, 0.0, 0) {
                        edit(app, key, format!("{name} {v:+.0}"), d, |s| {
                            *field(&mut s.modules.calibration) = v;
                        });
                    }
                }
            });
        },
    );
    if reset {
        edit(app, "cal", "Reset calibration".into(), false, |s| {
            s.modules.calibration = Calibration::default();
        });
    }
}
