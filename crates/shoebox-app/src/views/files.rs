//! File operations in the app: Tasks → Import, Tasks → History, and the
//! dialogs for move, rename, reject, trash and new folder. Every operation
//! shows its source → destination table first and runs as one batch.

use std::path::Path;

use eframe::egui::{self, Color32, RichText};
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

/// The photos an import will copy, as thumbnails (each with its source and
/// new name on hover).
fn previews(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    photos: &[shoebox_core::import::PlannedPhoto],
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
                        preview_cell(app, ui, p, source, CELL);
                    }
                });
                ui.add_space(GAP);
            }
        });
}

fn preview_cell(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    p: &shoebox_core::import::PlannedPhoto,
    source: &Path,
    cell: f32,
) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(cell, cell), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
    app.want_preview(p);
    let id = crate::jobs::photo_id(&p.src);
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
                p.format.label(),
                egui::FontId::proportional(11.0),
                ui.visuals().weak_text_color(),
            );
        }
    }
    let src = p
        .src
        .strip_prefix(source)
        .unwrap_or(&p.src)
        .display()
        .to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &src));
    resp.on_hover_ui(|ui| {
        ui.label(RichText::new(&src).monospace().small());
        ui.label(
            RichText::new(format!("▶ {}", short(app, &p.dst)))
                .monospace()
                .small(),
        );
    });
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
                if ui.button("Choose…").clicked() {
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
        && !app.import.running;
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
        }
        if let Some(m) = &app.import.message {
            ui.label(RichText::new(m).color(Color32::from_rgb(80, 200, 120)));
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
    let images = plan
        .copies
        .iter()
        .filter(|c| {
            !c.src
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
        })
        .count();
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
        if !plan.skipped.is_empty() {
            ui.label(format!(
                "· {} already in the library",
                plural(plan.skipped.len(), "file")
            ));
        }
        if !plan.repeats.is_empty() {
            ui.label(format!(
                "· {} repeated in the source",
                plural(plan.repeats.len(), "file")
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
        let go = format!("Import {}", plural(plan.copies.len(), "file"));
        if ui
            .add_enabled(
                !plan.copies.is_empty(),
                egui::Button::new(RichText::new(go).strong()),
            )
            .clicked()
        {
            app.run_import();
        }
    });
    folder_names(app, ui);
    let source = app.import.source.clone().unwrap_or_default();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut app.import.list, false, "Thumbnails");
        ui.selectable_value(&mut app.import.list, true, "List");
    });
    if app.import.list {
        let rows: Vec<(String, String)> = plan
            .copies
            .iter()
            .map(|c| {
                (
                    c.src
                        .strip_prefix(&source)
                        .unwrap_or(&c.src)
                        .display()
                        .to_string(),
                    short(app, &c.dst),
                )
            })
            .collect();
        table(ui, "import-table", &rows, ui.available_height() * 0.7);
    } else {
        previews(app, ui, &plan.photos, &source);
    }
    if !plan.skipped.is_empty() {
        egui::CollapsingHeader::new(format!("Already in the library ({})", plan.skipped.len()))
            .show(ui, |ui| {
                let rows: Vec<(String, String)> = plan
                    .skipped
                    .iter()
                    .map(|(s, e)| {
                        (
                            s.strip_prefix(&source).unwrap_or(s).display().to_string(),
                            short(app, e),
                        )
                    })
                    .collect();
                table(ui, "import-skipped", &rows, 200.0);
            });
    }
    if !plan.repeats.is_empty() {
        egui::CollapsingHeader::new(format!(
            "Repeated in the source, copied once ({})",
            plan.repeats.len()
        ))
        .show(ui, |ui| {
            let rel =
                |p: &std::path::PathBuf| p.strip_prefix(&source).unwrap_or(p).display().to_string();
            let rows: Vec<(String, String)> =
                plan.repeats.iter().map(|(s, e)| (rel(s), rel(e))).collect();
            table(ui, "import-repeats", &rows, 200.0);
        });
    }
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
                                ui.weak(plural(f.photos, "photo"));
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
