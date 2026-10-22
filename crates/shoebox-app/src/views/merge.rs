//! Photo merges (EDITING-SPEC, Phase 4 additions; SPEC, Merges): Merge to
//! HDR… in the photo menu (⌃H) opens a dialog listing the photos chosen,
//! with what's wrong if they can't be merged, the options, and what will
//! be written where (rule 6's dry run: one new file, from these photos,
//! which are only read). Merge runs on the develop thread's merge thread,
//! the status bar showing how far (with Stop); the library worker adds the
//! DNG, and the new photo is selected once the library shows it. The Info
//! panel lists a merge's sources, and a source's merges.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, RichText};
use shoebox_core::merges::Kind;
use shoebox_develop::merge::hdr::{self, Deghost};

use crate::app::ShoeboxApp;

#[derive(Default)]
pub struct State {
    /// The dialog, with these photos in capture order.
    pub open: Option<Vec<PathBuf>>,
    pub align: Option<bool>,
    pub deghost: Deghost,
    /// The reference, by place in the list (None: the middle exposure).
    pub reference: Option<usize>,
    /// Merging: how far (0–1), and how to stop it.
    pub progress: Option<f32>,
    cancel: Option<Arc<AtomicBool>>,
    /// Select this file once the library shows it.
    pub select: Option<PathBuf>,
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Open the dialog with the selected photos (each stack's primary).
pub fn open(app: &mut ShoeboxApp) {
    let mut photos: Vec<(Option<f64>, PathBuf)> = app
        .selected_stacks()
        .into_iter()
        .map(|s| {
            let p = &app.catalog.photos[app.catalog.stacks[s].primary];
            (p.meta.capture_seconds(), p.path.clone())
        })
        .collect();
    photos.sort_by(|a, b| {
        a.0.unwrap_or(f64::MAX)
            .total_cmp(&b.0.unwrap_or(f64::MAX))
            .then_with(|| a.1.cmp(&b.1))
    });
    let st = &mut app.merge;
    st.open = Some(photos.into_iter().map(|(_, p)| p).collect());
    st.reference = None;
}

/// "1/250 s · f/2.8 · ISO 400".
fn exposure(x: &shoebox_core::exif::ExifInfo) -> String {
    let mut parts = Vec::new();
    if let Some(e) = &x.exposure {
        let e = e.trim().trim_end_matches('s').trim();
        parts.push(format!("{e} s"));
    }
    if let Some(f) = x.fnumber {
        parts.push(format!("f/{f:.1}"));
    }
    if let Some(i) = x.iso {
        parts.push(format!("ISO {i}"));
    }
    parts.join(" · ")
}

/// Why these photos can't be merged to HDR (None: they can).
fn refusal(app: &ShoeboxApp, photos: &[PathBuf]) -> Option<String> {
    if !(hdr::MIN_FRAMES..=hdr::MAX_FRAMES).contains(&photos.len()) {
        return Some(format!(
            "Choose {} to {} bracketed photos.",
            hdr::MIN_FRAMES,
            hdr::MAX_FRAMES
        ));
    }
    let mut size = None;
    let mut camera = None;
    for p in photos {
        let Some(i) = app.index(crate::jobs::photo_id(p)) else {
            return Some(format!("{} isn't in the library any more.", name(p)));
        };
        let photo = &app.catalog.photos[i];
        if !photo.format.is_raw() {
            return Some(format!(
                "{} isn't a raw photo: HDR merges need raw files.",
                name(p)
            ));
        }
        if app.catalog.is_reference(i) {
            return Some(format!(
                "{} is in a reference folder, which is read-only.",
                name(p)
            ));
        }
        if photo.cloud {
            return Some(format!(
                "{} is in the cloud only: download it first.",
                name(p)
            ));
        }
        let s = (photo.meta.width, photo.meta.height);
        if *size.get_or_insert(s) != s {
            return Some("The photos aren't all the same size.".into());
        }
        let c = photo.meta.exif.model.clone();
        if *camera.get_or_insert(c.clone()) != c {
            return Some("The photos are from different cameras.".into());
        }
    }
    None
}

/// The dialog.
pub fn dialog(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let Some(photos) = app.merge.open.clone() else {
        return;
    };
    let refused = refusal(app, &photos);
    let busy = app.merge.progress.is_some();
    let rows: Vec<(String, String, Option<f64>)> = photos
        .iter()
        .map(|p| {
            let x = app
                .index(crate::jobs::photo_id(p))
                .map(|i| app.catalog.photos[i].meta.exif.clone())
                .unwrap_or_default();
            (name(p), exposure(&x), x.captured_stops())
        })
        .collect();
    // The middle exposure, as the merge picks it.
    let auto = {
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by(|&a, &b| {
            rows[a]
                .2
                .unwrap_or(0.0)
                .total_cmp(&rows[b].2.unwrap_or(0.0))
        });
        order
            .get(order.len().saturating_sub(1) / 2)
            .copied()
            .unwrap_or(0)
    };
    let reference = app
        .merge
        .reference
        .filter(|&r| r < rows.len())
        .unwrap_or(auto);
    let mut open = true;
    let mut go = false;
    let mut cancel = false;
    egui::Window::new("Merge to HDR")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!(
                "{} bracketed photos, merged into one raw photo with the range of all of them:",
                rows.len()
            ));
            ui.add_space(4.0);
            egui::Grid::new("merge-photos")
                .num_columns(3)
                .spacing(egui::vec2(12.0, 4.0))
                .show(ui, |ui| {
                    let base = rows.get(reference).and_then(|r| r.2);
                    for (i, (n, e, stops)) in rows.iter().enumerate() {
                        ui.label(n);
                        ui.weak(e);
                        let rel = match (stops, base) {
                            _ if i == reference => "reference".to_owned(),
                            (Some(s), Some(b)) => format!("{:+.1} EV", s - b),
                            _ => String::new(),
                        };
                        ui.weak(rel);
                        ui.end_row();
                    }
                });
            ui.add_space(6.0);
            let st = &mut app.merge;
            let mut align = st.align.unwrap_or(true);
            if ui
                .checkbox(&mut align, "Align the photos")
                .on_hover_text("For brackets taken hand-held; a tripod's need none")
                .changed()
            {
                st.align = Some(align);
            }
            ui.horizontal(|ui| {
                ui.label("Deghost");
                egui::ComboBox::from_id_salt("merge-deghost")
                    .selected_text(st.deghost.label())
                    .show_ui(ui, |ui| {
                        for d in Deghost::ALL {
                            ui.selectable_value(&mut st.deghost, d, d.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "Things that moved between the photos are taken from the reference alone",
                    );
            });
            ui.horizontal(|ui| {
                ui.label("Reference");
                let shown = match st.reference {
                    None => format!("Middle exposure ({})", rows[auto].0),
                    Some(r) => rows.get(r).map(|r| r.0.clone()).unwrap_or_default(),
                };
                egui::ComboBox::from_id_salt("merge-reference")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut st.reference, None, "Middle exposure");
                        for (i, r) in rows.iter().enumerate() {
                            ui.selectable_value(&mut st.reference, Some(i), &r.0);
                        }
                    })
                    .response
                    .on_hover_text(
                        "Its brightness, colour and EXIF become the merge's, and moving things come from it",
                    );
            });
            ui.add_space(6.0);
            if let Some(first) = photos.first() {
                let stem = first
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let folder = first
                    .parent()
                    .and_then(|f| f.file_name())
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(format!(
                    "Writes {stem}-{}.dng beside {} in {folder} (with -1, -2 if the name's taken). \
                     The photos themselves are only read.",
                    Kind::Hdr.suffix(),
                    name(first)
                ));
                ui.weak("The new photo's edit starts from the defaults. ⌘Z sends it to the Trash.");
            }
            if let Some(why) = &refused {
                ui.add_space(4.0);
                ui.colored_label(ui.visuals().warn_fg_color, why);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(refused.is_none() && !busy, egui::Button::new("Merge"))
                    .on_disabled_hover_text(if busy {
                        "A merge is running"
                    } else {
                        "These photos can't be merged"
                    })
                    .clicked()
                {
                    go = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
    if !open || cancel {
        app.merge.open = None;
    }
    if go {
        let sources: Vec<(PathBuf, u16)> = photos
            .iter()
            .filter_map(|p| {
                let i = app.index(crate::jobs::photo_id(p))?;
                Some((p.clone(), app.catalog.photos[i].meta.orientation()))
            })
            .collect();
        let flag = Arc::new(AtomicBool::new(false));
        let st = &mut app.merge;
        let job = crate::merge::Job {
            kind: Kind::Hdr,
            sources,
            options: hdr::Options {
                align: st.align.unwrap_or(true),
                deghost: st.deghost,
                reference: st.reference,
            },
            roots: app.catalog.roots.clone(),
            cancel: flag.clone(),
        };
        st.cancel = Some(flag);
        st.progress = Some(0.0);
        st.open = None;
        app.developer.merge(job);
    }
}

/// The merge thread's news (from `views::develop::poll`).
pub fn progress(app: &mut ShoeboxApp, f: f32) {
    if app.merge.progress.is_some() {
        app.merge.progress = Some(f);
    }
}

pub fn done(app: &mut ShoeboxApp, r: Result<crate::merge::Done, String>) {
    let stopped = app
        .merge
        .cancel
        .take()
        .is_some_and(|c| c.load(Ordering::Relaxed));
    app.merge.progress = None;
    match r {
        Ok(d) => app.command(crate::jobs::LibraryCommand::AddMerge(Box::new(d))),
        Err(_) if stopped => app.notice = Some("HDR merge stopped.".into()),
        Err(e) => app.error = Some(format!("HDR merge: {e}")),
    }
}

/// Stop the merge running.
pub fn stop(app: &mut ShoeboxApp) {
    if let Some(c) = &app.merge.cancel {
        c.store(true, Ordering::Relaxed);
    }
}

/// Each frame: select the new merge once the library shows it.
pub fn tick(app: &mut ShoeboxApp) {
    let Some(path) = app.merge.select.clone() else {
        return;
    };
    let id = crate::jobs::photo_id(&path);
    if app.index(id).is_none() {
        return;
    }
    app.merge.select = None;
    if app.shown.iter().any(|t| t.id == id) {
        app.activate(id);
        app.scroll_to_current = true;
    } else {
        app.show_in_all(id);
    }
}

/// The Info panel: what a merge was made from, and what a photo was
/// merged into.
pub fn info_section(app: &mut ShoeboxApp, ui: &mut egui::Ui, photo: &Path) {
    let of = shoebox_core::merges::of_file(&app.catalog.merges, photo).cloned();
    let into: Vec<PathBuf> = shoebox_core::merges::made_from(&app.catalog.merges, photo)
        .map(|m| m.file.clone())
        .collect();
    if of.is_none() && into.is_empty() {
        return;
    }
    let heading = |ui: &mut egui::Ui, t: &str| {
        ui.add_space(6.0);
        ui.label(
            RichText::new(t)
                .size(11.0)
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
    };
    let mut go = None;
    if let Some(m) = &of {
        heading(ui, &format!("{} OF", m.kind.label().to_uppercase()));
        for s in &m.sources {
            let there = app.index(crate::jobs::photo_id(&s.path)).is_some();
            ui.horizontal(|ui| {
                if there {
                    if ui
                        .link(name(&s.path))
                        .on_hover_text(s.path.display().to_string())
                        .clicked()
                    {
                        go = Some(s.path.clone());
                    }
                } else {
                    ui.weak(name(&s.path))
                        .on_hover_text("Not in the library where it was");
                }
            });
        }
    }
    if !into.is_empty() {
        heading(ui, "MERGED INTO");
        for f in &into {
            if ui
                .link(name(f))
                .on_hover_text(f.display().to_string())
                .clicked()
            {
                go = Some(f.clone());
            }
        }
    }
    if let Some(p) = go {
        app.show_in_all(crate::jobs::photo_id(&p));
    }
}
