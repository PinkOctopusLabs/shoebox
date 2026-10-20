//! Export (`docs/EDITING-SPEC.md`, Export): the dialog (⇧⌘E, or Export… in
//! the photo menu) exports the selection with every ticked preset; ⌥⇧⌘E
//! repeats the last export. Presets are edited here and saved as JSON.
//! Exports run on the develop thread's export thread; the status bar shows
//! progress, and the Info panel lists each photo's exports.

use std::collections::BTreeSet;
use std::path::PathBuf;

use eframe::egui::{self, RichText};
use shoebox_develop::export::{FileFormat, Metadata, OutputSize, Preset, Space, TiffCompression};

use crate::app::ShoeboxApp;
use crate::develop::{Clash, ExportItem, ExportJob};
use crate::jobs::{LibraryCommand, PhotoId};

#[derive(Default)]
pub struct State {
    pub open: bool,
    /// The presets (loaded from disk, or the starters).
    pub presets: Vec<Preset>,
    /// Ticked for this export, by name.
    pub ticked: BTreeSet<String>,
    /// The preset being edited below the list.
    pub editing: Option<usize>,
    /// Exporting: files written of the total.
    pub progress: Option<(usize, usize)>,
    /// The last export's presets (⌥⇧⌘E).
    pub last: Vec<String>,
    /// Exports of the photo the Info panel shows: (photo, rows).
    pub of_photo: Option<(PathBuf, Vec<(shoebox_core::cache::ExportRow, bool)>)>,
    asked_for: Option<PathBuf>,
    /// An export waiting for an answer: files it would land on are there.
    pub clashes: Option<(Box<ExportJob>, Vec<PathBuf>)>,
}

/// The photos to export: the selection, else the current photo.
fn targets(app: &ShoeboxApp) -> Vec<PhotoId> {
    let sel = app.selected_in_order();
    if sel.is_empty() {
        app.current.into_iter().collect()
    } else {
        sel
    }
}

/// Export these photos with these presets.
pub fn export(app: &mut ShoeboxApp, ids: &[PhotoId], presets: Vec<Preset>) {
    if presets.is_empty() || ids.is_empty() {
        return;
    }
    app.export.last = presets.iter().map(|p| p.name.clone()).collect();
    send(app, ids, presets);
}

/// Export one photo again with the preset it was exported with, to the
/// same file name (so the question of replacing it comes up).
pub fn reexport(app: &mut ShoeboxApp, row: &shoebox_core::cache::ExportRow) {
    let Ok(mut preset) = serde_json::from_str::<Preset>(&row.preset_json) else {
        app.error = Some(format!(
            "Can't re-export {}: its preset can't be read",
            row.path
        ));
        return;
    };
    let file = std::path::Path::new(&row.path);
    preset.folder = file.parent().map(PathBuf::from);
    if let Some(stem) = file.file_stem() {
        // The name as it was (braces can't be read as placeholders here).
        preset.naming = stem.to_string_lossy().replace('{', "(").replace('}', ")");
    }
    let id = crate::jobs::photo_id(std::path::Path::new(&row.photo));
    send(app, &[id], vec![preset]);
}

fn send(app: &mut ShoeboxApp, ids: &[PhotoId], presets: Vec<Preset>) {
    let mut items = Vec::new();
    for &id in ids {
        let Some(idx) = app.index(id) else { continue };
        let p = &app.catalog.photos[idx];
        if p.format.is_movie() {
            continue;
        }
        // The photo open in Develop: its edit as it is now (the copy shown).
        let open = app
            .develop
            .doc
            .as_ref()
            .filter(|_| app.develop.photo == Some(id));
        let copy_name = open
            .filter(|_| app.develop.copy_id() != shoebox_core::edits::MAIN)
            .map(|d| d.copy.name.clone());
        let settings = match open {
            Some(doc) => doc.settings().clone(),
            None => app
                .edited_json
                .get(&p.path)
                .and_then(|j| shoebox_develop::edit::EditDoc::from_json(j).ok())
                .map(|d| d.settings().clone())
                .unwrap_or_default(),
        };
        let date = p
            .meta
            .exif
            .capture_time
            .as_deref()
            .and_then(|t| t.get(..10))
            .map(|d| d.replace(':', "-"));
        items.push(ExportItem {
            id,
            path: p.path.clone(),
            format: p.format,
            orientation: p.meta.orientation(),
            settings,
            edit_key: app.edited.get(&p.path).cloned(),
            date,
            sidecar: p.sidecar.clone(),
            lens: crate::develop::shot(p),
            copy_name,
        });
    }
    if items.is_empty() {
        return;
    }
    app.export.progress = Some((0, items.len() * presets.len()));
    let roots = app.catalog.roots.iter().map(|r| r.path.clone()).collect();
    let reference = app
        .catalog
        .roots
        .iter()
        .filter(|r| r.role == shoebox_core::scan::Role::Reference)
        .map(|r| r.path.clone())
        .collect();
    app.developer.export(ExportJob {
        items,
        presets,
        roots,
        reference,
        on_clash: None,
    });
}

/// ⌥⇧⌘E: the last export's presets on the selection.
pub fn repeat(app: &mut ShoeboxApp) {
    let presets: Vec<Preset> = app
        .export
        .presets
        .iter()
        .filter(|p| app.export.last.contains(&p.name))
        .cloned()
        .collect();
    if presets.is_empty() {
        app.export.open = true;
        return;
    }
    let ids = targets(app);
    export(app, &ids, presets);
}

/// An export finished: remember what was written, and say so.
pub fn done(
    app: &mut ShoeboxApp,
    written: Vec<crate::develop::Exported>,
    errors: Vec<String>,
    no_metadata: bool,
) {
    app.export.progress = None;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let rows: Vec<shoebox_core::cache::ExportRow> = written
        .iter()
        .filter_map(|w| {
            Some(shoebox_core::cache::ExportRow {
                photo: w.photo.to_str()?.to_owned(),
                copy: shoebox_core::edits::MAIN.to_owned(),
                path: w.file.to_str()?.to_owned(),
                preset_name: w.preset.name.clone(),
                preset_json: serde_json::to_string(&w.preset).unwrap_or_default(),
                edit_key: w.edit_key.clone(),
                created_at: now,
            })
        })
        .collect();
    if !rows.is_empty() {
        app.command(LibraryCommand::RecordExports(rows));
    }
    app.export.of_photo = None;
    app.export.asked_for = None;
    let folders: BTreeSet<PathBuf> = written
        .iter()
        .filter_map(|w| w.file.parent().map(PathBuf::from))
        .collect();
    let mut msg = match (written.len(), folders.len()) {
        (0, _) => "Nothing was exported".to_owned(),
        (n, 1) => format!(
            "Exported {} to {}",
            plural(n),
            folders
                .iter()
                .next()
                .map(|f| f.display().to_string())
                .unwrap_or_default()
        ),
        (n, _) => format!("Exported {}", plural(n)),
    };
    if no_metadata && !written.is_empty() {
        msg.push_str(" (without metadata: install exiftool to copy it)");
    }
    app.notice = Some(msg);
    if !errors.is_empty() {
        app.error = Some(format!("Some exports failed: {}", errors.join("; ")));
    }
}

fn plural(n: usize) -> String {
    if n == 1 {
        "1 file".to_owned()
    } else {
        format!("{n} files")
    }
}

/// A preset's settings in a line.
fn summary(p: &Preset) -> String {
    let format = match p.format {
        FileFormat::Jpeg { quality, .. } => format!("JPEG {quality}"),
        FileFormat::Tiff { .. } => "TIFF 16-bit".to_owned(),
    };
    let size = match p.size {
        OutputSize::Full => "full size".to_owned(),
        OutputSize::LongEdge { px } => format!("{px} px"),
        OutputSize::Megapixels { mp } => format!("{mp} MP"),
    };
    format!("{format} · {} · {size}", p.space.label())
}

/// Files an export would land on are there: replace them, or save new
/// files beside them?
pub fn clashes(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let Some((_, files)) = &app.export.clashes else {
        return;
    };
    let n = files.len();
    let shown: Vec<String> = files
        .iter()
        .take(5)
        .map(|f| f.display().to_string())
        .collect();
    let mut choice: Option<Option<Clash>> = None;
    egui::Window::new(if n == 1 {
        "The file is already there"
    } else {
        "Files are already there"
    })
    .collapsible(false)
    .resizable(false)
    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
    .show(ctx, |ui| {
        ui.label(if n == 1 {
            "This export would save over a file that's already there:".to_owned()
        } else {
            format!("This export would save over {n} files that are already there:")
        });
        for f in &shown {
            ui.weak(f);
        }
        if n > shown.len() {
            ui.weak(format!("and {} more", n - shown.len()));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .button(if n == 1 { "Replace" } else { "Replace them" })
                .on_hover_text("Overwrite with the new export (once it's written)")
                .clicked()
            {
                choice = Some(Some(Clash::Replace));
            }
            if ui
                .button(if n == 1 {
                    "Save as a new file"
                } else {
                    "Save as new files"
                })
                .on_hover_text("Keep what's there; the new export's name gets -1, -2")
                .clicked()
            {
                choice = Some(Some(Clash::NewFile));
            }
            if ui.button("Cancel").clicked() {
                choice = Some(None);
            }
        });
    });
    match choice {
        Some(Some(c)) => {
            if let Some((mut job, _)) = app.export.clashes.take() {
                job.on_clash = Some(c);
                app.export.progress = Some((0, job.items.len() * job.presets.len()));
                app.developer.export(*job);
            }
        }
        Some(None) => {
            app.export.clashes = None;
            app.export.progress = None;
        }
        None => {}
    }
}

/// The dialog.
pub fn dialog(app: &mut ShoeboxApp, ctx: &egui::Context) {
    clashes(app, ctx);
    if !app.export.open {
        return;
    }
    let ids = targets(app);
    let mut open = true;
    let mut go = false;
    let mut changed = false;
    egui::Window::new("Export")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(match ids.len() {
                1 => "1 photo, with each preset ticked:".to_owned(),
                n => format!("{n} photos, with each preset ticked:"),
            });
            ui.add_space(4.0);
            let st = &mut app.export;
            for (i, p) in st.presets.iter().enumerate() {
                ui.horizontal(|ui| {
                    let mut on = st.ticked.contains(&p.name);
                    if ui.checkbox(&mut on, "").changed() {
                        if on {
                            st.ticked.insert(p.name.clone());
                        } else {
                            st.ticked.remove(&p.name);
                        }
                    }
                    if ui
                        .selectable_label(st.editing == Some(i), RichText::new(&p.name).strong())
                        .on_hover_text("Edit this preset")
                        .clicked()
                    {
                        st.editing = (st.editing != Some(i)).then_some(i);
                    }
                    ui.weak(summary(p));
                });
            }
            ui.horizontal(|ui| {
                if ui.button("New preset").clicked() {
                    let mut p = shoebox_develop::export::starters().remove(0);
                    p.name = unique_name(&st.presets, "New preset");
                    st.presets.push(p);
                    st.editing = Some(st.presets.len() - 1);
                    changed = true;
                }
                if let Some(i) = st.editing {
                    if ui.button("Duplicate").clicked() {
                        let mut p = st.presets[i].clone();
                        p.name = unique_name(&st.presets, &p.name);
                        st.presets.push(p);
                        st.editing = Some(st.presets.len() - 1);
                        changed = true;
                    }
                    if ui
                        .add_enabled(st.presets.len() > 1, egui::Button::new("Delete"))
                        .clicked()
                    {
                        let gone = st.presets.remove(i);
                        st.ticked.remove(&gone.name);
                        st.editing = None;
                        changed = true;
                    }
                }
            });
            if let Some(i) = st.editing
                && let Some(p) = st.presets.get_mut(i)
            {
                ui.separator();
                let before = p.clone();
                editor(ui, p);
                if *p != before {
                    if before.name != p.name && st.ticked.remove(&before.name) {
                        st.ticked.insert(p.name.clone());
                    }
                    changed = true;
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                let n = st.ticked.len();
                let label = match n {
                    0 => "Tick a preset".to_owned(),
                    1 => "Export".to_owned(),
                    n => format!("Export with {n} presets"),
                };
                if ui
                    .add_enabled(n > 0 && !ids.is_empty(), egui::Button::new(label))
                    .clicked()
                {
                    go = true;
                }
            });
            ui.weak(
                "A name that's taken gets -1, -2. Into your library, exports are new files \
                 (never replacing one), and ⌘Z undoes them.",
            );
        });
    if changed {
        app.developer.save_presets(app.export.presets.clone());
    }
    if go {
        let presets: Vec<Preset> = app
            .export
            .presets
            .iter()
            .filter(|p| app.export.ticked.contains(&p.name))
            .cloned()
            .collect();
        export(app, &ids, presets);
        open = false;
    }
    app.export.open &= open;
}

fn unique_name(list: &[Preset], base: &str) -> String {
    let names: BTreeSet<&str> = list.iter().map(|p| p.name.as_str()).collect();
    if !names.contains(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|n| !names.contains(n.as_str()))
        .unwrap_or_else(|| base.to_owned())
}

/// A preset's settings.
fn editor(ui: &mut egui::Ui, p: &mut Preset) {
    egui::Grid::new("export-preset")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut p.name);
            ui.end_row();

            ui.label("Format");
            let jpeg = matches!(p.format, FileFormat::Jpeg { .. });
            egui::ComboBox::from_id_salt("export-format")
                .selected_text(if jpeg { "JPEG" } else { "TIFF (16-bit)" })
                .show_ui(ui, |ui| {
                    if ui.selectable_label(jpeg, "JPEG").clicked() && !jpeg {
                        p.format = FileFormat::Jpeg {
                            quality: 90,
                            full_chroma: false,
                            progressive: true,
                        };
                    }
                    if ui.selectable_label(!jpeg, "TIFF (16-bit)").clicked() && jpeg {
                        p.format = FileFormat::Tiff {
                            compression: TiffCompression::Zip,
                        };
                    }
                });
            ui.end_row();
            match &mut p.format {
                FileFormat::Jpeg {
                    quality,
                    full_chroma,
                    progressive,
                } => {
                    ui.label("Quality");
                    ui.add(egui::Slider::new(quality, 60..=100));
                    ui.end_row();
                    ui.label("");
                    ui.horizontal(|ui| {
                        ui.checkbox(full_chroma, "Full colour detail (4:4:4)");
                        ui.checkbox(progressive, "Progressive");
                    });
                    ui.end_row();
                }
                FileFormat::Tiff { compression } => {
                    ui.label("Compression");
                    ui.horizontal(|ui| {
                        ui.selectable_value(compression, TiffCompression::Zip, "ZIP");
                        ui.selectable_value(compression, TiffCompression::Lzw, "LZW");
                    });
                    ui.end_row();
                }
            }

            ui.label("Colour space");
            egui::ComboBox::from_id_salt("export-space")
                .selected_text(p.space.label())
                .show_ui(ui, |ui| {
                    for s in [
                        Space::Srgb,
                        Space::DisplayP3,
                        Space::AdobeRgb,
                        Space::ProPhoto,
                    ] {
                        ui.selectable_value(&mut p.space, s, s.label());
                    }
                });
            ui.end_row();

            ui.label("Size");
            ui.horizontal(|ui| {
                let kind = match p.size {
                    OutputSize::Full => 0,
                    OutputSize::LongEdge { .. } => 1,
                    OutputSize::Megapixels { .. } => 2,
                };
                egui::ComboBox::from_id_salt("export-size")
                    .selected_text(["Full size", "Long edge", "Megapixels"][kind])
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(kind == 0, "Full size").clicked() {
                            p.size = OutputSize::Full;
                        }
                        if ui.selectable_label(kind == 1, "Long edge").clicked() && kind != 1 {
                            p.size = OutputSize::LongEdge { px: 2048 };
                        }
                        if ui.selectable_label(kind == 2, "Megapixels").clicked() && kind != 2 {
                            p.size = OutputSize::Megapixels { mp: 12.0 };
                        }
                    });
                match &mut p.size {
                    OutputSize::LongEdge { px } => {
                        ui.add(egui::DragValue::new(px).range(64..=20000).suffix(" px"));
                    }
                    OutputSize::Megapixels { mp } => {
                        ui.add(
                            egui::DragValue::new(mp)
                                .range(0.1..=200.0)
                                .speed(0.1)
                                .suffix(" MP"),
                        );
                    }
                    OutputSize::Full => {}
                }
            });
            ui.end_row();

            ui.label("Metadata");
            egui::ComboBox::from_id_salt("export-metadata")
                .selected_text(match p.metadata {
                    Metadata::All => "All",
                    Metadata::AllButLocation => "All but location",
                    Metadata::CopyrightOnly => "Copyright only",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.metadata, Metadata::All, "All");
                    ui.selectable_value(
                        &mut p.metadata,
                        Metadata::AllButLocation,
                        "All but location",
                    );
                    ui.selectable_value(&mut p.metadata, Metadata::CopyrightOnly, "Copyright only");
                });
            ui.end_row();

            ui.label("Folder");
            let mut folder = p
                .folder
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_default();
            let hint = crate::develop::default_folder(&p.name)
                .map(|f| f.display().to_string())
                .unwrap_or_default();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut folder)
                        .hint_text(hint)
                        .desired_width(320.0),
                )
                .changed()
            {
                p.folder = (!folder.trim().is_empty()).then(|| PathBuf::from(folder.trim()));
            }
            ui.end_row();

            ui.label("File name");
            ui.add(egui::TextEdit::singleline(&mut p.naming).desired_width(320.0))
                .on_hover_text(
                    "{name} the original's name, {preset}, {date} (capture), {seq} (1, 2, …)",
                );
            ui.end_row();
        });
}

/// The Info panel's list of a photo's exports (stale ones flagged).
pub fn info_section(app: &mut ShoeboxApp, ui: &mut egui::Ui, photo: &std::path::Path) {
    if app.export.asked_for.as_deref() != Some(photo) {
        app.export.asked_for = Some(photo.to_path_buf());
        app.export.of_photo = None;
        app.command(LibraryCommand::LoadExports(photo.to_path_buf()));
    }
    let Some((p, rows)) = &app.export.of_photo else {
        return;
    };
    if p != photo || rows.is_empty() {
        return;
    }
    let current = app.edited.get(photo).cloned();
    ui.add_space(6.0);
    ui.label(
        RichText::new("EXPORTS")
            .size(11.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
    let mut reveal = None;
    let mut again = None;
    for (r, there) in rows.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.label(&r.preset_name);
            let when = crate::app::local_time_ns(r.created_at * 1_000_000);
            ui.weak(when);
            if !there {
                ui.colored_label(ui.visuals().warn_fg_color, "missing")
                    .on_hover_text("The file isn't where it was exported to");
            } else if r.edit_key != current {
                ui.colored_label(ui.visuals().warn_fg_color, "stale")
                    .on_hover_text("The photo's edit has changed since this export");
            }
            if there && ui.small_button("Show").on_hover_text(&r.path).clicked() {
                reveal = Some(PathBuf::from(&r.path));
            }
            if ui
                .small_button("Re-export")
                .on_hover_text("Export again with the same preset and name")
                .clicked()
            {
                again = Some(r.clone());
            }
        });
    }
    if let Some(p) = reveal {
        crate::app::reveal(&p);
    }
    if let Some(r) = again {
        reexport(app, &r);
    }
}

/// The library worker's answer to `LoadExports`.
pub fn loaded(
    app: &mut ShoeboxApp,
    photo: PathBuf,
    rows: Vec<(shoebox_core::cache::ExportRow, bool)>,
) {
    if app.export.asked_for.as_ref() == Some(&photo) {
        app.export.of_photo = Some((photo, rows));
    }
}
