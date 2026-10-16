//! Application state and the frame loop.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use shoebox_core::catalog::{Catalog, Photo};
use shoebox_core::settings::Settings;
use shoebox_core::thumbs::ThumbCache;

use shoebox_core::marks::{Label, Marks, Pick};
use shoebox_core::query;

use crate::jobs::{
    Job, Key, Library, LibraryCommand, LibraryEvent, LibraryPaths, Loader, PhotoId, Priority, Size,
    photo_id,
};
use crate::textures::Textures;
use crate::{panels, views};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Grid,
    Masonry,
    Loupe,
    Compare,
    Survey,
    Timeline,
}

impl View {
    pub const ALL: [(View, &'static str, egui::Key); 6] = [
        (View::Grid, "Grid", egui::Key::G),
        (View::Masonry, "Masonry", egui::Key::M),
        (View::Loupe, "Loupe", egui::Key::E),
        (View::Compare, "Compare", egui::Key::C),
        (View::Survey, "Survey", egui::Key::N),
        (View::Timeline, "Timeline", egui::Key::T),
    ];

    pub fn has_filmstrip(self) -> bool {
        matches!(self, View::Loupe | View::Compare | View::Survey)
    }
}

/// Where the app keeps its state; tests point these at temp folders.
pub struct AppPaths {
    pub settings: PathBuf,
    pub cache_db: PathBuf,
    pub journal: PathBuf,
    pub thumbs: PathBuf,
}

impl AppPaths {
    pub fn default_locations() -> anyhow::Result<Self> {
        let data = shoebox_core::paths::default_data_dir()?;
        let cache = shoebox_core::paths::default_cache_dir()?;
        Ok(Self {
            settings: shoebox_core::paths::settings_file(&data),
            cache_db: shoebox_core::paths::cache_db(&data),
            journal: shoebox_core::paths::journal_db(&data),
            thumbs: cache.join("thumbs"),
        })
    }
}

/// Loupe zoom: fit to the window, or a scale where 1.0 = one image pixel
/// per screen pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    Fit,
    Scale(f32),
}

pub struct LoupeState {
    pub zoom: Zoom,
    pub pan: egui::Vec2,
}

/// A folder in the left panel, with the recursive count of images.
#[derive(Debug, Default)]
pub struct FolderNode {
    pub path: PathBuf,
    pub name: String,
    pub count: usize,
    pub children: std::collections::BTreeMap<String, FolderNode>,
}

pub struct ShoeboxApp {
    pub(crate) loader: Loader,
    library: Library,
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) settings: Settings,
    /// `ids[i]` is the stable id of `catalog.photos[i]`.
    pub(crate) ids: Vec<PhotoId>,
    index_of: HashMap<PhotoId, usize>,
    pub(crate) tree: Vec<FolderNode>,
    /// Folder filter from the left panel (None = whole library).
    pub(crate) folder: Option<PathBuf>,
    /// Catalog indices shown in the centre, in display order.
    pub(crate) shown: Vec<usize>,
    /// Bumped whenever `shown` changes (layout caches key on it).
    pub(crate) shown_gen: u64,
    pub(crate) selection: BTreeSet<PhotoId>,
    pub(crate) current: Option<PhotoId>,
    anchor: Option<PhotoId>,
    pub(crate) view: View,
    pub(crate) thumb_size: f32,
    /// Columns in the grid as last drawn (for Up/Down).
    pub(crate) grid_cols: usize,
    pub(crate) left_open: bool,
    pub(crate) right_open: bool,
    pub(crate) bars_open: bool,
    fullscreen: bool,
    pub(crate) loupe: LoupeState,
    pub(crate) textures: Textures,
    pub(crate) histograms: HashMap<PhotoId, Box<[[u32; 256]; 4]>>,
    pub(crate) siblings_of: Option<(PhotoId, Vec<usize>)>,
    pub(crate) masonry: views::masonry::Layout,
    /// Loads wanted this frame, sent to the loader at the end of the frame.
    wanted: Vec<(Key, Priority, Job)>,
    pub(crate) scan_progress: Option<(usize, usize)>,
    pub(crate) scanning: bool,
    pub(crate) error: Option<String>,
    /// When set, the centre view scrolls so this photo is visible.
    pub(crate) scroll_to_current: bool,
    pub(crate) filter: query::Filter,
    pub(crate) sort: query::Sort,
    pub(crate) sort_desc: bool,
    /// Stacks shown expanded, by their primary's id.
    pub(crate) expanded: HashSet<PhotoId>,
    pub(crate) can_undo: bool,
    pub(crate) can_redo: bool,
    /// Auto-advance after marking (also on while Caps Lock is down, macOS).
    pub(crate) auto_advance: bool,
    pub(crate) keyword_input: String,
    pub(crate) focus_keywords: bool,
    pub(crate) focus_search: bool,
    /// Name being typed when saving a smart filter.
    pub(crate) new_smart_filter: Option<String>,
    /// Keywords in use (for autocomplete), cameras and lenses (for filters).
    pub(crate) all_keywords: Vec<(String, usize)>,
    pub(crate) cameras: Vec<String>,
    pub(crate) lenses: Vec<String>,
    /// Tags the window Display P3 on macOS (None in tests / elsewhere).
    display: Option<crate::display::DisplayTagger>,
    /// Colour space images are rendered in for this display.
    target: shoebox_decode::Target,
}

impl ShoeboxApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: AppPaths) -> Self {
        let ctx = &cc.egui_ctx;
        let display = crate::display::DisplayTagger::new(cc);
        let target = crate::display::DisplayTagger::target(display.as_ref());
        tracing::info!("rendering images in {target:?}");
        let threads =
            std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(1).max(2));
        let (c1, c2) = (ctx.clone(), ctx.clone());
        let loader = Loader::new(threads, ThumbCache::new(paths.thumbs), move || {
            c1.request_repaint()
        });
        let library = Library::new(
            LibraryPaths {
                settings: paths.settings,
                cache_db: paths.cache_db,
                journal: paths.journal,
            },
            move || c2.request_repaint(),
        );
        library.send(LibraryCommand::Open);
        Self {
            loader,
            library,
            catalog: Arc::new(Catalog::default()),
            settings: Settings::default(),
            ids: Vec::new(),
            index_of: HashMap::new(),
            tree: Vec::new(),
            folder: None,
            shown: Vec::new(),
            shown_gen: 0,
            selection: BTreeSet::new(),
            current: None,
            anchor: None,
            view: View::Grid,
            thumb_size: 180.0,
            grid_cols: 1,
            left_open: true,
            right_open: true,
            bars_open: true,
            fullscreen: false,
            loupe: LoupeState {
                zoom: Zoom::Fit,
                pan: egui::Vec2::ZERO,
            },
            // 768 MB: ~3,000 small thumbnails plus a few full-size images.
            textures: Textures::new(768 << 20),
            histograms: HashMap::new(),
            siblings_of: None,
            masonry: views::masonry::Layout::default(),
            wanted: Vec::new(),
            scan_progress: None,
            scanning: true,
            error: None,
            scroll_to_current: false,
            display,
            target,
            filter: query::Filter::default(),
            sort: query::Sort::CaptureTime,
            sort_desc: false,
            expanded: HashSet::new(),
            can_undo: false,
            can_redo: false,
            auto_advance: false,
            keyword_input: String::new(),
            focus_keywords: false,
            focus_search: false,
            new_smart_filter: None,
            all_keywords: Vec::new(),
            cameras: Vec::new(),
            lenses: Vec::new(),
        }
    }

    /// True when no scan or image load is outstanding (used by tests).
    pub fn idle(&self) -> bool {
        !self.scanning && self.loader.busy() == 0
    }

    pub fn photo_count(&self) -> usize {
        self.catalog.photos.len()
    }

    pub fn current_is_nef(&self) -> bool {
        self.current
            .and_then(|c| self.photo(c))
            .is_some_and(|p| p.format == shoebox_decode::Format::Nef)
    }

    pub fn loupe_zoomed(&self) -> bool {
        self.loupe.zoom != Zoom::Fit
    }

    /// Marks of the photo with this file name (tests and diagnostics).
    pub fn marks_of(&self, file_name: &str) -> Option<Marks> {
        self.catalog
            .photos
            .iter()
            .find(|p| p.path.file_name().is_some_and(|n| n == file_name))
            .map(|p| p.marks.clone())
    }

    /// Replace the filter and rebuild the visible items.
    pub fn set_filter(&mut self, f: query::Filter) {
        self.filter = f;
        self.refilter();
    }

    pub fn view_name(&self) -> View {
        self.view
    }

    pub fn shown_count(&self) -> usize {
        self.shown.len()
    }

    pub fn set_view(&mut self, v: View) {
        if v == View::Loupe && self.current.is_none() {
            self.current = self.shown.first().map(|&i| self.ids[i]);
        }
        self.view = v;
        self.loupe = LoupeState {
            zoom: Zoom::Fit,
            pan: egui::Vec2::ZERO,
        };
        self.scroll_to_current = true;
    }

    pub fn command(&self, c: LibraryCommand) {
        self.library.send(c);
    }

    // --- catalog -----------------------------------------------------------

    fn apply_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.library.events.try_recv() {
            match ev {
                LibraryEvent::Catalog {
                    catalog,
                    settings,
                    complete,
                } => {
                    self.settings = settings;
                    self.set_catalog(catalog);
                    if complete {
                        self.scanning = false;
                        self.scan_progress = None;
                    }
                }
                LibraryEvent::Progress { done, total } => {
                    self.scanning = true;
                    self.scan_progress = Some((done, total));
                }
                LibraryEvent::History { can_undo, can_redo } => {
                    self.can_undo = can_undo;
                    self.can_redo = can_redo;
                }
                LibraryEvent::Error(e) => {
                    self.scanning = false;
                    self.error = Some(e);
                }
            }
        }
        for loaded in self.loader.drain() {
            match loaded.result {
                Ok(img) => {
                    if let Some(h) = loaded.histogram {
                        self.histograms.insert(loaded.key.photo, h);
                    }
                    self.textures.insert(ctx, loaded.key, &img);
                }
                Err(e) => {
                    tracing::warn!("{e}");
                    self.textures.failed.insert(loaded.key, e);
                }
            }
        }
    }

    fn set_catalog(&mut self, catalog: Arc<Catalog>) {
        self.ids = catalog.photos.iter().map(|p| photo_id(&p.path)).collect();
        self.index_of = self
            .ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();
        self.tree = build_tree(&catalog);
        self.catalog = catalog;
        let keep: HashSet<PhotoId> = self.ids.iter().copied().collect();
        self.textures.retain_photos(&keep);
        self.textures.failed.clear();
        self.selection.retain(|id| keep.contains(id));
        if self.current.is_some_and(|c| !keep.contains(&c)) {
            self.current = None;
        }
        self.siblings_of = None;
        self.all_keywords = query::keywords(&self.catalog);
        self.cameras = query::distinct(&self.catalog, |x| x.model.as_ref());
        self.lenses = query::distinct(&self.catalog, |x| x.lens.as_ref());
        self.refilter();
    }

    pub(crate) fn set_folder(&mut self, folder: Option<PathBuf>) {
        self.folder = folder;
        self.refilter();
        self.scroll_to_current = true;
    }

    /// Rebuild the visible items: one per stack passing the filter (its
    /// primary), followed by its other members when expanded.
    pub(crate) fn refilter(&mut self) {
        let mut f = self.filter.clone();
        f.folder = self.folder.clone();
        let stacks = query::run(&self.catalog, &f, self.sort, self.sort_desc);
        self.shown.clear();
        for s in stacks {
            let st = &self.catalog.stacks[s];
            self.shown.push(st.primary);
            if self.expanded.contains(&self.ids[st.primary]) {
                self.shown.extend(st.all().skip(1));
            }
        }
        self.shown_gen += 1;
    }

    /// The stack of photo `idx`.
    pub(crate) fn stack_of(&self, idx: usize) -> &shoebox_core::stacks::Stack {
        &self.catalog.stacks[self.catalog.stack_of[idx]]
    }

    /// Toggle the current photo's stack open/closed (S).
    fn toggle_stack(&mut self) {
        let Some(idx) = self.current.and_then(|c| self.index(c)) else {
            return;
        };
        let primary = self.ids[self.stack_of(idx).primary];
        if !self.expanded.remove(&primary) {
            self.expanded.insert(primary);
        }
        self.refilter();
    }

    // --- marks -------------------------------------------------------------

    /// Photos a mark applies to: the selection (or the current photo). A
    /// collapsed stack's primary stands for the whole stack when the
    /// "marks to all stack members" setting is on.
    fn mark_targets(&self) -> Vec<usize> {
        let mut base: Vec<usize> = self
            .shown
            .iter()
            .copied()
            .filter(|&i| self.selection.contains(&self.ids[i]))
            .collect();
        if base.is_empty()
            && let Some(i) = self.current.and_then(|c| self.index(c))
        {
            base.push(i);
        }
        let mut out = Vec::new();
        for i in base {
            let st = self.stack_of(i);
            let collapsed = st.primary == i && !self.expanded.contains(&self.ids[i]);
            if collapsed && self.settings.marks_to_all_stack_members {
                out.extend(st.all());
            } else {
                out.push(i);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The marks shown for the current photo.
    pub(crate) fn current_marks(&self) -> Option<Marks> {
        self.current
            .and_then(|c| self.photo(c))
            .map(|p| p.marks.clone())
    }

    /// Change marks on the targets: shown at once, written to sidecars in
    /// the background as one undoable batch.
    pub(crate) fn change_marks(&mut self, label: &str, f: impl Fn(&Marks) -> Marks) {
        let targets = self.mark_targets();
        if targets.is_empty() {
            return;
        }
        let catalog = Arc::make_mut(&mut self.catalog);
        let mut updates = Vec::new();
        for i in targets {
            let p = &mut catalog.photos[i];
            let new = f(&p.marks);
            if new != p.marks {
                p.marks = new.clone();
                updates.push((p.path.clone(), new));
            }
        }
        if !updates.is_empty() {
            self.library.send(LibraryCommand::SetMarks {
                updates,
                label: label.to_owned(),
            });
            self.all_keywords = query::keywords(&self.catalog);
        }
        // Caps Lock only counts with a real window (never in headless tests).
        if self.auto_advance || (self.display.is_some() && crate::platform::caps_lock()) {
            self.step(1);
        }
    }

    pub(crate) fn set_rating(&mut self, r: u8) {
        self.change_marks(&format!("rating {r}"), |m| Marks {
            rating: r,
            ..m.clone()
        });
    }

    pub(crate) fn set_pick(&mut self, pick: Option<Pick>) {
        self.change_marks("flag", |m| Marks { pick, ..m.clone() });
    }

    /// Set a colour label; setting the label a photo already has clears it.
    pub(crate) fn toggle_label(&mut self, label: Label) {
        let all_have = self
            .mark_targets()
            .iter()
            .all(|&i| self.catalog.photos[i].marks.label == Some(label));
        let new = if all_have { None } else { Some(label) };
        self.change_marks("label", |m| Marks {
            label: new,
            ..m.clone()
        });
    }

    pub(crate) fn add_keyword(&mut self, k: &str) {
        let k = normalise_keyword(k);
        if k.is_empty() {
            return;
        }
        let saved = self.auto_advance;
        self.auto_advance = false; // typing keywords shouldn't jump photos
        self.change_marks("keyword", |m| {
            let mut m = m.clone();
            if !m.keywords.contains(&k) {
                m.keywords.push(k.clone());
            }
            m
        });
        self.auto_advance = saved;
    }

    pub(crate) fn remove_keyword(&mut self, k: &str) {
        let saved = self.auto_advance;
        self.auto_advance = false;
        self.change_marks("keyword", |m| {
            let mut m = m.clone();
            m.keywords.retain(|x| x != k);
            m
        });
        self.auto_advance = saved;
    }

    pub(crate) fn photo(&self, id: PhotoId) -> Option<&Photo> {
        self.index_of.get(&id).map(|&i| &self.catalog.photos[i])
    }

    pub(crate) fn index(&self, id: PhotoId) -> Option<usize> {
        self.index_of.get(&id).copied()
    }

    // --- loading -----------------------------------------------------------

    /// Ask for an image this frame (no-op if loaded or failed).
    pub(crate) fn want(&mut self, idx: usize, size: Size, priority: Priority) {
        let key = Key {
            photo: self.ids[idx],
            size,
        };
        if self.textures.contains(&key) || self.textures.failed.contains_key(&key) {
            return;
        }
        let p = &self.catalog.photos[idx];
        self.wanted.push((
            key,
            priority,
            Job {
                path: p.path.clone(),
                format: p.format,
                size: p.size,
                mtime_ns: p.mtime_ns,
                orientation: p.meta.orientation(),
                target: self.target,
            },
        ));
    }

    // --- selection ---------------------------------------------------------

    /// Click with modifiers, as in Lightroom: plain = select one,
    /// Cmd = toggle, Shift = range from the anchor.
    pub(crate) fn click(&mut self, pos_in_shown: usize, modifiers: egui::Modifiers) {
        let id = self.ids[self.shown[pos_in_shown]];
        if modifiers.command {
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
            self.anchor = Some(id);
        } else if modifiers.shift
            && let Some(a) = self
                .anchor
                .and_then(|a| self.shown.iter().position(|&i| self.ids[i] == a))
        {
            let (lo, hi) = (a.min(pos_in_shown), a.max(pos_in_shown));
            self.selection = self.shown[lo..=hi].iter().map(|&i| self.ids[i]).collect();
        } else {
            self.selection = BTreeSet::from([id]);
            self.anchor = Some(id);
        }
        self.current = Some(id);
    }

    fn step(&mut self, delta: isize) {
        if self.shown.is_empty() {
            return;
        }
        let pos = self
            .current
            .and_then(|c| self.shown.iter().position(|&i| self.ids[i] == c))
            .map_or(0, |p| {
                (p as isize + delta).clamp(0, self.shown.len() as isize - 1) as usize
            });
        let id = self.ids[self.shown[pos]];
        self.current = Some(id);
        self.selection = BTreeSet::from([id]);
        self.anchor = Some(id);
        self.loupe = LoupeState {
            zoom: Zoom::Fit,
            pan: egui::Vec2::ZERO,
        };
        self.scroll_to_current = true;
    }

    /// Reveal the selection (in display order), or the current photo if
    /// nothing is selected.
    pub(crate) fn reveal_selection(&self) {
        let mut paths: Vec<&Path> = self
            .shown
            .iter()
            .filter(|&&i| self.selection.contains(&self.ids[i]))
            .map(|&i| self.catalog.photos[i].path.as_path())
            .collect();
        if paths.is_empty()
            && let Some(p) = self.current.and_then(|c| self.photo(c))
        {
            paths.push(&p.path);
        }
        reveal_all(&paths);
    }

    /// Right-click on a photo: like Finder, a click outside the selection
    /// selects just that photo first.
    pub(crate) fn secondary_click(&mut self, pos_in_shown: usize) {
        let id = self.ids[self.shown[pos_in_shown]];
        if !self.selection.contains(&id) {
            self.click(pos_in_shown, egui::Modifiers::NONE);
        }
    }

    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    pub(crate) fn current_pos(&self) -> Option<usize> {
        let c = self.current?;
        self.shown.iter().position(|&i| self.ids[i] == c)
    }

    // --- input -------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let input = ctx.input(|i| i.clone());
        let m = input.modifiers;
        let pressed = |k| input.key_pressed(k);
        if pressed(egui::Key::Tab) {
            if m.shift {
                let show = !(self.left_open || self.right_open || self.bars_open);
                (self.left_open, self.right_open, self.bars_open) = (show, show, show);
            } else {
                let show = !(self.left_open || self.right_open);
                (self.left_open, self.right_open) = (show, show);
            }
        }
        if m.command {
            if pressed(egui::Key::Z) {
                self.library.send(if m.shift {
                    LibraryCommand::Redo
                } else {
                    LibraryCommand::Undo
                });
            }
            if pressed(egui::Key::K) {
                self.focus_keywords = true;
                self.right_open = true;
            }
            if pressed(egui::Key::F) {
                self.focus_search = true;
                self.bars_open = true;
            }
            if pressed(egui::Key::A) {
                self.selection = self.shown.iter().map(|&i| self.ids[i]).collect();
            }
            if pressed(egui::Key::D) {
                self.selection.clear();
            }
            if pressed(egui::Key::R) {
                self.reveal_selection();
            }
            return;
        }
        for (v, _, key) in View::ALL {
            if pressed(key) {
                self.set_view(v);
            }
        }
        let digits = [
            egui::Key::Num0,
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
        ];
        for (r, k) in digits.into_iter().enumerate() {
            if pressed(k) {
                self.set_rating(r as u8);
            }
        }
        for (k, l) in [
            (egui::Key::Num6, Label::Red),
            (egui::Key::Num7, Label::Yellow),
            (egui::Key::Num8, Label::Green),
            (egui::Key::Num9, Label::Blue),
        ] {
            if pressed(k) {
                self.toggle_label(l);
            }
        }
        if pressed(egui::Key::P) {
            self.set_pick(Some(Pick::Pick));
        }
        if pressed(egui::Key::X) {
            self.set_pick(Some(Pick::Reject));
        }
        if pressed(egui::Key::U) {
            self.set_pick(None);
        }
        if pressed(egui::Key::S) {
            self.toggle_stack();
        }
        if pressed(egui::Key::Slash) {
            self.focus_search = true;
            self.bars_open = true;
        }
        if pressed(egui::Key::F) {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        if pressed(egui::Key::Escape) && self.fullscreen {
            self.fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if pressed(egui::Key::Enter) && self.current.is_some() {
            self.set_view(View::Loupe);
        }
        let cols = self.grid_cols as isize;
        if pressed(egui::Key::ArrowRight) {
            self.step(1);
        }
        if pressed(egui::Key::ArrowLeft) {
            self.step(-1);
        }
        if self.view == View::Grid {
            if pressed(egui::Key::ArrowDown) {
                self.step(cols);
            }
            if pressed(egui::Key::ArrowUp) {
                self.step(-cols);
            }
        }
        if self.view == View::Loupe && (pressed(egui::Key::Space) || pressed(egui::Key::Z)) {
            self.loupe.zoom = match self.loupe.zoom {
                Zoom::Fit => Zoom::Scale(1.0),
                Zoom::Scale(_) => Zoom::Fit,
            };
            self.loupe.pan = egui::Vec2::ZERO;
        }
    }
}

impl eframe::App for ShoeboxApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(d) = &self.display {
            d.ensure();
        }
        self.textures.begin_frame();
        self.apply_events(&ctx);
        self.handle_keys(&ctx);
        self.wanted.clear();

        let mut bars = self.bars_open;
        egui::Panel::top("toolbar").show_collapsible(ui, &mut bars, |ui| panels::toolbar(self, ui));
        egui::Panel::bottom("status")
            .show_collapsible(ui, &mut bars, |ui| panels::status(self, ui));
        self.bars_open = bars;

        let mut film = self.view.has_filmstrip() && self.bars_open;
        egui::Panel::bottom("filmstrip")
            .exact_size(92.0)
            .resizable(false)
            .show_collapsible(ui, &mut film, |ui| views::filmstrip::show(self, ui));

        let mut left = self.left_open;
        egui::Panel::left("library")
            .resizable(true)
            .default_size(260.0)
            .show_collapsible(ui, &mut left, |ui| panels::library(self, ui));
        self.left_open = left;
        let mut right = self.right_open;
        egui::Panel::right("info")
            .resizable(true)
            .default_size(280.0)
            .show_collapsible(ui, &mut right, |ui| panels::info(self, ui));
        self.right_open = right;

        egui::CentralPanel::default().show(ui, |ui| {
            if self.settings.roots.is_empty() && !self.scanning {
                panels::empty_library(self, ui);
                return;
            }
            match self.view {
                View::Grid => views::grid::show(self, ui),
                View::Masonry => views::masonry::show(self, ui),
                View::Loupe => views::loupe::show(self, ui),
                View::Compare | View::Survey | View::Timeline => panels::not_yet(self.view, ui),
            }
        });
        self.scroll_to_current = false;

        let wanted = std::mem::take(&mut self.wanted);
        self.loader.demand(wanted);
    }
}

fn build_tree(catalog: &Catalog) -> Vec<FolderNode> {
    catalog
        .roots
        .iter()
        .enumerate()
        .map(|(ri, root)| {
            let mut node = FolderNode {
                path: root.path.clone(),
                name: root.path.file_name().map_or_else(
                    || root.path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                ..Default::default()
            };
            for p in catalog.photos.iter().filter(|p| p.root == ri) {
                node.count += 1;
                let mut cur = &mut node;
                let parts: Vec<&str> = p.rel.split('/').collect();
                for part in &parts[..parts.len().saturating_sub(1)] {
                    let path = cur.path.join(part);
                    cur = cur
                        .children
                        .entry((*part).to_owned())
                        .or_insert_with(|| FolderNode {
                            path,
                            name: (*part).to_owned(),
                            ..Default::default()
                        });
                    cur.count += 1;
                }
            }
            node
        })
        .collect()
}

/// Menu label for [`reveal`] on this platform.
#[cfg(target_os = "macos")]
pub(crate) const REVEAL_LABEL: &str = "Reveal in Finder";
#[cfg(not(target_os = "macos"))]
pub(crate) const REVEAL_LABEL: &str = "Show in File Manager";

/// Show a file in Finder (macOS) or its folder in the file manager (Linux).
pub(crate) fn reveal(path: &Path) {
    reveal_all(&[path]);
}

/// Show files in Finder, selected together (macOS), or open their folders
/// in the file manager (Linux; at most a few windows).
pub(crate) fn reveal_all(paths: &[&Path]) {
    if paths.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-R")
        .args(paths)
        .spawn()
        .map(|_| ());
    #[cfg(not(target_os = "macos"))]
    let result = {
        let mut dirs: Vec<&Path> = paths.iter().map(|p| p.parent().unwrap_or(p)).collect();
        dirs.dedup();
        dirs.into_iter().take(5).try_for_each(|d| {
            std::process::Command::new("xdg-open")
                .arg(d)
                .spawn()
                .map(|_| ())
        })
    };
    if let Err(e) = result {
        tracing::warn!("reveal {}: {e}", paths[0].display());
    }
}

/// Keywords are stored as `|` paths; accept `>` or `/` as separators too
/// (`Places > Japan > Kyoto`).
pub(crate) fn normalise_keyword(k: &str) -> String {
    k.split(['|', '>', '/'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("|")
}
