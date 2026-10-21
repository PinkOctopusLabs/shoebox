//! File operations in the app: Tasks → Import, Tasks → History, and the
//! dialogs for move, rename, reject, trash and new folder. Every operation
//! shows its source → destination table first and runs as one batch.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use shoebox_core::import::Plan;
use shoebox_core::template::{Fields, Template};

use crate::app::ShoeboxApp;
use crate::jobs::{LibraryCommand, OrgRequest};
use crate::views::duplicates::plural;

/// `root name/relative path` when under a library root.
fn short(app: &ShoeboxApp, p: &Path) -> String {
    app.settings
        .roots
        .iter()
        .find_map(|r| {
            p.strip_prefix(&r.path).ok().map(|rel| {
                format!(
                    "{}/{}",
                    r.path.file_name().unwrap_or_default().to_string_lossy(),
                    rel.display()
                )
            })
        })
        .unwrap_or_else(|| p.display().to_string())
}

/// A template's result for a sample frame, or why it's invalid.
pub(crate) fn sample(template: &str, import: bool) -> Result<String, String> {
    let t = Template::parse(template).map_err(|e| e.to_string())?;
    let f = Fields {
        capture_time: Some("2026:05:01 12:34:56".into()),
        subsec: Some("77".into()),
        file_name: "DSC_0042.NEF".into(),
        camera: Some("NIKON Z f".into()),
        width: Some(6048),
        height: Some(4032),
        seq: 1,
    };
    let p = t.render(&f).display().to_string();
    if !import && p.contains('/') {
        Ok(format!("{p}  (moves files: has folders)"))
    } else {
        Ok(p)
    }
}

/// A source → destination table, virtualised.
fn table(ui: &mut egui::Ui, id: &str, rows: &[(String, String)], height: f32) {
    let row_h = 18.0;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(height)
        .auto_shrink([false, true])
        .show_rows(ui, row_h, rows.len(), |ui, range| {
            for (a, b) in &rows[range] {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(a).monospace().small()).truncate());
                    ui.weak("▶");
                    ui.add(egui::Label::new(RichText::new(b).monospace().small()).truncate());
                });
            }
        });
}

/// A photo's thumbnail fitted into `rect` (or `fallback` while it loads).
fn draw_thumb(app: &mut ShoeboxApp, ui: &egui::Ui, rect: egui::Rect, id: u64, fallback: &str) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
    match app.textures.best(id, crate::jobs::Size::Small) {
        Some(t) => {
            let [w, h] = t.size();
            painter.image(
                t.id(),
                super::fit(rect.shrink(3.0), w as f32 / h as f32),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                fallback,
                egui::FontId::proportional(11.0),
                ui.visuals().weak_text_color(),
            );
        }
    }
}

/// A source file's thumbnail (rendered from the source, not cached).
fn source_thumb(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    f: &shoebox_core::import::SourceFile,
    size: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    app.want_preview(f);
    draw_thumb(
        app,
        ui,
        rect,
        crate::jobs::photo_id(&f.path),
        f.format.label(),
    );
    resp
}

/// A library photo's thumbnail (from the thumbnail cache, as in the grid).
fn library_thumb(app: &mut ShoeboxApp, ui: &mut egui::Ui, path: &Path, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let id = crate::jobs::photo_id(path);
    app.want(id, crate::jobs::Size::Small, crate::jobs::Priority::Visible);
    draw_thumb(app, ui, rect, id, "?");
}

/// The photos an import will copy, as thumbnails (each with its source and
/// new name on hover).
fn previews(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    photos: &[&shoebox_core::import::PlannedPhoto],
    source: &Path,
) {
    const CELL: f32 = 112.0;
    const GAP: f32 = 4.0;
    let cols = (((ui.available_width() + GAP) / (CELL + GAP)).floor() as usize).max(1);
    let rows = photos.len().div_ceil(cols);
    egui::ScrollArea::vertical()
        .id_salt("import-previews")
        .max_height(ui.available_height() * 0.7)
        .auto_shrink([false, true])
        .show_rows(ui, CELL + GAP, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for p in photos.iter().skip(row * cols).take(cols) {
                        let resp = source_thumb(app, ui, &p.src, CELL);
                        let src = rel_to(source, &p.src.path);
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Image, true, &src)
                        });
                        resp.on_hover_ui(|ui| {
                            ui.label(RichText::new(&src).monospace().small());
                            ui.label(
                                RichText::new(format!("▶ {}", short(app, &p.dst)))
                                    .monospace()
                                    .small(),
                            );
                        });
                    }
                });
                ui.add_space(GAP);
            }
        });
}

/// `p` relative to the import source, for display.
fn rel_to(source: &Path, p: &Path) -> String {
    p.strip_prefix(source).unwrap_or(p).display().to_string()
}

/// Files left out of an import (already in the library, or repeated in the
/// source): each beside the file it matched, to check, with a way to copy
/// it anyway.
fn left_out(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    title: &str,
    matches: &[shoebox_core::import::Match],
    source: &Path,
) {
    use shoebox_core::import::Plan;
    if matches.is_empty() {
        return;
    }
    const THUMB: f32 = 64.0;
    let forced = matches.len() - Plan::left_out(matches);
    let head = if forced > 0 {
        format!("{title} ({}, {forced} copied anyway)", matches.len())
    } else {
        format!("{title} ({})", matches.len())
    };
    egui::CollapsingHeader::new(head)
        .id_salt(title)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("import-left-out", title))
                .max_height(320.0)
                .auto_shrink([false, true])
                .show_rows(ui, THUMB + 6.0, matches.len(), |ui, range| {
                    for m in &matches[range] {
                        ui.horizontal(|ui| {
                            source_thumb(app, ui, &m.src, THUMB);
                            ui.weak("=");
                            match &m.other_src {
                                Some(f) => {
                                    source_thumb(app, ui, f, THUMB);
                                }
                                None => library_thumb(app, ui, &m.other, THUMB),
                            }
                            ui.vertical(|ui| {
                                let src = rel_to(source, &m.src.path);
                                ui.label(RichText::new(&src).monospace().small());
                                let other = match m.other_src {
                                    Some(_) => rel_to(source, &m.other),
                                    None => short(app, &m.other),
                                };
                                ui.weak(RichText::new(format!("same as {other}")).small());
                                let (text, on) = if m.forced {
                                    ("Leave out", false)
                                } else {
                                    ("Import anyway", true)
                                };
                                ui.horizontal(|ui| {
                                    let r = ui.small_button(text);
                                    r.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::Role::Button,
                                            true,
                                            format!("{text}: {src}"),
                                        )
                                    });
                                    if r.clicked() {
                                        app.force_import(&m.src.path, on);
                                    }
                                    if m.forced {
                                        ui.weak(RichText::new("copied anyway").small());
                                    }
                                });
                            });
                        });
                        ui.add_space(6.0);
                    }
                });
        });
}

/// Where an import's copying is: a bar by bytes, files and bytes done, the
/// speed and time left so far, the file being copied, and Cancel.
fn copying(app: &mut ShoeboxApp, ui: &mut egui::Ui, c: &crate::app::Copying, source: &Path) {
    use crate::panels::{human_size, time_left};
    let fraction = if c.total_bytes > 0 {
        c.bytes as f32 / c.total_bytes as f32
    } else {
        c.done as f32 / c.files.max(1) as f32
    };
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(320.0)
                    .text(format!("Copying {} of {}", c.done, c.files)),
            );
            if ui.button("Cancel").clicked() {
                app.cancel_import();
            }
        });
        let mut line = format!("{} of {}", human_size(c.bytes), human_size(c.total_bytes));
        match c.rate() {
            Some(rate) => {
                line.push_str(&format!(" · {}/s", human_size(rate as u64)));
                if let Some(left) = c.left() {
                    line.push_str(&format!(" · {}", time_left(left)));
                }
            }
            None => line.push_str(" · starting…"),
        }
        ui.weak(line);
        if let Some(p) = &c.current {
            ui.weak(RichText::new(rel_to(source, p)).monospace().small());
        }
        if let Some(name) = app.import.volume.as_deref().and_then(Path::file_name) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "Keep {} connected until the import finishes.",
                    name.to_string_lossy()
                ),
            );
        }
    });
}

/// Quitting while an import copies: keep going, or stop it (cleanly, between
/// files) and quit once it has stopped.
fn quit_dialog(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let (done, files) = app
        .import
        .copying
        .as_ref()
        .map_or((0, 0), |c| (c.done, c.files));
    let resp = egui::Modal::new(egui::Id::unique("quit-while-importing")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading("An import is still copying");
        ui.add_space(4.0);
        ui.label(format!(
            "{done} of {files} files are copied. Stopping now keeps those (undo them from \
             History) and leaves the rest uncopied; finding photos again finishes the job."
        ));
        if let Some(name) = app.import.volume.as_deref().and_then(Path::file_name) {
            ui.label(format!(
                "Keep {} connected until Shoebox has quit.",
                name.to_string_lossy()
            ));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new("Keep Importing").strong())
                .clicked()
            {
                app.confirm_quit = false;
            }
            if ui.button("Stop and Quit").clicked() {
                app.stop_import_and_quit();
            }
        });
    });
    if resp.should_close() {
        app.confirm_quit = false;
    }
}

/// The plan's photos to show: all, or those the chosen folder's name goes
/// on (cached per plan and folder).
fn shown_photos(app: &mut ShoeboxApp, plan: &Arc<Plan>) -> Option<(PathBuf, Arc<Vec<usize>>)> {
    let folder = app.import.filter.clone()?;
    let key = Arc::as_ptr(plan) as usize;
    if let Some((f, k, idx)) = &app.import.shown
        && *f == folder
        && *k == key
    {
        return Some((folder, idx.clone()));
    }
    let origins: HashMap<&Path, &BTreeSet<PathBuf>> = plan
        .origins
        .iter()
        .map(|(p, rels)| (p.as_path(), rels))
        .collect();
    let idx: Arc<Vec<usize>> = Arc::new(
        plan.photos
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                origins
                    .get(p.dst.as_path())
                    .is_some_and(|rels| rels.iter().any(|r| r.starts_with(&folder)))
            })
            .map(|(i, _)| i)
            .collect(),
    );
    app.import.shown = Some((folder.clone(), key, idx.clone()));
    Some((folder, idx))
}

/// Tasks → Import.
pub fn import(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.heading("Import");
    ui.weak("Copies photos from a card or folder into the library. The source is never changed; every copy is checked byte for byte.");
    ui.add_space(6.0);
    if app.import.template.is_empty() {
        app.import.template = app.settings.import_template().to_owned();
    }
    let writable = app.settings.writable_roots();
    if app.import.root.is_none() {
        app.import.root = writable.first().cloned();
    }
    let busy = app.import.running || app.import.copying.is_some();
    egui::Grid::new("import-form")
        .num_columns(2)
        .spacing(egui::vec2(10.0, 6.0))
        .show(ui, |ui| {
            ui.label("From");
            ui.horizontal(|ui| {
                let text = app
                    .import
                    .source
                    .as_ref()
                    .map_or("No folder chosen".to_owned(), |p| p.display().to_string());
                ui.label(text);
                if ui
                    .add_enabled(!busy, egui::Button::new("Choose…"))
                    .clicked()
                {
                    app.picker.open(
                        crate::picker::FolderFor::ImportSource,
                        "Import from (a card or folder)",
                        None,
                    );
                }
            });
            ui.end_row();
            ui.label("Into");
            if writable.is_empty() {
                ui.weak("Add a library folder that isn't a reference folder first");
            } else {
                egui::ComboBox::from_id_salt("import-root")
                    .selected_text(
                        app.import
                            .root
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    )
                    .show_ui(ui, |ui| {
                        for r in &writable {
                            ui.selectable_value(
                                &mut app.import.root,
                                Some(r.clone()),
                                r.display().to_string(),
                            );
                        }
                    });
            }
            ui.end_row();
            ui.label("Names");
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.import.template).desired_width(460.0));
                match sample(&app.import.template, true) {
                    Ok(s) => ui.weak(format!("e.g. {s}")),
                    Err(e) => ui.colored_label(ui.visuals().warn_fg_color, e),
                };
            });
            ui.end_row();
        });
    let ready = app.import.source.is_some()
        && app.import.root.is_some()
        && Template::parse(&app.import.template).is_ok()
        && !busy;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(ready, egui::Button::new("Find photos"))
            .clicked()
        {
            app.plan_import();
        }
        if app.import.running {
            ui.add(egui::Spinner::new());
            if let Some((d, t)) = app.import.progress {
                ui.label(format!("Reading {d}/{t}"));
            }
            if ui.button("Cancel").clicked() {
                app.cancel_import();
            }
        }
        if let Some(m) = &app.import.message {
            ui.label(RichText::new(m).color(Color32::from_rgb(80, 200, 120)));
        }
        if app.import.copying.is_none()
            && let Some(name) = app.import.volume.as_deref().and_then(Path::file_name)
            && ui
                .button(format!("Eject {}", name.to_string_lossy()))
                .clicked()
        {
            app.eject_import_volume();
        }
    });
    ui.separator();
    let Some(plan) = app.import.plan.clone() else {
        return;
    };
    let plan = match plan {
        Ok(p) => p,
        Err(e) => {
            ui.colored_label(ui.visuals().warn_fg_color, e);
            return;
        }
    };
    let images = plan.photos.len();
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!(
                "{} to copy{}",
                plural(images, "photo"),
                if plan.copies.len() > images {
                    format!(" (and {})", plural(plan.copies.len() - images, "sidecar"))
                } else {
                    String::new()
                }
            ))
            .strong(),
        );
        let (skipped, repeats) = (Plan::left_out(&plan.skipped), Plan::left_out(&plan.repeats));
        if skipped > 0 {
            ui.label(format!(
                "· {} already in the library",
                plural(skipped, "file")
            ));
        }
        if repeats > 0 {
            ui.label(format!(
                "· {} repeated in the source",
                plural(repeats, "file")
            ));
        }
        if plan.undated > 0 {
            ui.label(format!(
                "· {} without a capture date (to _undated)",
                plan.undated
            ));
        }
        if plan.downloaded > 0 {
            ui.label(format!(
                "· {} downloaded from the cloud to read them",
                plural(plan.downloaded, "file")
            ));
        }
        if !plan.unreadable.is_empty() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("· {} unreadable", plan.unreadable.len()),
            );
        }
    });
    folder_names(app, ui);
    let source = app.import.source.clone().unwrap_or_default();
    let shown = shown_photos(app, &plan);
    ui.horizontal(|ui| {
        ui.selectable_value(&mut app.import.list, false, "Thumbnails");
        ui.selectable_value(&mut app.import.list, true, "List");
        if let Some((folder, idx)) = &shown {
            ui.separator();
            ui.label(format!(
                "{} named by {}",
                plural(idx.len(), "photo"),
                folder.display()
            ));
            if ui.small_button("Show all").clicked() {
                app.import.filter = None;
            }
        }
    });
    let photos: Vec<&shoebox_core::import::PlannedPhoto> = match &shown {
        Some((_, idx)) => idx.iter().filter_map(|&i| plan.photos.get(i)).collect(),
        None => plan.photos.iter().collect(),
    };
    if app.import.list {
        let rows: Vec<(String, String)> = if shown.is_some() {
            photos
                .iter()
                .map(|p| (rel_to(&source, &p.src.path), short(app, &p.dst)))
                .collect()
        } else {
            plan.copies
                .iter()
                .map(|c| (rel_to(&source, &c.src), short(app, &c.dst)))
                .collect()
        };
        table(ui, "import-table", &rows, ui.available_height() * 0.7);
    } else {
        previews(app, ui, &photos, &source);
    }
    // After the photos: look them over, then import.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let go = format!("Import {}", plural(plan.copies.len(), "file"));
        let button = egui::Button::new(
            RichText::new(go)
                .strong()
                .color(ui.visuals().selection.stroke.color),
        )
        .fill(ui.visuals().selection.bg_fill)
        .min_size(egui::vec2(0.0, 26.0));
        if ui
            .add_enabled(!plan.copies.is_empty() && !busy, button)
            .clicked()
        {
            app.run_import();
        }
        if let Some(c) = app.import.copying.clone() {
            copying(app, ui, &c, &source);
        } else if let Some(name) = app.import.root.as_deref().and_then(Path::file_name) {
            ui.weak(format!("into {}", name.to_string_lossy()));
        }
    });
    ui.add_space(6.0);
    left_out(app, ui, "Already in the library", &plan.skipped, &source);
    left_out(
        app,
        ui,
        "Repeated in the source, copied once",
        &plan.repeats,
        &source,
    );
    for (p, e) in &plan.unreadable {
        ui.colored_label(ui.visuals().warn_fg_color, format!("{}: {e}", p.display()));
    }
}

/// Import's folder list: what each source folder's name becomes on the
/// photos in it (an event, a keyword or nothing), renamable.
fn folder_names(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    use shoebox_core::import::{FolderUse, names};
    if app.import.folders.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(format!("Folder names ({})", app.import.folders.len()))
        .default_open(true)
        .show(ui, |ui| {
            ui.weak("What each folder's name becomes on the photos in it. Dates and camera folders start as nothing; the new folders come from the date.");
            let before = app.import.folders.clone();
            let filter = app.import.filter.clone();
            let mut clicked = None;
            egui::ScrollArea::vertical()
                .id_salt("import-folders")
                .max_height(260.0)
                .show(ui, |ui| {
                    egui::Grid::new("import-folders-grid")
                        .num_columns(4)
                        .spacing(egui::vec2(12.0, 4.0))
                        .striped(true)
                        .show(ui, |ui| {
                            for f in &mut app.import.folders {
                                let rel = f.rel.display().to_string();
                                let depth = f.rel.components().count().saturating_sub(1);
                                ui.horizontal(|ui| {
                                    let indent = depth as f32 * 16.0;
                                    ui.add_space(indent);
                                    ui.add_sized(
                                        [240.0 - indent.min(160.0), 20.0],
                                        egui::TextEdit::singleline(&mut f.name),
                                    )
                                    .on_hover_text(&rel);
                                });
                                let on = filter.as_ref() == Some(&f.rel);
                                let r = ui
                                    .selectable_label(
                                        on,
                                        RichText::new(plural(f.photos, "photo")).weak(),
                                    )
                                    .on_hover_text("Show only the photos this name goes on");
                                r.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::Role::Button,
                                        true,
                                        on,
                                        format!("{rel}: photos"),
                                    )
                                });
                                if r.clicked() {
                                    clicked = Some(if on { None } else { Some(f.rel.clone()) });
                                }
                                ui.horizontal(|ui| {
                                    for (kind, label) in [
                                        (FolderUse::Event, "Event"),
                                        (FolderUse::Keyword, "Keyword"),
                                        (FolderUse::Ignore, "Nothing"),
                                    ] {
                                        let sel = f.kind == kind;
                                        let r = ui.selectable_label(sel, label);
                                        r.widget_info(|| {
                                            egui::WidgetInfo::selected(
                                                egui::Role::Button,
                                                true,
                                                sel,
                                                format!("{rel}: {label}"),
                                            )
                                        });
                                        if r.clicked() {
                                            f.kind = kind;
                                        }
                                    }
                                });
                                let (path, keywords) = names(&before, &f.rel);
                                let mut said = Vec::new();
                                if !path.is_empty() {
                                    said.push(format!("Event {}", path.join(" › ")));
                                }
                                if !keywords.is_empty() {
                                    let k = if keywords.len() == 1 { "keyword" } else { "keywords" };
                                    said.push(format!("{k} {}", keywords.join(", ")));
                                }
                                ui.label(if said.is_empty() {
                                    "—".to_owned()
                                } else {
                                    said.join("; ")
                                });
                                ui.end_row();
                            }
                        });
                });
            if let Some(f) = clicked {
                app.import.filter = f;
            }
        });
}

/// Tasks → History: every batch, newest first; undo any that's still
/// consistent with the disk.
pub fn history(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading("History");
        if ui.small_button("Refresh").clicked() {
            app.command(LibraryCommand::LoadHistory);
        }
        match &app.history.message {
            Some(Ok(m)) => {
                ui.label(RichText::new(m).color(Color32::from_rgb(80, 200, 120)));
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            None => {}
        }
    });
    ui.weak("Undo leaves folders it created in place and moves imported copies to the Trash; nothing is ever deleted.");
    if app.history.interrupted() > 0 {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "An operation was interrupted (the app quit while it ran). Resume it below to finish it, or Roll back to reverse the steps that happened.",
        );
    }
    ui.separator();
    let batches: Vec<_> = app.history.batches.iter().rev().cloned().collect();
    let focus = app.history.focus.take();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for b in batches {
                let open = app.history.open.contains_key(&b.id);
                let r = ui.horizontal(|ui| {
                    let arrow = if open { "-" } else { "+" };
                    if ui
                        .small_button(arrow)
                        .on_hover_text("Show the files")
                        .clicked()
                    {
                        app.toggle_batch(b.id);
                    }
                    let (status, colour) = match b.status.as_str() {
                        "running" => ("interrupted", ui.visuals().warn_fg_color),
                        "failed" => ("stopped by an error", ui.visuals().warn_fg_color),
                        "undone" => ("undone", Color32::from_gray(130)),
                        _ => ("done", Color32::from_rgb(80, 200, 120)),
                    };
                    if b.status == "running"
                        && b.kind != "undo"
                        && ui
                            .button(RichText::new("Resume").strong())
                            .on_hover_text(
                                "Finish it: steps that already happened are kept, the rest run now",
                            )
                            .clicked()
                    {
                        app.resume_batch(b.id);
                    }
                    let undoable = b.status != "undone" && b.kind != "mkdir";
                    let (text, hint) = if b.status == "running" {
                        ("Roll back", "Reverse the steps that happened")
                    } else {
                        ("Undo", "Reverse this operation")
                    };
                    if ui
                        .add_enabled(undoable, egui::Button::new(text).small())
                        .on_hover_text(hint)
                        .clicked()
                    {
                        app.undo_batch(b.id);
                    }
                    ui.label(RichText::new(status).color(colour));
                    ui.label(RichText::new(format!("#{}", b.id)).monospace());
                    ui.weak(crate::app::local_time_ms(b.ts));
                    ui.weak(format!("{} · {}", b.kind, plural(b.ops as usize, "change")));
                    ui.add(egui::Label::new(RichText::new(&b.label).strong()).truncate())
                        .on_hover_text(&b.label);
                });
                if focus == Some(b.id) {
                    r.response.scroll_to_me(Some(egui::Align::Center));
                    ui.painter().rect_stroke(
                        r.response.rect.expand(2.0),
                        3.0,
                        egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                        egui::StrokeKind::Outside,
                    );
                }
                if let Some(ops) = app.history.open.get(&b.id) {
                    let rows: Vec<(String, String)> = ops
                        .iter()
                        .filter(|(op, ..)| op != "mkdir")
                        .map(|(op, src, dst, done)| {
                            let mark = if *done { "" } else { " (not done)" };
                            if src == dst {
                                (format!("{op}{mark}"), short(app, src))
                            } else {
                                (format!("{op}{mark}: {}", short(app, src)), short(app, dst))
                            }
                        })
                        .collect();
                    ui.indent(("ops", b.id), |ui| {
                        table(ui, &format!("ops-{}", b.id), &rows, 240.0)
                    });
                }
            }
        });
}

/// The rename and new-folder dialogs, and the dry-run table for every
/// move, rename, reject and trash.
pub fn dialogs(app: &mut ShoeboxApp, ctx: &egui::Context) {
    if app.confirm_quit {
        quit_dialog(app, ctx);
    }
    if let Some((photos, mut template)) = app.org.rename.take() {
        let mut keep = true;
        let mut go = false;
        egui::Modal::new(egui::Id::unique("rename")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading(format!("Rename {}", plural(photos.len(), "stack")));
            ui.weak("Every file of a stack gets the name, with its own extension; virtual copies keep their -N. Without a / files stay in their folder.");
            let r = ui.add(egui::TextEdit::singleline(&mut template).desired_width(f32::INFINITY));
            r.request_focus();
            let ok = match sample(&template, false) {
                Ok(s) => {
                    ui.weak(format!("e.g. {s}"));
                    true
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                    false
                }
            };
            ui.weak("Tokens: {YYYY} {MM} {DD} {HH} {mm} {ss} {YYYY-MM-DD} {YYYYMMDD} {HHMMSS} {subsec} {name} {camera} {width} {height} {seq} {ext}");
            ui.horizontal(|ui| {
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(ok, egui::Button::new("Preview…")).clicked() || (ok && enter) {
                    go = true;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
        });
        if go {
            if template != app.settings.rename_template() {
                let mut s = app.settings.clone();
                s.rename_template = template.clone();
                app.settings = s.clone();
                app.command(LibraryCommand::SaveSettings(Box::new(s)));
            }
            app.request_org(OrgRequest::Rename { photos, template });
        } else if keep {
            app.org.rename = Some((photos, template));
        }
    }

    if let Some((parent, mut name)) = app.org.new_folder.take() {
        let mut keep = true;
        egui::Modal::new(egui::Id::unique("new-folder")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("New folder");
            ui.weak(format!("In {}", short(app, &parent)));
            let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Name"));
            r.request_focus();
            let valid =
                !name.trim().is_empty() && !name.contains('/') && !name.trim().starts_with('.');
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if (ui.add_enabled(valid, egui::Button::new("Create")).clicked()
                    || (valid && enter))
                    && keep
                {
                    app.command(LibraryCommand::NewFolder(parent.join(name.trim())));
                    keep = false;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
        });
        if keep {
            app.org.new_folder = Some((parent, name));
        }
    }

    let Some(preview) = app.org.preview.clone() else {
        return;
    };
    let mut close = false;
    let mut confirm = false;
    egui::Modal::new(egui::Id::unique("organise")).show(ctx, |ui| {
        ui.set_width(760.0);
        match &preview {
            Err(e) => {
                ui.heading("Can't do that");
                ui.label(e);
                if ui.button("OK").clicked() {
                    close = true;
                }
            }
            Ok(p) => {
                let verb = p.request.verb();
                let trash = matches!(p.request, OrgRequest::Trash { .. });
                ui.heading(format!("{verb}: {}", plural(p.moves.len(), "file")));
                if trash {
                    ui.label("These go to the Trash, where Finder's Put Back can restore them. Undo here can't bring them back.");
                }
                for n in &p.notes {
                    ui.weak(n);
                }
                let rows: Vec<(String, String)> = p
                    .moves
                    .iter()
                    .map(|m| {
                        (
                            short(app, &m.src),
                            if trash { "Trash".to_owned() } else { short(app, &m.dst) },
                        )
                    })
                    .collect();
                table(ui, "org-table", &rows, 360.0);
                ui.add_space(6.0);
                if !trash {
                    ui.weak("One batch: ⌘Z (or History) puts it all back.");
                }
                ui.horizontal(|ui| {
                    let go = format!("{verb} {}", plural(p.moves.len(), "file"));
                    if ui
                        .add_enabled(!p.moves.is_empty(), egui::Button::new(RichText::new(go).strong()))
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
        }
    });
    if confirm {
        app.confirm_org();
    } else if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.org.preview = None;
    }
}
