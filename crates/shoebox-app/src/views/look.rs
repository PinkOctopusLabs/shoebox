//! Develop's look modules: tone curve, colour (HSL), black and white and
//! calibration (`shoebox_develop::cpu::look`), each a collapsible section.

use eframe::egui::{self, Color32, RichText, Sense, pos2, vec2};
use shoebox_develop::edit::{
    BlackAndWhite, Calibration, Clarity, Colour, Defringe, Dehaze, HUES, LensCorrection,
    NoiseReduction, Settings, Sharpening, ToneCurve, Transform,
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
    super::point_colour::section(app, ui, &s.modules.point_colour);
    super::grading::section(app, ui, &s.modules.colour_grading);
    detail(app, ui, s);
    lens(app, ui, &s.modules.lens);
    defringe(app, ui, &s.modules.defringe);
    transform(app, ui, &s.geometry.transform);
    black_and_white(app, ui, &s.modules.black_and_white);
    calibration(app, ui, &s.modules.calibration);
    effects(app, ui, &s.modules.effects);
}

/// A post-crop vignette and grain.
fn effects(app: &mut ShoeboxApp, ui: &mut egui::Ui, e: &shoebox_develop::edit::Effects) {
    let changed = *e != Default::default();
    let reset = section(ui, "Effects", changed, false, |ui| {
        let (v, g) = (&e.vignette, &e.grain);
        ui.label(RichText::new("Vignette").small().strong());
        egui::Grid::new("vignette-grid")
            .num_columns(2)
            .show(ui, |ui| {
                type Field = fn(&mut shoebox_develop::edit::Vignette) -> &mut f64;
                let rows: [(&str, &str, f64, f64, f64, f64, Field); 5] = [
                    ("Amount", "vg-a", v.amount, -100.0, 100.0, 0.0, |v| {
                        &mut v.amount
                    }),
                    ("Midpoint", "vg-m", v.midpoint, 0.0, 100.0, 50.0, |v| {
                        &mut v.midpoint
                    }),
                    ("Roundness", "vg-r", v.roundness, -100.0, 100.0, 0.0, |v| {
                        &mut v.roundness
                    }),
                    ("Feather", "vg-f", v.feather, 0.0, 100.0, 50.0, |v| {
                        &mut v.feather
                    }),
                    ("Highlights", "vg-h", v.highlights, 0.0, 100.0, 0.0, |v| {
                        &mut v.highlights
                    }),
                ];
                for (name, key, value, lo, hi, default, field) in rows {
                    if let Some(x) = slider(ui, name, value, lo..=hi, default, 0) {
                        edit(
                            app,
                            key,
                            format!("Vignette {} {x:.0}", name.to_lowercase()),
                            |s| {
                                *field(&mut s.modules.effects.vignette) = x;
                            },
                        );
                    }
                }
            });
        ui.label(RichText::new("Grain").small().strong());
        egui::Grid::new("grain-grid").num_columns(2).show(ui, |ui| {
            type Field = fn(&mut shoebox_develop::edit::Grain) -> &mut f64;
            let rows: [(&str, &str, f64, f64, Field); 3] = [
                ("Amount", "gr-a", g.amount, 0.0, |g| &mut g.amount),
                ("Size", "gr-s", g.size, 25.0, |g| &mut g.size),
                ("Roughness", "gr-r", g.roughness, 50.0, |g| &mut g.roughness),
            ];
            for (name, key, value, default, field) in rows {
                if let Some(x) = slider(ui, name, value, 0.0..=100.0, default, 0) {
                    edit(
                        app,
                        key,
                        format!("Grain {} {x:.0}", name.to_lowercase()),
                        |s| {
                            *field(&mut s.modules.effects.grain) = x;
                        },
                    );
                }
            }
        });
    });
    if reset {
        edit(app, "effects", "Reset effects".into(), |s| {
            s.modules.effects = Default::default()
        });
    }
}

/// Purple and green fringes along hard edges.
fn defringe(app: &mut ShoeboxApp, ui: &mut egui::Ui, d: &Defringe) {
    let reset = section(ui, "Defringe", *d != Defringe::default(), false, |ui| {
        egui::Grid::new("fringe-grid")
            .num_columns(2)
            .show(ui, |ui| {
                if let Some(v) = slider(ui, "Purple", d.purple, 0.0..=100.0, 0.0, 0) {
                    edit(app, "fringe-p", format!("Defringe purple {v:.0}"), |s| {
                        s.modules.defringe.purple = v;
                    });
                }
                if let Some(v) = slider(ui, "Green", d.green, 0.0..=100.0, 0.0, 0) {
                    edit(app, "fringe-g", format!("Defringe green {v:.0}"), |s| {
                        s.modules.defringe.green = v;
                    });
                }
            });
    });
    if reset {
        edit(app, "fringe", "Reset defringe".into(), |s| {
            s.modules.defringe = Defringe::default();
        });
    }
}

/// Perspective (vertical, horizontal), aspect, scale and offsets.
fn transform(app: &mut ShoeboxApp, ui: &mut egui::Ui, t: &Transform) {
    let reset = section(ui, "Transform", *t != Transform::default(), false, |ui| {
        ui.horizontal(|ui| {
            ui.label("Upright");
            for m in shoebox_develop::upright::Mode::ALL {
                let hint = match m {
                    shoebox_develop::upright::Mode::Auto => {
                        "Level and straighten, without overdoing it"
                    }
                    shoebox_develop::upright::Mode::Level => "Make the horizon level",
                    shoebox_develop::upright::Mode::Vertical => {
                        "Level, and make vertical lines vertical"
                    }
                    shoebox_develop::upright::Mode::Full => {
                        "Level, and straighten vertical and horizontal lines"
                    }
                };
                if ui.button(m.label()).on_hover_text(hint).clicked() {
                    upright(app, m);
                }
            }
        });
        egui::Grid::new("transform-grid")
            .num_columns(2)
            .show(ui, |ui| {
                let rows: [TransformRow; 6] = [
                    ("Vertical", "tf-v", t.vertical, -100.0..=100.0, 0.0, |t| {
                        &mut t.vertical
                    }),
                    (
                        "Horizontal",
                        "tf-h",
                        t.horizontal,
                        -100.0..=100.0,
                        0.0,
                        |t| &mut t.horizontal,
                    ),
                    ("Aspect", "tf-a", t.aspect, -100.0..=100.0, 0.0, |t| {
                        &mut t.aspect
                    }),
                    ("Scale", "tf-s", t.scale, 50.0..=150.0, 100.0, |t| {
                        &mut t.scale
                    }),
                    ("X offset", "tf-x", t.offset_x, -100.0..=100.0, 0.0, |t| {
                        &mut t.offset_x
                    }),
                    ("Y offset", "tf-y", t.offset_y, -100.0..=100.0, 0.0, |t| {
                        &mut t.offset_y
                    }),
                ];
                for (name, key, value, range, default, field) in rows {
                    if let Some(v) = slider(ui, name, value, range, default, 0) {
                        edit(app, key, format!("{name} {v:+.0}"), |s| {
                            *field(&mut s.geometry.transform) = v;
                        });
                    }
                }
            });
        let mut c = t.constrain;
        if ui
            .checkbox(&mut c, "Constrain")
            .on_hover_text("Zoom in just enough that no blank edge shows")
            .changed()
        {
            edit(
                app,
                "tf-c",
                format!("Constrain {}", if c { "on" } else { "off" }),
                |s| {
                    s.geometry.transform.constrain = c;
                },
            );
        }
    });
    if reset {
        edit(app, "transform", "Reset transform".into(), |s| {
            s.geometry.transform = Transform::default();
        });
    }
}

/// Auto-upright: the photo's lines decide its rotation (with the largest
/// crop that stays inside) and perspective, as one step.
pub fn upright(app: &mut ShoeboxApp, mode: shoebox_develop::upright::Mode) {
    let o = super::develop::options(app);
    let st = &app.develop;
    let (Some(ready), Some(doc)) = (&st.ready, &st.doc) else {
        return;
    };
    let s = doc.settings();
    let lines = shoebox_develop::upright::analyse(&ready.meta, &ready.src, s, o.orientation);
    let c = shoebox_develop::upright::solve(&lines, mode);
    let (w, h) =
        shoebox_develop::cpu::upright(ready.src.rgb.width, ready.src.rgb.height, o.orientation);
    let crop = shoebox_develop::cpu::max_crop(w as f64, h as f64, c.rotate);
    app.develop.last_step_done();
    edit(app, "upright", format!("Upright: {}", mode.label()), |s| {
        s.geometry.rotate = c.rotate;
        s.geometry.crop = crop;
        let t = &mut s.geometry.transform;
        t.vertical = c.vertical;
        t.horizontal = c.horizontal;
        t.constrain = true;
    });
}

/// A transform slider: name, history key, value, range, default, field.
type TransformRow = (
    &'static str,
    &'static str,
    f64,
    std::ops::RangeInclusive<f64>,
    f64,
    fn(&mut Transform) -> &mut f64,
);

/// Clarity and dehaze.
fn presence(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &Settings) {
    let (c, d) = (&s.modules.clarity, &s.modules.dehaze);
    let changed = *c != Clarity::default() || *d != Dehaze::default();
    let reset = section(ui, "Presence", changed, true, |ui| {
        egui::Grid::new("presence-grid")
            .num_columns(2)
            .show(ui, |ui| {
                if let Some(v) = slider(ui, "Texture", c.texture, -100.0..=100.0, 0.0, 0) {
                    edit(app, "texture", format!("Texture {v:+.0}"), |s| {
                        s.modules.clarity.texture = v;
                    });
                }
                if let Some(v) = slider(ui, "Clarity", c.amount, -100.0..=100.0, 0.0, 0) {
                    edit(app, "clarity", format!("Clarity {v:+.0}"), |s| {
                        s.modules.clarity.amount = v;
                    });
                }
                if let Some(v) = slider(ui, "Dehaze", d.amount, -100.0..=100.0, 0.0, 0) {
                    edit(app, "dehaze", format!("Dehaze {v:+.0}"), |s| {
                        s.modules.dehaze.amount = v;
                    });
                }
            });
    });
    if reset {
        edit(app, "presence", "Reset presence".into(), |s| {
            s.modules.clarity = Clarity::default();
            s.modules.dehaze = Dehaze::default();
        });
    }
}

/// Sharpening and noise reduction (applied before everything else; best
/// judged at 100%).
fn detail(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &Settings) {
    let (sh, nr) = (&s.modules.sharpening, &s.modules.noise_reduction);
    let ai = &s.modules.denoise_ai;
    let changed = *sh != Sharpening::default()
        || *nr != NoiseReduction::default()
        || *ai != shoebox_develop::edit::DenoiseAi::default();
    let raw = app
        .develop
        .ready
        .as_ref()
        .is_some_and(|r| r.src.referred == shoebox_develop::cpu::Referred::Scene);
    let reset = section(ui, "Detail", changed, false, |ui| {
        ui.weak("Best judged at 100% (Z).");
        ui.label(RichText::new("Sharpening").small().strong());
        egui::Grid::new("sharpen-grid")
            .num_columns(2)
            .show(ui, |ui| {
                if let Some(v) = slider(ui, "Amount", sh.amount, 0.0..=150.0, 0.0, 0) {
                    edit(app, "sh-amount", format!("Sharpening {v:.0}"), |s| {
                        s.modules.sharpening.amount = v;
                    });
                }
                if let Some(v) = slider(ui, "Radius", sh.radius, 0.5..=3.0, 1.0, 1) {
                    edit(app, "sh-radius", format!("Sharpening radius {v:.1}"), |s| {
                        s.modules.sharpening.radius = v;
                    });
                }
                if let Some(v) = slider(ui, "Detail", sh.detail, 0.0..=100.0, 25.0, 0) {
                    edit(app, "sh-detail", format!("Sharpening detail {v:.0}"), |s| {
                        s.modules.sharpening.detail = v;
                    });
                }
                if let Some(v) = slider(ui, "Masking", sh.masking, 0.0..=100.0, 0.0, 0) {
                    edit(app, "sh-mask", format!("Sharpening masking {v:.0}"), |s| {
                        s.modules.sharpening.masking = v;
                    });
                }
            });
        ui.label(RichText::new("Noise reduction").small().strong());
        if raw {
            ai_denoise(app, ui, ai);
        }
        egui::Grid::new("nr-grid").num_columns(2).show(ui, |ui| {
            if let Some(v) = slider(ui, "Luminance", nr.luminance, 0.0..=100.0, 0.0, 0) {
                edit(app, "nr-lum", format!("Noise reduction {v:.0}"), |s| {
                    s.modules.noise_reduction.luminance = v;
                });
            }
            if let Some(v) = slider(ui, "Detail ", nr.detail, 0.0..=100.0, 50.0, 0) {
                edit(
                    app,
                    "nr-detail",
                    format!("Noise reduction detail {v:.0}"),
                    |s| {
                        s.modules.noise_reduction.detail = v;
                    },
                );
            }
            if let Some(v) = slider(ui, "Colour", nr.chroma, 0.0..=100.0, 0.0, 0) {
                edit(
                    app,
                    "nr-chroma",
                    format!("Colour noise reduction {v:.0}"),
                    |s| {
                        s.modules.noise_reduction.chroma = v;
                    },
                );
            }
        });
    });
    if reset {
        edit(app, "detail", "Reset detail".into(), |s| {
            s.modules.sharpening = Sharpening::default();
            s.modules.noise_reduction = NoiseReduction::default();
            s.modules.denoise_ai = Default::default();
        });
    }
}

/// AI raw denoise: on or off, its strength, and how it's getting on.
fn ai_denoise(app: &mut ShoeboxApp, ui: &mut egui::Ui, ai: &shoebox_develop::edit::DenoiseAi) {
    let mut on = ai.enabled;
    if ui
        .checkbox(&mut on, "AI denoise")
        .on_hover_text(
            "Denoise and demosaic the raw data with RawNIND's model \
             (Benoit Brummer, CC BY 4.0): best on high-ISO photos. \
             Takes several seconds the first time; the photo shows as it was until then.",
        )
        .changed()
    {
        edit(
            app,
            "ai-on",
            format!("AI denoise {}", if on { "on" } else { "off" }),
            |s| s.modules.denoise_ai.enabled = on,
        );
    }
    if !ai.enabled {
        return;
    }
    let model = app.denoiser.clone();
    if !model.ready() {
        egui::Grid::new("ai-dl").num_columns(1).show(ui, |ui| {
            crate::models::download_ui(
                ui,
                &model,
                "the AI denoise model",
                "RawNIND's UtNet2, once, from the authors' Google Drive; it runs on this Mac",
            );
        });
        return;
    }
    if let Some(f) = app.develop.ai_progress {
        ui.add(egui::ProgressBar::new(f).text("Denoising…"));
    } else if let Some(e) = &app.develop.ai_error {
        ui.weak(format!("AI denoise couldn't run: {e}"));
    }
    egui::Grid::new("ai-grid").num_columns(2).show(ui, |ui| {
        if let Some(v) = slider(ui, "Strength", ai.strength, 0.0..=100.0, 100.0, 0) {
            edit(app, "ai-strength", format!("AI denoise {v:.0}"), |s| {
                s.modules.denoise_ai.strength = v;
            });
        }
    });
}

/// Distortion, vignetting and chromatic aberration, from the camera's own
/// data and Lensfun.
fn lens(app: &mut ShoeboxApp, ui: &mut egui::Ui, lc: &LensCorrection) {
    let shot = app.develop.ready.as_ref().and_then(|r| r.src.lens.clone());
    // The frame's size as the pipe sees it (a raw's full size).
    let size = app.develop.ready.as_ref().map(|r| {
        if r.meta.width > 0 {
            (r.meta.width, r.meta.height)
        } else {
            (r.src.rgb.width, r.src.rgb.height)
        }
    });
    let reset = section(
        ui,
        "Lens corrections",
        *lc != LensCorrection::default(),
        false,
        |ui| {
            let Some(shot) = shot else {
                ui.weak("The photo doesn't say which camera and lens took it.");
                return;
            };
            let mut on = lc.enabled;
            if ui.checkbox(&mut on, "Correct the lens").changed() {
                let label = if on {
                    "Lens corrections on"
                } else {
                    "Lens corrections off"
                };
                edit(app, "lens", label.into(), |s| s.modules.lens.enabled = on);
            }
            if !lc.enabled {
                return;
            }
            let found = size.and_then(|(w, h)| shoebox_develop::lens::grid(&shot, lc, w, h));
            match &found {
                Some(g) => ui.weak(format!("Using: {}", g.source)),
                None => ui.weak(format!(
                    "No correction known for {}",
                    if shot.lens.is_empty() {
                        "this lens"
                    } else {
                        &shot.lens
                    }
                )),
            };
            egui::Grid::new("lens-grid").num_columns(2).show(ui, |ui| {
                if let Some(v) = slider(ui, "Distortion", lc.distortion, 0.0..=200.0, 100.0, 0) {
                    edit(app, "lens-d", format!("Distortion {v:.0}%"), |s| {
                        s.modules.lens.distortion = v;
                    });
                }
                if let Some(v) = slider(ui, "Vignetting", lc.vignetting, 0.0..=200.0, 100.0, 0) {
                    edit(app, "lens-v", format!("Vignetting {v:.0}%"), |s| {
                        s.modules.lens.vignetting = v;
                    });
                }
            });
            let mut ca = lc.chromatic;
            if ui
                .checkbox(&mut ca, "Remove chromatic aberration")
                .changed()
            {
                edit(
                    app,
                    "lens-ca",
                    format!("Chromatic aberration {}", if ca { "on" } else { "off" }),
                    |s| {
                        s.modules.lens.chromatic = ca;
                    },
                );
            }
            let chosen = lc.lens.clone();
            let mut pick = None;
            ui.horizontal(|ui| {
                ui.label("Lens");
                egui::ComboBox::from_id_salt("lens-profile")
                    .selected_text(chosen.as_deref().unwrap_or("Automatic"))
                    .width(ui.available_width() - 8.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(chosen.is_none(), "Automatic").clicked() {
                            pick = Some(None);
                        }
                        for name in shoebox_develop::lens::lenses_for(&shot) {
                            let on = chosen.as_deref() == Some(name.as_str());
                            if ui.selectable_label(on, &name).clicked() {
                                pick = Some(Some(name));
                            }
                        }
                    });
            });
            if let Some(choice) = pick {
                let label = format!("Lens {}", choice.as_deref().unwrap_or("automatic"));
                edit(app, "lens-pick", label, |s| s.modules.lens.lens = choice);
            }
        },
    );
    if reset {
        edit(app, "lens", "Reset lens corrections".into(), |s| {
            s.modules.lens = LensCorrection::default();
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
                if let Some(v) = slider(ui, name, t.regions[i], -100.0..=100.0, 0.0, 0) {
                    edit(
                        app,
                        key,
                        format!("Curve {} {v:+.0}", name.to_lowercase()),
                        |s| {
                            s.modules.tone_curve.regions[i] = v;
                        },
                    );
                }
            }
        });
    });
    if reset {
        edit(app, "curve", "Reset tone curve".into(), |s| {
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

/// The point curve: drag a point to move it, click or drag elsewhere to
/// add one, double-click (or right-click) a point to remove it. RGB shows the parametric curve too.
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
        "Click or drag to add or move a point; double-click or right-click a point to remove it",
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
        set = Some(points.clone());
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
                    set = Some(points.clone());
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
        set = Some(points.clone());
    }
    // A click away from the points adds one there too.
    if set.is_none()
        && resp.clicked()
        && let Some(at) = resp.interact_pointer_pos()
        && near(at, &points).is_none()
    {
        let (x, y) = from_screen(at);
        if x > 0.01 && x < 0.99 {
            let i = points.partition_point(|p| p[0] < x);
            points.insert(i, [x, y]);
            set = Some(points.clone());
        }
    }
    if resp.drag_stopped() {
        app.develop.curve_drag = None;
    }
    if let Some(pts) = set {
        let name = CHANNELS[channel].0;
        edit(app, "curve", format!("Curve {name}"), |s| {
            *points_mut(&mut s.modules.tone_curve, channel) = pts;
        });
    }
}

fn colour(app: &mut ShoeboxApp, ui: &mut egui::Ui, c: &Colour) {
    let reset = section(ui, "Colour", *c != Colour::default(), true, |ui| {
        egui::Grid::new("colour-grid")
            .num_columns(2)
            .show(ui, |ui| {
                if let Some(v) = slider(ui, "Vibrance", c.vibrance, -100.0..=100.0, 0.0, 0) {
                    edit(app, "vibrance", format!("Vibrance {v:+.0}"), |s| {
                        s.modules.colour.vibrance = v;
                    });
                }
                if let Some(v) = slider(ui, "Saturation", c.saturation, -100.0..=100.0, 0.0, 0) {
                    edit(app, "saturation", format!("Saturation {v:+.0}"), |s| {
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
                                let Some(v) = slider(ui, name, values[i], -100.0..=100.0, 0.0, 0)
                                else {
                                    continue;
                                };
                                let label = format!("{name} {} {v:+.0}", title.to_lowercase());
                                edit(app, keys[i], label, |s| {
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
        edit(app, "colour", "Reset colour".into(), |s| {
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
                edit(app, "bw", label.into(), |s| {
                    s.modules.black_and_white.enabled = on;
                });
            }
            ui.add_enabled_ui(b.enabled, |ui| {
                egui::Grid::new("bw-grid").num_columns(2).show(ui, |ui| {
                    for (i, name) in HUES.iter().enumerate() {
                        if let Some(v) = slider(ui, name, b.mix[i], -100.0..=100.0, 0.0, 0) {
                            edit(app, MIX_KEYS[i], format!("B&W {name} {v:+.0}"), |s| {
                                s.modules.black_and_white.mix[i] = v;
                            });
                        }
                    }
                });
            });
        },
    );
    if reset {
        edit(app, "bw", "Reset black & white".into(), |s| {
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
                    if let Some(v) = slider(ui, name, value, -100.0..=100.0, 0.0, 0) {
                        edit(app, key, format!("{name} {v:+.0}"), |s| {
                            *field(&mut s.modules.calibration) = v;
                        });
                    }
                }
            });
        },
    );
    if reset {
        edit(app, "cal", "Reset calibration".into(), |s| {
            s.modules.calibration = Calibration::default();
        });
    }
}
