//! Develop (`docs/EDITING-SPEC.md`, Develop view): edit a raw photo. D opens
//! it on the current photo; G or Escape goes back; the arrows and the
//! filmstrip move between photos.
//!
//! The develop thread prepares the photo (converting a High efficiency NEF,
//! decoding and demosaicing the proxy) while the embedded preview shows.
//! The live pipe then runs on the GPU (eframe's device) into a texture egui
//! draws directly; with no GPU it runs on the CPU. The edit lives in the
//! library worker's catalog (kept a moment after a slider settles) and its
//! `.shoebox/edits` file (written after 2 s idle and on leaving a photo).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Rect, RichText, pos2};
use shoebox_develop::cpu::{self, Frame, Options, Target};
use shoebox_develop::edit::{
    Demosaic, EditDoc, Highlights, Settings, Source, WbMode, WhiteBalance,
};
use shoebox_develop::gpu::{Output, Pipe};

use crate::app::ShoeboxApp;
use crate::develop::{Event, Ready, Request};
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
    /// The slider being dragged: one drag is one history step.
    dragging: Option<&'static str>,
    /// Show the photo as shot (\\).
    pub before: bool,
    /// Show clipped highlights and shadows (J).
    pub warnings: bool,
    /// The render's histogram (R, G, B, luma).
    pub histogram: Option<Box<[[u32; 256]; 4]>>,
    view: Option<Rendered>,
}

/// What's on screen: the texture and what it was rendered from.
struct Rendered {
    key: (PhotoId, String, bool, bool),
    size: [usize; 2],
    gpu: Option<GpuView>,
    cpu: Option<egui::TextureHandle>,
}

struct GpuView {
    pipe: Pipe,
    source: shoebox_develop::gpu::Source,
    /// The demosaiced image uploaded (to tell when it changes).
    from: Arc<cpu::Demosaiced>,
    out: Option<(eframe::wgpu::Texture, egui::TextureId, [usize; 2])>,
    histogram: Option<shoebox_develop::gpu::Histogram>,
}

impl State {
    /// Something rendered from the raw data is on screen.
    pub fn rendered(&self) -> bool {
        self.view
            .as_ref()
            .is_some_and(|v| Some(v.key.0) == self.photo)
    }

    fn reset(&mut self) {
        let (view, warnings) = (self.view.take(), self.warnings);
        *self = State {
            view,
            warnings,
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
pub fn edit_loaded(app: &mut ShoeboxApp, root: PathBuf, rel: String, json: Option<String>) {
    let st = &mut app.develop;
    if st.file.as_ref() != Some(&(root.clone(), rel.clone())) || st.doc.is_some() {
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
    app.develop.doc = Some(doc);
}

/// Start of a frame in Develop (before the panels draw): take the develop
/// thread's news and follow the current photo.
pub fn tick(app: &mut ShoeboxApp) {
    poll(app);
    follow(app);
}

/// Take the develop thread's news.
fn poll(app: &mut ShoeboxApp) {
    while let Ok(ev) = app.developer.events.try_recv() {
        let st = &mut app.develop;
        match ev {
            Event::Ready(r) if Some(r.id) == st.photo => {
                st.ready = Some(r);
                st.status = Status::Ready;
            }
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
        if p.format != shoebox_decode::Format::Nef {
            app.develop.status = Status::Unsupported;
            return;
        }
        let root = app.catalog.roots[p.root].path.clone();
        app.develop.file = Some((root.clone(), p.rel.clone()));
        app.develop.read_only = app.catalog.is_reference(idx);
        app.command(LibraryCommand::LoadEdit {
            root,
            rel: p.rel.clone(),
        });
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
        let path = app.catalog.photos[idx].path.clone();
        app.developer.open(Request { id, path, raw });
    }
}

/// The orientation and output space of the render.
fn options(app: &ShoeboxApp) -> Options {
    let orientation = app
        .develop
        .photo
        .and_then(|id| app.index(id))
        .map_or(1, |i| app.catalog.photos[i].meta.orientation());
    Options {
        long_edge: None,
        orientation,
        target: match app.target {
            shoebox_decode::Target::DisplayP3 => Target::DisplayP3,
            _ => Target::Srgb,
        },
    }
}

/// Render the edit if it changed since the last render.
fn render(app: &mut ShoeboxApp, ctx: &egui::Context, gpu: Option<&eframe::egui_wgpu::RenderState>) {
    let o = options(app);
    let st = &mut app.develop;
    let (Some(ready), Some(doc)) = (&st.ready, &st.doc) else {
        return;
    };
    let settings = if st.before {
        Settings::default()
    } else {
        doc.settings().clone()
    };
    let key = (ready.id, doc.render_hash(), st.before, st.warnings);
    if st.view.as_ref().is_some_and(|v| v.key == key) {
        // A histogram on its way back from the GPU.
        if let (Some(rs), Some(h)) = (
            gpu,
            st.view
                .as_mut()
                .and_then(|v| v.gpu.as_mut())
                .and_then(|g| g.histogram.as_mut()),
        ) && let Some(counts) = h.poll(&rs.device, false)
        {
            st.histogram = Some(counts);
        }
        return;
    }
    let mut frame = cpu::frame(&ready.meta, &ready.src, &settings, &o);
    frame.warnings = st.warnings;
    let size = [frame.width, frame.height];
    let mut view = st.view.take().unwrap_or(Rendered {
        key: key.clone(),
        size,
        gpu: None,
        cpu: None,
    });
    view.key = key;
    view.size = size;
    let on_gpu = gpu.and_then(|rs| render_gpu(&mut view, rs, ready, &frame));
    if on_gpu.is_none() {
        // No GPU (or it failed): the same pipe on the CPU.
        let img = cpu::apply(&ready.src.rgb, &frame, true);
        if !st.warnings {
            st.histogram = Some(Box::new(cpu::histogram(&img)));
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
        match &mut view.cpu {
            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
            None => {
                view.cpu = Some(ctx.load_texture("develop", image, egui::TextureOptions::LINEAR))
            }
        }
        view.gpu = None;
    }
    st.view = Some(view);
}

/// Run the pipe on eframe's device into a texture egui shows.
fn render_gpu(
    view: &mut Rendered,
    rs: &eframe::egui_wgpu::RenderState,
    ready: &Ready,
    frame: &Frame,
) -> Option<()> {
    let device = &rs.device;
    if view
        .gpu
        .as_ref()
        .is_none_or(|g| !Arc::ptr_eq(&g.from, &ready.src))
    {
        let pipe = match view.gpu.take() {
            Some(g) => g.pipe,
            None => match Pipe::new(device, Output::Rgba16Float) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("develop on the GPU: {e}; using the CPU");
                    return None;
                }
            },
        };
        let source = pipe.upload(device, &rs.queue, &ready.src.rgb);
        let old = view.gpu.take().and_then(|g| g.out);
        let histogram = shoebox_develop::gpu::Histogram::new(device)
            .map_err(|e| tracing::warn!("histogram on the GPU: {e}"))
            .ok();
        view.gpu = Some(GpuView {
            pipe,
            source,
            from: ready.src.clone(),
            out: old,
            histogram,
        });
    }
    let g = view.gpu.as_mut()?;
    let size = [frame.width, frame.height];
    let out = match g.out.take() {
        Some((t, id, s)) if s == size => (t, id),
        old => {
            let t = g
                .pipe
                .output_texture(device, size[0] as u32, size[1] as u32);
            let tv = t.create_view(&Default::default());
            let mut r = rs.renderer.write();
            let id = match old {
                Some((_, id, _)) => {
                    r.update_egui_texture_from_wgpu_texture(
                        device,
                        &tv,
                        eframe::wgpu::FilterMode::Linear,
                        id,
                    );
                    id
                }
                None => r.register_native_texture(device, &tv, eframe::wgpu::FilterMode::Linear),
            };
            (t, id)
        }
    };
    let tv = out.0.create_view(&Default::default());
    let mut enc = device.create_command_encoder(&Default::default());
    g.pipe.encode(device, &mut enc, &g.source, frame, true, &tv);
    // Count the render (not the clipping overlay).
    let counted = !frame.warnings
        && g.histogram
            .as_mut()
            .is_some_and(|h| h.encode(device, &mut enc, &tv, size[0] as u32, size[1] as u32));
    rs.queue.submit([enc.finish()]);
    if counted && let Some(h) = &mut g.histogram {
        h.start_read();
    }
    g.out = Some((out.0, out.1, size));
    Some(())
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
    let resp = ui.allocate_rect(rect, egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(18));
    let st = &app.develop;
    let shown: Option<(egui::TextureId, [usize; 2])> =
        st.view.as_ref().filter(|v| v.key.0 == id).and_then(|v| {
            v.gpu
                .as_ref()
                .and_then(|g| g.out.as_ref().map(|(_, t, s)| (*t, *s)))
                .or_else(|| v.cpu.as_ref().map(|t| (t.id(), v.size)))
        });
    let (tex, size) = match shown {
        Some(s) => (Some(s.0), [s.1[0] as f32, s.1[1] as f32]),
        None => {
            // The embedded preview while the raw is prepared.
            app.want_open(idx, Size::Full);
            match app.textures.best(id, Size::Full) {
                Some(t) => (Some(t.id()), [t.size()[0] as f32, t.size()[1] as f32]),
                None => (None, [3.0, 2.0]),
            }
        }
    };
    let fit = (rect.width() / size[0]).min(rect.height() / size[1]);
    let target = Rect::from_center_size(rect.center(), egui::vec2(size[0], size[1]) * fit);
    if let Some(t) = tex {
        painter.image(
            t,
            target,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    let note = match &app.develop.status {
        Status::Ready if app.develop.before => Some("Before".to_owned()),
        Status::Ready => None,
        Status::Preparing => Some("Preparing raw…".to_owned()),
        Status::Converting => Some("Converting to DNG (High efficiency NEF)…".to_owned()),
        Status::NeedsConverter => Some(
            "This NEF uses Nikon's High efficiency compression, which Shoebox can't decode yet. \
             Install Adobe's free DNG Converter and Shoebox will convert it when you edit. \
             Your original stays untouched."
                .to_owned(),
        ),
        Status::Unsupported => Some("Develop edits NEF raw files for now".to_owned()),
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

/// Keep the edit after sliders settle, write it after a pause.
fn tick_saving(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let held = ctx.input(|i| i.pointer.any_down());
    if !held {
        app.develop.dragging = None;
    }
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

/// Change the edit as one step (or, while `key`'s slider is dragged, fold
/// into the step the drag began).
fn edit(
    app: &mut ShoeboxApp,
    key: &'static str,
    label: String,
    dragging: bool,
    f: impl FnOnce(&mut Settings),
) {
    let st = &mut app.develop;
    if st.read_only {
        return;
    }
    let Some(doc) = &mut st.doc else { return };
    let changed = if st.dragging == Some(key) {
        doc.amend(&label, f)
    } else {
        doc.change(&label, f)
    };
    if dragging {
        st.dragging = Some(key);
    }
    if changed {
        st.changed = Some(Instant::now());
        st.unsaved.get_or_insert_with(Instant::now);
        st.before = false;
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
    if moved {
        st.changed = Some(Instant::now());
        st.unsaved.get_or_insert_with(Instant::now);
    }
}

/// A slider row; double-click resets it. Returns the new value when moved,
/// and whether it's being dragged.
fn slider(
    ui: &mut egui::Ui,
    name: &str,
    value: f64,
    range: std::ops::RangeInclusive<f64>,
    default: f64,
    decimals: usize,
) -> Option<(f64, bool)> {
    let mut v = value;
    ui.label(name);
    let r = ui.add(egui::Slider::new(&mut v, range).fixed_decimals(decimals));
    ui.end_row();
    if r.double_clicked() {
        return Some((default, false));
    }
    (r.changed() && v != value).then_some((v, r.dragged()))
}

fn module(ui: &mut egui::Ui, title: &str, reset: bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            clicked = ui
                .add_enabled(reset, egui::Button::new("Reset").small())
                .on_hover_text("Back to the default")
                .clicked();
        });
    });
    clicked
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
        ui.separator();
    }
    let st = &app.develop;
    let (Some(doc), Some(ready)) = (&st.doc, &st.ready) else {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| ui.weak("Preparing…"));
        return;
    };
    let s = doc.settings().clone();
    let defaults = Settings::default();
    let read_only = st.read_only;
    let meta = ready.meta.clone();
    let src_size = (ready.src.rgb.width, ready.src.rgb.height);
    let orientation = options(app).orientation;
    ui.add_enabled_ui(!read_only, |ui| {
        // Sliders fill the panel beside their names and values.
        ui.spacing_mut().slider_width = (ui.available_width() - 170.0).max(80.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if read_only {
                    ui.weak("In a reference folder: shown as edited, but not editable.");
                    ui.add_space(6.0);
                }

                // White balance.
                let wb = &s.modules.white_balance;
                if module(ui, "White balance", *wb != WhiteBalance::default()) {
                    edit(app, "wb", "Reset white balance".into(), false, |s| {
                        s.modules.white_balance = WhiteBalance::default();
                    });
                }
                let resolved = shoebox_develop::wb::resolve(&meta, wb);
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
                            if ui.selectable_label(wb.mode == m, label).clicked() && wb.mode != m {
                                edit(app, "wb", format!("White balance {label}"), false, |s| {
                                    let w = &mut s.modules.white_balance;
                                    w.mode = m;
                                    if m == WbMode::Custom {
                                        w.temp = Some(resolved.temp.round());
                                        w.tint = Some(resolved.tint.round());
                                    } else {
                                        w.temp = None;
                                        w.tint = None;
                                    }
                                });
                            }
                        }
                    });
                egui::Grid::new("wb-grid").num_columns(2).show(ui, |ui| {
                    if let Some((t, d)) = slider(
                        ui,
                        "Temperature",
                        resolved.temp.round(),
                        2000.0..=12000.0,
                        resolved.temp,
                        0,
                    ) {
                        let tint = resolved.tint.round();
                        edit(app, "temp", format!("Temperature {t:.0} K"), d, |s| {
                            s.modules.white_balance = WhiteBalance {
                                mode: WbMode::Custom,
                                temp: Some(t),
                                tint: Some(tint),
                            };
                        });
                    }
                    if let Some((n, d)) = slider(
                        ui,
                        "Tint",
                        resolved.tint.round(),
                        -150.0..=150.0,
                        resolved.tint,
                        0,
                    ) {
                        let temp = resolved.temp.round();
                        edit(app, "tint", format!("Tint {n:+.0}"), d, |s| {
                            s.modules.white_balance = WhiteBalance {
                                mode: WbMode::Custom,
                                temp: Some(temp),
                                tint: Some(n),
                            };
                        });
                    }
                });
                ui.separator();

                // Exposure.
                let e = &s.modules.exposure;
                if module(ui, "Exposure", *e != defaults.modules.exposure) {
                    edit(app, "ev", "Reset exposure".into(), false, |s| {
                        s.modules.exposure = Default::default();
                    });
                }
                egui::Grid::new("ev-grid").num_columns(2).show(ui, |ui| {
                    if let Some((v, d)) = slider(ui, "Exposure", e.ev, -5.0..=5.0, 0.0, 2) {
                        edit(app, "ev", format!("Exposure {v:+.2}"), d, |s| {
                            s.modules.exposure.ev = v
                        });
                    }
                });
                ui.separator();

                // Tone (the display transform).
                let t = &s.modules.display_transform;
                if module(ui, "Tone", *t != defaults.modules.display_transform) {
                    edit(app, "tone", "Reset tone".into(), false, |s| {
                        s.modules.display_transform = Default::default();
                    });
                }
                egui::Grid::new("tone-grid").num_columns(2).show(ui, |ui| {
                    if let Some((v, d)) = slider(ui, "Contrast", t.contrast, -100.0..=100.0, 0.0, 0)
                    {
                        edit(app, "contrast", format!("Contrast {v:+.0}"), d, |s| {
                            s.modules.display_transform.contrast = v;
                        });
                    }
                    if let Some((v, d)) = slider(ui, "Whites", t.white_ev, -3.0..=3.0, 0.0, 2) {
                        edit(app, "whites", format!("Whites {v:+.2}"), d, |s| {
                            s.modules.display_transform.white_ev = v;
                        });
                    }
                    if let Some((v, d)) = slider(ui, "Blacks", t.black_ev, -3.0..=3.0, 0.0, 2) {
                        edit(app, "blacks", format!("Blacks {v:+.2}"), d, |s| {
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
                        false,
                        |s| {
                            s.modules.display_transform.preserve_hue = hue;
                        },
                    );
                }
                ui.separator();

                // Geometry: rotation, with the largest crop that stays inside.
                let g = &s.geometry;
                if module(ui, "Rotate", *g != defaults.geometry) {
                    edit(app, "rotate", "Reset rotation".into(), false, |s| {
                        s.geometry = Default::default()
                    });
                }
                egui::Grid::new("geo-grid").num_columns(2).show(ui, |ui| {
                    if let Some((v, d)) = slider(ui, "Angle", g.rotate, -45.0..=45.0, 0.0, 1) {
                        let (w, h) = cpu::upright(src_size.0, src_size.1, orientation);
                        let crop = cpu::max_crop(w as f64, h as f64, v);
                        edit(app, "rotate", format!("Rotate {v:+.1}°"), d, |s| {
                            s.geometry.rotate = v;
                            s.geometry.crop = crop;
                        });
                    }
                });
                ui.separator();

                // Raw: how the sensor data becomes RGB (prepared again on change).
                let r = &s.modules.raw;
                if module(ui, "Raw", *r != defaults.modules.raw) {
                    edit(app, "raw", "Reset raw".into(), false, |s| {
                        s.modules.raw = Default::default()
                    });
                }
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
                            ui.selectable_value(&mut hl, Highlights::InpaintOpposed, "Reconstruct");
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
                    edit(app, "raw", format!("Highlights {hl:?}"), false, |s| {
                        s.modules.raw.highlights = hl
                    });
                }
                if dm != r.demosaic {
                    edit(app, "raw", format!("Demosaic {dm:?}"), false, |s| {
                        s.modules.raw.demosaic = dm
                    });
                }
            });
    });
}

/// The left panel in Develop: the edit's history.
pub fn history(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.set_min_width(200.0);
    ui.add_space(6.0);
    ui.label(
        RichText::new("HISTORY")
            .size(11.0)
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
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
            st.changed = Some(Instant::now());
            st.unsaved.get_or_insert_with(Instant::now);
        }
    }
}
