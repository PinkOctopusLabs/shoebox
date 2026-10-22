//! Export (`docs/EDITING-SPEC.md`, Export): the dialog (⇧⌘E, or Export… in
//! the photo menu) exports the selection with every ticked preset; ⌥⇧⌘E
//! repeats the last export. Presets are edited here and saved as JSON.
//! Exports run on the develop thread's export thread; the status bar shows
//! progress, and the Info panel lists each photo's exports.

use std::collections::BTreeSet;
use std::path::PathBuf;

use eframe::egui::{self, RichText};
use shoebox_develop::export::{
    Destination, FileFormat, Metadata, OutputSize, Preset, Space, TiffCompression,
};
use shoebox_develop::overlay::{Anchor, Border, Mark, Watermark};

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
    send(app, ids, presets, None);
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
    let photo = crate::jobs::photo_id(std::path::Path::new(&row.photo));
    // A copy's export is redone from that copy.
    let id = if row.copy == shoebox_core::edits::MAIN {
        photo
    } else {
        crate::jobs::copy_id(photo, &row.copy)
    };
    // In the library the new file takes the old one's place (which goes to
    // the Trash); outside, the usual question.
    let replaces = (preset.destination == Destination::Library).then(|| file.to_path_buf());
    send(app, &[id], vec![preset], replaces);
}

fn send(app: &mut ShoeboxApp, ids: &[PhotoId], presets: Vec<Preset>, replaces: Option<PathBuf>) {
    let mut items = Vec::new();
    for &id in ids {
        let Some(idx) = app.index(id) else { continue };
        let p = &app.catalog.photos[idx];
        if p.format.is_movie() {
            continue;
        }
        // A virtual copy's tile exports that copy; the file, the master.
        let copy = app.copy_info(id);
        let copy_id = copy.map_or(shoebox_core::edits::MAIN, |c| c.id.as_str());
        // Open in Develop: the edit as it is now.
        let open = app.develop.doc.as_ref().filter(|_| {
            app.develop.photo == Some(app.ids[idx]) && app.develop.copy_id() == copy_id
        });
        let copy_name = copy.map(|c| c.name.clone());
        let copy_id = copy_id.to_owned();
        // What it's checked against for staleness: that copy's edit.
        let edit_key = match copy {
            Some(c) => c.key.clone(),
            None => app.edited.get(&p.path).cloned(),
        };
        let json = match copy {
            Some(c) => Some(&c.json),
            None => app.edited_json.get(&p.path),
        };
        let settings = match open {
            Some(doc) => doc.settings().clone(),
            None => json
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
            edit_key,
            date,
            sidecar: p.sidecar.clone(),
            lens: crate::develop::shot(p),
            copy_name,
            copy: copy_id,
            replaces: replaces.clone(),
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
    // Only exports in the library are kept track of.
    let rows: Vec<shoebox_core::cache::ExportRow> = written
        .iter()
        .filter(|w| w.tracked)
        .filter_map(|w| {
            Some(shoebox_core::cache::ExportRow {
                photo: w.photo.to_str()?.to_owned(),
                copy: w.copy.clone(),
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
        FileFormat::Png => "PNG 16-bit".to_owned(),
        FileFormat::Avif {
            quality, ten_bit, ..
        } => format!("AVIF {quality}{}", if ten_bit { "" } else { " 8-bit" }),
        FileFormat::LinearDng => "Linear DNG".to_owned(),
    };
    let size = match p.size {
        OutputSize::Full => "full size".to_owned(),
        OutputSize::LongEdge { px } => format!("{px} px"),
        OutputSize::Megapixels { mp } => format!("{mp} MP"),
    };
    if p.format == FileFormat::LinearDng {
        // The source as it is: no space, size or overlays.
        return format!("{format} · camera colour · full size");
    }
    let mut out = format!("{format} · {} · {size}", p.space.label());
    if p.destination == Destination::Library {
        out.push_str(" · in library");
    }
    if p.border.is_some() {
        out.push_str(" · border");
    }
    if p.watermark.is_some() {
        out.push_str(" · watermark");
    }
    out
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
                "A name that's taken gets -1, -2. Exports beside the original join its stack, \
                 and ⌘Z undoes them.",
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

fn destination_label(d: Destination) -> &'static str {
    match d {
        Destination::Folder => "A folder outside the library",
        Destination::Library => "Beside the original, in the library",
    }
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
            egui::ComboBox::from_id_salt("export-format")
                .selected_text(p.format.label())
                .show_ui(ui, |ui| {
                    for f in [
                        FileFormat::Jpeg {
                            quality: 90,
                            full_chroma: false,
                            progressive: true,
                        },
                        FileFormat::Tiff {
                            compression: TiffCompression::Zip,
                        },
                        FileFormat::Png,
                        {
                            let a = shoebox_develop::encode::Avif::default();
                            FileFormat::Avif {
                                quality: a.quality,
                                ten_bit: a.ten_bit,
                                speed: a.speed,
                            }
                        },
                        FileFormat::LinearDng,
                    ] {
                        let on = std::mem::discriminant(&f) == std::mem::discriminant(&p.format);
                        if ui.selectable_label(on, f.label()).clicked() && !on {
                            p.format = f;
                            // What the new format can't hold.
                            let spaces = f.spaces();
                            if !spaces.is_empty() && !spaces.contains(&p.space) {
                                p.space = Space::Srgb;
                            }
                            if !f.into_library() {
                                p.destination = Destination::Folder;
                            }
                        }
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
                FileFormat::Png => {}
                FileFormat::Avif {
                    quality,
                    ten_bit,
                    speed,
                } => {
                    ui.label("Quality");
                    ui.add(egui::Slider::new(quality, 50..=100));
                    ui.end_row();
                    ui.label("Speed");
                    ui.add(egui::Slider::new(speed, 1..=10)).on_hover_text(format!(
                        "Slower makes slightly smaller files; {} is the usual",
                        shoebox_develop::encode::AVIF_SPEED
                    ));
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(ten_bit, "10 bits a channel");
                    ui.end_row();
                }
                FileFormat::LinearDng => {
                    ui.label("");
                    ui.weak(
                        "The raw photo demosaiced, with AI denoise as the edit has it, for \
                         other raw editors: nothing else of the edit. Raw photos only.",
                    );
                    ui.end_row();
                }
            }

            let spaces = p.format.spaces();
            if !spaces.is_empty() {
                ui.label("Colour space");
                egui::ComboBox::from_id_salt("export-space")
                    .selected_text(p.space.label())
                    .show_ui(ui, |ui| {
                        for &s in spaces {
                            ui.selectable_value(&mut p.space, s, s.label());
                        }
                    });
                ui.end_row();
            }
            // A linear DNG is the source as it is: no size, border or
            // watermark.
            let rendered = p.format != FileFormat::LinearDng;
            if rendered {
                size(ui, p);
            }

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

            ui.label("Destination");
            egui::ComboBox::from_id_salt("export-destination")
                .selected_text(destination_label(p.destination))
                .show_ui(ui, |ui| {
                    for d in [Destination::Folder, Destination::Library] {
                        let allowed = d == Destination::Folder || p.format.into_library();
                        ui.add_enabled_ui(allowed, |ui| {
                            ui.selectable_value(&mut p.destination, d, destination_label(d))
                                .on_hover_text(match d {
                                    Destination::Folder => {
                                        "Files for elsewhere; Shoebox doesn't keep track of them"
                                    }
                                    Destination::Library => {
                                        "An edited version in the photo's stack, flagged when the edit changes and re-exported in place"
                                    }
                                })
                                .on_disabled_hover_text(
                                    "The library doesn't show AVIF or DNG files yet",
                                );
                        });
                    }
                });
            ui.end_row();

            if p.destination == Destination::Folder {
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
            }
            ui.label("File name");
            ui.add(egui::TextEdit::singleline(&mut p.naming).desired_width(320.0))
                .on_hover_text(
                    "{name} the original's name, {preset}, {date} (capture), {seq} (1, 2, …)",
                );
            ui.end_row();

            if rendered {
                border(ui, p);
                watermark(ui, p);
            }
        });
}

/// The size row of the preset editor.
fn size(ui: &mut egui::Ui, p: &mut Preset) {
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
}

/// A path typed in, None when empty.
fn path_field(ui: &mut egui::Ui, path: Option<&PathBuf>, hint: &str) -> Option<Option<PathBuf>> {
    let mut text = path.map(|f| f.display().to_string()).unwrap_or_default();
    ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text(hint)
            .desired_width(320.0),
    )
    .changed()
    .then(|| (!text.trim().is_empty()).then(|| PathBuf::from(text.trim())))
}

/// The border rows of the preset editor.
fn border(ui: &mut egui::Ui, p: &mut Preset) {
    ui.label("Border");
    ui.horizontal(|ui| {
        let mut on = p.border.is_some();
        if ui.checkbox(&mut on, "").changed() {
            p.border = on.then(Border::default);
        }
        if let Some(b) = &mut p.border {
            ui.add(
                egui::DragValue::new(&mut b.width)
                    .range(0.1..=25.0)
                    .speed(0.1)
                    .suffix(" %"),
            )
            .on_hover_text("Width, as a share of the long edge");
            ui.color_edit_button_srgb(&mut b.colour);
        }
    });
    ui.end_row();
}

/// The watermark rows of the preset editor.
fn watermark(ui: &mut egui::Ui, p: &mut Preset) {
    ui.label("Watermark");
    let kind = match p.watermark.as_ref().map(|w| &w.mark) {
        None => 0,
        Some(Mark::Text { .. }) => 1,
        Some(Mark::Image { .. }) => 2,
    };
    egui::ComboBox::from_id_salt("export-watermark")
        .selected_text(["None", "Text", "Image"][kind])
        .show_ui(ui, |ui| {
            if ui.selectable_label(kind == 0, "None").clicked() {
                p.watermark = None;
            }
            if ui.selectable_label(kind == 1, "Text").clicked() && kind != 1 {
                p.watermark = Some(Watermark::text("©"));
            }
            if ui.selectable_label(kind == 2, "Image").clicked() && kind != 2 {
                p.watermark = Some(Watermark {
                    mark: Mark::Image {
                        path: PathBuf::new(),
                    },
                    size: 15.0,
                    ..Watermark::text("")
                });
            }
        });
    ui.end_row();
    let Some(w) = &mut p.watermark else { return };
    match &mut w.mark {
        Mark::Text { text, font, colour } => {
            ui.label("Text");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(text).desired_width(280.0));
                ui.color_edit_button_srgb(colour);
            });
            ui.end_row();
            ui.label("Font");
            if let Some(f) = path_field(ui, font.as_ref(), "Shoebox's own (a .ttf or .otf file)") {
                *font = f;
            }
            ui.end_row();
        }
        Mark::Image { path } => {
            ui.label("Image");
            if let Some(f) = path_field(
                ui,
                Some(&*path).filter(|p| !p.as_os_str().is_empty()),
                "A PNG file",
            ) {
                *path = f.unwrap_or_default();
            }
            ui.end_row();
        }
    }
    ui.label("");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut w.size)
                .range(0.5..=100.0)
                .speed(0.1)
                .prefix("Size ")
                .suffix(" %"),
        )
        .on_hover_text("Text: its size; an image: its width; as a share of the long edge");
        ui.add(
            egui::DragValue::new(&mut w.opacity)
                .range(0.0..=100.0)
                .prefix("Opacity ")
                .suffix(" %"),
        );
        ui.add(
            egui::DragValue::new(&mut w.margin)
                .range(0.0..=25.0)
                .speed(0.1)
                .prefix("Margin ")
                .suffix(" %"),
        );
    });
    ui.end_row();
    ui.label("Position");
    egui::ComboBox::from_id_salt("export-anchor")
        .selected_text(w.anchor.label())
        .show_ui(ui, |ui| {
            for a in Anchor::ALL {
                ui.selectable_value(&mut w.anchor, a, a.label());
            }
        });
    ui.end_row();
}

/// The Info panel's list of a photo's exports (stale ones flagged).
/// Tile `id`'s exports as listed (those of its copy, or the master's),
/// each with whether its file is there and whether it's stale (the edit it
/// was made from has changed since).
pub fn listed(app: &ShoeboxApp, id: PhotoId) -> Vec<(shoebox_core::cache::ExportRow, bool, bool)> {
    let Some((photo, rows)) = &app.export.of_photo else {
        return Vec::new();
    };
    let copy = app.copy_of(id).unwrap_or(shoebox_core::edits::MAIN);
    let current = match app.copy_info(id) {
        Some(c) => c.key.as_ref(),
        None => app.edited.get(photo),
    };
    rows.iter()
        .filter(|(r, _)| r.copy == copy)
        .map(|(r, there)| (r.clone(), *there, r.edit_key.as_ref() != current))
        .collect()
}

/// The exports of tile `id` (the file's master edit, or a copy's).
pub fn info_section(app: &mut ShoeboxApp, ui: &mut egui::Ui, id: PhotoId, photo: &std::path::Path) {
    if app.export.asked_for.as_deref() != Some(photo) {
        app.export.asked_for = Some(photo.to_path_buf());
        app.export.of_photo = None;
        app.command(LibraryCommand::LoadExports(photo.to_path_buf()));
    }
    if app.export.of_photo.as_ref().is_none_or(|(p, _)| p != photo) {
        return;
    }
    let rows = listed(app, id);
    if rows.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new("EXPORTS")
            .size(11.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
    let mut reveal = None;
    let mut again = None;
    for (r, there, stale) in rows {
        ui.horizontal_wrapped(|ui| {
            ui.label(&r.preset_name);
            let when = crate::app::local_time_ns(r.created_at * 1_000_000);
            ui.weak(when);
            if !there {
                ui.colored_label(ui.visuals().warn_fg_color, "missing")
                    .on_hover_text("The file isn't where it was exported to");
            } else if stale {
                ui.colored_label(ui.visuals().warn_fg_color, "stale")
                    .on_hover_text("The edit has changed since this export");
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
