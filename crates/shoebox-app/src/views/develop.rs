//! Develop (`docs/EDITING-SPEC.md`, Develop view): edit a photo (raw files
//! and finished images alike). D opens
//! it on the current photo; G or Escape goes back; the arrows and the
//! filmstrip move between photos.
//!
//! The develop thread prepares the photo (converting a High efficiency NEF,
//! decoding and demosaicing the proxy) while the embedded preview shows.
//! The live pipe then runs on the GPU (eframe's device) into a texture egui
//! draws directly; with no GPU it runs on the CPU. The edit lives in the
//! library worker's catalog (kept a moment after a slider settles) and its
//! `.shoebox/edits` file (written after 2 s idle and on leaving a photo).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect, RichText, pos2};
use shoebox_develop::cpu::{self, Frame, Options, Target};
use shoebox_develop::edit::{
    Demosaic, EditDoc, Highlights, Settings, Source, WbMode, WhiteBalance,
};
use shoebox_develop::export::Space;
use shoebox_develop::gpu::{Output, Pipe};

use crate::app::ShoeboxApp;
use crate::develop::{Event, Overlays, Ready, Request};
use crate::jobs::{LibraryCommand, PhotoId, Size};

/// Keep a changed edit in the catalog once sliders have been still this
/// long, and write it to its file after this much idle.
const KEEP_AFTER: Duration = Duration::from_millis(300);
const SAVE_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Status {
    #[default]
    Preparing,
    Converting,
    NeedsConverter,
    /// Not a raw file Develop can edit yet.
    Unsupported,
    Failed(String),
    Ready,
}

/// How the original is shown: flick between it and the edit (\\), or a
/// split with a divider to drag, original on the left (Y).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compare {
    #[default]
    Edited,
    Original,
    Split,
}

#[derive(Default)]
pub struct State {
    pub photo: Option<PhotoId>,
    /// The photo's root and path within it (where its edit lives).
    pub file: Option<(PathBuf, String)>,
    pub doc: Option<EditDoc>,
    pub ready: Option<Ready>,
    pub status: Status,
    /// What the develop thread was last asked for.
    requested: Option<(PhotoId, shoebox_develop::edit::Raw)>,
    /// In a reference root: shown, never changed.
    pub read_only: bool,
    /// Changed since the catalog was last given it / since the last save.
    changed: Option<Instant>,
    unsaved: Option<Instant>,
    /// The control that made the last history step, and the history's
    /// length after it: more changes to the same setting fold into that
    /// step rather than adding lines.
    last: Option<(&'static str, usize)>,
    pub compare: Compare,
    /// Where the split's divider is, as a fraction of the image's width
    /// (0: not moved yet, the middle).
    split_at: f32,
    /// Where the divider was last drawn (screen x; tests find it there).
    pub divider_x: Option<f32>,
    /// Where the photo was last drawn.
    pub photo_rect: Option<Rect>,
    /// Show clipped highlights and shadows (J).
    pub warnings: bool,
    /// Soft proofing (S): the edit as `proof_space` would hold it, and
    /// with `gamut_warning`, the colours it can't hold in magenta.
    pub proof: bool,
    pub proof_space: Space,
    pub gamut_warning: bool,
    /// Picking a neutral for white balance (W): the next click on the photo.
    pub picking: bool,
    /// Zoomed to 100% (Z): the frame point at the centre of the view, as
    /// fractions of the frame. None: fit.
    pub zoom: Option<egui::Pos2>,
    /// The full-size render's size, once the develop thread has said.
    full_size: Option<(PhotoId, (usize, usize))>,
    /// The sharp full-resolution part shown over the zoomed draft.
    region: Option<RegionShown>,
    /// A region rendered, waiting to become a texture.
    pending_region: Option<PendingRegion>,
    /// The last region asked for: (key, region).
    asked_region: Option<(u64, [usize; 4])>,
    /// Drawing a crop (R): the whole frame shows, the crop over it.
    pub crop: Option<super::crop::CropEdit>,
    /// The render's histogram (R, G, B, luma).
    pub histogram: Option<Box<[[u32; 256]; 4]>>,
    /// The snapshot being renamed, and its name so far.
    renaming: Option<(usize, String)>,
    /// A new photo (no edit yet): its first look (Auto, or the chosen
    /// preset) is applied once it's prepared.
    auto_due: bool,
    /// The copy being edited ("" is the master) and the photo's copies
    /// (id, name), the master first.
    pub(super) copy: String,
    pub(crate) copies: Vec<(String, String)>,
    /// The copy being renamed, and its name so far.
    pub(super) renaming_copy: Option<(String, String)>,
    /// Edit presets (loaded once; kept from photo to photo).
    pub looks: Vec<shoebox_develop::preset::Preset>,
    /// Settings shown instead of the edit while a preset is hovered.
    pub(super) preview: Option<Settings>,
    /// The preset last applied, the settings before it, and its amount (the
    /// Amount slider re-applies it).
    pub(super) applied: Option<super::presets::Applied>,
    /// The "Save preset" window: name, group and parts.
    pub(super) saving_look: Option<super::presets::Saving>,
    /// The "Copy settings" window: the parts ticked.
    pub copying: Option<std::collections::BTreeSet<shoebox_develop::preset::Part>>,
    /// The tone curve channel shown (0 RGB, 1–3 red, green, blue), and
    /// the point being dragged.
    pub(super) curve_channel: usize,
    pub(super) curve_drag: Option<usize>,
    gpu: Option<GpuShared>,
    edited: Option<Shown>,
    original: Option<Shown>,
}

/// A rendered region from the develop thread: photo, key, region, the full
/// render's size, and its pixels.
type PendingRegion = (PhotoId, u64, [usize; 4], (usize, usize), cpu::Rgb);

/// A full-resolution part of the photo on screen.
struct RegionShown {
    id: PhotoId,
    key: u64,
    region: [usize; 4],
    texture: egui::TextureHandle,
}

/// What a zoomed region depends on (settings and the overlays).
fn region_key(settings: &Settings, overlays: Overlays) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    EditDoc::settings_hash(settings).hash(&mut h);
    overlays.hash(&mut h);
    h.finish()
}

impl State {
    /// What's drawn over the edited render.
    pub fn overlays(&self) -> Overlays {
        Overlays {
            clipping: self.warnings,
            proof: self.proof.then_some(self.proof_space),
            gamut: self.gamut_warning,
        }
    }
}

/// Display values as an egui image.
fn color_image(img: &cpu::Rgb) -> egui::ColorImage {
    let pixels: Vec<Color32> = img
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            Color32::from_rgb(q(p[0]), q(p[1]), q(p[2]))
        })
        .collect();
    egui::ColorImage::new([img.width, img.height], pixels)
}

/// The GPU side, shared by the edited and original renders.
struct GpuShared {
    pipe: Pipe,
    source: shoebox_develop::gpu::Source,
    /// The image before the detail stage, when there is one (the
    /// original, for comparing).
    base: Option<shoebox_develop::gpu::Source>,
    /// What was uploaded: the demosaiced image, and the detail stage's
    /// result from it when there was one (to tell when either changes).
    from: (Arc<cpu::Demosaiced>, Option<Arc<cpu::Rgb>>),
    histogram: Option<shoebox_develop::gpu::Histogram>,
}

/// A render on screen and what it was rendered from.
struct Shown {
    key: (PhotoId, String, u64),
    size: [usize; 2],
    /// On the GPU: the texture egui draws.
    gpu: Option<(eframe::wgpu::Texture, egui::TextureId)>,
    /// Without a GPU: a CPU render.
    cpu: Option<egui::TextureHandle>,
}

impl Shown {
    fn texture(&self) -> Option<egui::TextureId> {
        self.gpu
            .as_ref()
            .map(|g| g.1)
            .or_else(|| self.cpu.as_ref().map(egui::TextureHandle::id))
    }
}

impl State {
    /// Something rendered from the raw data is on screen.
    pub fn rendered(&self) -> bool {
        self.edited
            .as_ref()
            .is_some_and(|v| Some(v.key.0) == self.photo)
    }

    /// The original is rendered for comparing.
    pub fn original_rendered(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|v| Some(v.key.0) == self.photo)
    }

    /// The sharp full-resolution region shown while zoomed.
    pub fn zoom_region(&self) -> Option<[usize; 4]> {
        self.region
            .as_ref()
            .filter(|r| Some(r.id) == self.photo)
            .map(|r| r.region)
    }

    /// The split's divider (fraction of the image's width).
    pub fn split(&self) -> f32 {
        if self.split_at <= 0.0 {
            0.5
        } else {
            self.split_at
        }
    }

    fn reset(&mut self) {
        *self = State {
            gpu: self.gpu.take(),
            edited: self.edited.take(),
            original: self.original.take(),
            warnings: self.warnings,
            proof: self.proof,
            proof_space: self.proof_space,
            gamut_warning: self.gamut_warning,
            zoom: self.zoom,
            compare: self.compare,
            split_at: self.split_at,
            looks: std::mem::take(&mut self.looks),
            ..Default::default()
        };
    }
}

/// Leave the photo being edited: hand its edit to the catalog and write
/// it out now.
pub fn leave(app: &mut ShoeboxApp) {
    keep(app, true);
    app.develop.photo = None;
}

/// Give the catalog the edit if it changed (`now`: also write the file).
fn keep(app: &mut ShoeboxApp, now: bool) {
    let st = &mut app.develop;
    let put = match (st.changed.take(), &st.doc, &st.file) {
        (Some(_), Some(doc), Some((root, rel))) => Some(LibraryCommand::PutEdit {
            root: root.clone(),
            rel: rel.clone(),
            copy: st.copy_id().to_owned(),
            json: doc.to_json(),
        }),
        _ => None,
    };
    if put.is_some() {
        st.unsaved.get_or_insert_with(Instant::now);
    }
    let save = now && st.unsaved.take().is_some();
    if let Some(c) = put {
        app.command(c);
    }
    if save {
        app.command(LibraryCommand::SaveEdits);
    }
}

/// The library worker's answer to `LoadEdit`.
pub fn edit_loaded(
    app: &mut ShoeboxApp,
    root: PathBuf,
    rel: String,
    copy: String,
    copies: Vec<(String, String)>,
    json: Option<String>,
) {
    let st = &mut app.develop;
    if st.file.as_ref() != Some(&(root.clone(), rel.clone())) {
        return;
    }
    st.copies = copies;
    if st.doc.is_some() || copy != st.copy_id() {
        return;
    }
    let photo = st
        .photo
        .and_then(|id| app.index(id))
        .map(|i| &app.catalog.photos[i]);
    let fresh = || {
        EditDoc::new(Source {
            path: rel.clone(),
            blake3: String::new(),
            size: None,
            camera: photo.and_then(|p| p.meta.exif.model.clone()),
            lens: photo.and_then(|p| p.meta.exif.lens.clone()),
        })
    };
    let doc = match json.as_deref().map(EditDoc::from_json) {
        Some(Ok(d)) => d,
        Some(Err(e)) => {
            tracing::warn!("the edit of {rel} can't be read ({e}); starting afresh");
            fresh()
        }
        None => fresh(),
    };
    // Only the master gets a first look; a copy starts as made.
    app.develop.auto_due = json.is_none() && copy == shoebox_core::edits::MAIN;
    app.develop.doc = Some(doc);
    app.develop.last = None;
}

impl State {
    /// The copy being edited (`edits::MAIN` for the master).
    pub fn copy_id(&self) -> &str {
        if self.copy.is_empty() {
            shoebox_core::edits::MAIN
        } else {
            &self.copy
        }
    }
}

/// Switch Develop to another copy of the photo: the current one is kept,
/// the other loaded.
fn switch_copy(app: &mut ShoeboxApp, id: &str) {
    if app.develop.copy_id() == id {
        return;
    }
    keep(app, true);
    let st = &mut app.develop;
    let Some((root, rel)) = st.file.clone() else {
        return;
    };
    st.copy = id.to_owned();
    st.doc = None;
    st.applied = None;
    st.auto_due = false;
    app.command(LibraryCommand::LoadEdit {
        root,
        rel,
        copy: id.to_owned(),
    });
}

/// A new virtual copy of the edit as it is now, opened.
fn new_copy(app: &mut ShoeboxApp) {
    let st = &app.develop;
    let Some(doc) = &st.doc else { return };
    let n = (1..)
        .find(|n| !st.copies.iter().any(|(id, _)| *id == format!("copy-{n}")))
        .unwrap_or(1);
    let (id, name) = (format!("copy-{n}"), format!("Copy {n}"));
    let copy = doc.copy_of(&id, &name);
    keep(app, true);
    let st = &mut app.develop;
    st.copies.push((id.clone(), name));
    st.copy = id;
    st.doc = Some(copy);
    st.applied = None;
    touched(st);
    keep(app, true);
}

/// Copies of the photo's edit: the master and virtual copies.
fn copies(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let st = &app.develop;
    if st.doc.is_none() {
        return;
    }
    let read_only = st.read_only;
    let current = st.copy_id().to_owned();
    let list = st.copies.clone();
    let mut action: Option<(&str, String)> = None;
    ui.horizontal(|ui| {
        heading(ui, "COPIES");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui
                .add_enabled(!read_only, egui::Button::new("+").small())
                .on_hover_text("A virtual copy: another edit of the same photo");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, !read_only, "New copy"));
            if r.clicked() {
                action = Some(("new", String::new()));
            }
        });
    });
    // Before the first edit is kept, the master isn't listed yet.
    let shown: Vec<(String, String)> = if list.iter().any(|(id, _)| id == shoebox_core::edits::MAIN)
    {
        list
    } else {
        std::iter::once((shoebox_core::edits::MAIN.to_owned(), "Master".to_owned()))
            .chain(list)
            .collect()
    };
    for (id, name) in &shown {
        if let Some((at, text)) = &mut app.develop.renaming_copy
            && at == id
        {
            let r = ui.text_edit_singleline(text);
            if r.lost_focus() {
                let done = ui.input(|i| !i.key_pressed(egui::Key::Escape));
                action = Some((
                    "renamed",
                    if done {
                        text.trim().to_owned()
                    } else {
                        String::new()
                    },
                ));
            } else if !r.has_focus() {
                r.request_focus();
            }
            continue;
        }
        let r = ui.add(egui::Button::selectable(*id == current, name));
        if r.clicked() {
            action = Some(("open", id.clone()));
        }
        if id != shoebox_core::edits::MAIN && !read_only {
            r.context_menu(|ui| {
                if ui.button("Rename…").clicked() {
                    action = Some(("rename", id.clone()));
                }
                if ui.button("Delete").clicked() {
                    action = Some(("delete", id.clone()));
                }
            });
        }
    }
    match action {
        Some(("new", _)) => new_copy(app),
        Some(("open", id)) => switch_copy(app, &id),
        Some(("rename", id)) => {
            // Renaming changes the copy's document: open it first.
            switch_copy(app, &id);
            let name = shown
                .iter()
                .find(|c| c.0 == id)
                .map(|c| c.1.clone())
                .unwrap_or_default();
            app.develop.renaming_copy = Some((id, name));
        }
        Some(("renamed", name)) => {
            let st = &mut app.develop;
            if let Some((id, _)) = st.renaming_copy.take()
                && !name.is_empty()
            {
                if let Some(c) = st.copies.iter_mut().find(|c| c.0 == id) {
                    c.1 = name.clone();
                }
                if st.copy_id() == id
                    && let Some(doc) = &mut st.doc
                {
                    doc.copy.name = name;
                    touched(st);
                }
            }
        }
        Some(("delete", id)) => {
            if app.develop.copy_id() == id {
                switch_copy(app, shoebox_core::edits::MAIN);
            }
            let st = &mut app.develop;
            st.copies.retain(|c| c.0 != id);
            if let Some((root, rel)) = st.file.clone() {
                app.command(LibraryCommand::RemoveCopy {
                    root,
                    rel,
                    copy: id,
                });
            }
        }
        _ => {}
    }
}

/// A new photo's first look, once its edit and its preparation are both
/// in: the preset chosen for new photos, else Auto (unless turned off).
fn first_look(app: &mut ShoeboxApp) {
    let st = &app.develop;
    if !st.auto_due || st.read_only {
        return;
    }
    let (Some(ready), Some(_)) = (&st.ready, &st.doc) else {
        return;
    };
    if Some(ready.id) != st.photo {
        return;
    }
    app.develop.auto_due = false;
    if let Some(name) = app.settings.new_photo_preset.clone()
        && let Some(p) = app.develop.looks.iter().find(|p| p.name == name).cloned()
    {
        edit(app, "preset", format!("Preset {name}"), |s| {
            *s = p.apply(s, 1.0)
        });
        return;
    }
    if !app.settings.develop_auto_off {
        run_auto(app);
    }
}

/// Auto on the photo in Develop, as a step (raw photos: it's measured as
/// they're prepared).
pub fn run_auto(app: &mut ShoeboxApp) {
    let st = &app.develop;
    let Some(ready) = &st.ready else { return };
    let Some(stats) = ready.auto else { return };
    let iso = st
        .photo
        .and_then(|id| app.photo(id))
        .and_then(|p| p.meta.exif.iso);
    let size = if ready.meta.width > 0 {
        (ready.meta.width, ready.meta.height)
    } else {
        (ready.src.rgb.width, ready.src.rgb.height)
    };
    let lens = ready.src.lens.as_ref().is_some_and(|shot| {
        let on = shoebox_develop::edit::LensCorrection {
            enabled: true,
            ..Default::default()
        };
        shoebox_develop::lens::grid(shot, &on, size.0, size.1).is_some()
    });
    let profile = shoebox_develop::auto::profile_for(&ready.meta.model);
    edit(app, "auto", "Auto".into(), |s| {
        *s = shoebox_develop::auto::settings(s, &stats, iso, lens);
        s.modules.input_colour.profile = profile;
    });
}

/// Start of a frame in Develop (before the panels draw): take the develop
/// thread's news and follow the current photo.
pub fn tick(app: &mut ShoeboxApp) {
    follow(app);
    first_look(app);
}

/// Take the develop thread's news (every frame: previews arrive in any
/// view).
pub fn poll(app: &mut ShoeboxApp) {
    while let Ok(ev) = app.developer.events.try_recv() {
        if let Event::PreviewReady(id, key) = ev {
            app.preview_ready(id, key);
            continue;
        }
        let ev = match ev {
            Event::Status(s) => {
                app.raw_status = Some(s);
                continue;
            }
            Event::Looks(list) => {
                app.develop.looks = list;
                continue;
            }
            Event::Presets(list) => {
                if app.export.ticked.is_empty()
                    && let Some(first) = list.first()
                {
                    app.export.ticked.insert(first.name.clone());
                }
                app.export.presets = list;
                continue;
            }
            Event::ExportClashes { job, files } => {
                app.export.progress = None;
                app.export.clashes = Some((job, files));
                continue;
            }
            Event::ExportProgress { done, total, .. } => {
                app.export.progress = Some((done, total));
                continue;
            }
            Event::ExportDone {
                written,
                errors,
                no_metadata,
                into_library,
            } => {
                if !into_library.is_empty() {
                    app.command(LibraryCommand::AddExports(into_library));
                }
                super::export::done(app, written, errors, no_metadata);
                continue;
            }
            ev => ev,
        };
        let st = &mut app.develop;
        match ev {
            Event::Ready(r) if Some(r.id) == st.photo => {
                st.ready = Some(r);
                st.status = Status::Ready;
            }
            Event::Region {
                id,
                key,
                region,
                full,
                rgb,
            } if Some(id) == st.photo => st.pending_region = Some((id, key, region, full, rgb)),
            Event::Converting(id) if Some(id) == st.photo => st.status = Status::Converting,
            Event::NeedsConverter(id) if Some(id) == st.photo => st.status = Status::NeedsConverter,
            Event::Failed(id, e) if Some(id) == st.photo => st.status = Status::Failed(e),
            _ => {}
        }
    }
}

/// Follow the current photo: load its edit and ask for it to be prepared.
fn follow(app: &mut ShoeboxApp) {
    let Some((id, idx)) = app.current.and_then(|id| app.index(id).map(|i| (id, i))) else {
        return;
    };
    if app.develop.photo != Some(id) {
        keep(app, true);
        app.develop.reset();
        app.develop.photo = Some(id);
        let p = &app.catalog.photos[idx];
        if p.format.is_movie() {
            app.develop.status = Status::Unsupported;
            return;
        }
        let root = app.catalog.roots[p.root].path.clone();
        app.develop.file = Some((root.clone(), p.rel.clone()));
        app.develop.read_only = app.catalog.is_reference(idx);
        app.command(LibraryCommand::LoadEdit {
            root,
            rel: p.rel.clone(),
            copy: shoebox_core::edits::MAIN.to_owned(),
        });
        // The photos either side, converted while this one is edited.
        let near: Vec<PathBuf> = app
            .current_pos()
            .into_iter()
            .flat_map(|pos| [pos + 1, pos.wrapping_sub(1)])
            .filter_map(|n| app.shown.get(n).copied())
            .filter(|&j| app.needs_dng(j))
            .map(|j| app.catalog.photos[j].path.clone())
            .collect();
        app.developer.convert_ahead(near);
    }
    let st = &mut app.develop;
    let Some(doc) = &st.doc else { return };
    let raw = doc.settings().modules.raw.clone();
    let have = st.ready.as_ref().is_some_and(|r| r.raw == raw);
    if !have && st.requested.as_ref() != Some(&(id, raw.clone())) {
        st.requested = Some((id, raw.clone()));
        if st.ready.is_none() {
            st.status = Status::Preparing;
        }
        let p = &app.catalog.photos[idx];
        app.developer.open(Request {
            id,
            path: p.path.clone(),
            format: p.format,
            raw,
            lens: crate::develop::shot(p),
        });
    }
}

/// The orientation and output space of the render.
fn options(app: &ShoeboxApp) -> Options {
    let upright = app.develop.ready.as_ref().is_some_and(|r| r.upright);
    let orientation = if upright {
        1
    } else {
        app.develop
            .photo
            .and_then(|id| app.index(id))
            .map_or(1, |i| app.catalog.photos[i].meta.orientation())
    };
    Options {
        long_edge: None,
        orientation,
        target: match app.target {
            shoebox_decode::Target::DisplayP3 => Target::DisplayP3,
            _ => Target::Srgb,
        },
    }
}

/// The original for comparing: no adjustments, but the edit's crop and
/// rotation (so a split lines up) and raw settings (the same demosaic).
fn original_settings(doc: &EditDoc) -> Settings {
    let s = doc.settings();
    let mut o = Settings {
        geometry: s.geometry.clone(),
        ..Default::default()
    };
    o.modules.raw = s.modules.raw.clone();
    o
}

/// Render the edit (and the original when it's shown) if they changed.
fn render(app: &mut ShoeboxApp, ctx: &egui::Context, gpu: Option<&eframe::egui_wgpu::RenderState>) {
    let o = options(app);
    let st = &mut app.develop;
    let (Some(ready), Some(doc)) = (&st.ready, &st.doc) else {
        return;
    };
    // The settings shown (while cropping, the whole rotated frame).
    // A preset being hovered shows instead of the edit.
    let mut settings = st.preview.clone().unwrap_or_else(|| doc.settings().clone());
    if st.crop.is_some() {
        settings.geometry.crop = [0.0, 0.0, 1.0, 1.0];
    }
    // The GPU pipe, with this photo's demosaiced image uploaded (after the
    // detail stage: noise reduction, sharpening, texture).
    let detailed = ready.src.detailed(&settings);
    let same = |g: &GpuShared| {
        Arc::ptr_eq(&g.from.0, &ready.src)
            && match (&g.from.1, &detailed) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    };
    let mut on_gpu = gpu;
    if let Some(rs) = gpu
        && st.gpu.as_ref().is_none_or(|g| !same(g))
    {
        let pipe = match st.gpu.take() {
            Some(g) => Ok(g.pipe),
            None => Pipe::new(&rs.device, Output::Rgba16Float),
        };
        match pipe {
            Ok(pipe) => {
                let source = pipe.upload(
                    &rs.device,
                    &rs.queue,
                    detailed.as_deref().unwrap_or(&ready.src.rgb),
                );
                let base = detailed
                    .is_some()
                    .then(|| pipe.upload(&rs.device, &rs.queue, &ready.src.rgb));
                let histogram = shoebox_develop::gpu::Histogram::new(&rs.device)
                    .map_err(|e| tracing::warn!("histogram on the GPU: {e}"))
                    .ok();
                st.gpu = Some(GpuShared {
                    pipe,
                    source,
                    base,
                    from: (ready.src.clone(), detailed.clone()),
                    histogram,
                });
            }
            Err(e) => {
                tracing::warn!("develop on the GPU: {e}; using the CPU");
                on_gpu = None;
            }
        }
    }
    let overlays = st.overlays();
    let on_gpu = on_gpu.zip(st.gpu.as_mut());

    // A histogram on its way back from the GPU.
    let mut on_gpu = on_gpu;
    if let Some((rs, g)) = &mut on_gpu
        && let Some(h) = &mut g.histogram
        && let Some(counts) = h.poll(&rs.device, false)
    {
        st.histogram = Some(counts);
    }

    let mut frame = cpu::frame(&ready.meta, &ready.src, &settings, &o);
    overlays.apply(&mut frame);
    let key = (ready.id, EditDoc::settings_hash(&settings), overlays.key());
    if st.edited.as_ref().is_none_or(|v| v.key != key) {
        let h = render_into(
            &mut st.edited,
            key,
            &frame,
            (detailed.as_deref().unwrap_or(&ready.src.rgb), false),
            on_gpu.as_mut(),
            ctx,
            true,
        );
        if h.is_some() {
            st.histogram = h;
        }
    }
    if st.compare != Compare::Edited && st.crop.is_none() {
        let s = original_settings(doc);
        let frame = cpu::frame(&ready.meta, &ready.src, &s, &o);
        let key = (
            ready.id,
            format!("original {}", EditDoc::settings_hash(&s)),
            0,
        );
        if st.original.as_ref().is_none_or(|v| v.key != key) {
            render_into(
                &mut st.original,
                key,
                &frame,
                (&ready.src.rgb, true),
                on_gpu.as_mut(),
                ctx,
                false,
            );
        }
    }
}

/// Render `frame` into `slot` (on the GPU when there's one). With `count`,
/// also count its histogram: on the GPU it arrives on a later frame; on the
/// CPU it's returned.
fn render_into(
    slot: &mut Option<Shown>,
    key: (PhotoId, String, u64),
    frame: &Frame,
    // The source on the CPU, and whether it's the unprocessed one (the
    // GPU's `base`).
    (img, base): (&cpu::Rgb, bool),
    gpu: Option<&mut (&eframe::egui_wgpu::RenderState, &mut GpuShared)>,
    ctx: &egui::Context,
    count: bool,
) -> Option<Box<[[u32; 256]; 4]>> {
    let size = [frame.width, frame.height];
    let mut shown = slot.take().unwrap_or(Shown {
        key: key.clone(),
        size,
        gpu: None,
        cpu: None,
    });
    let mut counts = None;
    match gpu {
        Some((rs, g)) => {
            let device = &rs.device;
            let tex = match shown.gpu.take() {
                Some((t, id)) if shown.size == size => (t, id),
                old => {
                    let t = g
                        .pipe
                        .output_texture(device, size[0] as u32, size[1] as u32);
                    let tv = t.create_view(&Default::default());
                    let mut r = rs.renderer.write();
                    let id = match old {
                        Some((_, id)) => {
                            r.update_egui_texture_from_wgpu_texture(
                                device,
                                &tv,
                                eframe::wgpu::FilterMode::Linear,
                                id,
                            );
                            id
                        }
                        None => {
                            r.register_native_texture(device, &tv, eframe::wgpu::FilterMode::Linear)
                        }
                    };
                    (t, id)
                }
            };
            let tv = tex.0.create_view(&Default::default());
            let mut enc = device.create_command_encoder(&Default::default());
            let source = match (&g.base, base) {
                (Some(b), true) => b,
                _ => &g.source,
            };
            g.pipe.encode(device, &mut enc, source, frame, true, &tv);
            // Count the render, not the clipping or gamut overlay.
            let counted = count
                && !frame.warnings
                && !frame.gamut_warning
                && g.histogram.as_mut().is_some_and(|h| {
                    h.encode(device, &mut enc, &tv, size[0] as u32, size[1] as u32)
                });
            rs.queue.submit([enc.finish()]);
            if counted && let Some(h) = &mut g.histogram {
                h.start_read();
            }
            shown.gpu = Some(tex);
            shown.cpu = None;
        }
        None => {
            // No GPU (or it failed): the same pipe on the CPU.
            let img = cpu::apply(img, frame, true);
            if count && !frame.warnings && !frame.gamut_warning {
                counts = Some(Box::new(cpu::histogram(&img)));
            }
            let pixels: Vec<Color32> = img
                .data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| {
                    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    Color32::from_rgb(q(p[0]), q(p[1]), q(p[2]))
                })
                .collect();
            let image = egui::ColorImage::new(size, pixels);
            match &mut shown.cpu {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    shown.cpu =
                        Some(ctx.load_texture("develop", image, egui::TextureOptions::LINEAR))
                }
            }
            shown.gpu = None;
        }
    }
    shown.key = key;
    shown.size = size;
    *slot = Some(shown);
    counts
}

/// The centre: the photo as edited (the embedded preview until then).
pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui, gpu: Option<&eframe::egui_wgpu::RenderState>) {
    let Some((id, idx)) = app.current.and_then(|id| app.index(id).map(|i| (id, i))) else {
        ui.centered_and_justified(|ui| ui.weak("Select a photo to edit"));
        return;
    };
    let ctx = ui.ctx().clone();
    render(app, &ctx, gpu);
    tick_saving(app, &ctx);

    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(18));
    let st = &app.develop;
    let current = |s: &Option<Shown>| {
        s.as_ref()
            .filter(|v| v.key.0 == id)
            .and_then(|v| v.texture().map(|t| (t, v.size)))
    };
    let edited = current(&st.edited);
    let original = current(&st.original).filter(|o| edited.is_some_and(|e| e.1 == o.1));
    let compare = if st.crop.is_some() {
        Compare::Edited
    } else {
        st.compare
    };
    let (tex, size) = match edited {
        Some((t, s)) => (Some(t), [s[0] as f32, s[1] as f32]),
        None => {
            // The embedded preview while the raw is prepared.
            app.want_open(idx, Size::Full);
            match app.textures.best(id, Size::Full) {
                Some(t) => (Some(t.id()), [t.size()[0] as f32, t.size()[1] as f32]),
                None => (None, [3.0, 2.0]),
            }
        }
    };
    // The photo below a strip for the Edited / Original / Split bar.
    let area = Rect::from_min_max(
        rect.min + egui::vec2(0.0, 36.0),
        rect.max - egui::vec2(0.0, 6.0),
    );
    let fit = (area.width() / size[0]).min(area.height() / size[1]);
    let fit_target = Rect::from_center_size(area.center(), egui::vec2(size[0], size[1]) * fit);
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    // Zoomed to 100%: one full-size pixel per screen pixel, placed by the
    // centre point; the proxy shows enlarged until the sharp part arrives.
    let ppp = ctx.pixels_per_point();
    let zoom = app
        .develop
        .zoom
        .filter(|_| app.develop.crop.is_none() && edited.is_some());
    let full_size = match app.develop.full_size {
        Some((pid, (w, h))) if pid == id => (w as f32, h as f32),
        _ => {
            // Until the develop thread says: the photo's size over the
            // proxy's.
            let photo = &app.catalog.photos[idx];
            let long = photo.meta.width.max(photo.meta.height).unwrap_or(0) as f32;
            let proxy = app
                .develop
                .ready
                .as_ref()
                .map_or(1.0, |r| r.src.rgb.width.max(r.src.rgb.height) as f32);
            let f = (long / proxy).max(1.0);
            (size[0] * f, size[1] * f)
        }
    };
    let target = match zoom {
        Some(c) => {
            let shown = egui::vec2(full_size.0 / ppp, full_size.1 / ppp);
            Rect::from_min_size(
                area.center() - egui::vec2(c.x * shown.x, c.y * shown.y),
                shown,
            )
        }
        None => fit_target,
    };
    let painter = painter.with_clip_rect(area);
    app.develop.photo_rect = Some(target);
    // Double-click toggles fit and 100% at that point; drag or scroll pans.
    if app.develop.crop.is_none() && edited.is_some() && !app.develop.picking {
        if resp.double_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            app.develop.zoom = match app.develop.zoom {
                Some(_) => None,
                None => Some(pos2(
                    ((p.x - target.left()) / target.width()).clamp(0.0, 1.0),
                    ((p.y - target.top()) / target.height()).clamp(0.0, 1.0),
                )),
            };
        }
        if let Some(c) = &mut app.develop.zoom {
            let mut d = egui::Vec2::ZERO;
            if resp.dragged() {
                d += resp.drag_delta();
            }
            if resp.hovered() {
                d += ui.input(|i| i.smooth_scroll_delta);
            }
            if d != egui::Vec2::ZERO {
                c.x = (c.x - d.x / target.width()).clamp(0.0, 1.0);
                c.y = (c.y - d.y / target.height()).clamp(0.0, 1.0);
            }
        }
    }
    if app.develop.picking && app.develop.crop.is_none() && edited.is_some() {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && target.contains(p)
        {
            let at = egui::pos2(
                (p.x - target.left()) / target.width() * size[0],
                (p.y - target.top()) / target.height() * size[1],
            );
            app.develop.picking = false;
            pick_neutral(app, at);
        }
    }
    match (compare, tex, original) {
        (Compare::Original, Some(_), Some((o, _))) => {
            painter.image(o, target, full, Color32::WHITE);
            chip(
                &painter,
                target.left_top() + egui::vec2(10.0, 10.0),
                "Original",
            );
        }
        (Compare::Split, Some(e), Some((o, _))) => {
            // Original left of the divider, the edit right of it.
            let s = app.develop.split();
            let x = target.left() + target.width() * s;
            let left = Rect::from_min_max(target.min, pos2(x, target.bottom()));
            let right = Rect::from_min_max(pos2(x, target.top()), target.max);
            painter.image(
                o,
                left,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(s, 1.0)),
                Color32::WHITE,
            );
            painter.image(
                e,
                right,
                Rect::from_min_max(pos2(s, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            app.develop.divider_x = Some(x);
            let line =
                Rect::from_min_max(pos2(x - 6.0, target.top()), pos2(x + 6.0, target.bottom()));
            let drag = ui
                .interact(
                    line,
                    ui.scope_id().with("split-divider"),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if drag.dragged()
                && let Some(p) = drag.interact_pointer_pos()
            {
                app.develop.split_at = ((p.x - target.left()) / target.width()).clamp(0.02, 0.98);
            }
            // A white line with a dark edge shows on light and dark photos.
            let ends = [pos2(x, target.top()), pos2(x, target.bottom())];
            painter.line_segment(ends, egui::Stroke::new(4.0, Color32::from_black_alpha(140)));
            painter.line_segment(ends, egui::Stroke::new(2.0, Color32::WHITE));
            let knob = pos2(x, target.center().y);
            painter.circle(
                knob,
                12.0,
                Color32::WHITE,
                egui::Stroke::new(1.5, Color32::from_black_alpha(160)),
            );
            painter.text(
                knob,
                egui::Align2::CENTER_CENTER,
                "↔",
                egui::FontId::proportional(13.0),
                Color32::BLACK,
            );
            chip(
                &painter,
                target.left_top() + egui::vec2(10.0, 10.0),
                "Original",
            );
            let w = 64.0;
            chip(
                &painter,
                pos2(target.right() - w - 10.0, target.top() + 10.0),
                "Edited",
            );
        }
        (_, Some(t), _) => {
            painter.image(t, target, full, Color32::WHITE);
        }
        _ => {}
    }
    if app.develop.proof && compare != Compare::Original && tex.is_some() {
        chip(
            &painter,
            target.left_bottom() + egui::vec2(10.0, -30.0),
            &format!("Proof: {}", app.develop.proof_space.label()),
        );
    }
    if zoom.is_some() && compare == Compare::Edited {
        zoomed_region(app, &ctx, &painter, target, area, full_size, ppp);
    }
    // Cropping: the crop over the whole frame, its aspect and Done above.
    if let (Some(_), Some((_, s))) = (&app.develop.crop, edited) {
        let degrees = app
            .develop
            .doc
            .as_ref()
            .map_or(0.0, |d| d.settings().geometry.rotate);
        let (w, h) = (s[0] as f64, s[1] as f64);
        if let Some(c) = &mut app.develop.crop {
            c.ui(ui, target, w, h, degrees);
        }
        let bar = Rect::from_center_size(
            rect.center_top() + egui::vec2(0.0, 18.0),
            egui::vec2(330.0, 26.0),
        );
        let mut done = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            ui.horizontal(|ui| {
                let Some(c) = &mut app.develop.crop else {
                    return;
                };
                let name = super::crop::ASPECTS
                    .iter()
                    .find(|a| a.0 == c.aspect)
                    .map_or("Free", |a| a.1);
                let mut chosen = None;
                egui::ComboBox::from_id_salt("crop-aspect")
                    .selected_text(format!("Aspect: {name}"))
                    .show_ui(ui, |ui| {
                        for (key, label) in super::crop::ASPECTS {
                            if ui.selectable_label(c.aspect == key, label).clicked() {
                                chosen = Some(key);
                            }
                        }
                    });
                if let Some(a) = chosen {
                    c.set_aspect(a, w, h, degrees);
                }
                if ui
                    .button("Done")
                    .on_hover_text("Apply the crop (Enter)")
                    .clicked()
                {
                    done = Some(true);
                }
                if ui
                    .button("Cancel")
                    .on_hover_text("Leave it as it was (Escape)")
                    .clicked()
                {
                    done = Some(false);
                }
            });
        });
        if let Some(apply) = done {
            finish_crop(app, apply);
        }
    } else if edited.is_some() {
        // Edited / Original / Split, over the top of the photo.
        let bar = Rect::from_center_size(
            rect.center_top() + egui::vec2(0.0, 18.0),
            egui::vec2(230.0, 26.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            ui.horizontal(|ui| {
                let c = &mut app.develop.compare;
                ui.selectable_value(c, Compare::Edited, "Edited")
                    .on_hover_text("The photo as edited");
                ui.selectable_value(c, Compare::Original, "Original")
                    .on_hover_text("The photo before any edits (\\)");
                ui.selectable_value(c, Compare::Split, "Split")
                    .on_hover_text("Original on the left, edited on the right; drag the line (Y)");
            });
        });
    }
    let note = match &app.develop.status {
        Status::Ready => None,
        Status::Preparing => Some("Preparing…".to_owned()),
        Status::Converting => Some("Converting to DNG (High efficiency NEF)…".to_owned()),
        Status::NeedsConverter => Some(
            "This NEF uses Nikon's High efficiency compression, which Shoebox can't decode yet. \
             Install Adobe's free DNG Converter and Shoebox will convert it when you edit. \
             Your original stays untouched."
                .to_owned(),
        ),
        Status::Unsupported => Some("Movies can't be edited".to_owned()),
        Status::Failed(e) => Some(format!("Can't develop this photo: {e}")),
    };
    if let Some(n) = note {
        let galley = painter.layout(
            n,
            egui::FontId::proportional(13.0),
            Color32::WHITE,
            (rect.width() - 80.0).clamp(200.0, 560.0),
        );
        let at = rect.center_bottom() - egui::vec2(galley.size().x / 2.0, galley.size().y + 24.0);
        painter.rect_filled(
            Rect::from_min_size(at, galley.size()).expand(8.0),
            6.0,
            Color32::from_black_alpha(180),
        );
        painter.galley(at, galley, Color32::WHITE);
    }
    if app.develop.status == Status::NeedsConverter {
        let link = Rect::from_center_size(
            rect.center_top() + egui::vec2(0.0, 40.0),
            egui::vec2(260.0, 28.0),
        );
        if ui
            .put(link, egui::Button::new("Download Adobe DNG Converter"))
            .clicked()
        {
            ctx.open_url(egui::OpenUrl::new_tab(
                "https://helpx.adobe.com/camera-raw/using/adobe-dng-converter.html",
            ));
        }
    }
    if let Some(pos) = app.current_pos() {
        super::photo_menu(app, &resp, pos);
    }
}

/// A quarter turn (⌘] clockwise, ⌘[ anticlockwise).
pub fn turn(app: &mut ShoeboxApp, clockwise: bool) {
    let label = if clockwise {
        "Rotate right"
    } else {
        "Rotate left"
    };
    edit(app, "turn", label.into(), |s| s.geometry.turn(clockwise));
    if let (Some(c), Some(doc)) = (&mut app.develop.crop, &app.develop.doc) {
        let g = &doc.settings().geometry;
        *c = super::crop::CropEdit::new(g.crop, &g.aspect);
    }
}

/// The eyedropper: the white balance that makes the source neutral around
/// `at`, a point in the render (output pixels).
fn pick_neutral(app: &mut ShoeboxApp, at: egui::Pos2) {
    let o = options(app);
    let st = &app.develop;
    let (Some(ready), Some(doc)) = (&st.ready, &st.doc) else {
        return;
    };
    let f = cpu::frame(&ready.meta, &ready.src, doc.settings(), &o);
    let (sx, sy) = f.source_at(at.x, at.y);
    let img = &ready.src.rgb;
    let (cx, cy) = (sx.round() as i64, sy.round() as i64);
    let mut sum = [0.0f64; 3];
    let mut n = 0.0;
    for y in cy - 2..=cy + 2 {
        for x in cx - 2..=cx + 2 {
            if x >= 0 && y >= 0 && (x as usize) < img.width && (y as usize) < img.height {
                let p = img.px(x as usize, y as usize);
                for c in 0..3 {
                    sum[c] += f64::from(p[c]);
                }
                n += 1.0;
            }
        }
    }
    if n == 0.0 {
        return;
    }
    let patch = sum.map(|v| v / n);
    let (temp, tint) = shoebox_develop::wb::neutral_at(&ready.meta, &ready.src, patch);
    // Within the sliders' ranges, so the result can be adjusted from there.
    let (temp, tint) = (
        temp.round().clamp(2000.0, 12000.0),
        tint.round().clamp(-150.0, 150.0),
    );
    edit(
        app,
        "wb",
        format!("White balance picked ({temp:.0} K, {tint:+.0})"),
        |s| {
            s.modules.white_balance = WhiteBalance {
                mode: WbMode::Custom,
                temp: Some(temp),
                tint: Some(tint),
            };
        },
    );
}

/// Start drawing a crop (R), from the edit's crop.
pub fn start_crop(app: &mut ShoeboxApp) {
    let st = &mut app.develop;
    if st.read_only || st.crop.is_some() {
        return;
    }
    if let Some(doc) = &st.doc {
        let g = &doc.settings().geometry;
        st.crop = Some(super::crop::CropEdit::new(g.crop, &g.aspect));
    }
}

/// Finish drawing a crop: apply it as one step, or leave it as it was.
pub fn finish_crop(app: &mut ShoeboxApp, apply: bool) {
    let Some(c) = app.develop.crop.take() else {
        return;
    };
    if apply {
        edit(app, "crop", "Crop".into(), |s| {
            s.geometry.crop = c.rect;
            s.geometry.aspect = c.aspect.clone();
        });
    }
}

/// Zoomed: show the sharp full-resolution part over the draft, asking for
/// a new one when the view moves beyond it or the edit changes.
fn zoomed_region(
    app: &mut ShoeboxApp,
    ctx: &egui::Context,
    painter: &egui::Painter,
    target: Rect,
    area: Rect,
    full_size: (f32, f32),
    ppp: f32,
) {
    let o = options(app);
    let st = &mut app.develop;
    let (true, Some(doc), Some(id)) = (st.ready.is_some(), &st.doc, st.photo) else {
        return;
    };
    let settings = doc.settings().clone();
    if let Some((pid, key, region, full, rgb)) = st.pending_region.take() {
        let image = color_image(&rgb);
        st.full_size = Some((pid, full));
        match &mut st.region {
            Some(r) => {
                r.texture.set(image, egui::TextureOptions::NEAREST);
                (r.id, r.key, r.region) = (pid, key, region);
            }
            None => {
                st.region = Some(RegionShown {
                    id: pid,
                    key,
                    region,
                    texture: ctx.load_texture("develop-zoom", image, egui::TextureOptions::NEAREST),
                })
            }
        }
    }
    let key = region_key(&settings, st.overlays());
    let to_screen = |r: [usize; 4]| {
        Rect::from_min_size(
            target.min + egui::vec2(r[0] as f32 / ppp, r[1] as f32 / ppp),
            egui::vec2(r[2] as f32 / ppp, r[3] as f32 / ppp),
        )
    };
    let current = st.region.as_ref().filter(|r| r.id == id && r.key == key);
    if let Some(r) = current {
        painter.image(
            r.texture.id(),
            to_screen(r.region),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    // What's visible, in full-size pixels; ask for it with a margin.
    let vis = target.intersect(area);
    if vis.width() <= 0.0 || vis.height() <= 0.0 {
        return;
    }
    let px = |v: f32| (v * ppp).max(0.0) as usize;
    let (x0, y0) = (px(vis.min.x - target.min.x), px(vis.min.y - target.min.y));
    let (x1, y1) = (px(vis.max.x - target.min.x), px(vis.max.y - target.min.y));
    let covered = current.is_some_and(|r| {
        let [rx, ry, rw, rh] = r.region;
        rx <= x0
            && ry <= y0
            && rx + rw >= x1.min(full_size.0 as usize)
            && ry + rh >= y1.min(full_size.1 as usize)
    });
    if covered {
        return;
    }
    let (mx, my) = ((x1 - x0) / 3, (y1 - y0) / 3);
    let (fw, fh) = (full_size.0 as usize, full_size.1 as usize);
    let wanted = [
        x0.saturating_sub(mx),
        y0.saturating_sub(my),
        (x1 + mx).min(fw).saturating_sub(x0.saturating_sub(mx)),
        (y1 + my).min(fh).saturating_sub(y0.saturating_sub(my)),
    ];
    if st.asked_region == Some((key, wanted)) {
        return;
    }
    st.asked_region = Some((key, wanted));
    let overlays = st.overlays();
    let Some(photo) = app.index(id).map(|i| &app.catalog.photos[i]) else {
        return;
    };
    let request = Request {
        id,
        path: photo.path.clone(),
        format: photo.format,
        raw: settings.modules.raw.clone(),
        lens: crate::develop::shot(photo),
    };
    app.developer.region(crate::develop::RegionJob {
        request,
        settings,
        options: o,
        overlays,
        region: wanted,
        key,
    });
}

/// A small label over the photo.
pub(crate) fn chip(painter: &egui::Painter, at: egui::Pos2, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        Color32::WHITE,
    );
    painter.rect_filled(
        Rect::from_min_size(at, galley.size()).expand(5.0),
        4.0,
        Color32::from_black_alpha(160),
    );
    painter.galley(at, galley, Color32::WHITE);
}

/// Keep the edit after sliders settle, write it after a pause.
fn tick_saving(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let held = ctx.input(|i| i.pointer.any_down());
    let st = &app.develop;
    if let Some(t) = st.changed
        && !held
    {
        let left = KEEP_AFTER.saturating_sub(t.elapsed());
        if left.is_zero() {
            keep(app, false);
        } else {
            ctx.request_repaint_after(left);
        }
    }
    if let Some(t) = app.develop.unsaved {
        let left = SAVE_AFTER.saturating_sub(t.elapsed());
        if left.is_zero() {
            keep(app, true);
        } else {
            ctx.request_repaint_after(left);
        }
    }
}

/// Change the edit as one step, or, when the last step was the same
/// setting's (`key`), fold into it: dragging a slider, or nudging it again,
/// is one history line. Presets, Auto and pasting are always steps of their
/// own.
pub(super) fn edit(
    app: &mut ShoeboxApp,
    key: &'static str,
    label: String,
    f: impl FnOnce(&mut Settings),
) {
    let st = &mut app.develop;
    if st.read_only {
        return;
    }
    let Some(doc) = &mut st.doc else { return };
    let before = doc.history().len();
    // A preset's Amount folds into the preset it scales.
    let fold = st.last.is_some_and(|(k, n)| {
        n == before
            && (k == key && !matches!(key, "preset" | "auto" | "paste")
                || (k == "preset" && key == "preset-amount"))
    });
    let changed = if fold {
        doc.amend(&label, f)
    } else {
        doc.change(&label, f)
    };
    if changed {
        // A step that folded back to where it began is gone: nothing to
        // fold into.
        let after = doc.history().len();
        st.last = (after >= before).then_some((key, after));
        st.changed = Some(Instant::now());
        st.unsaved.get_or_insert_with(Instant::now);
        // Editing shows the edit (a split stays, to watch the change).
        if st.compare == Compare::Original {
            st.compare = Compare::Edited;
        }
    }
}

/// Undo or redo in the edit's history (⌘Z / ⌘⇧Z in Develop).
pub fn undo(app: &mut ShoeboxApp, redo: bool) {
    let st = &mut app.develop;
    let Some(doc) = &mut st.doc else { return };
    if st.read_only {
        return;
    }
    let moved = if redo { doc.redo() } else { doc.undo() };
    st.last = None;
    if moved {
        st.changed = Some(Instant::now());
        st.unsaved.get_or_insert_with(Instant::now);
    }
}

/// A slider row; double-click resets it. Returns the new value when moved.
pub(super) fn slider(
    ui: &mut egui::Ui,
    name: &str,
    value: f64,
    range: std::ops::RangeInclusive<f64>,
    default: f64,
    decimals: usize,
) -> Option<f64> {
    let mut v = value;
    ui.label(name);
    let width = ui.spacing().slider_width;
    let r = ui.add(egui::Slider::new(&mut v, range.clone()).fixed_decimals(decimals));
    ui.end_row();
    if r.double_clicked() {
        return Some(default);
    }
    // Shift-drag: a tenth of the speed, from where the value was.
    if r.dragged() && ui.input(|i| i.modifiers.shift_only()) {
        v = fine(value, r.drag_delta().x, range, width, decimals);
    }
    // The slider rounds a value it shows to its decimals (Auto's 0.35 is
    // 0.35000000000000003): that's no edit unless someone is using it.
    let held = r.dragged()
        || r.is_pointer_button_down_on()
        || (r.contains_pointer() && ui.input(|i| i.pointer.any_down()));
    let tiny = (v - value).abs() < 0.5 * 10f64.powi(-(decimals as i32));
    if tiny && !held && !r.has_focus() {
        return None;
    }
    ((r.changed() || r.dragged()) && v != value).then_some(v)
}

/// `value` moved by a drag of `dx` points at a tenth of the slider's speed,
/// kept in range, to one more decimal than shown.
fn fine(
    value: f64,
    dx: f32,
    range: std::ops::RangeInclusive<f64>,
    width: f32,
    decimals: usize,
) -> f64 {
    let span = range.end() - range.start();
    let v = value + f64::from(dx) * span / f64::from(width.max(1.0)) / 10.0;
    let q = 10f64.powi(decimals as i32 + 1);
    ((v * q).round() / q).clamp(*range.start(), *range.end())
}

/// The camera settings under the histogram: ISO, shutter, aperture, focal
/// length and lens, as far as the file says.
pub fn exif_summary(x: &shoebox_core::exif::ExifInfo) -> Option<String> {
    let shutter = x.exposure.as_ref().map(|e| {
        if e.ends_with('s') {
            e.clone()
        } else {
            format!("{e} s")
        }
    });
    let parts: Vec<String> = [
        x.iso.map(|i| format!("ISO {i}")),
        shutter,
        x.fnumber.map(|f| format!("f/{}", trim(f))),
        x.focal.map(|f| format!("{} mm", trim(f))),
        x.lens.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// 2.8 → "2.8", 8.0 → "8", 50.0 → "50".
fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map_or(s.clone(), str::to_owned)
}

/// A photo in a reference folder can't be edited: offer to import its
/// folder (Import shows what it would copy first).
fn offer_import(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.weak("In a reference folder: shown as edited, but not editable.");
    let dir = app
        .develop
        .photo
        .and_then(|id| app.photo(id))
        .and_then(|p| p.path.parent().map(Path::to_path_buf));
    let into = app.settings.writable_roots().first().cloned();
    if let (Some(dir), Some(into)) = (dir, into)
        && ui
            .button("Import this folder…")
            .on_hover_text(
                "Copy this photo's folder into your library, where it can be edited. \
                 You'll see what would be copied first.",
            )
            .clicked()
    {
        app.set_view(crate::app::View::Import);
        app.set_import_source(dir);
        app.import.root.get_or_insert(into);
        app.plan_import();
    }
    ui.add_space(6.0);
}

/// Soft proofing: on and off (S), the space, and the gamut warning.
fn soft_proof(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let st = &mut app.develop;
    ui.horizontal(|ui| {
        ui.toggle_value(&mut st.proof, "Soft proof")
            .on_hover_text("Show the edit as an export in this colour space would hold it (S)");
        ui.add_enabled_ui(st.proof, |ui| {
            egui::ComboBox::from_id_salt("proof-space")
                .selected_text(st.proof_space.label())
                .show_ui(ui, |ui| {
                    for s in [
                        Space::Srgb,
                        Space::DisplayP3,
                        Space::AdobeRgb,
                        Space::ProPhoto,
                    ] {
                        ui.selectable_value(&mut st.proof_space, s, s.label());
                    }
                });
            ui.toggle_value(&mut st.gamut_warning, "Gamut")
                .on_hover_text("Show in magenta the colours this space can't hold");
        });
    });
}

/// The right panel in Develop: the modules.
pub fn controls(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if let Some(h) = &app.develop.histogram {
        ui.add_space(4.0);
        crate::panels::histogram(ui, h);
        let total: u32 = h[3].iter().sum::<u32>().max(1);
        let pct = |n: u32| 100.0 * n as f32 / total as f32;
        let hi = (0..3).map(|c| h[c][255]).max().unwrap_or(0);
        let lo = h[3][0];
        ui.horizontal(|ui| {
            let mut w = app.develop.warnings;
            if ui
                .toggle_value(&mut w, "Clipping")
                .on_hover_text("Show clipped highlights (red) and shadows (blue) (J)")
                .changed()
            {
                app.develop.warnings = w;
            }
            ui.weak(format!(
                "highlights {:.1}% · shadows {:.1}%",
                pct(hi),
                pct(lo)
            ));
        });
    }
    soft_proof(app, ui);
    if let Some(text) = app
        .develop
        .photo
        .and_then(|id| app.index(id))
        .and_then(|i| exif_summary(&app.catalog.photos[i].meta.exif))
    {
        ui.weak(text);
    }
    if app.develop.histogram.is_some() {
        ui.separator();
    }
    let st = &app.develop;
    let (Some(_), Some(ready)) = (&st.doc, &st.ready) else {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| ui.weak("Preparing…"));
        return;
    };
    let can_auto = ready.auto.is_some() && !st.read_only;
    // Camera match is offered when there's a profile for this camera.
    let model = ready.meta.model.clone();
    let matched = !model.is_empty() && shoebox_develop::profile::has(&model);
    let profile = st
        .doc
        .as_ref()
        .map(|d| d.settings().modules.input_colour.profile);
    let mut pick = None;
    let mut auto = false;
    ui.horizontal(|ui| {
        auto = ui
            .add_enabled(can_auto, egui::Button::new("Auto"))
            .on_hover_text(
                "Exposure, noise reduction, sharpening, dehaze and lens corrections from the photo itself",
            )
            .clicked();
        if matched || profile == Some(shoebox_develop::edit::Profile::CameraMatch) {
            use shoebox_develop::edit::Profile;
            let name = |p: Option<Profile>| match p {
                Some(Profile::CameraMatch) => "Camera match",
                _ => "Matrix",
            };
            egui::ComboBox::from_id_salt("input-profile")
                .selected_text(name(profile))
                .show_ui(ui, |ui| {
                    for p in [Profile::Matrix, Profile::CameraMatch] {
                        if ui.selectable_label(profile == Some(p), name(Some(p))).clicked() {
                            pick = Some(p);
                        }
                    }
                })
                .response
                .on_hover_text("Matrix: the camera's colour, neutral. Camera match: a look fitted to this camera's own JPEGs");
        }
    });
    if auto {
        run_auto(app);
        return;
    }
    if let Some(p) = pick
        && Some(p) != profile
    {
        let label = match p {
            shoebox_develop::edit::Profile::CameraMatch => "Profile Camera match",
            shoebox_develop::edit::Profile::Matrix => "Profile Matrix",
        };
        edit(app, "profile", label.into(), |s| {
            s.modules.input_colour.profile = p;
        });
    }
    let st = &app.develop;
    let (Some(doc), Some(ready)) = (&st.doc, &st.ready) else {
        return;
    };
    let s = doc.settings().clone();
    let defaults = Settings::default();
    let read_only = st.read_only;
    let meta = ready.meta.clone();
    let display_referred = ready.src.referred == cpu::Referred::Display;
    let src_size = (ready.src.rgb.width, ready.src.rgb.height);
    let orientation = options(app).orientation;
    if read_only {
        offer_import(app, ui);
    }
    ui.add_enabled_ui(!read_only, |ui| {
        // Sliders fill the panel beside their names and values.
        ui.spacing_mut().slider_width = (ui.available_width() - 170.0).max(80.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // White balance.
                let wb = &s.modules.white_balance;
                let reset = super::look::section(
                    ui,
                    "White balance",
                    *wb != WhiteBalance::default(),
                    true,
                    |ui| {
                        let (temp, tint) = if display_referred {
                            let (t, n, _) = shoebox_develop::wb::relative(wb);
                            (t, n)
                        } else {
                            let r = shoebox_develop::wb::resolve(&meta, wb);
                            (r.temp, r.tint)
                        };
                        let modes = [
                            (WbMode::AsShot, "As shot"),
                            (WbMode::Daylight, "Daylight"),
                            (WbMode::Cloudy, "Cloudy"),
                            (WbMode::Shade, "Shade"),
                            (WbMode::Tungsten, "Tungsten"),
                            (WbMode::Fluorescent, "Fluorescent"),
                            (WbMode::Flash, "Flash"),
                            (WbMode::Custom, "Custom"),
                        ];
                        let name = modes.iter().find(|m| m.0 == wb.mode).map_or("", |m| m.1);
                        egui::ComboBox::from_id_salt("wb-mode")
                            .selected_text(name)
                            .show_ui(ui, |ui| {
                                for (m, label) in modes {
                                    if ui.selectable_label(wb.mode == m, label).clicked()
                                        && wb.mode != m
                                    {
                                        edit(app, "wb", format!("White balance {label}"), |s| {
                                            let w = &mut s.modules.white_balance;
                                            w.mode = m;
                                            if m == WbMode::Custom {
                                                w.temp = Some(temp.round());
                                                w.tint = Some(tint.round());
                                            } else {
                                                w.temp = None;
                                                w.tint = None;
                                            }
                                        });
                                    }
                                }
                            });
                        let mut picking = app.develop.picking;
                        if ui
                            .toggle_value(&mut picking, "Pick neutral")
                            .on_hover_text("Click something that should be grey or white (W)")
                            .changed()
                        {
                            app.develop.picking = picking;
                        }
                        egui::Grid::new("wb-grid").num_columns(2).show(ui, |ui| {
                            if let Some(t) =
                                slider(ui, "Temperature", temp.round(), 2000.0..=12000.0, temp, 0)
                            {
                                let tint = tint.round();
                                edit(app, "temp", format!("Temperature {t:.0} K"), |s| {
                                    s.modules.white_balance = WhiteBalance {
                                        mode: WbMode::Custom,
                                        temp: Some(t),
                                        tint: Some(tint),
                                    };
                                });
                            }
                            if let Some(n) =
                                slider(ui, "Tint", tint.round(), -150.0..=150.0, tint, 0)
                            {
                                let temp = temp.round();
                                edit(app, "tint", format!("Tint {n:+.0}"), |s| {
                                    s.modules.white_balance = WhiteBalance {
                                        mode: WbMode::Custom,
                                        temp: Some(temp),
                                        tint: Some(n),
                                    };
                                });
                            }
                        });
                    },
                );
                if reset {
                    edit(app, "wb", "Reset white balance".into(), |s| {
                        s.modules.white_balance = WhiteBalance::default();
                    });
                }

                // Exposure.
                let e = &s.modules.exposure;
                let reset = super::look::section(
                    ui,
                    "Exposure",
                    *e != defaults.modules.exposure,
                    true,
                    |ui| {
                        egui::Grid::new("ev-grid").num_columns(2).show(ui, |ui| {
                            if let Some(v) = slider(ui, "Exposure", e.ev, -5.0..=5.0, 0.0, 2) {
                                edit(app, "ev", format!("Exposure {v:+.2}"), |s| {
                                    s.modules.exposure.ev = v
                                });
                            }
                        });
                    },
                );
                if reset {
                    edit(app, "ev", "Reset exposure".into(), |s| {
                        s.modules.exposure = Default::default();
                    });
                }

                // Tone (the display transform).
                let t = &s.modules.display_transform;
                let hs = &s.modules.highlights_shadows;
                let toned = *t != defaults.modules.display_transform
                    || *hs != defaults.modules.highlights_shadows;
                let reset = super::look::section(ui, "Tone", toned, true, |ui| {
                    egui::Grid::new("tone-grid").num_columns(2).show(ui, |ui| {
                        if let Some(v) = slider(ui, "Contrast", t.contrast, -100.0..=100.0, 0.0, 0)
                        {
                            edit(app, "contrast", format!("Contrast {v:+.0}"), |s| {
                                s.modules.display_transform.contrast = v;
                            });
                        }
                        if let Some(v) =
                            slider(ui, "Highlights", hs.highlights, -100.0..=100.0, 0.0, 0)
                        {
                            edit(app, "highlights", format!("Highlights {v:+.0}"), |s| {
                                s.modules.highlights_shadows.highlights = v;
                            });
                        }
                        if let Some(v) = slider(ui, "Shadows", hs.shadows, -100.0..=100.0, 0.0, 0) {
                            edit(app, "shadows", format!("Shadows {v:+.0}"), |s| {
                                s.modules.highlights_shadows.shadows = v;
                            });
                        }
                        if let Some(v) = slider(ui, "Whites", t.white_ev, -3.0..=3.0, 0.0, 2) {
                            edit(app, "whites", format!("Whites {v:+.2}"), |s| {
                                s.modules.display_transform.white_ev = v;
                            });
                        }
                        if let Some(v) = slider(ui, "Blacks", t.black_ev, -3.0..=3.0, 0.0, 2) {
                            edit(app, "blacks", format!("Blacks {v:+.2}"), |s| {
                                s.modules.display_transform.black_ev = v;
                            });
                        }
                    });
                    let mut hue = t.preserve_hue;
                    if ui.checkbox(&mut hue, "Preserve hue").changed() {
                        edit(
                            app,
                            "hue",
                            format!("Preserve hue {}", if hue { "on" } else { "off" }),
                            |s| {
                                s.modules.display_transform.preserve_hue = hue;
                            },
                        );
                    }
                });
                if reset {
                    edit(app, "tone", "Reset tone".into(), |s| {
                        s.modules.display_transform = Default::default();
                        s.modules.highlights_shadows = Default::default();
                    });
                }

                super::look::modules(app, ui, &s);

                // Geometry: crop, and rotation (with the largest crop that
                // stays inside).
                let g = &s.geometry;
                let reset = super::look::section(
                    ui,
                    "Crop & rotate",
                    *g != defaults.geometry,
                    true,
                    |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .button("Rotate left")
                                .on_hover_text("A quarter turn anticlockwise (⌘[)")
                                .clicked()
                            {
                                turn(app, false);
                            }
                            if ui
                                .button("Rotate right")
                                .on_hover_text("A quarter turn clockwise (⌘])")
                                .clicked()
                            {
                                turn(app, true);
                            }
                            if ui
                                .button("Flip")
                                .on_hover_text("Mirror left to right")
                                .clicked()
                            {
                                edit(app, "flip", "Flip".into(), |s| s.geometry.mirror());
                            }
                        });
                        egui::Grid::new("geo-grid").num_columns(2).show(ui, |ui| {
                            if let Some(v) = slider(ui, "Angle", g.rotate, -45.0..=45.0, 0.0, 1) {
                                let (w, h) = cpu::upright(src_size.0, src_size.1, orientation);
                                let crop = cpu::max_crop(w as f64, h as f64, v);
                                edit(app, "rotate", format!("Rotate {v:+.1}°"), |s| {
                                    s.geometry.rotate = v;
                                    s.geometry.crop = crop;
                                });
                                if let Some(c) = &mut app.develop.crop {
                                    c.rect = crop;
                                }
                            }
                        });
                        let cropping = app.develop.crop.is_some();
                        if ui
                            .button(if cropping { "Done cropping" } else { "Crop…" })
                            .on_hover_text("Draw the crop on the photo (R)")
                            .clicked()
                        {
                            if cropping {
                                finish_crop(app, true);
                            } else {
                                start_crop(app);
                            }
                        }
                    },
                );
                if reset {
                    edit(app, "rotate", "Reset rotation".into(), |s| {
                        s.geometry = Default::default()
                    });
                }

                if display_referred {
                    // A finished image has no raw stages.
                    return;
                }
                // Raw: how the sensor data becomes RGB (prepared again on change).
                let r = &s.modules.raw;
                let reset =
                    super::look::section(ui, "Raw", *r != defaults.modules.raw, false, |ui| {
                        let mut hl = r.highlights;
                        let mut dm = r.demosaic;
                        egui::Grid::new("raw-grid").num_columns(2).show(ui, |ui| {
                            ui.label("Highlights");
                            egui::ComboBox::from_id_salt("raw-highlights")
                                .selected_text(match hl {
                                    Highlights::InpaintOpposed => "Reconstruct",
                                    Highlights::Clip => "Clip",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut hl,
                                        Highlights::InpaintOpposed,
                                        "Reconstruct",
                                    );
                                    ui.selectable_value(&mut hl, Highlights::Clip, "Clip");
                                });
                            ui.end_row();
                            ui.label("Demosaic");
                            egui::ComboBox::from_id_salt("raw-demosaic")
                                .selected_text(match dm {
                                    Demosaic::Rcd => "RCD",
                                    Demosaic::Bilinear => "Bilinear",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut dm, Demosaic::Rcd, "RCD");
                                    ui.selectable_value(&mut dm, Demosaic::Bilinear, "Bilinear");
                                });
                            ui.end_row();
                        });
                        if hl != r.highlights {
                            edit(app, "raw", format!("Highlights {hl:?}"), |s| {
                                s.modules.raw.highlights = hl
                            });
                        }
                        if dm != r.demosaic {
                            edit(app, "raw", format!("Demosaic {dm:?}"), |s| {
                                s.modules.raw.demosaic = dm
                            });
                        }
                        let mut hot = r.hot_pixels;
                        if ui.checkbox(&mut hot, "Fix hot and dead pixels").changed() {
                            let label = format!("Hot pixels {}", if hot { "on" } else { "off" });
                            edit(app, "raw", label, |s| s.modules.raw.hot_pixels = hot);
                        }
                        // Levels re-prepare the photo (the develop thread takes the
                        // newest, so a drag doesn't queue them up).
                        egui::Grid::new("raw-levels").num_columns(2).show(ui, |ui| {
                            if let Some(v) =
                                slider(ui, "Black point", r.black, -0.05..=0.05, 0.0, 3)
                            {
                                edit(app, "raw-black", format!("Raw black {v:+.3}"), |s| {
                                    s.modules.raw.black = v;
                                });
                            }
                            if let Some(v) = slider(ui, "White point", r.white, 0.5..=1.2, 1.0, 2) {
                                edit(app, "raw-white", format!("Raw white {v:.2}"), |s| {
                                    s.modules.raw.white = v;
                                });
                            }
                        });
                    });
                if reset {
                    edit(app, "raw", "Reset raw".into(), |s| {
                        s.modules.raw = Default::default()
                    });
                }
            });
    });
}

/// The left panel in Develop: the edit's history.
pub fn history(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.set_min_width(200.0);
    ui.add_space(6.0);
    copies(app, ui);
    ui.add_space(10.0);
    snapshots(app, ui);
    ui.add_space(10.0);
    super::presets::panel(app, ui);
    ui.add_space(10.0);
    heading(ui, "HISTORY");
    let st = &app.develop;
    let Some(doc) = &st.doc else { return };
    let at = doc.at();
    let steps: Vec<String> = doc.history().iter().map(|s| s.label.clone()).collect();
    let mut jump = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if ui.selectable_label(at == 0, "Original").clicked() {
                jump = Some(0);
            }
            for (i, label) in steps.iter().enumerate() {
                let r = ui.selectable_label(at == i + 1, label);
                if r.clicked() {
                    jump = Some(i + 1);
                }
            }
        });
    if let Some(n) = jump {
        let st = &mut app.develop;
        if let Some(doc) = &mut st.doc
            && !st.read_only
            && doc.jump(n)
        {
            st.last = None;
            touched(st);
        }
    }
}

pub(super) fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(11.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
}

/// The edit changed outside a slider: show it and save it.
pub(super) fn touched(st: &mut State) {
    st.changed = Some(Instant::now());
    st.unsaved.get_or_insert_with(Instant::now);
}

/// What a click in the snapshot list does.
enum SnapshotAction {
    New,
    Restore(usize),
    Rename(usize),
    Renamed(usize, String),
    Remove(usize),
}

/// Named settings to come back to: + names the edit as it is now; a click
/// goes back to one (a history step, so it undoes); right-click renames or
/// deletes.
fn snapshots(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let st = &mut app.develop;
    let Some(doc) = &st.doc else { return };
    let read_only = st.read_only;
    let names: Vec<String> = doc.snapshots.iter().map(|s| s.name.clone()).collect();
    let mut action = None;
    ui.horizontal(|ui| {
        heading(ui, "SNAPSHOTS");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui
                .add_enabled(!read_only, egui::Button::new("+").small())
                .on_hover_text("New snapshot of the edit as it is now");
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, !read_only, "New snapshot")
            });
            if r.clicked() {
                action = Some(SnapshotAction::New);
            }
        });
    });
    if names.is_empty() {
        ui.weak("None");
    }
    for (i, name) in names.iter().enumerate() {
        if let Some((at, text)) = &mut st.renaming
            && *at == i
        {
            let r = ui.text_edit_singleline(text);
            if r.lost_focus() {
                let done = ui.input(|i| !i.key_pressed(egui::Key::Escape));
                action = Some(if done {
                    SnapshotAction::Renamed(i, text.trim().to_owned())
                } else {
                    SnapshotAction::Renamed(i, name.clone())
                });
            } else if !r.has_focus() {
                r.request_focus();
            }
            continue;
        }
        let r = ui
            .add_enabled(!read_only, egui::Button::selectable(false, name))
            .on_hover_text("Go back to this snapshot");
        if r.clicked() {
            action = Some(SnapshotAction::Restore(i));
        }
        r.context_menu(|ui| {
            if ui.button("Rename…").clicked() {
                action = Some(SnapshotAction::Rename(i));
            }
            if ui.button("Delete").clicked() {
                action = Some(SnapshotAction::Remove(i));
            }
        });
    }
    let Some(action) = action else { return };
    let Some(doc) = &mut st.doc else { return };
    match action {
        SnapshotAction::New => {
            let n = doc.snapshots.len() + 1;
            doc.snapshot(&format!("Snapshot {n}"));
            st.renaming = Some((n - 1, format!("Snapshot {n}")));
        }
        SnapshotAction::Restore(i) => {
            if doc.restore(i) && st.compare == Compare::Original {
                st.compare = Compare::Edited;
            }
        }
        SnapshotAction::Rename(i) => st.renaming = Some((i, names[i].clone())),
        SnapshotAction::Renamed(i, name) => {
            if !name.is_empty() {
                doc.rename_snapshot(i, &name);
            }
            st.renaming = None;
        }
        SnapshotAction::Remove(i) => {
            doc.remove_snapshot(i);
            st.renaming = None;
        }
    }
    touched(st);
}

#[cfg(test)]
mod tests {
    use super::exif_summary;
    use shoebox_core::exif::ExifInfo;

    #[test]
    fn shift_drag_is_a_tenth_as_fast() {
        // A 200-point slider over -5..5: 20 points is 1 EV, shifted 0.1.
        assert_eq!(super::fine(0.5, 20.0, -5.0..=5.0, 200.0, 2), 0.6);
        assert_eq!(super::fine(0.5, -20.0, -5.0..=5.0, 200.0, 2), 0.4);
        assert_eq!(super::fine(4.95, 40.0, -5.0..=5.0, 200.0, 2), 5.0);
        // Kelvin shown whole moves in tenths.
        assert_eq!(super::fine(5000.0, 1.0, 2000.0..=12000.0, 200.0, 0), 5005.0);
    }

    #[test]
    fn camera_settings_line() {
        let x = ExifInfo {
            iso: Some(800),
            exposure: Some("1/250 s".into()),
            fnumber: Some(2.8),
            focal: Some(40.0),
            lens: Some("NIKKOR Z 40mm f/2".into()),
            ..ExifInfo::default()
        };
        assert_eq!(
            exif_summary(&x).as_deref(),
            Some("ISO 800 · 1/250 s · f/2.8 · 40 mm · NIKKOR Z 40mm f/2")
        );
        let y = ExifInfo {
            exposure: Some("1/60".into()),
            fnumber: Some(8.0),
            ..ExifInfo::default()
        };
        assert_eq!(exif_summary(&y).as_deref(), Some("1/60 s · f/8"));
        assert_eq!(exif_summary(&ExifInfo::default()), None);
    }
}
