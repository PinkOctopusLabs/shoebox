//! iCloud Photos in the app (PHOTOS-SPEC, Import): the Import screen's
//! second source, its album table and progress; Settings › iCloud Photos;
//! the Info panel's "From iCloud Photos" row. The work runs on the Photos
//! thread (`crate::photos`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, RichText};
use shoebox_core::photos::ledger::{AlbumChoice, AlbumUse};
use shoebox_core::photos::plan::{AlbumRow, Favourite};
use shoebox_core::photos::sync::{Progress, Report};
use shoebox_core::photos::{Access, ResourceKind};
use shoebox_core::settings::Settings;

use crate::app::ShoeboxApp;
use crate::jobs::LibraryCommand;
use crate::photos::{Command, Event, PlanRequest, Planned};
use crate::views::duplicates::plural;

/// The iCloud Photos side of Import, and the Info panel's row.
#[derive(Default)]
pub struct State {
    /// Import's source is iCloud Photos (not a card or folder).
    pub on: bool,
    pub access: Option<Access>,
    pub listing: bool,
    pub planned: Option<Arc<Planned>>,
    /// The plan's albums, as the person edits them.
    pub albums: Vec<AlbumRow>,
    /// Copying: where it stands, and when it started.
    pub running: Option<(Progress, Instant)>,
    pub report: Option<Report>,
    pub error: Option<String>,
    /// The Info panel's question and its answer.
    asked: Option<PathBuf>,
    #[allow(clippy::type_complexity)]
    origin: Option<(
        PathBuf,
        Option<(ResourceKind, i64)>,
        Option<crate::jobs::SentToPhotos>,
    )>,
}

impl State {
    pub fn busy(&self) -> bool {
        self.listing || self.running.is_some()
    }
}

/// List Photos and plan a sync into Import's chosen root.
pub fn plan(app: &mut ShoeboxApp) {
    let Some(root) = app.import.root.clone() else {
        return;
    };
    let template = if app.import.template.trim().is_empty() {
        app.settings.import_template().to_owned()
    } else {
        app.import.template.clone()
    };
    let st = &mut app.icloud;
    st.planned = None;
    st.albums.clear();
    st.report = None;
    st.error = None;
    st.listing = true;
    let roots: Vec<PathBuf> = app.settings.roots.iter().map(|r| r.path.clone()).collect();
    app.photos.send(Command::Plan(Box::new(PlanRequest {
        root,
        roots,
        writable: app.settings.writable_roots(),
        template,
        options: app.settings.photos.clone(),
        catalog: app.catalog.clone(),
    })));
}

/// Copy what the plan lists, with the albums as chosen.
pub fn run(app: &mut ShoeboxApp) {
    let albums: HashMap<String, AlbumChoice> = app
        .icloud
        .albums
        .iter()
        .map(|a| (a.id.clone(), a.choice.clone()))
        .collect();
    let total = app
        .icloud
        .planned
        .as_ref()
        .map_or(0, |p| p.plan.new.len() + p.plan.updated.len());
    app.icloud.running = Some((
        Progress {
            total,
            ..Progress::default()
        },
        Instant::now(),
    ));
    app.icloud.report = None;
    app.photos.send(Command::Run(albums));
}

/// Stop copying between files (what's copied stays).
pub fn cancel(app: &mut ShoeboxApp) {
    app.photos.cancel();
}

/// Take in what the Photos thread says (each frame).
pub fn tick(app: &mut ShoeboxApp) {
    while let Ok(ev) = app.photos.events.try_recv() {
        if matches!(
            ev,
            Event::PushPlanned(_) | Event::Pushed(_) | Event::PushUndone(_)
        ) {
            crate::views::push::event(app, ev);
            continue;
        }
        if crate::views::push::waiting(app) {
            let why = match &ev {
                Event::Failed(e) => Some(e.clone()),
                Event::Access(a) if !matches!(a, Access::Full | Access::Limited) => Some(
                    "Shoebox can't see your Photos library: turn it on in System Settings › \
                     Privacy & Security › Photos."
                        .to_owned(),
                ),
                _ => None,
            };
            if let Some(why) = why {
                app.push.planning = false;
                app.push.sending = false;
                app.push.error = Some(why);
                continue;
            }
        }
        let st = &mut app.icloud;
        match ev {
            Event::Access(a) => {
                st.access = Some(a);
                if !matches!(a, Access::Full | Access::Limited) {
                    st.listing = false;
                }
            }
            Event::Listing => st.listing = true,
            Event::Planned(p) => {
                st.listing = false;
                st.albums = p.plan.albums.clone();
                st.planned = Some(p);
            }
            Event::Progress(p) => {
                if let Some((now, _)) = &mut st.running {
                    *now = p;
                }
            }
            Event::Done(r) => {
                st.running = None;
                st.planned = None;
                st.albums.clear();
                let mut m = format!(
                    "Copied {} from iCloud Photos ({})",
                    plural(r.copied, "photo"),
                    plural(r.files, "file")
                );
                if let Some(b) = r.batch {
                    m.push_str(&format!(", batch {b}"));
                }
                m.push_str(". Undo moves the copies to the Trash.");
                app.notice = Some(m);
                app.icloud.report = Some(r);
            }
            Event::Failed(e) => {
                st.listing = false;
                st.running = None;
                st.error = Some(e);
            }
            Event::PushPlanned(_) | Event::Pushed(_) | Event::PushUndone(_) => {}
        }
    }
}

/// The Import screen with iCloud Photos as its source.
pub fn import(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.weak(
        "Copies the photos and videos in your Photos library (and iCloud Photos) into the \
         library, at full quality: the originals, downloaded from iCloud where needed. Photos \
         is never changed, and photos copied before are never copied again.",
    );
    ui.add_space(6.0);
    let busy = app.icloud.busy();
    let writable = app.settings.writable_roots();
    if app.import.root.is_none() {
        app.import.root = writable.first().cloned();
    }
    egui::Grid::new("icloud-form")
        .num_columns(2)
        .spacing(egui::vec2(10.0, 6.0))
        .show(ui, |ui| {
            ui.label("Into");
            if writable.is_empty() {
                ui.weak("Add a library folder that isn't a reference folder first");
            } else {
                egui::ComboBox::from_id_salt("icloud-root")
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
                match crate::views::files::sample(&app.import.template, true) {
                    Ok(s) => ui.weak(format!("e.g. {s}")),
                    Err(e) => ui.colored_label(ui.visuals().warn_fg_color, e),
                };
            });
            ui.end_row();
        });
    let ready = app.import.root.is_some()
        && shoebox_core::template::Template::parse(&app.import.template).is_ok()
        && !busy;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(ready, egui::Button::new("Find photos"))
            .clicked()
        {
            plan(app);
        }
        if app.icloud.listing {
            ui.add(egui::Spinner::new());
            ui.label("Listing your Photos library…");
        }
    });
    access_note(app, ui);
    if let Some(e) = &app.icloud.error {
        ui.colored_label(ui.visuals().warn_fg_color, e);
    }
    if let Some(r) = app.icloud.report.clone() {
        report(app, ui, &r);
    }
    let Some(planned) = app.icloud.planned.clone() else {
        return;
    };
    ui.add_space(6.0);
    summary(ui, &planned);
    albums(app, ui);
    ui.add_space(6.0);
    let p = &planned.plan;
    let work = p.new.len() + p.updated.len() + p.joined.len() + p.favourited.len();
    ui.horizontal(|ui| {
        let label = if p.new.is_empty() && work > 0 {
            "Bring photos up to date".to_owned()
        } else {
            format!("Import {}", plural(p.new.len(), "photo"))
        };
        let button = egui::Button::new(
            RichText::new(label)
                .strong()
                .color(ui.visuals().selection.stroke.color),
        )
        .fill(ui.visuals().selection.bg_fill)
        .min_size(egui::vec2(0.0, 26.0));
        if ui.add_enabled(work > 0 && !busy, button).clicked() {
            run(app);
        }
        if let Some(name) = planned.root.file_name() {
            ui.weak(format!("into {}", name.to_string_lossy()));
        }
    });
    if let Some((p, started)) = app.icloud.running.clone() {
        progress(app, ui, &p, started);
    }
}

fn access_note(app: &ShoeboxApp, ui: &mut egui::Ui) {
    let text = match app.icloud.access {
        Some(Access::Denied) => {
            "Shoebox can't see your Photos library. Turn it on in System Settings › Privacy & \
             Security › Photos, then Find photos again."
        }
        Some(Access::Limited) => {
            "Shoebox can see only the photos you chose for it (System Settings › Privacy & \
             Security › Photos): only those are copied."
        }
        _ => return,
    };
    ui.colored_label(ui.visuals().warn_fg_color, text);
}

fn summary(ui: &mut egui::Ui, planned: &Planned) {
    let p = &planned.plan;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("{} to copy", plural(p.new.len(), "photo"))).strong());
        let parts = [
            (p.synced, "copied before"),
            (
                p.updated.len(),
                "edited again in Photos (the new edit is copied too)",
            ),
            (p.joined.len(), "copied before and added to albums since"),
            (p.favourited.len(), "copied before and favourited since"),
            (p.hidden, "hidden in Photos, left out"),
            (p.burst_frames, "burst frames nobody kept, left out"),
        ];
        for (n, what) in parts {
            if n > 0 {
                ui.label(format!("· {n} {what}"));
            }
        }
    });
    if !p.left.is_empty() {
        let n: usize = p.left.iter().map(|(_, n)| n).sum();
        ui.weak(format!(
            "{} left albums in Photos since the last import; nothing changes here for that.",
            plural(n, "photo")
        ));
    }
    if !p.new.is_empty() {
        ui.weak(
            "Downloads from iCloud can take hours for a large library: Shoebox keeps the Mac \
             awake, and stops if the disk gets within 10 GB of full. Stopping keeps what's \
             copied; the next import carries on.",
        );
    }
}

/// What each album becomes on the photos in it.
fn albums(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if app.icloud.albums.is_empty() {
        return;
    }
    let busy = app.icloud.busy();
    egui::CollapsingHeader::new(format!("Albums ({})", app.icloud.albums.len()))
        .default_open(true)
        .show(ui, |ui| {
            ui.weak(
                "What each album becomes on its photos. Events nest as Photos' album folders do.",
            );
            egui::ScrollArea::vertical()
                .id_salt("icloud-albums")
                .max_height(260.0)
                .show(ui, |ui| {
                    egui::Grid::new("icloud-albums-grid")
                        .num_columns(3)
                        .spacing(egui::vec2(12.0, 4.0))
                        .striped(true)
                        .show(ui, |ui| {
                            for a in &mut app.icloud.albums {
                                let path = a.path.join(" › ");
                                ui.add_enabled_ui(!busy, |ui| {
                                    ui.add_sized(
                                        [260.0, 20.0],
                                        egui::TextEdit::singleline(&mut a.choice.name),
                                    )
                                })
                                .inner
                                .on_hover_text(format!("In Photos: {path}"));
                                ui.weak(plural(a.photos, "photo"));
                                ui.horizontal(|ui| {
                                    for (kind, label) in [
                                        (AlbumUse::Event, "Event"),
                                        (AlbumUse::Keyword, "Keyword"),
                                        (AlbumUse::Nothing, "Nothing"),
                                    ] {
                                        let sel = a.choice.kind == kind;
                                        let r = ui.add_enabled(
                                            !busy,
                                            egui::Button::selectable(sel, label),
                                        );
                                        r.widget_info(|| {
                                            egui::WidgetInfo::selected(
                                                egui::Role::Button,
                                                true,
                                                sel,
                                                format!("{path}: {label}"),
                                            )
                                        });
                                        if r.clicked() {
                                            a.choice.kind = kind;
                                        }
                                    }
                                });
                                ui.end_row();
                            }
                        });
                });
        });
}

fn progress(app: &mut ShoeboxApp, ui: &mut egui::Ui, p: &Progress, started: Instant) {
    use crate::panels::{human_size, time_left};
    let fraction = p.done as f32 / p.total.max(1) as f32;
    ui.horizontal(|ui| {
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(320.0)
                .text(format!("{} of {}", p.done, p.total)),
        );
        if ui.button("Stop").clicked() {
            cancel(app);
        }
    });
    let secs = started.elapsed().as_secs_f64();
    let mut line = format!("{} downloaded", human_size(p.bytes));
    if secs > 2.0 && p.bytes > 0 {
        line.push_str(&format!(
            " · {}/s",
            human_size((p.bytes as f64 / secs) as u64)
        ));
        if p.done > 0 {
            let left = secs / p.done as f64 * p.total.saturating_sub(p.done) as f64;
            line.push_str(&format!(" · {}", time_left(left)));
        }
    }
    ui.weak(line);
    if let Some(name) = &p.current {
        ui.weak(RichText::new(name).monospace().small());
    }
}

fn report(app: &mut ShoeboxApp, ui: &mut egui::Ui, r: &Report) {
    let mut parts = vec![format!(
        "Copied {} ({})",
        plural(r.copied, "photo"),
        plural(r.files, "file")
    )];
    if r.in_library > 0 {
        parts.push(format!("{} already in the library", r.in_library));
    }
    if r.dated_by_photos > 0 {
        parts.push(format!("{} dated by Photos", r.dated_by_photos));
    }
    if r.marked > 0 {
        parts.push(format!(
            "{} given album events or ratings",
            plural(r.marked, "photo")
        ));
    }
    ui.label(parts.join(" · "));
    if r.stopped {
        ui.weak("Stopped as asked: what's copied stays, and the next import carries on.");
    }
    if r.low_space {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Stopped: the disk is within 10 GB of full. Free some space, then import again to carry on.",
        );
    }
    if !r.failed.is_empty() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("{} couldn't be copied:", plural(r.failed.len(), "photo")),
        );
        for (id, why) in r.failed.iter().take(20) {
            ui.weak(format!("{}: {why}", id.local));
        }
        if ui.button("Try again").clicked() {
            plan(app);
        }
    }
}

/// The Info panel's row: whether this file came from iCloud Photos.
pub fn info_row(app: &mut ShoeboxApp, ui: &mut egui::Ui, photo: &Path) {
    if app.icloud.asked.as_deref() != Some(photo) {
        app.icloud.asked = Some(photo.to_path_buf());
        app.icloud.origin = None;
        app.command(LibraryCommand::LoadFromPhotos(photo.to_path_buf()));
    }
    let Some((p, found, sent)) = app.icloud.origin.clone() else {
        return;
    };
    if p != photo {
        return;
    }
    if let Some((kind, at)) = found {
        let what = match kind {
            ResourceKind::Edited => "Photos' edit, from iCloud Photos",
            _ => "From iCloud Photos",
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(what);
            ui.weak(crate::app::local_time_ns(at * 1_000_000));
        });
    }
    if let Some(s) = sent {
        let stale = app.edited.get(photo).cloned() != s.render_hash;
        let mut again = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Sent to Photos");
            ui.weak(crate::app::local_time_ns(s.at * 1_000_000));
            if stale {
                ui.colored_label(ui.visuals().warn_fg_color, "edited since")
                    .on_hover_text("The edit has changed since it was sent");
            }
            again = ui
                .small_button("Send Again")
                .on_hover_text("Send this photo's edit to Photos again")
                .clicked();
        });
        if again {
            let id = crate::jobs::photo_id(photo);
            crate::views::push::open_for(app, &[id]);
        }
    }
}

/// The library worker's answer to `LoadFromPhotos`.
pub fn loaded(
    app: &mut ShoeboxApp,
    photo: PathBuf,
    found: Option<(ResourceKind, i64)>,
    sent: Option<crate::jobs::SentToPhotos>,
) {
    if app.icloud.asked.as_ref() == Some(&photo) {
        app.icloud.origin = Some((photo, found, sent));
    }
}

/// Settings › iCloud Photos.
pub fn settings(ui: &mut egui::Ui, s: &mut Settings) {
    let o = &mut s.photos;
    ui.checkbox(&mut o.live_videos, "Copy Live Photos' movies");
    ui.checkbox(
        &mut o.photos_edits,
        "Copy Photos' edits too, beside the originals",
    );
    ui.checkbox(&mut o.all_burst_frames, "Copy every frame of a burst");
    ui.checkbox(&mut o.hidden, "Copy hidden photos (keyword Photos|Hidden)");
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Favourites become");
        egui::ComboBox::from_id_salt("icloud-favourite")
            .selected_text(favourite_name(o.favourite))
            .show_ui(ui, |ui| {
                for f in [
                    Favourite::Rating(5),
                    Favourite::Rating(4),
                    Favourite::Rating(3),
                    Favourite::Keyword,
                    Favourite::Pick,
                    Favourite::Nothing,
                ] {
                    ui.selectable_value(&mut o.favourite, f, favourite_name(f));
                }
            });
    });
    ui.add(
        egui::Label::new(
            RichText::new(
                "Originals are always copied at full quality. Photos' edits are copied as \
                 a separate -photos file, since Shoebox can't redo Photos' adjustments. Tasks \
                 › Import, with iCloud Photos as the source, copies what's new.",
            )
            .weak(),
        )
        .wrap(),
    );
}

fn favourite_name(f: Favourite) -> String {
    match f {
        Favourite::Rating(n) => "★".repeat(usize::from(n.min(5))),
        Favourite::Keyword => "Keyword Photos|Favourite".into(),
        Favourite::Pick => "Pick flag".into(),
        Favourite::Nothing => "Nothing".into(),
    }
}

impl ShoeboxApp {
    /// A send finished: the Info panel's row asks again.
    pub(crate) fn icloud_sent(&mut self) {
        self.icloud.asked = None;
        self.icloud.origin = None;
    }

    /// The iCloud Photos plan's new and copied-before counts (tests).
    pub fn icloud_counts(&self) -> Option<(usize, usize)> {
        let p = &self.icloud.planned.as_ref()?.plan;
        Some((p.new.len(), p.synced))
    }

    /// The last iCloud Photos import's copied and failed counts (tests).
    pub fn icloud_report(&self) -> Option<(usize, usize)> {
        let r = self.icloud.report.as_ref()?;
        Some((r.copied, r.failed.len()))
    }

    /// Open Settings at its iCloud Photos tab (tests).
    #[doc(hidden)]
    pub fn open_icloud_settings(&mut self) {
        self.open_settings();
        self.settings_ui.tab = crate::views::settings::Tab::Photos;
    }
}
