//! Send to Photos (PHOTOS-SPEC, Push): the dialog's dry-run table (what
//! each photo becomes in Photos), a preset for the files (JPEG, PNG or
//! TIFF; edits of Photos' own photos are always JPEG), an album; then the
//! photos are rendered as an export into the cache and the Photos thread
//! puts them into Photos, recording them in one batch (undo reverts them
//! there first).

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, RichText};
use shoebox_core::photos::push::{Action, Item, Rendered, Row};
use shoebox_develop::export::{Destination, FileFormat, Metadata, OutputSize, Preset, Space};

use crate::app::ShoeboxApp;
use crate::develop::{Clash, ExportItem, ExportJob};
use crate::jobs::PhotoId;
use crate::photos::Command;
use crate::views::duplicates::plural;

/// The built-in preset's name (not one of the export presets).
pub const PHOTOS_PRESET: &str = "Photos (JPEG, full size)";

#[derive(Default)]
pub struct State {
    pub open: bool,
    /// What exporting the photos renders (movies have none), by photo and
    /// copy.
    items: Vec<ExportItem>,
    pub planning: bool,
    pub rows: Option<Vec<Row>>,
    /// The preset for new photos (empty: the built-in one).
    pub preset: String,
    pub album_on: bool,
    pub album: String,
    /// Rendering: the jobs' tags still to finish, and the files so far.
    pending: Vec<u64>,
    rendered: Vec<Rendered>,
    pub sending: bool,
    pub error: Option<String>,
    next_tag: u64,
}

/// Shoebox's own preset for Photos: full-size JPEG in Display P3.
pub fn photos_preset() -> Preset {
    Preset {
        name: PHOTOS_PRESET.into(),
        format: FileFormat::Jpeg {
            quality: 92,
            full_chroma: true,
            progressive: true,
        },
        space: Space::DisplayP3,
        size: OutputSize::Full,
        metadata: Metadata::All,
        destination: Destination::Folder,
        folder: None,
        naming: "{name}".into(),
        border: None,
        watermark: None,
    }
}

/// Open the dialog for the selection (else the current photo).
pub fn open(app: &mut ShoeboxApp) {
    let ids = crate::views::export::targets(app);
    open_for(app, &ids);
}

/// Open the dialog for these tiles.
pub fn open_for(app: &mut ShoeboxApp, ids: &[PhotoId]) {
    let items = crate::views::export::items(app, ids, None);
    let mut plan: Vec<Item> = items
        .iter()
        .map(|i| Item {
            photo: i.path.clone(),
            copy: i.copy.clone(),
            render_hash: i.edit_key.clone(),
            video: false,
        })
        .collect();
    for &id in ids {
        if let Some(idx) = app.index(id)
            && app.catalog.photos[idx].format.is_movie()
        {
            plan.push(Item {
                photo: app.catalog.photos[idx].path.clone(),
                copy: shoebox_core::edits::MAIN.into(),
                render_hash: None,
                video: true,
            });
        }
    }
    if plan.is_empty() {
        return;
    }
    let st = &mut app.push;
    st.open = true;
    st.items = items;
    st.rows = None;
    st.error = None;
    st.planning = true;
    if st.album.is_empty() {
        st.album = "Shoebox".into();
    }
    let roots = app.settings.roots.iter().map(|r| r.path.clone()).collect();
    app.photos.send(Command::PlanPush { items: plan, roots });
}

/// The dialog.
pub fn dialog(app: &mut ShoeboxApp, ctx: &egui::Context) {
    if !app.push.open {
        return;
    }
    let mut open = true;
    let mut send = false;
    egui::Window::new("Send to Photos")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Photos gets each photo as rendered here, like an export. A photo that \
                         came from Photos goes back as an edit of it: Photos keeps the original \
                         and can revert. Others become new photos in Photos.",
                    )
                    .weak(),
                )
                .wrap(),
            );
            ui.add_space(6.0);
            if app.push.planning {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("Looking in Photos…");
                });
            }
            if let Some(e) = &app.push.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            let Some(rows) = app.push.rows.clone() else {
                return;
            };
            table(ui, &rows);
            ui.add_space(6.0);
            let sends = rows.iter().filter(|r| r.action.sends()).count();
            let new = rows.iter().any(|r| r.action == Action::New);
            if new {
                ui.horizontal(|ui| {
                    ui.label("New photos as");
                    let names = preset_names(app);
                    let shown = if app.push.preset.is_empty() {
                        PHOTOS_PRESET.to_owned()
                    } else {
                        app.push.preset.clone()
                    };
                    egui::ComboBox::from_id_salt("push-preset")
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut app.push.preset, String::new(), PHOTOS_PRESET);
                            for n in names {
                                ui.selectable_value(&mut app.push.preset, n.clone(), n);
                            }
                        });
                });
            }
            if rows.iter().any(|r| r.action.edits()) {
                ui.weak("Edits of photos already in Photos are sent as full-size JPEGs (Photos' rule for edits).");
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut app.push.album_on, "Add to the album");
                ui.add_enabled(
                    app.push.album_on,
                    egui::TextEdit::singleline(&mut app.push.album).desired_width(200.0),
                );
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let busy = app.push.sending || !app.push.pending.is_empty();
                let label = format!("Send {}", plural(sends, "photo"));
                let button = egui::Button::new(
                    RichText::new(label)
                        .strong()
                        .color(ui.visuals().selection.stroke.color),
                )
                .fill(ui.visuals().selection.bg_fill)
                .min_size(egui::vec2(0.0, 26.0));
                if ui.add_enabled(sends > 0 && !busy, button).clicked() {
                    send = true;
                }
                if busy {
                    ui.add(egui::Spinner::new());
                    ui.label(if app.push.sending {
                        "Sending to Photos… (macOS may ask you to allow it)"
                    } else {
                        "Rendering…"
                    });
                }
            });
        });
    if send {
        start(app);
    }
    if !open && app.push.pending.is_empty() && !app.push.sending {
        app.push.open = false;
        app.push.rows = None;
    }
}

fn table(ui: &mut egui::Ui, rows: &[Row]) {
    egui::ScrollArea::vertical()
        .id_salt("push-table")
        .max_height(280.0)
        .show(ui, |ui| {
            egui::Grid::new("push-grid")
                .num_columns(2)
                .spacing(egui::vec2(16.0, 4.0))
                .striped(true)
                .show(ui, |ui| {
                    for r in rows {
                        let name = r
                            .item
                            .photo
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        ui.label(&name)
                            .on_hover_text(r.item.photo.display().to_string());
                        let (text, warn) = describe(&r.action);
                        if warn {
                            ui.colored_label(ui.visuals().warn_fg_color, text);
                        } else if r.action.sends() {
                            ui.label(text);
                        } else {
                            ui.weak(text);
                        }
                        ui.end_row();
                    }
                });
        });
}

/// What a row will do, and whether it needs a warning colour.
fn describe(a: &Action) -> (String, bool) {
    match a {
        Action::New => ("A new photo in Photos".into(), false),
        Action::Edit {
            replaces: true,
            kept: Some(k),
            ..
        } => (
            format!(
                "An edit of its photo in Photos; replaces the edit made in Photos (kept here as {})",
                k.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            true,
        ),
        Action::Edit { replaces: true, .. } => (
            "An edit of its photo in Photos; the edit made in Photos will be lost from Photos"
                .into(),
            true,
        ),
        Action::Edit { .. } => (
            "An edit of its photo in Photos (the original stays)".into(),
            false,
        ),
        Action::Update { .. } => ("Updates what was sent before".into(), false),
        Action::UpToDate => ("Not sent: up to date in Photos".into(), false),
        Action::InPhotos => ("Not sent: already in Photos, not edited here".into(), false),
        Action::Video => ("Not sent: Shoebox doesn't edit videos".into(), false),
    }
}

/// Export presets that can go to Photos (JPEG, PNG, TIFF).
fn preset_names(app: &ShoeboxApp) -> Vec<String> {
    app.export
        .presets
        .iter()
        .filter(|p| {
            matches!(
                p.format,
                FileFormat::Jpeg { .. } | FileFormat::Png | FileFormat::Tiff { .. }
            )
        })
        .map(|p| p.name.clone())
        .collect()
}

/// Render the rows that send: as exports into the cache, one job a preset.
fn start(app: &mut ShoeboxApp) {
    let Some(rows) = app.push.rows.clone() else {
        return;
    };
    let chosen = app
        .export
        .presets
        .iter()
        .find(|p| p.name == app.push.preset)
        .cloned()
        .filter(|p| {
            matches!(
                p.format,
                FileFormat::Jpeg { .. } | FileFormat::Png | FileFormat::Tiff { .. }
            )
        })
        .unwrap_or_else(photos_preset);
    let mut by_preset: Vec<(Preset, Vec<ExportItem>)> = Vec::new();
    for r in rows.iter().filter(|r| r.action.sends()) {
        let Some(item) = app
            .push
            .items
            .iter()
            .find(|i| i.path == r.item.photo && i.copy == r.item.copy)
            .cloned()
        else {
            continue;
        };
        let preset = if r.action.edits() {
            photos_preset()
        } else {
            chosen.clone()
        };
        match by_preset.iter_mut().find(|(p, _)| p.name == preset.name) {
            Some((_, v)) => v.push(item),
            None => by_preset.push((preset, vec![item])),
        }
    }
    let (roots, reference) = crate::views::export::roots(app);
    app.push.rendered.clear();
    app.push.error = None;
    for (mut preset, items) in by_preset {
        app.push.next_tag += 1;
        let tag = app.push.next_tag;
        // Outside the library, in the cache, named as the photo.
        preset.destination = Destination::Folder;
        preset.folder = Some(app.photos_push_dir.join(tag.to_string()));
        preset.naming = "{name}".into();
        app.push.pending.push(tag);
        app.developer.export(ExportJob {
            items,
            presets: vec![preset],
            roots: roots.clone(),
            reference: reference.clone(),
            on_clash: Some(Clash::NewFile),
            tag: Some(tag),
        });
    }
}

/// An export job for a send finished: once all have, send them.
pub fn rendered(
    app: &mut ShoeboxApp,
    tag: u64,
    written: Vec<crate::develop::Exported>,
    errors: Vec<String>,
) {
    let st = &mut app.push;
    if !st.pending.contains(&tag) {
        return;
    }
    st.pending.retain(|&t| t != tag);
    if !errors.is_empty() {
        st.error = Some(format!(
            "Some photos couldn't be rendered: {}",
            errors.join("; ")
        ));
    }
    let rows = st.rows.clone().unwrap_or_default();
    for w in written {
        if let Some(r) = rows
            .iter()
            .find(|r| r.item.photo == w.photo && r.item.copy == w.copy && r.action.sends())
        {
            st.rendered.push(Rendered {
                row: r.clone(),
                file: w.file,
            });
        }
    }
    if !st.pending.is_empty() {
        return;
    }
    let rendered = std::mem::take(&mut st.rendered);
    if rendered.is_empty() {
        return;
    }
    st.sending = true;
    let album = st
        .album_on
        .then(|| st.album.trim().to_owned())
        .filter(|a| !a.is_empty());
    let roots = app.settings.roots.iter().map(|r| r.path.clone()).collect();
    app.photos.send(Command::Push {
        rendered,
        roots,
        album,
    });
}

/// Take in the Photos thread's answers about sends.
pub fn event(app: &mut ShoeboxApp, ev: crate::photos::Event) {
    use crate::photos::Event;
    match ev {
        Event::PushPlanned(rows) => {
            app.push.planning = false;
            app.push.rows = Some(rows);
        }
        Event::Pushed(r) => {
            app.push.sending = false;
            app.push.open = false;
            app.push.rows = None;
            let mut m = format!("Sent {} to Photos", plural(r.created + r.edited, "photo"));
            if r.edited > 0 {
                m.push_str(&format!(" ({} as edits of their photos there)", r.edited));
            }
            m.push_str(". Undo takes it back in Photos too.");
            app.notice = Some(m);
            if !r.failed.is_empty() {
                let why: Vec<String> = r
                    .failed
                    .iter()
                    .map(|(p, e)| {
                        format!(
                            "{}: {e}",
                            p.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                app.error = Some(format!("Some photos weren't sent: {}", why.join("; ")));
            }
            app.icloud_sent();
        }
        Event::PushUndone(r) => match r {
            Ok(n) => app.notice = Some(format!("Took back {} in Photos.", plural(n, "photo"))),
            Err(e) => app.error = Some(format!("Couldn't undo the send: {e}")),
        },
        _ => {}
    }
}

/// Whether the dialog is waiting on the Photos thread.
pub fn waiting(app: &ShoeboxApp) -> bool {
    app.push.planning || app.push.sending
}

/// What each photo was sent as (for tests): file name → description.
pub fn described(app: &ShoeboxApp) -> HashMap<String, String> {
    app.push
        .rows
        .iter()
        .flatten()
        .map(|r| {
            (
                r.item
                    .photo
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                describe(&r.action).0,
            )
        })
        .collect()
}

/// Where a send's renders go (for the app's paths).
pub fn dir(staging: &std::path::Path) -> PathBuf {
    staging.join("push")
}

impl ShoeboxApp {
    /// Open Send to Photos for the selection (tests).
    #[doc(hidden)]
    pub fn open_send_to_photos(&mut self) {
        open(self);
    }

    /// The Send to Photos dialog's rows: file name → what it'll do (tests).
    pub fn send_to_photos_rows(&self) -> HashMap<String, String> {
        described(self)
    }

    /// Whether Send to Photos is open (tests).
    pub fn send_to_photos_open(&self) -> bool {
        self.push.open
    }
}
