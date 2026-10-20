//! The Settings window (⌘,): library folders, sidecars, naming, map, memory
//! and updates. Changes apply and save as they're made.

use eframe::egui;
use shoebox_core::settings::{DEFAULT_DNG_CACHE_GB, DEFAULT_IMAGE_MEMORY_MB, Settings};
use shoebox_core::template::{DEFAULT_IMPORT, DEFAULT_RENAME};

use crate::app::ShoeboxApp;
use crate::jobs::LibraryCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Library,
    Sidecars,
    Naming,
    Search,
    People,
    Map,
    Memory,
    Raw,
    Updates,
}

impl Tab {
    const ALL: [Tab; 9] = [
        Tab::Library,
        Tab::Sidecars,
        Tab::Naming,
        Tab::Search,
        Tab::People,
        Tab::Map,
        Tab::Memory,
        Tab::Raw,
        Tab::Updates,
    ];

    fn name(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::Sidecars => "Sidecars",
            Tab::Naming => "Naming",
            Tab::Search => "Search",
            Tab::People => "People",
            Tab::Map => "Map",
            Tab::Memory => "Memory",
            Tab::Raw => "Raw",
            Tab::Updates => "Updates",
        }
    }
}

/// The window's state.
#[derive(Default)]
pub struct State {
    pub open: bool,
    pub tab: Tab,
    /// Templates as typed (saved only while they're valid).
    import: String,
    rename: String,
}

impl State {
    /// Open the window, starting the template fields from `settings`.
    pub fn open(&mut self, settings: &Settings) {
        self.open = true;
        self.import = settings.import_template().to_owned();
        self.rename = settings.rename_template().to_owned();
    }
}

pub fn show(app: &mut ShoeboxApp, ctx: &egui::Context) {
    if !app.settings_ui.open {
        return;
    }
    let mut open = true;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    ui.selectable_value(&mut app.settings_ui.tab, tab, tab.name());
                }
            });
            ui.separator();
            ui.set_min_width(540.0);
            ui.set_min_height(260.0);
            let mut s = app.settings.clone();
            match app.settings_ui.tab {
                Tab::Library => library(app, ui),
                Tab::Sidecars => sidecars(ui, &mut s),
                Tab::Naming => naming(app, ui, &mut s),
                Tab::Search => search(app, ui, &mut s),
                Tab::People => people(app, ui, &mut s),
                Tab::Map => map(ui, &mut s),
                Tab::Memory => memory(app, ui, &mut s),
                Tab::Raw => raw(app, ui, &mut s),
                Tab::Updates => updates(ui),
            }
            if s != app.settings {
                app.save_settings(s);
            }
        });
    app.settings_ui.open &= open;
}

fn note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).weak()).wrap());
}

fn library(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if app.settings.roots.is_empty() {
        ui.label("No folders yet.");
    }
    egui::Grid::new("settings-roots")
        .num_columns(3)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            for root in app.settings.roots.clone() {
                ui.label(crate::panels::short_path(&root.path))
                    .on_hover_text(root.path.display().to_string());
                let mut reference = root.reference;
                if ui
                    .checkbox(&mut reference, "Reference")
                    .on_hover_text("Read-only: never moved, renamed or written to")
                    .changed()
                {
                    app.scanning = true;
                    app.command(LibraryCommand::SetReference(root.path.clone(), reference));
                }
                if ui
                    .button("Remove")
                    .on_hover_text("Stop showing this folder (its files are untouched)")
                    .clicked()
                {
                    app.remove_root(root.path.clone());
                }
                ui.end_row();
            }
        });
    ui.add_space(6.0);
    if ui.button("Add Folder…").clicked() {
        crate::panels::add_folder(app);
    }
    ui.add_space(6.0);
    note(
        ui,
        "Your folders are the library. Removing one only stops Shoebox showing it; \
         nothing on disk changes. Reference folders are read-only: duplicates of \
         their files can be found elsewhere, but they are never moved or written to.",
    );
}

fn sidecars(ui: &mut egui::Ui, s: &mut Settings) {
    ui.checkbox(
        &mut s.marks_to_all_stack_members,
        "Write ratings, flags and labels to every file of a photo",
    );
    note(
        ui,
        "Off: marks on a photo go to its primary file's sidecar (the NEF of a \
         NEF + JPG). On: every member gets them, so apps that only look at the \
         JPEG or TIFF see them too.",
    );
    ui.add_space(8.0);
    ui.checkbox(
        &mut s.write_stem_sidecar_for_raw,
        "Also write name.xmp for RAW files",
    );
    note(
        ui,
        "Shoebox writes name.ext.xmp (DSC_0042.NEF.xmp), so a NEF and its JPG \
         never share a sidecar. Lightroom-style name.xmp is always read; turn \
         this on for tools that only read that.",
    );
}

/// A template field: edit, example or error, and Default. Returns the
/// template to save, if it's valid.
fn template_field(
    ui: &mut egui::Ui,
    text: &mut String,
    default: &str,
    import: bool,
) -> Option<String> {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(text).desired_width(440.0));
        if ui.button("Default").clicked() {
            *text = default.to_owned();
        }
    });
    match crate::views::files::sample(text, import) {
        Ok(example) => {
            ui.weak(format!("e.g. {example}"));
            Some(text.clone())
        }
        Err(e) => {
            ui.colored_label(ui.visuals().warn_fg_color, e);
            None
        }
    }
}

fn naming(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &mut Settings) {
    ui.strong("Import");
    note(
        ui,
        "Where imported files go, relative to the destination folder.",
    );
    let mut import = std::mem::take(&mut app.settings_ui.import);
    if let Some(t) = template_field(ui, &mut import, DEFAULT_IMPORT, true) {
        s.import_template = if t == DEFAULT_IMPORT {
            String::new()
        } else {
            t
        };
    }
    app.settings_ui.import = import;
    ui.add_space(10.0);
    ui.strong("Rename");
    note(
        ui,
        "The new name when renaming files (folders here would move them).",
    );
    let mut rename = std::mem::take(&mut app.settings_ui.rename);
    if let Some(t) = template_field(ui, &mut rename, DEFAULT_RENAME, false) {
        s.rename_template = if t == DEFAULT_RENAME {
            String::new()
        } else {
            t
        };
    }
    app.settings_ui.rename = rename;
    ui.add_space(10.0);
    note(
        ui,
        "Tokens: {YYYY} {YY} {MM} {DD} {HH} {mm} {ss} {YYYY-MM-DD} {YYYYMMDD} {HHMMSS} \
         {subsec} {name} {camera} {width} {height} {seq} {ext} {EXT}. Files without \
         a capture date go to _undated/.",
    );
}

fn search(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &mut Settings) {
    use crate::semantic::ModelState as M;
    ui.checkbox(&mut s.semantic_search, "Search photos by what's in them");
    note(
        ui,
        "Describe a photo, like \"dog on a beach\" or \"Sonnenuntergang\", and Shoebox \
         finds the photos that match, best first, in any language. Right-click a photo \
         for Find Similar Photos. It uses Google's SigLIP 2 model, which runs on this \
         Mac: your photos never leave it.",
    );
    ui.add_space(8.0);
    let gb = crate::semantic::download_bytes() as f64 / 1e9;
    if !s.semantic_search {
        note(
            ui,
            &format!(
                "Turning this on downloads the model ({gb:.1} GB) once, from {}.",
                crate::semantic::model_source()
            ),
        );
        return;
    }
    let status = app.sem.status.clone();
    match &status.model {
        M::Off => {
            ui.add(egui::Spinner::new());
        }
        M::Downloading { done, total } => {
            ui.label(format!(
                "Downloading the model ({gb:.1} GB, once) from {}",
                crate::semantic::model_source()
            ));
            ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).show_percentage());
            if ui.button("Cancel").clicked() {
                app.cancel_model_download();
                s.semantic_search = false;
            }
        }
        M::Verifying => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Checking the download…");
            });
        }
        M::Converting => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Preparing the model…");
            });
        }
        M::Failed(e) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Couldn't get the model: {e}"),
            );
            if ui.button("Try Again").clicked() {
                app.retry_semantic();
            }
        }
        M::Ready => {
            let done = status.indexed + status.skipped;
            if status.indexing() {
                ui.label(format!("Indexing: {} of {} photos", done, status.total));
                ui.add(
                    egui::ProgressBar::new(done as f32 / status.total.max(1) as f32)
                        .show_percentage(),
                );
                note(
                    ui,
                    "Search works meanwhile, over the photos indexed so far. Newest \
                     photos go first.",
                );
            } else {
                ui.label(format!("Ready: {} photos indexed.", status.indexed));
            }
            if status.skipped > 0 {
                ui.weak(format!("{} couldn't be read", status.skipped));
            }
        }
    }
}

fn people(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &mut Settings) {
    use crate::semantic::ModelState as M;
    ui.checkbox(&mut s.people, "Find people in photos");
    note(
        ui,
        "Shoebox finds the faces in your photos and groups them by likeness, so you \
         can see all the photos of someone and give them a name. Names are kept in \
         Shoebox's own folder, and added to photos as People keywords only when you \
         ask. It uses OpenCV's YuNet and SFace models, which run on this Mac: your \
         photos and faces never leave it.",
    );
    ui.add_space(8.0);
    let mb = crate::people::download_bytes() as f64 / 1e6;
    if !s.people {
        note(
            ui,
            &format!(
                "Turning this on downloads the face models ({mb:.0} MB) once, from {}.",
                crate::people::model_source()
            ),
        );
        return;
    }
    let status = app.ppl.status.clone();
    match &status.model {
        M::Off => {
            ui.add(egui::Spinner::new());
        }
        M::Downloading { done, total } => {
            ui.label(format!("Downloading the face models ({mb:.0} MB, once)"));
            ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).show_percentage());
            if ui.button("Cancel").clicked() {
                app.cancel_people_download();
                s.people = false;
            }
        }
        M::Verifying | M::Converting => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label("Checking the download…");
            });
        }
        M::Failed(e) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Couldn't get the face models: {e}"),
            );
            if ui.button("Try Again").clicked() {
                app.retry_people();
            }
        }
        M::Ready => {
            let done = status.indexed + status.skipped;
            if status.indexing() {
                ui.label(format!(
                    "Finding faces: {} of {} photos",
                    done, status.total
                ));
                ui.add(
                    egui::ProgressBar::new(done as f32 / status.total.max(1) as f32)
                        .show_percentage(),
                );
            } else {
                ui.label(format!("Ready: {} photos looked at.", status.indexed));
            }
            if let Some(v) = &app.ppl.view {
                let named = v.groups.iter().filter(|g| g.name.is_some()).count();
                ui.weak(format!(
                    "{} faces; {} named, {} unnamed groups",
                    v.faces,
                    named,
                    v.groups.len() - named
                ));
            }
        }
    }
}

fn map(ui: &mut egui::Ui, s: &mut Settings) {
    let mut online = !s.offline_map;
    ui.checkbox(&mut online, "Load map tiles from OpenStreetMap");
    s.offline_map = !online;
    note(
        ui,
        "Tiles are downloaded over HTTPS and cached. The tile server sees which \
         areas of the map you look at, never your photos or their positions. \
         Off: the map shows your photos on a blank background.",
    );
}

fn memory(app: &ShoeboxApp, ui: &mut egui::Ui, s: &mut Settings) {
    let mut mb = (s.image_memory_bytes() >> 20) as u32;
    ui.horizontal(|ui| {
        ui.label("Image memory");
        ui.add(
            egui::Slider::new(&mut mb, 256..=4096)
                .step_by(128.0)
                .suffix(" MB"),
        );
        if ui.button("Default").clicked() {
            mb = DEFAULT_IMAGE_MEMORY_MB;
        }
    });
    s.image_memory_mb = if mb == DEFAULT_IMAGE_MEMORY_MB { 0 } else { mb };
    ui.weak(format!("In use now: {} MB", app.textures.used() >> 20));
    note(
        ui,
        "Graphics memory for thumbnails and full images. More keeps more of them \
         ready when you scroll back; the least recently seen are dropped first.",
    );
}

/// The DNG converter for High efficiency NEFs, and the DNG cache.
fn raw(app: &mut ShoeboxApp, ui: &mut egui::Ui, s: &mut Settings) {
    if app.raw_status.is_none() {
        app.developer.check();
        app.raw_status = Some(Default::default());
        ui.ctx().request_repaint();
    }
    let status = app.raw_status.clone().unwrap_or_default();
    ui.label(egui::RichText::new("New photos in Develop").strong());
    let mut auto = !s.develop_auto_off;
    ui.checkbox(&mut auto, "Apply Auto the first time a raw photo is opened");
    s.develop_auto_off = !auto;
    ui.horizontal(|ui| {
        ui.label("Or start them from the preset");
        let chosen = s.new_photo_preset.clone();
        egui::ComboBox::from_id_salt("new-photo-preset")
            .selected_text(chosen.as_deref().unwrap_or("(none)"))
            .show_ui(ui, |ui| {
                if ui.selectable_label(chosen.is_none(), "(none)").clicked() {
                    s.new_photo_preset = None;
                }
                for p in &app.develop.looks {
                    let on = chosen.as_deref() == Some(p.name.as_str());
                    if ui.selectable_label(on, &p.name).clicked() {
                        s.new_photo_preset = Some(p.name.clone());
                    }
                }
            });
    });
    note(
        ui,
        "Auto sets exposure, noise reduction, sharpening, dehaze and lens corrections from \
         the photo itself, as ordinary slider values you can change. A preset chosen here \
         replaces it.",
    );
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Adobe DNG Converter").strong());
    match &status.converter {
        Some((path, version)) => {
            ui.label(format!(
                "Found{}: {}",
                version
                    .as_deref()
                    .map(|v| format!(" (version {v})"))
                    .unwrap_or_default(),
                path.display()
            ));
        }
        None => {
            ui.label("Not found.");
        }
    }
    ui.horizontal(|ui| {
        if ui.button("Download Adobe DNG Converter").clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(
                "https://helpx.adobe.com/camera-raw/using/adobe-dng-converter.html",
            ));
        }
        if ui.button("Check again").clicked() {
            app.raw_status = None;
        }
    });
    let mut path = s
        .dng_converter
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label("Installed elsewhere:");
        if ui
            .add(
                egui::TextEdit::singleline(&mut path)
                    .hint_text("/Applications/Adobe DNG Converter.app")
                    .desired_width(300.0),
            )
            .changed()
        {
            s.dng_converter =
                (!path.trim().is_empty()).then(|| std::path::PathBuf::from(path.trim()));
        }
    });
    let he = app
        .catalog
        .photos
        .iter()
        .filter(|p| {
            p.meta
                .exif
                .nef_compression
                .is_some_and(|c| shoebox_decode::nef::NefCompression::from_tag(c).needs_dng())
        })
        .count();
    note(
        ui,
        &format!(
            "{he} of your photos use Nikon's High efficiency compression, which no open decoder \
             reads yet. Shoebox converts each to a DNG with Adobe's free converter the first \
             time you edit it (your original is only read). Setting the camera's RAW recording \
             to Lossless compression avoids this for new photos."
        ),
    );
    ui.add_space(8.0);
    ui.label(egui::RichText::new("DNG cache").strong());
    ui.checkbox(
        &mut s.preconvert,
        "Convert likely edits while Shoebox is idle",
    );
    note(
        ui,
        "After a minute without use, picks and photos rated 3 stars or more are converted, \
         newest first, up to half the cache, so they open in Develop at once. The photos \
         either side of the one in Develop are always converted ahead.",
    );
    let mut gb = match s.dng_cache_gb {
        0 => DEFAULT_DNG_CACHE_GB,
        g => g,
    };
    ui.horizontal(|ui| {
        ui.label("Keep up to");
        ui.add(egui::Slider::new(&mut gb, 2..=200).suffix(" GB"));
    });
    s.dng_cache_gb = if gb == DEFAULT_DNG_CACHE_GB { 0 } else { gb };
    ui.horizontal(|ui| {
        ui.weak(format!(
            "In use: {}",
            crate::panels::human_size(status.cache_bytes)
        ));
        if ui
            .button("Clear")
            .on_hover_text("Remove the converted DNGs (made again when needed)")
            .clicked()
        {
            app.developer.clear_cache();
        }
    });
    note(
        ui,
        "Converted DNGs are kept in Shoebox's cache, never in your library; the least \
         recently used go first beyond the limit.",
    );
    if !app.unattached.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Edits without a photo").strong());
        note(
            ui,
            &format!(
                "Shoebox couldn't find the photo of {} edit{} (moved or deleted outside \
                 Shoebox). They're kept; if the photo comes back, its edit is found again.",
                app.unattached.len(),
                if app.unattached.len() == 1 { "" } else { "s" }
            ),
        );
        egui::ScrollArea::vertical()
            .max_height(140.0)
            .id_salt("unattached")
            .show(ui, |ui| {
                for p in &app.unattached {
                    ui.horizontal(|ui| {
                        if ui.small_button("Show").clicked() {
                            crate::app::reveal(p);
                        }
                        ui.weak(p.display().to_string());
                    });
                }
            });
    }
}

fn updates(ui: &mut egui::Ui) {
    ui.label(format!("Shoebox {}", env!("CARGO_PKG_VERSION")));
    ui.add_space(6.0);
    match crate::updater::prefs() {
        Some(mut p) => {
            let before = p;
            ui.checkbox(&mut p.auto_check, "Check for updates automatically");
            ui.add_enabled_ui(p.auto_check, |ui| {
                ui.checkbox(
                    &mut p.auto_install,
                    "Download and install updates automatically",
                );
            });
            if p != before {
                crate::updater::set_prefs(p);
            }
            note(
                ui,
                "Updates come from the project's GitHub releases and install only if \
                 they carry the project's update signature. An update installs when \
                 you quit Shoebox, and never while a file operation is running.",
            );
            ui.add_space(6.0);
            if ui.button("Check Now").clicked() {
                crate::updater::check_now();
            }
        }
        None => note(
            ui,
            "This build doesn't update itself (it wasn't built as a release). \
             Released builds update from the project's GitHub releases.",
        ),
    }
}
