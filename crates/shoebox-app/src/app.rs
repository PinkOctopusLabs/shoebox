//! Application state and the frame loop.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use shoebox_core::catalog::{Catalog, Photo};
use shoebox_core::settings::Settings;
use shoebox_core::thumbs::ThumbCache;

use shoebox_core::events::{self, Event};
use shoebox_core::marks::{Label, Marks, Pick};
use shoebox_core::query;

use shoebox_core::review::{Decision, Review};

use crate::jobs::{
    BatchRow, DupePreview, Job, Key, Library, LibraryCommand, LibraryEvent, LibraryPaths, Loader,
    OrgPreview, OrgRequest, PhotoId, Priority, Size, photo_id,
};
use crate::textures::Textures;
use crate::{panels, views};
use shoebox_core::collections::Collection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Grid,
    Masonry,
    Loupe,
    Compare,
    Survey,
    Timeline,
    /// Tasks → Duplicates: the list of duplicate groups.
    Duplicates,
    /// Tasks → Import.
    Import,
    /// Tasks → History: every batch, with undo.
    History,
    /// Photos with a GPS position on a map.
    Map,
    /// Editing a raw photo (D).
    Develop,
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
        matches!(
            self,
            View::Loupe | View::Compare | View::Survey | View::Develop
        )
    }

    /// Views of the filtered photos, which get the filter bar.
    pub fn has_filters(self) -> bool {
        !matches!(
            self,
            View::Duplicates | View::Import | View::History | View::Develop
        )
    }
}

/// Compare: the "select" and a "candidate" side by side, with zoom and pan
/// shared (as a zoom over fit and a centre in image fractions, so files of
/// different resolutions show the same part of the picture).
#[derive(Debug, Clone)]
pub struct CompareState {
    pub select: Option<PhotoId>,
    pub candidate: Option<PhotoId>,
    /// 1 = fit.
    pub zoom: f32,
    /// Image point at the centre of each pane (0..1).
    pub center: egui::Pos2,
    /// The select's fit scale as last drawn (for Space/Z → 100%).
    pub select_fit: f32,
}

impl Default for CompareState {
    fn default() -> Self {
        Self {
            select: None,
            candidate: None,
            zoom: 1.0,
            center: egui::pos2(0.5, 0.5),
            select_fit: 1.0,
        }
    }
}

/// Tasks → Import.
#[derive(Default)]
pub struct ImportState {
    pub source: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub template: String,
    pub running: bool,
    pub progress: Option<(usize, usize)>,
    pub plan: Option<Result<Arc<shoebox_core::import::Plan>, String>>,
    /// The plan's folders, with what each name becomes (edited here).
    pub folders: Vec<shoebox_core::import::Folder>,
    /// Show the photos to copy as a source → destination list, not
    /// thumbnails.
    pub list: bool,
    /// Left-out source files to copy anyway.
    pub force: HashSet<PathBuf>,
    /// Show only the photos this source folder's name goes on.
    pub filter: Option<PathBuf>,
    /// Copying, once Import is clicked.
    pub copying: Option<Copying>,
    /// The plan's photos `filter` shows: (folder, plan, indices), cached.
    pub shown: Option<(PathBuf, usize, Arc<Vec<usize>>)>,
    /// The card or disk the import copies from, when it can be ejected
    /// (kept after the import for its Eject button).
    pub volume: Option<PathBuf>,
    pub message: Option<String>,
}

/// Where an import's copying is.
#[derive(Debug, Clone)]
pub struct Copying {
    /// Copies finished, of `files`.
    pub done: usize,
    pub files: usize,
    pub bytes: u64,
    pub total_bytes: u64,
    /// The file being copied.
    pub current: Option<PathBuf>,
    pub started: std::time::Instant,
}

impl Copying {
    /// Bytes a second so far (once there's enough to go on).
    pub fn rate(&self) -> Option<f64> {
        let secs = self.started.elapsed().as_secs_f64();
        (secs >= 1.0 && self.bytes > 0).then(|| self.bytes as f64 / secs)
    }

    /// Seconds left at the rate so far.
    pub fn left(&self) -> Option<f64> {
        Some(self.total_bytes.saturating_sub(self.bytes) as f64 / self.rate()?)
    }
}

/// Move, rename, reject and trash: dialogs and the dry-run table.
#[derive(Default)]
pub struct OrgState {
    pub preview: Option<Result<OrgPreview, String>>,
    /// Rename dialog: (files, template being typed).
    pub rename: Option<(Vec<PathBuf>, String)>,
    /// New folder dialog: (parent, name being typed).
    pub new_folder: Option<(PathBuf, String)>,
}

/// Tasks → History.
#[derive(Default)]
pub struct HistoryState {
    pub batches: Vec<BatchRow>,
    /// Batches shown expanded, with their operations once loaded.
    pub open: HashMap<i64, Vec<(String, PathBuf, PathBuf, bool)>>,
    /// Scroll to and highlight this batch.
    pub focus: Option<i64>,
    pub message: Option<Result<String, String>>,
}

impl HistoryState {
    /// Batches started but never finished (the app quit or crashed).
    pub fn interrupted(&self) -> usize {
        self.batches
            .iter()
            .filter(|b| b.status == "running")
            .count()
    }
}

/// Where to look for duplicates: everything, or within one folder, event
/// or collection (both files of a match in it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DupeScope {
    #[default]
    All,
    Folder(PathBuf),
    /// By event id.
    Event(String),
    /// By collection id.
    Collection(String),
}

/// Duplicate review state.
#[derive(Default)]
pub struct DupeState {
    pub review: Option<Arc<Review>>,
    /// Where the next find looks.
    pub scope: DupeScope,
    /// Where the groups shown were found.
    pub found_in: Option<DupeScope>,
    pub running: bool,
    pub progress: Option<(usize, usize)>,
    pub config: shoebox_core::dupes::Config,
    pub decisions: HashMap<PathBuf, Decision>,
    /// The group open in Compare or Survey.
    pub open: Option<usize>,
    /// The dry-run table being shown (or why applying is refused).
    pub preview: Option<Result<DupePreview, String>>,
    pub message: Option<String>,
    /// Also list groups of reference files only (nothing there can move).
    pub show_reference_only: bool,
    /// Thumbnail width in the list (the size slider); see [`Self::thumb`].
    pub thumb_width: f32,
}

impl DupeState {
    pub const THUMB_WIDTHS: std::ops::RangeInclusive<f32> = 150.0..=480.0;

    /// A thumbnail's size in the list (4:3, the smallest until the slider
    /// is moved).
    pub fn thumb(&self) -> egui::Vec2 {
        let w = self
            .thumb_width
            .clamp(*Self::THUMB_WIDTHS.start(), *Self::THUMB_WIDTHS.end());
        egui::vec2(w, (w * 0.75).round())
    }

    /// Groups the list shows, in order.
    pub fn visible(&self) -> Vec<usize> {
        let Some(r) = &self.review else {
            return Vec::new();
        };
        (0..r.groups.len())
            .filter(|&g| self.show_reference_only || !r.groups[g].reference_only(r))
            .collect()
    }
}

/// Where the app keeps its state; tests point these at temp folders.
#[derive(Clone)]
pub struct AppPaths {
    pub settings: PathBuf,
    pub cache_db: PathBuf,
    pub journal: PathBuf,
    pub thumbs: PathBuf,
    /// Trash folder instead of the system Trash (tests only).
    pub trash_dir: Option<PathBuf>,
    /// Map tile cache; None turns map tiles (and the network) off.
    pub tile_cache: Option<PathBuf>,
    /// Stored image embeddings (semantic search).
    pub embeddings_db: PathBuf,
    /// The semantic search model: downloaded on first use, or a test model.
    pub model: crate::semantic::ModelSource,
    /// The face models: downloaded on first use, or a test model.
    pub faces: crate::people::FaceSource,
    /// The object model (SAM 2.1): downloaded when first asked for, into
    /// `dir`, through `staging`.
    pub objects: (PathBuf, PathBuf),
    /// Names given to people.
    pub people: PathBuf,
    /// Develop's caches (DNGs of High efficiency NEFs, under `dng/`).
    pub raw_cache: PathBuf,
    /// Export presets (JSON).
    pub export_presets: PathBuf,
}

impl AppPaths {
    /// Apple's layout (Application Support and Caches, by bundle id), or
    /// everything under `$SHOEBOX_DATA_DIR` when that's set (the same
    /// variable as the CLI's; useful for trying a build without touching
    /// your real settings).
    pub fn default_locations() -> anyhow::Result<Self> {
        let (data, cache) = match std::env::var_os("SHOEBOX_DATA_DIR").filter(|v| !v.is_empty()) {
            Some(d) => {
                let d = PathBuf::from(d);
                (d.clone(), d.join("cache"))
            }
            None => (
                shoebox_core::paths::default_data_dir()?,
                shoebox_core::paths::default_cache_dir()?,
            ),
        };
        Ok(Self {
            settings: shoebox_core::paths::settings_file(&data),
            cache_db: shoebox_core::paths::cache_db(&data),
            journal: shoebox_core::paths::journal_db(&data),
            thumbs: cache.join("thumbs"),
            trash_dir: None,
            tile_cache: Some(cache.join("tiles")),
            embeddings_db: shoebox_core::paths::embeddings_db(&data),
            model: crate::semantic::ModelSource::Download {
                dir: shoebox_core::paths::models_dir(&data).join("siglip2-base-patch16-224"),
                staging: cache.join("model-download"),
            },
            faces: crate::people::FaceSource::Download {
                dir: shoebox_core::paths::models_dir(&data).join("faces-yunet-sface"),
                staging: cache.join("model-download"),
            },
            objects: (
                shoebox_core::paths::models_dir(&data).join("sam2.1-hiera-tiny"),
                cache.join("model-download"),
            ),
            people: shoebox_core::paths::people_file(&data),
            raw_cache: cache.clone(),
            export_presets: data.join("export-presets.json"),
        })
    }
}

/// Loupe zoom: fit to the window, or a scale where 1.0 = one image pixel
/// per screen pixel.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Zoom {
    #[default]
    Fit,
    Scale(f32),
}

#[derive(Default)]
pub struct LoupeState {
    pub zoom: Zoom,
    pub pan: egui::Vec2,
    /// The photo the zoom belongs to: another photo starts at Fit.
    pub photo: Option<PhotoId>,
    /// Showing an edited photo as it was before the edit (\).
    pub original: bool,
    /// What was last drawn: the original or not, and the image's size.
    pub drawn: Option<(bool, [usize; 2])>,
}

/// A folder in the left panel, with the recursive count of photos (stacks
/// with a file in it or below: what selecting the folder shows).
#[derive(Debug, Default)]
pub struct FolderNode {
    pub path: PathBuf,
    pub name: String,
    pub count: usize,
    pub children: std::collections::BTreeMap<String, FolderNode>,
    /// The last stack counted here (1-based), so a stack counts once.
    counted: usize,
}

impl FolderNode {
    fn count_stack(&mut self, stamp: usize) {
        if self.counted != stamp {
            self.counted = stamp;
            self.count += 1;
        }
    }
}

/// One place in the grid and the other views: a file (its own id), or one
/// of the file's virtual copies (`jobs::copy_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// The file's catalog index.
    pub idx: usize,
    pub id: PhotoId,
}

pub struct ShoeboxApp {
    pub(crate) loader: Loader,
    library: Library,
    pub(crate) catalog: Arc<Catalog>,
    pub(crate) settings: Settings,
    pub(crate) settings_ui: views::settings::State,
    /// Where map tiles cache when they're on (None: never fetch tiles).
    tile_cache: Option<PathBuf>,
    /// `ids[i]` is the stable id of `catalog.photos[i]`.
    pub(crate) ids: Vec<PhotoId>,
    index_of: HashMap<PhotoId, usize>,
    pub(crate) tree: Vec<FolderNode>,
    /// Folder filter from the left panel (None = whole library).
    /// Folders the grid shows (any of them, and below); ⌘-click adds one.
    pub(crate) folders: Vec<PathBuf>,
    /// The tiles shown in the centre, in display order.
    pub(crate) shown: Vec<Tile>,
    /// Bumped whenever `shown` changes (layout caches key on it).
    pub(crate) shown_gen: u64,
    pub(crate) selection: BTreeSet<PhotoId>,
    pub(crate) current: Option<PhotoId>,
    anchor: Option<PhotoId>,
    pub(crate) view: View,
    /// Where Escape takes the loupe back to (the view it was opened from).
    back_view: View,
    pub(crate) thumb_size: f32,
    /// Columns in the grid as last drawn (for Up/Down).
    pub(crate) grid_cols: usize,
    pub(crate) left_open: bool,
    pub(crate) right_open: bool,
    pub(crate) bars_open: bool,
    /// The rating, flag and label filters over the photos (the funnel).
    pub(crate) filters_open: bool,
    fullscreen: bool,
    pub(crate) loupe: LoupeState,
    pub(crate) textures: Textures,
    pub(crate) histograms: HashMap<PhotoId, Box<[[u32; 256]; 4]>>,
    pub(crate) siblings_of: Option<(PhotoId, Vec<usize>)>,
    pub(crate) masonry: views::masonry::Layout,
    pub(crate) grid: views::grid::Layout,
    /// Loads wanted this frame, sent to the loader at the end of the frame.
    wanted: Vec<(Key, Priority, Job)>,
    pub(crate) scan_progress: Option<(usize, usize)>,
    /// Download from Cloud running: (done, total).
    pub(crate) download_progress: Option<(usize, usize)>,
    /// Cloud-only photos opened in the loupe, downloading.
    fetching: HashSet<PhotoId>,
    /// Photos in the cloud only.
    pub(crate) cloud_count: usize,
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
    /// One undo history over two stacks: the library's (files, sidecars)
    /// and People's (names). When each one's next undo was made and next
    /// redo was undone; ⌘Z takes the newer of the two, ⌘⇧Z the more
    /// recently undone.
    undo_at: UndoTimes,
    /// Auto-advance after marking (also on while Caps Lock is down, macOS).
    pub(crate) auto_advance: bool,
    pub(crate) keyword_input: String,
    pub(crate) focus_keywords: bool,
    pub(crate) focus_search: bool,
    /// Name being typed when saving a smart filter.
    pub(crate) new_smart_filter: Option<String>,
    /// A photo to show among all photos, from the next frame.
    pub(crate) show_in_all_request: Option<PhotoId>,
    /// Keywords in use (for autocomplete), cameras and lenses (for filters).
    pub(crate) all_keywords: Vec<(String, usize)>,
    pub(crate) cameras: Vec<String>,
    pub(crate) lenses: Vec<String>,
    pub(crate) events: Vec<Event>,
    /// Member stacks per event id.
    pub(crate) event_members: HashMap<String, HashSet<usize>>,
    /// Each event's first and last capture times (`YYYY-MM-DD HH:MM:SS`),
    /// from its photos, for the panel's years and order.
    pub(crate) event_spans: HashMap<String, (String, String)>,
    /// Event chosen in the left panel (restricts the grid to its stacks).
    pub(crate) selected_event: Option<String>,
    /// The event being created or edited.
    pub(crate) event_edit: Option<EventEdit>,
    /// Tags the window Display P3 on macOS (None in tests / elsewhere).
    display: Option<crate::display::DisplayTagger>,
    /// Colour space images are rendered in for this display.
    pub(crate) target: shoebox_decode::Target,
    /// Develop: the photo being edited.
    pub(crate) develop: views::develop::State,
    /// The develop thread (prepares raw photos).
    pub(crate) developer: crate::develop::Develop,
    /// The object model (SAM 2.1) for click masks.
    pub(crate) objects: std::sync::Arc<crate::objects::Objects>,
    /// Edited photos: photo path → its preview's key.
    pub(crate) edited: HashMap<PathBuf, String>,
    /// Edited photos' edit documents (for export).
    pub(crate) edited_json: HashMap<PathBuf, String>,
    /// Export: the dialog, presets and progress.
    pub(crate) export: views::export::State,
    /// Previews rendered and on disk, by key.
    previews_ready: HashSet<String>,
    /// The DNG converter and cache, as last checked (Settings → Raw).
    pub(crate) raw_status: Option<crate::develop::RawStatus>,
    /// Edit files whose photo the last scan didn't find (Settings → Raw).
    pub(crate) unattached: Vec<PathBuf>,
    /// What the develop thread was last told (converter, cache limit).
    raw_config: Option<(Option<PathBuf>, u64)>,
    /// The last keyboard or mouse input, and whether likely edits are
    /// being converted since (Settings → Raw: idle pre-convert).
    last_input: Instant,
    converting_idle: bool,
    /// The catalog changed since the likely edits were listed.
    idle_stale: bool,
    /// How long without input counts as idle (tests shorten it).
    idle_after: Duration,
    pub(crate) compare: CompareState,
    /// Photos in Survey, in order.
    pub(crate) survey: Vec<PhotoId>,
    pub(crate) dupes: DupeState,
    pub(crate) import: ImportState,
    /// The open folder picker, if any.
    pub(crate) picker: crate::picker::Picker,
    /// Asking whether to stop an import and quit.
    pub(crate) confirm_quit: bool,
    /// Quit once the import has stopped.
    pub(crate) quit_when_stopped: bool,
    /// Quit asked for other than by the menu or the window (tests).
    quit_asked: bool,
    pub(crate) org: OrgState,
    pub(crate) history: HistoryState,
    pub(crate) collections: Vec<Collection>,
    /// Member stacks per collection id.
    pub(crate) collection_members: HashMap<String, HashSet<usize>>,
    /// Collection chosen in the left panel (restricts the grid to it).
    pub(crate) selected_collection: Option<String>,
    /// Name being typed for a new collection, or (id, name) when renaming.
    pub(crate) collection_edit: Option<(Option<String>, String)>,
    /// A short note for the status bar (what the last operation did).
    pub(crate) notice: Option<String>,
    /// Photos' virtual copies: their tiles when the stack is open, and the
    /// grid's "EDITED +n".
    pub(crate) copies: HashMap<PathBuf, Vec<crate::jobs::CopyInfo>>,
    /// Copy tiles' ids → the photo's path and the copy's id.
    pub(crate) copy_tiles: HashMap<PhotoId, (PathBuf, String)>,
    /// Settings copied in Develop (⇧⌘C), for pasting onto other photos.
    pub(crate) settings_clipboard: Option<shoebox_develop::preset::Preset>,
    pub(crate) timeline: views::timeline::State,
    pub(crate) map: views::map::State,
    /// Left-panel sections folded away.
    pub(crate) collapsed: HashSet<&'static str>,
    /// Settings came from disk (after that, only roots come from the worker).
    settings_loaded: bool,
    semantic: crate::semantic::Semantic,
    pub(crate) sem: crate::semantic::SearchState,
    people: crate::people::PeopleWorker,
    pub(crate) ppl: crate::people::PeopleState,
}

impl ShoeboxApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: AppPaths) -> Self {
        let ctx = &cc.egui_ctx;
        crate::icons::install(ctx);
        crate::platform::own_titlebar_clicks(cc);
        crate::theme::install(ctx);
        let tile_cache = paths.tile_cache.clone();
        let objects =
            crate::objects::Objects::register(paths.objects.0.clone(), paths.objects.1.clone());
        let display = crate::display::DisplayTagger::new(cc);
        let target = crate::display::DisplayTagger::target(display.as_ref());
        tracing::info!("rendering images in {target:?}");
        let threads =
            std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(1).max(2));
        let (c1, c2, c3, c4) = (ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone());
        let loader = Loader::new(threads, ThumbCache::new(paths.thumbs), move || {
            c1.request_repaint()
        });
        let library = Library::new(
            LibraryPaths {
                settings: paths.settings,
                cache_db: paths.cache_db,
                journal: paths.journal,
                trash_dir: paths.trash_dir,
            },
            move || c2.request_repaint(),
        );
        library.send(LibraryCommand::Open);
        let people = crate::people::PeopleWorker::new(
            paths.faces,
            paths.embeddings_db.clone(),
            paths.people,
            move || c4.request_repaint(),
        );
        let semantic =
            crate::semantic::Semantic::new(paths.model, paths.embeddings_db, move || {
                c3.request_repaint()
            });
        let c5 = ctx.clone();
        let developer =
            crate::develop::Develop::new(paths.raw_cache, paths.export_presets, move || {
                c5.request_repaint()
            });
        developer.load_presets();
        developer.load_looks();
        Self {
            loader,
            library,
            catalog: Arc::new(Catalog::default()),
            settings: Settings::default(),
            settings_ui: Default::default(),
            tile_cache: tile_cache.clone(),
            ids: Vec::new(),
            index_of: HashMap::new(),
            tree: Vec::new(),
            folders: Vec::new(),
            shown: Vec::new(),
            shown_gen: 0,
            selection: BTreeSet::new(),
            current: None,
            anchor: None,
            view: View::Grid,
            back_view: View::Grid,
            thumb_size: 180.0,
            grid_cols: 1,
            left_open: true,
            right_open: true,
            bars_open: true,
            filters_open: false,
            fullscreen: false,
            loupe: LoupeState::default(),
            // 768 MB: ~3,000 small thumbnails plus a few full-size images.
            textures: Textures::new(768 << 20),
            histograms: HashMap::new(),
            siblings_of: None,
            masonry: views::masonry::Layout::default(),
            grid: views::grid::Layout::default(),
            wanted: Vec::new(),
            scan_progress: None,
            download_progress: None,
            fetching: HashSet::new(),
            cloud_count: 0,
            scanning: true,
            error: None,
            scroll_to_current: false,
            display,
            target,
            develop: Default::default(),
            developer,
            objects,
            edited: HashMap::new(),
            edited_json: HashMap::new(),
            export: Default::default(),
            previews_ready: HashSet::new(),
            raw_status: None,
            unattached: Vec::new(),
            raw_config: None,
            last_input: Instant::now(),
            converting_idle: false,
            idle_stale: false,
            idle_after: Duration::from_secs(60),
            events: Vec::new(),
            event_members: HashMap::new(),
            event_spans: HashMap::new(),
            selected_event: None,
            event_edit: None,
            filter: query::Filter::default(),
            sort: query::Sort::CaptureTime,
            sort_desc: false,
            expanded: HashSet::new(),
            can_undo: false,
            can_redo: false,
            undo_at: UndoTimes::default(),
            auto_advance: false,
            keyword_input: String::new(),
            focus_keywords: false,
            focus_search: false,
            new_smart_filter: None,
            show_in_all_request: None,
            all_keywords: Vec::new(),
            cameras: Vec::new(),
            lenses: Vec::new(),
            compare: CompareState::default(),
            survey: Vec::new(),
            dupes: DupeState::default(),
            import: ImportState::default(),
            picker: crate::picker::Picker::default(),
            confirm_quit: false,
            quit_when_stopped: false,
            quit_asked: false,
            org: OrgState::default(),
            history: HistoryState::default(),
            collections: Vec::new(),
            collection_members: HashMap::new(),
            selected_collection: None,
            collection_edit: None,
            notice: None,
            settings_clipboard: None,
            copies: HashMap::new(),
            copy_tiles: HashMap::new(),
            timeline: views::timeline::State::default(),
            map: views::map::State::new(tile_cache),
            collapsed: HashSet::new(),
            settings_loaded: false,
            semantic,
            sem: Default::default(),
            people,
            ppl: Default::default(),
        }
    }

    /// True when no scan or image load is outstanding (used by tests).
    pub fn idle(&self) -> bool {
        !self.scanning && self.loader.busy() == 0
    }

    /// Image loads outstanding (used by tests).
    pub fn loading(&self) -> usize {
        self.loader.busy()
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

    /// Unseen change of the photo with this file name (tests and diagnostics).
    pub fn change_of(&self, file_name: &str) -> Option<shoebox_core::changes::Change> {
        self.catalog
            .photos
            .iter()
            .find(|p| p.path.file_name().is_some_and(|n| n == file_name))
            .and_then(|p| p.change.clone())
    }

    /// Duplicate groups found so far (None before a search).
    fn name_of(&self, id: PhotoId) -> Option<String> {
        self.photo(id)
            .and_then(|p| p.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
    }

    /// Compare's (select, candidate) by file name.
    pub fn compare_names(&self) -> (Option<String>, Option<String>) {
        (
            self.compare.select.and_then(|id| self.name_of(id)),
            self.compare.candidate.and_then(|id| self.name_of(id)),
        )
    }

    /// Survey's photos by file name, and the active one.
    pub fn survey_names(&self) -> (Vec<String>, Option<String>) {
        (
            self.survey
                .iter()
                .filter_map(|&id| self.name_of(id))
                .collect(),
            self.current.and_then(|id| self.name_of(id)),
        )
    }

    /// Every reviewed file in a visible group: (path, decision, how it
    /// matches, the group's kept copy). For reports from test runs.
    pub fn review_rows(&self) -> Vec<(PathBuf, Decision, String, PathBuf)> {
        let Some(r) = &self.dupes.review else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for g in self.dupes.visible() {
            let g = &r.groups[g];
            let keeper = r.files[g.entries[0].file].path.clone();
            for e in &g.entries {
                let path = r.files[e.file].path.clone();
                out.push((
                    path.clone(),
                    self.decision(&path),
                    views::duplicates::match_text(r, g.entries[0].file, e),
                    keeper.clone(),
                ));
            }
        }
        out
    }

    /// Groups the list shows (reference-only groups hidden).
    pub fn dupe_groups(&self) -> Option<usize> {
        self.dupes
            .review
            .as_ref()
            .map(|_| self.dupes.visible().len())
    }

    /// Files in the groups the list shows (tests).
    pub fn dupe_files(&self) -> Vec<PathBuf> {
        let Some(r) = &self.dupes.review else {
            return Vec::new();
        };
        self.dupes
            .visible()
            .into_iter()
            .flat_map(|g| r.groups[g].entries.iter())
            .map(|e| r.files[e.file].path.clone())
            .collect()
    }

    /// The duplicate search is idle and its result is in.
    pub fn dupes_idle(&self) -> bool {
        !self.dupes.running && self.dupes.review.is_some()
    }

    pub fn dupe_message(&self) -> Option<&str> {
        self.dupes.message.as_deref()
    }

    /// The dry-run table is showing (Some(Err) if applying was refused).
    pub fn dupe_preview(&self) -> Option<Result<usize, String>> {
        self.dupes
            .preview
            .as_ref()
            .map(|p| p.as_ref().map(|p| p.moves.len()).map_err(Clone::clone))
    }

    /// Decide for the reviewed file with this name (tests).
    pub fn decide(&mut self, file_name: &str, d: Decision) {
        let path = self.dupes.review.as_ref().and_then(|r| {
            r.files
                .iter()
                .find(|f| f.path.file_name().is_some_and(|n| n == file_name))
                .map(|f| f.path.clone())
        });
        if let Some(p) = path {
            self.set_decision(&p, d);
        }
    }

    pub fn decision_of(&self, file_name: &str) -> Decision {
        self.dupes
            .decisions
            .iter()
            .find(|(p, _)| p.file_name().is_some_and(|n| n == file_name))
            .map_or(Decision::Undecided, |(_, d)| *d)
    }

    pub fn event_names(&self) -> Vec<String> {
        self.events.iter().map(|e| e.name.clone()).collect()
    }

    /// What the grid shows: (folders, event name) (tests).
    pub fn scope(&self) -> (usize, Option<String>) {
        let event = self
            .selected_event
            .as_ref()
            .and_then(|id| self.events.iter().find(|e| &e.id == id))
            .map(|e| e.name.clone());
        (self.folders.len(), event)
    }

    /// The left panel's photo count for an event (tests).
    pub fn event_count(&self, name: &str) -> usize {
        self.events
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| self.event_members.get(&e.id))
            .map_or(0, |m| m.len())
    }

    /// Replace the filter and rebuild the visible items.
    pub fn set_filter(&mut self, f: query::Filter) {
        self.filter = f;
        self.refilter();
    }

    pub fn view_name(&self) -> View {
        self.view
    }

    /// Develop's edit of the current photo, once loaded (tests).
    pub fn develop_doc(&self) -> Option<&shoebox_develop::edit::EditDoc> {
        self.develop.doc.as_ref()
    }

    /// Develop's histogram of the render, and whether clipping is shown
    /// (tests).
    pub fn develop_histogram(&self) -> (Option<u32>, bool) {
        (
            self.develop.histogram.as_ref().map(|h| h[3].iter().sum()),
            self.develop.warnings,
        )
    }

    /// Develop's overlays: clipping, soft proofing, gamut warning (tests).
    pub fn develop_overlays(&self) -> crate::develop::Overlays {
        self.develop.overlays()
    }

    /// Develop's comparison: the mode, the split's divider, and whether the
    /// original is rendered (tests).
    pub fn develop_compare(&self) -> (views::develop::Compare, f32, bool) {
        (
            self.develop.compare,
            self.develop.split(),
            self.develop.original_rendered(),
        )
    }

    /// Point every export preset at `folder` (tests: never ~/Pictures).
    pub fn set_export_folder(&mut self, folder: &Path) {
        for p in &mut self.export.presets {
            p.folder = Some(folder.to_path_buf());
        }
    }

    /// Change every export preset (tests).
    pub fn edit_export_presets(&mut self, mut f: impl FnMut(&mut shoebox_develop::export::Preset)) {
        self.export.presets.iter_mut().for_each(&mut f);
    }

    /// Export presets loaded, an export running, files listed for the
    /// current photo, and the last notice and error (tests).
    pub fn export_state(&self) -> (usize, bool, Option<usize>, Option<String>, Option<String>) {
        (
            self.export.presets.len(),
            self.export.progress.is_some(),
            self.export.of_photo.as_ref().map(|(_, r)| r.len()),
            self.notice.clone(),
            self.error.clone(),
        )
    }

    /// An export is waiting to be told what to do with files already there
    /// (tests).
    pub fn export_asking(&self) -> bool {
        self.export.clashes.is_some()
    }

    /// What the loupe last drew: the photo before its edit or not, and the
    /// image's size (tests).
    pub fn loupe_drawn(&self) -> Option<(bool, [usize; 2])> {
        self.loupe.drawn
    }

    /// Develop's copies of the open photo, and the one being edited (tests).
    pub fn develop_copies(&self) -> (Vec<String>, String) {
        (
            self.develop.copies.iter().map(|c| c.1.clone()).collect(),
            self.develop.copy_id().to_owned(),
        )
    }

    /// Virtual copies the photo with this file name has (the grid's count;
    /// tests).
    pub fn copy_count(&self, name: &str) -> usize {
        self.catalog
            .photos
            .iter()
            .find(|p| p.path.file_name().is_some_and(|n| n == name))
            .and_then(|p| self.copies.get(&p.path))
            .map_or(0, Vec::len)
    }

    /// Where the library knows a photo with virtual copies, by its file
    /// name (tests).
    pub fn copy_path(&self, name: &str) -> Option<PathBuf> {
        self.copies
            .keys()
            .find(|p| p.file_name().is_some_and(|n| n == name))
            .cloned()
    }

    /// The current tile's exports listed in the Info panel: whether each is
    /// stale (tests).
    pub fn exports_stale(&self) -> Vec<bool> {
        self.current
            .map(|id| views::export::listed(self, id))
            .unwrap_or_default()
            .into_iter()
            .map(|(_, _, stale)| stale)
            .collect()
    }

    /// Export the selection with preset `i` (tests).
    pub fn export_with(&mut self, i: usize) {
        let ids: Vec<PhotoId> = self.selection.iter().copied().collect();
        if let Some(p) = self.export.presets.get(i).cloned() {
            views::export::export(self, &ids, vec![p]);
        }
    }

    /// The current tile's virtual copy, if it's a copy's (tests).
    pub fn current_copy(&self) -> Option<String> {
        self.current
            .and_then(|c| self.copy_of(c))
            .map(str::to_owned)
    }

    /// The edit presets' names (tests).
    pub fn develop_presets(&self) -> Vec<String> {
        self.develop.looks.iter().map(|p| p.name.clone()).collect()
    }

    /// Edit files the last scan couldn't find a photo for (tests).
    pub fn unattached_edits(&self) -> &[PathBuf] {
        &self.unattached
    }

    /// Edited photos, and previews of edits ready (tests).
    pub fn edited_previews(&self) -> (usize, usize) {
        (self.edited.len(), self.previews_ready.len())
    }

    /// Develop's crop tool is open, and where the photo is drawn (tests).
    pub fn develop_cropping(&self) -> bool {
        self.develop.crop.is_some()
    }

    /// Develop's zoom: the centre point (None: fit), and the sharp region
    /// shown (tests).
    pub fn develop_zoom(&self) -> (Option<egui::Pos2>, Option<[usize; 4]>) {
        (self.develop.zoom, self.develop.zoom_region())
    }

    /// Where Develop drew the photo last frame (tests).
    pub fn develop_photo_rect(&self) -> Option<egui::Rect> {
        self.develop.photo_rect
    }

    /// Develop's local adjustments: the mask selected, its part, and how
    /// many layers the last render had (tests).
    pub fn develop_local(&self) -> (Option<String>, usize, usize) {
        let t = &self.develop.local;
        (t.selected.clone(), t.part, self.develop.layers_rendered())
    }

    /// Develop's reference photo's file name (tests).
    pub fn develop_reference(&self) -> Option<String> {
        let i = self.index(self.develop.reference?)?;
        self.catalog.photos[i]
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    }

    /// The Exposure of a photo's edit, by file name, as the catalog last
    /// said (tests).
    pub fn exposure_of(&self, file_name: &str) -> Option<f64> {
        let (path, json) = self
            .edited_json
            .iter()
            .find(|(p, _)| p.file_name().is_some_and(|n| n == file_name))?;
        let _ = path;
        let doc = shoebox_develop::edit::EditDoc::from_json(json).ok()?;
        Some(doc.settings().modules.exposure.ev)
    }

    /// How much of the photo the first mask covers in the last render
    /// (0–1), and whether a subject search is still running (tests).
    pub fn develop_mask_coverage(&self) -> (f32, bool) {
        let finding = self
            .develop
            .ready
            .as_ref()
            .is_some_and(|r| r.src.cache.finding().0);
        let cover = self.develop.layers_mask(0).map_or(0.0, |m| {
            m.iter().map(|&v| f32::from(v)).sum::<f32>() / (255.0 * m.len().max(1) as f32)
        });
        (cover, finding)
    }

    /// Develop's spot tool: on, and the spot selected (tests).
    pub fn develop_spots(&self) -> (bool, Option<usize>) {
        (self.develop.spots.active, self.develop.spots.selected)
    }

    /// Where Develop's split divider is drawn (tests).
    pub fn develop_divider_x(&self) -> Option<f32> {
        self.develop.divider_x
    }

    /// Develop is showing the photo rendered from its raw data (tests).
    pub fn develop_ready(&self) -> bool {
        self.develop.ready.is_some() && self.develop.rendered()
    }

    /// The left panel's photo count for a folder of this name (tests).
    pub fn folder_count(&self, name: &str) -> usize {
        fn find(n: &FolderNode, name: &str) -> Option<usize> {
            if n.name == name {
                return Some(n.count);
            }
            n.children.values().find_map(|c| find(c, name))
        }
        self.tree.iter().find_map(|n| find(n, name)).unwrap_or(0)
    }

    /// Grid day headings are on and make sense (sorted by capture time,
    /// not a best-match search).
    pub(crate) fn day_headers_shown(&self) -> bool {
        self.settings.day_headers
            && self.sort == query::Sort::CaptureTime
            && self.semantic_query().is_none()
    }

    /// The capture day ("YYYY:MM:DD") of photo `i`'s stack.
    pub(crate) fn capture_day(&self, i: usize) -> Option<String> {
        let primary = self.catalog.stacks[self.catalog.stack_of[i]].primary;
        let t = self.catalog.photos[primary]
            .meta
            .exif
            .capture_time
            .as_deref()?;
        t.get(..10).map(str::to_owned)
    }

    /// Images that couldn't be displayed (tests).
    pub fn failed_count(&self) -> usize {
        self.textures.failed.len()
    }

    /// Whether any filter is set (tests).
    pub fn filtered(&self) -> bool {
        !self.filter.is_empty() || !self.folders.is_empty()
    }

    pub fn shown_count(&self) -> usize {
        self.shown.len()
    }

    /// File names of the shown photos, in order (tests).
    pub fn shown_names(&self) -> Vec<String> {
        self.shown
            .iter()
            .filter_map(|t| {
                let name = self.catalog.photos[t.idx].path.file_name()?;
                let name = name.to_string_lossy().into_owned();
                Some(match self.copy_info(t.id) {
                    Some(c) => format!("{name} ({})", c.name),
                    None => name,
                })
            })
            .collect()
    }

    pub fn set_view(&mut self, v: View) {
        if v == View::Loupe && self.current.is_none() {
            self.current = self.shown.first().map(|t| t.id);
        }
        // Compare and Survey outside a duplicate group start from the selection.
        if self.dupes.open.is_none() {
            match v {
                View::Compare if self.view != View::Compare => self.start_compare(),
                View::Survey if self.view != View::Survey => {
                    self.survey = self.selected_in_order().into_iter().take(9).collect();
                }
                _ => {}
            }
        }
        if !matches!(v, View::Compare | View::Survey | View::Duplicates) {
            self.dupes.open = None;
        }
        if matches!(v, View::Loupe | View::Develop)
            && !matches!(self.view, View::Loupe | View::Develop)
        {
            self.back_view = self.view;
        }
        if v == View::Develop && self.current.is_none() {
            self.current = self.shown.first().map(|t| t.id);
        }
        if self.view == View::Develop && v != View::Develop {
            views::develop::leave(self);
        }
        self.view = v;
        self.loupe = LoupeState::default();
        self.scroll_to_current = true;
    }

    pub fn command(&self, c: LibraryCommand) {
        self.library.send(c);
    }

    // --- catalog -----------------------------------------------------------

    fn apply_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.library.events.try_recv() {
            match ev {
                LibraryEvent::Edits(list) => self.note_edits(list),
                LibraryEvent::Exports(photo, rows) => views::export::loaded(self, photo, rows),
                LibraryEvent::Unattached(paths) => self.unattached = paths,
                LibraryEvent::EditLoaded {
                    root,
                    rel,
                    copy,
                    copies,
                    json,
                } => {
                    views::develop::edit_loaded(self, root, rel, copy, copies, json);
                }
                LibraryEvent::Copies(list) => self.note_copies(list),
                LibraryEvent::Catalog {
                    published,
                    settings,
                    complete,
                } => {
                    // After the first load the UI owns every setting but the
                    // library folders: a worker's echo of an earlier save
                    // mustn't undo a later change.
                    if self.settings_loaded {
                        self.settings.roots = settings.roots;
                    } else {
                        self.settings = settings;
                        self.settings_loaded = true;
                    }
                    self.apply_settings();
                    self.set_catalog(*published);
                    self.semantic.send(crate::semantic::Command::Catalog(
                        self.catalog.clone(),
                        complete,
                    ));
                    self.people.send(crate::people::Command::Catalog(
                        self.catalog.clone(),
                        complete,
                    ));
                    if complete {
                        self.scanning = false;
                        self.scan_progress = None;
                    }
                }
                LibraryEvent::Downloading { done, total } => {
                    self.download_progress = (done < total).then_some((done, total));
                }
                LibraryEvent::Progress { done, total } => {
                    self.scanning = true;
                    self.scan_progress = Some((done, total));
                }
                LibraryEvent::Events(events) => {
                    self.events = events;
                    if self
                        .selected_event
                        .as_ref()
                        .is_some_and(|id| !self.events.iter().any(|e| &e.id == id))
                    {
                        self.selected_event = None;
                    }
                    self.recompute_events();
                    self.refilter();
                }
                LibraryEvent::ImportProgress { done, total } => {
                    self.import.running = true;
                    self.import.progress = Some((done, total));
                }
                LibraryEvent::ImportPlanned(p) => {
                    self.import.running = false;
                    self.import.progress = None;
                    self.import.force.clear();
                    self.import.filter = None;
                    self.import.shown = None;
                    if p.is_none() {
                        self.import.message = Some("Cancelled; nothing was copied.".into());
                    }
                    self.import.folders = match &p {
                        Some(Ok(p)) => p.folders.clone(),
                        _ => Vec::new(),
                    };
                    self.import.plan = p;
                }
                LibraryEvent::ImportReplanned(p) => {
                    self.import.shown = None;
                    self.import.plan = Some(Ok(p));
                }
                LibraryEvent::ImportCopying {
                    done,
                    files,
                    bytes,
                    total_bytes,
                    current,
                } => {
                    let started = self
                        .import
                        .copying
                        .as_ref()
                        .map_or_else(std::time::Instant::now, |c| c.started);
                    self.import.copying = Some(Copying {
                        done,
                        files,
                        bytes,
                        total_bytes,
                        current,
                        started,
                    });
                }
                LibraryEvent::ImportCancelled { batch, copied } => {
                    self.import.copying = None;
                    self.import.plan = None;
                    self.import.folders.clear();
                    let m = format!(
                        "Cancelled after copying {copied} files (batch {batch}). Undo moves \
                         them to the Trash, or Find photos again to finish: what's copied is \
                         then already in the library and still gets its folder names."
                    );
                    self.import.message = Some(m.clone());
                    self.notice = Some(m);
                }
                LibraryEvent::Imported {
                    batch,
                    copied,
                    named,
                } => {
                    self.import.copying = None;
                    self.import.plan = None;
                    self.import.folders.clear();
                    let names = if named > 0 {
                        format!("; {named} given their folder's name")
                    } else {
                        String::new()
                    };
                    let m = format!(
                        "Imported {copied} files (batch {batch}){names}. Undo moves the copies to the Trash."
                    );
                    self.import.message = Some(m.clone());
                    self.notice = Some(m);
                }
                LibraryEvent::OrgPreview(p) => self.org.preview = Some(p),
                LibraryEvent::Notice(n) => self.notice = Some(n),
                LibraryEvent::Organised { batch, files, verb } => {
                    self.org.preview = None;
                    self.notice = Some(format!(
                        "{verb}: {files} files (batch {batch}). ⌘Z undoes it."
                    ));
                }
                LibraryEvent::Collections(list) => {
                    self.collections = list;
                    if self
                        .selected_collection
                        .as_ref()
                        .is_some_and(|id| !self.collections.iter().any(|c| &c.id == id))
                    {
                        self.selected_collection = None;
                    }
                    self.recompute_collections();
                    self.refilter();
                }
                LibraryEvent::Batches(b) => self.history.batches = b,
                LibraryEvent::BatchOps(id, ops) => {
                    self.history.open.insert(id, ops);
                }
                LibraryEvent::ImportEnded => self.import.copying = None,
                LibraryEvent::Ejected(r) => match r {
                    Ok(name) => {
                        self.import.volume = None;
                        self.notice = Some(format!("{name} was ejected; it's safe to remove."));
                    }
                    Err(e) => self.error = Some(e),
                },
                LibraryEvent::BatchResumed(r) => {
                    self.history.message = Some(r.map(|n| {
                        format!("Finished the interrupted operation ({n} steps left to run).")
                    }));
                }
                LibraryEvent::BatchUndone(r) => {
                    self.history.message = Some(r.map(|u| format!("Undone (as batch {u}).")));
                }
                LibraryEvent::Duplicates(r) => {
                    self.dupes.running = false;
                    self.dupes.progress = None;
                    // Groups are renumbered: go back to the list.
                    if self.dupes.open.take().is_some()
                        && matches!(self.view, View::Compare | View::Survey)
                    {
                        self.view = View::Duplicates;
                    }
                    self.dupes.review = Some(r);
                }
                LibraryEvent::DupeProgress { done, total } => {
                    self.dupes.running = true;
                    self.dupes.progress = Some((done, total));
                }
                LibraryEvent::DupePreview(p) => self.dupes.preview = Some(p),
                LibraryEvent::DupesApplied { batch, moved } => {
                    self.dupes.preview = None;
                    // Done: keep only the "keep" decisions.
                    self.dupes.decisions.retain(|_, d| *d == Decision::Keep);
                    self.dupes.message = Some(match batch {
                        Some(b) => {
                            format!("Moved {moved} files to _duplicates (batch {b}). ⌘Z undoes it.")
                        }
                        None => "Applied.".to_owned(),
                    });
                    self.dupes.running = true; // the search re-runs
                }
                LibraryEvent::Changes(changes) => {
                    let catalog = Arc::make_mut(&mut self.catalog);
                    for (path, change) in changes {
                        if let Some(&i) = self.index_of.get(&photo_id(&path)) {
                            catalog.photos[i].change = change;
                        }
                    }
                }
                LibraryEvent::History {
                    undo_at,
                    redo_at,
                    fresh,
                } => {
                    self.undo_at.library = (undo_at, redo_at);
                    if fresh {
                        // A new action: nothing of People's to redo either.
                        self.people.send(crate::people::Command::ClearRedo);
                    }
                    self.update_can_undo();
                }
                LibraryEvent::Error(e) => {
                    self.scanning = false;
                    self.error = Some(e);
                }
            }
        }
        let mut fetched = Vec::new();
        for loaded in self.loader.drain() {
            match loaded.result {
                Ok(img) => {
                    if let Some(h) = loaded.histogram {
                        self.histograms.insert(loaded.key.photo, h);
                    }
                    self.textures.insert(ctx, loaded.key, &img);
                    // Opening a cloud-only photo downloaded it: read it now.
                    if loaded.key.size == Size::Full
                        && self.fetching.remove(&loaded.key.photo)
                        && let Some(p) = self.photo(loaded.key.photo)
                    {
                        fetched.push(p.path.clone());
                    }
                }
                Err(e) => {
                    if e != crate::jobs::IN_CLOUD {
                        tracing::warn!("{e}");
                    }
                    self.fetching.remove(&loaded.key.photo);
                    self.textures.failed.insert(loaded.key, e);
                }
            }
        }
        if !fetched.is_empty() {
            self.library.send(LibraryCommand::Download(fetched));
        }
    }

    /// Swap in a catalog the library worker prepared (see `jobs::Published`;
    /// everything proportional to the library size happens there).
    fn set_catalog(&mut self, p: crate::jobs::Published) {
        self.ids = p.ids;
        self.index_of = p.index_of;
        self.tree = p.tree;
        self.catalog = p.catalog;
        self.all_keywords = p.keywords;
        self.cameras = p.cameras;
        self.lenses = p.lenses;
        // Likely edits are worked out again from the new catalog.
        self.idle_stale = true;
        // Drop what belongs to photos that are gone, and the images of
        // files that changed on disk so they reload.
        let stale = p.stale;
        // Copies of photos that are gone go too.
        let index_of = &self.index_of;
        self.copies
            .retain(|path, _| index_of.contains_key(&crate::jobs::photo_id(path)));
        self.copy_tiles
            .retain(|_, (path, _)| index_of.contains_key(&crate::jobs::photo_id(path)));
        let copy_tiles = &self.copy_tiles;
        // A copy's tile goes with its file.
        let file = |id: &PhotoId| {
            copy_tiles
                .get(id)
                .map_or(*id, |(path, _)| crate::jobs::photo_id(path))
        };
        let keep = |id: &PhotoId| index_of.contains_key(&file(id)) && !stale.contains(&file(id));
        self.textures.retain_photos(keep);
        self.histograms.retain(|id, _| keep(id));
        self.textures.failed.clear();
        self.cloud_count = self.catalog.photos.iter().filter(|p| p.cloud).count();
        self.selection.retain(|id| keep(id));
        if self.current.is_some_and(|c| !keep(&c)) {
            self.current = None;
        }
        self.siblings_of = None;
        self.recompute_events();
        self.recompute_collections();
        self.refilter();
    }

    /// Open the Settings window.
    pub fn open_settings(&mut self) {
        self.settings_ui.open(&self.settings);
    }

    pub fn settings_open(&self) -> bool {
        self.settings_ui.open
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether the map would fetch tiles (tests).
    pub fn map_tiles_on(&self) -> bool {
        self.map.tile_cache.is_some()
    }

    /// Use and save changed settings (library folders change through their
    /// own commands).
    pub(crate) fn save_settings(&mut self, s: Settings) {
        self.settings = s.clone();
        self.apply_settings();
        self.command(LibraryCommand::SaveSettings(Box::new(s)));
    }

    /// A High efficiency NEF: Develop needs a DNG of it.
    pub(crate) fn needs_dng(&self, idx: usize) -> bool {
        let p = &self.catalog.photos[idx];
        p.format == shoebox_decode::Format::Nef
            && p.meta
                .exif
                .nef_compression
                .is_some_and(|c| shoebox_decode::nef::NefCompression::from_tag(c).needs_dng())
    }

    /// After a while without input, convert likely edits (when the setting
    /// is on); input stops it after the file being converted.
    fn tick_idle(&mut self, ctx: &egui::Context) {
        let active = ctx.input(|i| !i.events.is_empty() || i.pointer.delta() != egui::Vec2::ZERO);
        if active {
            self.last_input = Instant::now();
            if std::mem::take(&mut self.converting_idle) {
                self.developer.convert_ahead(Vec::new());
            }
            return;
        }
        if !self.settings.preconvert {
            return;
        }
        if self.converting_idle {
            if std::mem::take(&mut self.idle_stale) {
                self.developer.convert_ahead(self.likely_edits());
            }
            return;
        }
        let wait = self.idle_after.saturating_sub(self.last_input.elapsed());
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        self.converting_idle = true;
        self.idle_stale = false;
        let list = self.likely_edits();
        tracing::info!("idle: converting up to {} likely edits", list.len());
        self.developer.convert_ahead(list);
    }

    /// High efficiency NEFs likely to be edited: picks and 3 stars and up,
    /// outside reference folders, newest first, as many as fill half the
    /// DNG cache (about 30 MB each).
    fn likely_edits(&self) -> Vec<PathBuf> {
        let limit = self
            .raw_config
            .as_ref()
            .map_or(shoebox_develop::dng::DEFAULT_LIMIT, |r| r.1);
        let room = (limit / 2 / (30 << 20)).max(1) as usize;
        let mut likely: Vec<usize> = (0..self.catalog.photos.len())
            .filter(|&i| {
                let m = &self.catalog.photos[i].marks;
                (m.pick == Some(shoebox_core::marks::Pick::Pick) || m.rating >= 3)
                    && !self.catalog.is_reference(i)
                    && self.needs_dng(i)
            })
            .collect();
        likely.sort_by(|&a, &b| {
            let t = |i: usize| self.catalog.photos[i].meta.exif.capture_time.clone();
            t(b).cmp(&t(a))
        });
        likely
            .into_iter()
            .take(room)
            .map(|i| self.catalog.photos[i].path.clone())
            .collect()
    }

    /// Count input-free time as idle after this long (tests).
    pub fn set_idle_after(&mut self, d: Duration) {
        self.idle_after = d;
    }

    /// Apply the settings the UI uses directly.
    fn apply_settings(&mut self) {
        self.textures.set_budget(self.settings.image_memory_bytes());
        let gb = match self.settings.dng_cache_gb {
            0 => shoebox_core::settings::DEFAULT_DNG_CACHE_GB,
            g => g,
        };
        let raw = (self.settings.dng_converter.clone(), u64::from(gb) << 30);
        if self.raw_config.as_ref() != Some(&raw) {
            self.developer.configure(raw.0.clone(), raw.1);
            self.raw_config = Some(raw);
            self.raw_status = None;
        }
        let tiles = if self.settings.offline_map {
            None
        } else {
            self.tile_cache.clone()
        };
        if self.map.tile_cache != tiles {
            self.map = views::map::State::new(tiles);
        }
        self.sem
            .set_enabled(&self.semantic, self.settings.semantic_search);
        self.ppl.set_enabled(&self.people, self.settings.people);
    }

    // --- people --------------------------------------------------------------

    fn tick_people(&mut self) {
        use crate::people::Event as E;
        let mut changed = false;
        while let Ok(ev) = self.people.events.try_recv() {
            match ev {
                E::Status(s) => self.ppl.status = s,
                E::People(v) => {
                    // An unnamed group shown may have been named, merged or
                    // regrouped: follow its face (merged ones become one).
                    let follow = |key: crate::people::GroupKey| match &key {
                        crate::people::GroupKey::Unnamed(face) if v.find(&key).is_none() => v
                            .groups
                            .iter()
                            .find(|g| g.faces.contains(face))
                            .map(|g| g.key.clone()),
                        _ => v.find(&key).map(|_| key),
                    };
                    let showing = self.ppl.showing.take().and_then(follow);
                    let mut also: Vec<_> = std::mem::take(&mut self.ppl.also)
                        .into_iter()
                        .filter_map(follow)
                        .filter(|k| Some(k) != showing.as_ref())
                        .collect();
                    also.dedup();
                    self.ppl.showing =
                        showing.or_else(|| (!also.is_empty()).then(|| also.remove(0)));
                    self.ppl.also = also;
                    self.ppl.view = Some(v);
                    // New groups: the People list's counts are worked out
                    // again with the photos shown.
                    changed = true;
                }
                E::Error(e) => self.error = Some(e),
                E::History { undo_at, redo_at } => {
                    self.undo_at.people = (undo_at, redo_at);
                    self.update_can_undo();
                }
            }
        }
        if changed {
            self.refilter();
        }
    }

    /// Show the photos of this person (or unnamed group).
    pub(crate) fn show_person(&mut self, key: crate::people::GroupKey) {
        self.ppl.showing = Some(key);
        self.ppl.also.clear();
        self.ppl.name_input.clear();
        self.sem.similar = None;
        self.sem.tag = None;
        if !matches!(self.view, View::Grid | View::Masonry) {
            self.set_view(View::Grid);
        }
        self.refilter();
        self.scroll_to_current = true;
    }

    fn update_can_undo(&mut self) {
        let (l, p) = (self.undo_at.library, self.undo_at.people);
        self.can_undo = l.0.is_some() || p.0.is_some();
        self.can_redo = l.1.is_some() || p.1.is_some();
    }

    /// Undo the newest change, whether to files or to names (⌘Z).
    pub fn undo(&mut self) {
        let (l, p) = (self.undo_at.library.0, self.undo_at.people.0);
        // Until the stack reports back, what was just undone is the next to
        // redo (so a quick ⇧⌘Z goes to the right stack).
        let now = Some(events::now_ms());
        if p.is_some() && p >= l {
            self.people.send(crate::people::Command::Undo);
            self.undo_at.people.1 = now;
        } else if l.is_some() {
            self.library.send(LibraryCommand::Undo);
            self.undo_at.library.1 = now;
        }
    }

    /// Redo the change undone most recently (⇧⌘Z).
    pub fn redo(&mut self) {
        let (l, p) = (self.undo_at.library.1, self.undo_at.people.1);
        // …and what was just redone is the next to undo.
        let now = Some(events::now_ms());
        if p.is_some() && p >= l {
            self.people.send(crate::people::Command::Redo);
            self.undo_at.people.0 = now;
        } else if l.is_some() {
            self.library.send(LibraryCommand::Redo);
            self.undo_at.library.0 = now;
        }
    }

    /// A change to names: the library's redo goes (a new action).
    fn people_command(&mut self, c: crate::people::Command) {
        self.people.send(c);
        self.library.send(LibraryCommand::ClearRedo);
    }

    pub(crate) fn clear_person(&mut self) {
        self.ppl.showing = None;
        self.ppl.also.clear();
        self.refilter();
    }

    /// ⌘-click on a person: show their photos too, or stop.
    pub(crate) fn toggle_person(&mut self, key: crate::people::GroupKey) {
        if self.ppl.showing.is_none() {
            return self.show_person(key);
        }
        if self.ppl.showing.as_ref() == Some(&key) {
            self.ppl.showing = (!self.ppl.also.is_empty()).then(|| self.ppl.also.remove(0));
        } else if let Some(k) = self.ppl.also.iter().position(|k| k == &key) {
            self.ppl.also.remove(k);
        } else {
            self.ppl.also.push(key);
        }
        self.ppl.name_input.clear();
        self.refilter();
    }

    /// Name these groups as one person (merging them; a name someone
    /// already has merges into them). Taught to the face matching: their
    /// faces become that person's examples.
    pub(crate) fn name_person(&mut self, groups: Vec<crate::people::GroupKey>, name: String) {
        self.people_command(crate::people::Command::Name { groups, name });
    }

    /// The faces of `photo` in the group shown aren't that person.
    pub(crate) fn not_person(&mut self, photo: PhotoId) {
        let Some(g) = self.ppl.shown_group() else {
            return;
        };
        let crate::people::GroupKey::Person(person) = &g.key else {
            return;
        };
        let Some(path) = self.photo(photo).map(|p| p.path.clone()) else {
            return;
        };
        let faces: Vec<_> = g.faces.iter().filter(|f| f.path == path).cloned().collect();
        let person = person.clone();
        self.people_command(crate::people::Command::NotPerson { person, faces });
    }

    pub(crate) fn forget_person(&mut self, person: String) {
        self.people_command(crate::people::Command::Forget { person });
        self.ppl.showing = None;
        self.refilter();
    }

    /// Add `keyword` to every photo shown (the photos of a person).
    pub(crate) fn keyword_shown(&mut self, keyword: &str) {
        let primaries: Vec<PhotoId> = self
            .shown
            .iter()
            .filter(|t| t.id == self.ids[t.idx])
            .filter(|t| self.catalog.stacks[self.catalog.stack_of[t.idx]].primary == t.idx)
            .map(|t| t.id)
            .collect();
        self.selection = primaries.iter().copied().collect();
        if self.current.is_none_or(|c| !self.selection.contains(&c)) {
            self.current = primaries.first().copied();
        }
        self.add_keyword(keyword);
    }

    /// People in a photo (its stack's primary): group index and label.
    pub(crate) fn people_in(&self, id: PhotoId) -> Vec<(crate::people::GroupKey, String)> {
        let (Some(view), Some(i)) = (&self.ppl.view, self.index(id)) else {
            return Vec::new();
        };
        let primary = self.catalog.stacks[self.catalog.stack_of[i]].primary;
        view.by_photo
            .get(&self.ids[primary])
            .into_iter()
            .flatten()
            .map(|&g| {
                let g = &view.groups[g];
                (g.key.clone(), self.ppl.label(g))
            })
            .collect()
    }

    /// People status (tests and the settings window).
    pub fn people_status(&self) -> &crate::semantic::Status {
        &self.ppl.status
    }

    /// Groups as (label, photo count), in the order listed (tests).
    /// The People list as shown: groups with photos among those shown, and
    /// how many (tests).
    pub fn people_listed(&self) -> Vec<(String, usize)> {
        self.ppl
            .view
            .as_ref()
            .map(|v| {
                v.groups
                    .iter()
                    .zip(&self.ppl.counts)
                    .filter(|(_, n)| **n > 0)
                    .map(|(g, n)| (self.ppl.label(g), *n))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn people_groups(&self) -> Vec<(String, usize)> {
        self.ppl
            .view
            .as_ref()
            .map(|v| {
                v.groups
                    .iter()
                    .map(|g| (self.ppl.label(g), g.photos.len()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn retry_people(&mut self) {
        self.ppl.retry(&self.people);
    }

    pub(crate) fn cancel_people_download(&mut self) {
        self.people.send(crate::people::Command::CancelDownload);
        let mut s = self.settings.clone();
        s.people = false;
        self.save_settings(s);
    }

    // --- semantic search -----------------------------------------------------

    /// The semantic query the UI is asking for, if any.
    pub(crate) fn semantic_query(&self) -> Option<crate::semantic::Query> {
        use crate::semantic::Query;
        if !self.settings.semantic_search {
            return None;
        }
        if let Some((_, p)) = &self.sem.similar {
            return Some(Query::Similar(p.clone()));
        }
        if let Some(t) = self.sem.tag {
            return Some(Query::Tag(t));
        }
        let t = self.filter.search.trim();
        (self.sem.by_content && !t.is_empty()).then(|| Query::Text(t.to_owned()))
    }

    /// Show photos that look like this one (from the next frame: this is
    /// called from inside the grid, which `shown` must not change under).
    pub(crate) fn find_similar(&mut self, id: PhotoId) {
        self.sem.similar_request = Some(id);
    }

    fn start_similar(&mut self, id: PhotoId) {
        let Some(i) = self.index(id) else { return };
        let primary = self.catalog.stacks[self.catalog.stack_of[i]].primary;
        self.sem.similar = Some((self.ids[primary], self.catalog.photos[primary].path.clone()));
        self.sem.tag = None;
        self.ppl.showing = None;
        self.filter.search.clear();
        self.sem.results = None;
        if !matches!(self.view, View::Grid | View::Masonry) {
            self.set_view(View::Grid);
        }
        self.refilter();
    }

    /// Show the photos with this suggested tag.
    pub(crate) fn show_tag(&mut self, label: u16) {
        self.sem.tag = Some(label);
        self.sem.similar = None;
        self.ppl.showing = None;
        self.sem.results = None;
        if !matches!(self.view, View::Grid | View::Masonry) {
            self.set_view(View::Grid);
        }
        self.refilter();
        self.scroll_to_current = true;
    }

    pub(crate) fn clear_tag(&mut self) {
        self.sem.tag = None;
        self.sem.results = None;
        self.refilter();
    }

    /// Suggested tags of a photo (its stack's primary), best first, without
    /// those it already has as keywords.
    pub(crate) fn suggested_tags(&self, id: PhotoId) -> Vec<String> {
        let (Some(tags), Some(i)) = (&self.sem.tags, self.index(id)) else {
            return Vec::new();
        };
        let primary = self.catalog.stacks[self.catalog.stack_of[i]].primary;
        let have = &self.catalog.photos[primary].marks.keywords;
        tags.by_photo
            .get(&self.ids[primary])
            .into_iter()
            .flatten()
            .map(|&(l, _)| tags.names[l as usize].clone())
            .filter(|n| !have.iter().any(|k| k.eq_ignore_ascii_case(n)))
            .collect()
    }

    /// Things: (tag, photos), most photos first (tests).
    pub fn things(&self) -> Vec<(String, usize)> {
        self.sem
            .tags
            .as_ref()
            .map(|t| {
                self.sem
                    .counts
                    .iter()
                    .map(|&(l, n)| (t.names[l as usize].clone(), n))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Suggested tags of a photo (tests).
    pub fn tags_of(&self, file_name: &str) -> Vec<String> {
        self.catalog
            .photos
            .iter()
            .position(|p| p.path.file_name().is_some_and(|n| n == file_name))
            .map(|i| self.suggested_tags(self.ids[i]))
            .unwrap_or_default()
    }

    /// Stop showing similar photos.
    pub(crate) fn clear_similar(&mut self) {
        self.sem.similar = None;
        self.sem.results = None;
        self.refilter();
    }

    /// Semantic search status (tests and the settings window).
    pub fn semantic_status(&self) -> &crate::semantic::Status {
        &self.sem.status
    }

    pub(crate) fn retry_semantic(&mut self) {
        self.sem.retry(&self.semantic);
    }

    pub(crate) fn cancel_model_download(&mut self) {
        self.semantic.send(crate::semantic::Command::CancelDownload);
        let mut s = self.settings.clone();
        s.semantic_search = false;
        self.save_settings(s);
    }

    fn tick_semantic(&mut self, ctx: &egui::Context) {
        use crate::semantic::Event as E;
        if let Some(id) = self.sem.similar_request.take() {
            self.start_similar(id);
        }
        let mut changed = false;
        while let Ok(ev) = self.semantic.events.try_recv() {
            match ev {
                E::Status(s) => self.sem.status = s,
                E::Results { id, hits } => {
                    if let Some(q) = self.sem.answered(id) {
                        let scores = hits
                            .into_iter()
                            .map(|(p, s)| (crate::jobs::photo_id(&p), s))
                            .collect();
                        self.sem.results = Some((q, scores));
                        self.sem.error = None;
                        changed = true;
                    }
                }
                E::Failed { id, error } => {
                    if let Some(q) = self.sem.answered(id) {
                        self.sem.results = Some((q, HashMap::new()));
                        self.sem.error = Some(error);
                        changed = true;
                    }
                }
                E::Tags(t) => {
                    if self.sem.tag.is_some_and(|l| t.names.len() <= l as usize) {
                        self.sem.tag = None;
                    }
                    self.sem.tags = Some(t);
                    if self.sem.tag.is_some() {
                        self.sem.results = None; // recount
                    }
                    changed = true; // Things counts the photos in view
                }
            }
        }
        self.sem.typed(&self.filter.search);
        let want = self.semantic_query();
        if want.is_none() && self.sem.results.is_some() {
            self.sem.results = None;
            changed = true;
        }
        if let Some(crate::semantic::Query::Tag(l)) = want {
            // Answered here, from the tags.
            let q = crate::semantic::Query::Tag(l);
            if let Some(tags) = &self.sem.tags
                && self.sem.results.as_ref().is_none_or(|(r, _)| *r != q)
            {
                let scores = tags
                    .by_photo
                    .iter()
                    .filter_map(|(id, t)| t.iter().find(|x| x.0 == l).map(|x| (*id, x.1)))
                    .collect();
                self.sem.results = Some((q, scores));
                changed = true;
            }
            self.sem.request(&self.semantic, None);
        } else if let Some(wait) = self.sem.request(&self.semantic, want) {
            ctx.request_repaint_after(wait);
        }
        if changed {
            self.refilter();
            self.scroll_to_current = true;
        }
    }

    /// Stop showing a library folder (nothing on disk changes).
    pub(crate) fn remove_root(&mut self, path: PathBuf) {
        self.scanning = true;
        if self.folders.iter().any(|f| f.starts_with(&path)) {
            self.set_folders(Vec::new());
        }
        self.command(LibraryCommand::RemoveRoot(path));
    }

    /// Room at the left of the top bar for the window buttons: on macOS
    /// the bar is the title bar (except in full screen).
    pub(crate) fn titlebar_inset(&self, ctx: &egui::Context) -> f32 {
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if cfg!(target_os = "macos") && self.display.is_some() && !full {
            76.0
        } else {
            6.0
        }
    }

    /// A new source picked in the sidebar: nothing selected, and back to
    /// the grid (or the browsing view the loupe was opened from).
    pub(crate) fn new_source(&mut self) {
        self.selection.clear();
        self.current = None;
        self.anchor = None;
        let browsing = |v| matches!(v, View::Grid | View::Masonry | View::Timeline | View::Map);
        if !browsing(self.view) {
            self.set_view(if browsing(self.back_view) {
                self.back_view
            } else {
                View::Grid
            });
        }
    }

    /// "All photos": no folder, event, collection, person, tag or similar.
    pub(crate) fn show_everything(&mut self) {
        self.new_source();
        self.selected_event = None;
        self.selected_collection = None;
        self.ppl.showing = None;
        self.sem.tag = None;
        self.sem.similar = None;
        self.set_folders(Vec::new());
    }

    /// Whether the photos shown are fewer than all of them: a folder,
    /// event, collection, person, tag, similar or a filter.
    pub(crate) fn narrowed(&self) -> bool {
        self.filtered()
            || self.selected_event.is_some()
            || self.selected_collection.is_some()
            || self.ppl.showing.is_some()
            || self.sem.tag.is_some()
            || self.sem.similar.is_some()
    }

    /// Show this photo among all photos, by capture time in the grid, so
    /// the ones taken around it are beside it (from the next frame: this is
    /// called from inside the views, which `shown` must not change under).
    pub(crate) fn show_in_all(&mut self, id: PhotoId) {
        self.show_in_all_request = Some(id);
    }

    fn start_show_in_all(&mut self, id: PhotoId) {
        let Some(i) = self.index(id) else { return };
        self.filter = query::Filter::default();
        self.show_everything();
        if self.view != View::Grid {
            self.set_view(View::Grid);
        }
        if self.sort != query::Sort::CaptureTime {
            self.sort = query::Sort::CaptureTime;
            self.refilter();
        }
        // A file of a folded stack has no tile of its own: its stack's.
        let shown = |a: &Self, id| a.shown.iter().any(|t| t.id == id);
        let id = if shown(self, id) {
            id
        } else {
            self.ids[self.stack_of(i).primary]
        };
        self.activate(id);
        self.scroll_to_current = true;
    }

    /// Show these folders. The grid shows one source at a time: folders,
    /// an event or a collection, so choosing folders clears the others.
    pub(crate) fn set_folders(&mut self, folders: Vec<PathBuf>) {
        if !folders.is_empty() {
            self.selected_event = None;
            self.selected_collection = None;
        }
        self.folders = folders;
        self.refilter();
        self.scroll_to_current = true;
    }

    /// A click on a folder: just this one (or none, if it was the only one
    /// shown); with ⌘ or Shift, add it to the folders shown or take it out.
    pub(crate) fn click_folder(&mut self, path: &Path, add: bool) {
        self.new_source();
        let mut folders = self.folders.clone();
        if add {
            if let Some(k) = folders.iter().position(|f| f == path) {
                folders.remove(k);
            } else {
                folders.push(path.to_path_buf());
            }
        } else if folders == [path] {
            folders.clear();
        } else {
            folders = vec![path.to_path_buf()];
        }
        self.set_folders(folders);
    }

    /// Rebuild the visible items: one per stack passing the filter (its
    /// primary), followed by its other members when expanded.
    pub(crate) fn refilter(&mut self) {
        let mut f = self.filter.clone();
        if let [one] = self.folders.as_slice() {
            f.folder = Some(one.clone());
        }
        let semantic = self.semantic_query().is_some();
        if semantic {
            f.search.clear(); // the text is a description, not a name
        }
        let mut stacks = query::run(&self.catalog, &f, self.sort, self.sort_desc);
        if self.folders.len() > 1 {
            stacks.retain(|&s| {
                self.catalog.stacks[s].all().any(|i| {
                    let p = &self.catalog.photos[i].path;
                    self.folders.iter().any(|f| p.starts_with(f))
                })
            });
        }
        if let Some(members) = self
            .selected_event
            .as_ref()
            .and_then(|id| self.event_members.get(id))
        {
            stacks.retain(|s| members.contains(s));
        }
        if let Some(members) = self
            .selected_collection
            .as_ref()
            .and_then(|id| self.collection_members.get(id))
        {
            stacks.retain(|s| members.contains(s));
        }
        // Each tag's photos among these (the Things list's counts).
        if let Some(tags) = &self.sem.tags {
            let mut n = vec![0usize; tags.names.len()];
            for &s in &stacks {
                let id = self.ids[self.catalog.stacks[s].primary];
                for &(l, _) in tags.by_photo.get(&id).map_or(&[][..], Vec::as_slice) {
                    if let Some(c) = n.get_mut(l as usize) {
                        *c += 1;
                    }
                }
            }
            let mut counts: Vec<(u16, usize)> = n
                .into_iter()
                .enumerate()
                .filter(|&(_, c)| c > 0)
                .map(|(l, c)| (l as u16, c))
                .collect();
            counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.sem.counts = counts;
        }
        if semantic {
            // Best match first; nothing until the first results are in.
            let empty = HashMap::new();
            let scores = self.sem.results.as_ref().map_or(&empty, |(_, s)| s);
            let score = |s: usize| {
                scores
                    .get(&self.ids[self.catalog.stacks[s].primary])
                    .copied()
            };
            stacks.retain(|&s| score(s).is_some());
            stacks.sort_by(|&a, &b| score(b).unwrap_or(0.0).total_cmp(&score(a).unwrap_or(0.0)));
        }
        // Each person's photos among these (the People list's counts), then
        // the people chosen narrow them further.
        if let Some(view) = self.ppl.view.as_ref() {
            let here: HashSet<PhotoId> = stacks
                .iter()
                .map(|&s| self.ids[self.catalog.stacks[s].primary])
                .collect();
            self.ppl.counts = view
                .groups
                .iter()
                .map(|g| g.photos.iter().filter(|p| here.contains(p)).count())
                .collect();
        }
        let people = self.ppl.shown_groups();
        if !people.is_empty() {
            let photos: HashSet<PhotoId> = people
                .iter()
                .flat_map(|g| g.photos.iter().copied())
                .collect();
            stacks.retain(|&s| photos.contains(&self.ids[self.catalog.stacks[s].primary]));
        }
        self.shown.clear();
        for s in stacks {
            let st = &self.catalog.stacks[s];
            let file = |i: usize| Tile {
                idx: i,
                id: self.ids[i],
            };
            self.shown.push(file(st.primary));
            if self.expanded.contains(&self.ids[st.primary]) {
                self.shown.extend(st.all().skip(1).map(file));
                // Then the files' virtual copies.
                for i in st.all() {
                    let p = &self.catalog.photos[i].path;
                    for c in self.copies.get(p).map_or(&[][..], Vec::as_slice) {
                        self.shown.push(Tile {
                            idx: i,
                            id: crate::jobs::copy_id(self.ids[i], &c.id),
                        });
                    }
                }
            }
        }
        self.shown_gen += 1;
    }

    fn recompute_collections(&mut self) {
        self.collection_members = self
            .collections
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    shoebox_core::collections::members(&self.catalog, c),
                )
            })
            .collect();
    }

    fn recompute_events(&mut self) {
        self.event_members = events::memberships(&self.catalog, &self.events);
        self.event_spans = self
            .event_members
            .iter()
            .filter_map(|(id, stacks)| {
                let times = stacks.iter().filter_map(|&s| {
                    self.catalog.photos[self.catalog.stacks[s].primary]
                        .meta
                        .exif
                        .capture_time
                        .as_deref()
                });
                let (lo, hi) =
                    times.fold((None, None), |(lo, hi): (Option<&str>, Option<&str>), t| {
                        (
                            Some(lo.map_or(t, |l| l.min(t))),
                            Some(hi.map_or(t, |h| h.max(t))),
                        )
                    });
                // EXIF `YYYY:MM:DD HH:MM:SS` → `YYYY-MM-DD HH:MM:SS`.
                let fix = |t: &str| t.replacen(':', "-", 2);
                Some((id.clone(), (fix(lo?), fix(hi?))))
            })
            .collect();
    }

    /// Where an event sorts: its first photo's time, or (with no dated
    /// photos yet) when it was made.
    pub(crate) fn event_key(&self, e: &Event) -> String {
        self.event_spans.get(&e.id).map_or_else(
            || {
                jiff::Timestamp::from_millisecond(e.updated)
                    .map(|t| {
                        t.to_zoned(jiff::tz::TimeZone::system())
                            .strftime("%Y-%m-%d %H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_default()
            },
            |(lo, _)| lo.clone(),
        )
    }

    /// When an event's photos were taken: "24 Apr 2026" or a range.
    pub(crate) fn event_dates(&self, id: &str) -> Option<String> {
        let (lo, hi) = self.event_spans.get(id)?;
        let day = |t: &str| {
            jiff::civil::Date::strptime("%Y-%m-%d", t.get(..10)?)
                .ok()
                .map(|d| d.strftime("%-d %b %Y").to_string())
        };
        let (a, b) = (day(lo)?, day(hi)?);
        Some(if a == b { a } else { format!("{a} to {b}") })
    }

    /// Stacks of the selected photos (or the current one).
    pub(crate) fn selected_stacks(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .selection
            .iter()
            .chain(self.current.as_ref().filter(|_| self.selection.is_empty()))
            .filter_map(|id| self.index(*id))
            .map(|i| self.catalog.stack_of[i])
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Open the editor for a new event spanning the selection (Cmd+E).
    pub(crate) fn new_event_from_selection(&mut self, parent: Option<String>) {
        let stacks = self.selected_stacks();
        self.new_event_from(&stacks, parent);
    }

    /// Every stack the grid shows (a folder's photos, say).
    pub(crate) fn shown_stacks(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .shown
            .iter()
            .map(|t| self.catalog.stack_of[t.idx])
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Open the editor for a new event with these stacks in it.
    pub(crate) fn new_event_from(&mut self, stacks: &[usize], parent: Option<String>) {
        self.event_edit = Some(EventEdit {
            id: None,
            parent,
            name: String::new(),
            photos: self.paths_of(stacks),
            error: None,
        });
        self.left_open = true;
        self.collapsed.remove("Events"); // the editor is in that section
    }

    pub(crate) fn edit_event(&mut self, id: &str) {
        if let Some(e) = self.events.iter().find(|e| e.id == id) {
            self.event_edit = Some(EventEdit {
                id: Some(e.id.clone()),
                parent: e.parent.clone(),
                name: e.name.clone(),
                photos: Vec::new(),
                error: None,
            });
        }
    }

    /// The primaries' paths of these stacks.
    fn paths_of(&self, stacks: &[usize]) -> Vec<PathBuf> {
        stacks
            .iter()
            .map(|&s| {
                self.catalog.photos[self.catalog.stacks[s].primary]
                    .path
                    .clone()
            })
            .collect()
    }

    /// Validate and save the editor's event.
    pub(crate) fn save_event_edit(&mut self) {
        let Some(ed) = self.event_edit.as_mut() else {
            return;
        };
        let name = ed.name.trim().to_owned();
        if name.is_empty() {
            ed.error = Some("Give the event a name".into());
            return;
        }
        // The name is a keyword level (`Events|Japan Trip|Kyoto`).
        if name.contains('|') {
            ed.error = Some("Names can't contain |".into());
            return;
        }
        if self
            .events
            .iter()
            .any(|e| e.parent == ed.parent && e.name == name && Some(&e.id) != ed.id.as_ref())
        {
            ed.error = Some("There's already an event with that name here".into());
            return;
        }
        let ed = self.event_edit.take().unwrap_or_default();
        let mut list = self.events.clone();
        let change = match ed
            .id
            .as_ref()
            .and_then(|id| list.iter_mut().find(|e| &e.id == id))
        {
            Some(e) => {
                e.name = name.clone();
                e.updated = events::now_ms();
                None
            }
            None => {
                let id = events::new_id();
                list.push(Event {
                    id: id.clone(),
                    name: name.clone(),
                    parent: ed.parent.clone(),
                    updated: events::now_ms(),
                });
                (!ed.photos.is_empty()).then(|| events::EventChange {
                    event: id,
                    photos: ed.photos.clone(),
                    add: true,
                })
            }
        };
        self.save_events(list, change, &format!("event: {name}"));
    }

    /// Add the selection to an event, or take it out.
    pub(crate) fn event_include_selection(&mut self, id: &str, include: bool) {
        let stacks = self.selected_stacks();
        self.event_include(id, &stacks, include);
    }

    /// Add stacks to an event, or take them out (marked on the photos).
    pub(crate) fn event_include(&mut self, id: &str, stacks: &[usize], include: bool) {
        let Some(e) = self.events.iter().find(|e| e.id == id) else {
            return;
        };
        let label = format!(
            "{} event: {}",
            if include { "add to" } else { "remove from" },
            e.name
        );
        let change = events::EventChange {
            event: id.to_owned(),
            photos: self.paths_of(stacks),
            add: include,
        };
        self.save_events(self.events.clone(), Some(change), &label);
    }

    /// Delete an event; its sub-events move up to its parent.
    pub(crate) fn delete_event(&mut self, id: &str) {
        let Some(gone) = self.events.iter().find(|e| e.id == id).cloned() else {
            return;
        };
        let mut list: Vec<Event> = self.events.iter().filter(|e| e.id != id).cloned().collect();
        for e in list.iter_mut().filter(|e| e.parent.as_deref() == Some(id)) {
            e.parent = gone.parent.clone();
            e.updated = events::now_ms();
        }
        if self.selected_event.as_deref() == Some(id) {
            self.selected_event = None;
        }
        self.save_events(list, None, &format!("delete event: {}", gone.name));
    }

    fn save_events(&mut self, list: Vec<Event>, change: Option<events::EventChange>, label: &str) {
        self.library.send(LibraryCommand::SaveEvents {
            events: list,
            change,
            label: label.to_owned(),
        });
    }

    pub(crate) fn select_event(&mut self, id: Option<String>) {
        if id.is_some() {
            self.new_source();
            self.folders.clear();
            self.selected_collection = None;
        }
        self.selected_event = id;
        self.refilter();
        self.scroll_to_current = true;
    }

    /// The stack of photo `idx`.
    pub(crate) fn stack_of(&self, idx: usize) -> &shoebox_core::stacks::Stack {
        &self.catalog.stacks[self.catalog.stack_of[idx]]
    }

    /// Toggle the current photo's stack open/closed (S).
    /// Show one file of a stack (the info panel's list): its stack opens,
    /// so the file has its own tile, and it becomes the current photo (the
    /// one the loupe and info panel show).
    pub(crate) fn show_member(&mut self, idx: usize) {
        let primary = self.ids[self.stack_of(idx).primary];
        if self.expanded.insert(primary) {
            self.refilter();
        }
        self.activate(self.ids[idx]);
        self.scroll_to_current = true;
    }

    /// An open stack folds once the current photo is somewhere else (moving
    /// between its own files keeps it open).
    fn fold_stacks_left(&mut self) {
        if self.expanded.is_empty() {
            return;
        }
        let here = self
            .current
            .and_then(|c| self.index(c))
            .map(|i| self.ids[self.stack_of(i).primary]);
        let before = self.expanded.len();
        self.expanded.retain(|p| Some(*p) == here);
        if self.expanded.len() != before {
            self.refilter();
        }
    }

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

    // --- compare, survey and duplicate review --------------------------------

    /// Selected photos in display order.
    pub(crate) fn selected_in_order(&self) -> Vec<PhotoId> {
        self.shown
            .iter()
            .map(|t| t.id)
            .filter(|id| self.selection.contains(id))
            .collect()
    }

    /// Compare the first two selected photos, or the current one and the next.
    fn start_compare(&mut self) {
        let sel = self.selected_in_order();
        let (a, b) = if sel.len() >= 2 {
            (Some(sel[0]), Some(sel[1]))
        } else {
            let a = self.current.or_else(|| self.shown.first().map(|t| t.id));
            let next = a
                .and_then(|a| self.shown.iter().position(|t| t.id == a))
                .and_then(|p| self.shown.get(p + 1))
                .map(|t| t.id);
            (a, next)
        };
        self.compare = CompareState {
            select: a,
            candidate: b,
            ..CompareState::default()
        };
        if let Some(a) = a {
            self.current = Some(a);
        }
    }

    /// Photos Compare's arrows step through: the open group, else the grid.
    pub(crate) fn compare_pool(&self) -> Vec<PhotoId> {
        match self.open_group_ids() {
            Some(ids) => ids,
            None => self.shown.iter().map(|t| t.id).collect(),
        }
    }

    /// Step the candidate (never onto the select).
    pub(crate) fn step_candidate(&mut self, delta: isize) {
        let pool: Vec<PhotoId> = self
            .compare_pool()
            .into_iter()
            .filter(|&id| Some(id) != self.compare.select)
            .collect();
        if pool.is_empty() {
            return;
        }
        let pos = self
            .compare
            .candidate
            .and_then(|c| pool.iter().position(|&x| x == c))
            .map_or(0, |p| {
                (p as isize + delta).clamp(0, pool.len() as isize - 1) as usize
            });
        self.compare.candidate = Some(pool[pos]);
        self.activate(pool[pos]);
    }

    /// Make the candidate the select (Up arrow).
    pub(crate) fn swap_compare(&mut self) {
        let c = &mut self.compare;
        std::mem::swap(&mut c.select, &mut c.candidate);
    }

    /// The photo marks apply to in Compare and Survey.
    pub(crate) fn activate(&mut self, id: PhotoId) {
        self.current = Some(id);
        self.selection = BTreeSet::from([id]);
        self.anchor = Some(id);
    }

    /// Remove a photo from Survey (its X).
    pub(crate) fn survey_remove(&mut self, id: PhotoId) {
        self.survey.retain(|&x| x != id);
        if self.current == Some(id) {
            self.current = self.survey.first().copied();
        }
        self.selection.remove(&id);
    }

    /// Catalog ids of the open group's files, keeper first.
    pub(crate) fn open_group_ids(&self) -> Option<Vec<PhotoId>> {
        let r = self.dupes.review.as_ref()?;
        let g = r.groups.get(self.dupes.open?)?;
        Some(
            g.entries
                .iter()
                .map(|e| photo_id(&r.files[e.file].path))
                .filter(|id| self.index_of.contains_key(id))
                .collect(),
        )
    }

    pub fn find_duplicates(&mut self) {
        self.dupes.running = true;
        self.dupes.message = None;
        let only = self.scope_files(&self.dupes.scope).map(Arc::new);
        self.dupes.found_in = Some(self.dupes.scope.clone());
        self.library
            .send(LibraryCommand::FindDuplicates(self.dupes.config, only));
    }

    /// Find duplicates within an event, collection or folder (its menu).
    pub fn find_duplicates_in(&mut self, scope: DupeScope) {
        self.dupes.scope = scope;
        self.dupes.open = None;
        self.set_view(View::Duplicates);
        self.find_duplicates();
    }

    /// The files of a scope's photos (every file of their stacks); None for
    /// everything.
    pub(crate) fn scope_files(&self, scope: &DupeScope) -> Option<HashSet<PathBuf>> {
        let stacks = match scope {
            DupeScope::All => return None,
            DupeScope::Folder(f) => {
                return Some(
                    self.catalog
                        .photos
                        .iter()
                        .filter(|p| p.path.starts_with(f))
                        .map(|p| p.path.clone())
                        .collect(),
                );
            }
            DupeScope::Event(id) => self.event_members.get(id),
            DupeScope::Collection(id) => self.collection_members.get(id),
        };
        Some(
            stacks
                .into_iter()
                .flatten()
                .filter_map(|&s| self.catalog.stacks.get(s))
                .flat_map(|st| st.all())
                .map(|i| self.catalog.photos[i].path.clone())
                .collect(),
        )
    }

    /// "All photos", "Trip", "2026-04-24": what a scope is called.
    pub(crate) fn scope_name(&self, scope: &DupeScope) -> String {
        match scope {
            DupeScope::All => "All photos".to_owned(),
            DupeScope::Folder(f) => f.file_name().map_or_else(
                || f.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            DupeScope::Event(id) => self
                .events
                .iter()
                .find(|e| &e.id == id)
                .map_or_else(|| "(deleted event)".to_owned(), |e| e.name.clone()),
            DupeScope::Collection(id) => self
                .collections
                .iter()
                .find(|c| &c.id == id)
                .map_or_else(|| "(deleted collection)".to_owned(), |c| c.name.clone()),
        }
    }

    /// What the duplicates shown were found in (tests).
    pub fn duplicates_found_in(&self) -> Option<String> {
        self.dupes.found_in.as_ref().map(|s| self.scope_name(s))
    }

    pub(crate) fn decision(&self, path: &Path) -> Decision {
        self.dupes.decisions.get(path).copied().unwrap_or_default()
    }

    pub(crate) fn set_decision(&mut self, path: &Path, d: Decision) {
        self.dupes.decisions.insert(path.to_path_buf(), d);
    }

    /// Apply the auto-select rules to one group (None = every group).
    pub fn auto_select(&mut self, group: Option<usize>) {
        let Some(r) = self.dupes.review.clone() else {
            return;
        };
        for (gi, g) in r.groups.iter().enumerate() {
            if group.is_some_and(|x| x != gi) {
                continue;
            }
            for (e, d) in g
                .entries
                .iter()
                .zip(shoebox_core::review::auto_select(&r, g))
            {
                self.dupes.decisions.insert(r.files[e.file].path.clone(), d);
            }
        }
    }

    /// Open a group: Compare for two files, Survey for more.
    pub fn open_group(&mut self, g: usize) {
        self.dupes.open = Some(g);
        let ids = self.open_group_ids().unwrap_or_default();
        if ids.len() == 2 {
            self.compare = CompareState {
                select: Some(ids[0]),
                candidate: Some(ids[1]),
                ..CompareState::default()
            };
            self.view = View::Compare;
        } else {
            self.survey = ids.iter().copied().take(9).collect();
            self.view = View::Survey;
        }
        if let Some(&first) = ids.first() {
            self.activate(first);
        }
    }

    /// Next or previous group, staying in review.
    pub(crate) fn step_group(&mut self, delta: isize) {
        let Some(g) = self.dupes.open else { return };
        let visible = self.dupes.visible();
        let Some(pos) = visible.iter().position(|&x| x == g) else {
            return;
        };
        let next = (pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
        self.open_group(visible[next]);
    }

    pub(crate) fn close_group(&mut self) {
        self.dupes.open = None;
        self.view = View::Duplicates;
    }

    /// Ask the library worker for the dry-run table.
    pub fn request_apply(&mut self) {
        self.dupes.message = None;
        self.library.send(LibraryCommand::PreviewDuplicates(
            self.dupes.decisions.clone(),
        ));
    }

    /// The person confirmed the dry-run table.
    pub fn confirm_apply(&mut self) {
        self.dupes.preview = None;
        self.library.send(LibraryCommand::ApplyDuplicates(
            self.dupes.decisions.clone(),
        ));
    }

    /// Files marked to move (for the Apply button).
    pub(crate) fn marked_to_move(&self) -> usize {
        self.dupes
            .decisions
            .values()
            .filter(|d| **d == Decision::Move)
            .count()
    }

    // --- files: move, rename, reject, trash, import ----------------------------

    /// Files of the selected stacks (primaries stand for their stacks).
    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected_stacks()
            .into_iter()
            .map(|s| {
                self.catalog.photos[self.catalog.stacks[s].primary]
                    .path
                    .clone()
            })
            .collect()
    }

    /// Ask for the dry-run table of a move, rename, reject or trash.
    pub fn request_org(&mut self, req: OrgRequest) {
        if req.photos().is_empty() {
            return;
        }
        self.org.preview = None;
        self.library.send(LibraryCommand::PreviewOrganise(req));
    }

    /// The person confirmed the dry-run table.
    pub fn confirm_org(&mut self) {
        if let Some(Ok(p)) = self.org.preview.take() {
            self.library.send(LibraryCommand::RunOrganise(p.request));
        }
    }

    /// Cmd+Backspace: the selection to `_rejected/`.
    pub fn reject_selection(&mut self) {
        let photos = self.selected_paths();
        self.request_org(OrgRequest::Reject { photos });
    }

    /// Every rejected stack shown to `_rejected/`.
    pub(crate) fn reject_all_rejected(&mut self) {
        let photos = self.rejected_shown();
        self.request_org(OrgRequest::Reject { photos });
    }

    pub(crate) fn rejected_shown(&self) -> Vec<PathBuf> {
        self.shown
            .iter()
            .filter(|t| t.id == self.ids[t.idx]) // files, not copies
            .map(|t| &self.catalog.photos[t.idx])
            .filter(|p| p.marks.pick == Some(Pick::Reject))
            .map(|p| p.path.clone())
            .collect()
    }

    /// Open the rename dialog for the selection.
    pub fn start_rename(&mut self) {
        let photos = self.selected_paths();
        if !photos.is_empty() {
            self.org.rename = Some((photos, self.settings.rename_template().to_owned()));
        }
    }

    /// Move the selection into `folder` (after the dry-run table).
    pub fn move_selection_to(&mut self, folder: PathBuf) {
        let photos = self.selected_paths();
        self.request_org(OrgRequest::MoveTo { photos, folder });
    }

    pub(crate) fn photos_for(&self, ids: &[PhotoId]) -> Vec<PathBuf> {
        ids.iter()
            .filter_map(|&id| self.photo(id).map(|p| p.path.clone()))
            .collect()
    }

    pub fn plan_import(&mut self) {
        let (Some(source), Some(root)) = (self.import.source.clone(), self.import.root.clone())
        else {
            return;
        };
        self.import.plan = None;
        self.import.message = None;
        self.import.running = true;
        let template = if self.import.template.trim().is_empty() {
            self.settings.import_template().to_owned()
        } else {
            self.import.template.clone()
        };
        if template != self.settings.import_template() {
            let mut s = self.settings.clone();
            s.import_template = template.clone();
            self.settings = s.clone();
            self.library.send(LibraryCommand::SaveSettings(Box::new(s)));
        }
        self.library.send(LibraryCommand::PlanImport {
            source,
            root,
            template,
        });
    }

    pub fn run_import(&mut self) {
        let files = match &self.import.plan {
            Some(Ok(p)) => p.copies.len(),
            _ => 0,
        };
        self.import.volume = self.import.source.as_deref().and_then(ejectable_volume);
        self.import.copying = Some(Copying {
            done: 0,
            files,
            bytes: 0,
            total_bytes: 0,
            current: None,
            started: std::time::Instant::now(),
        });
        self.import.message = None;
        self.library
            .send(LibraryCommand::RunImport(self.import.folders.clone()));
    }

    /// Show the import as copying (`None`: not), for snapshots of the
    /// status (tests).
    #[doc(hidden)]
    pub fn show_copying(&mut self, c: Option<(usize, usize, u64, u64, PathBuf)>) {
        self.import.copying = c.map(|(done, files, bytes, total_bytes, current)| Copying {
            done,
            files,
            bytes,
            total_bytes,
            current: Some(current),
            started: std::time::Instant::now() - std::time::Duration::from_secs(60),
        });
    }

    /// Stop finding or copying photos.
    pub fn cancel_import(&mut self) {
        self.library.cancel_import();
    }

    /// Copy a left-out source file anyway (or leave it out again).
    pub(crate) fn force_import(&mut self, src: &Path, on: bool) {
        if on {
            self.import.force.insert(src.to_path_buf());
        } else {
            self.import.force.remove(src);
        }
        self.library
            .send(LibraryCommand::ReplanImport(self.import.force.clone()));
    }

    // --- history ---------------------------------------------------------------

    /// Open History at a batch (from a change badge's link).
    pub(crate) fn show_batch(&mut self, batch: i64) {
        self.history.focus = Some(batch);
        self.library.send(LibraryCommand::LoadBatch(batch));
        self.history.open.entry(batch).or_default();
        self.set_view(View::History);
    }

    pub(crate) fn toggle_batch(&mut self, batch: i64) {
        if self.history.open.remove(&batch).is_none() {
            self.history.open.insert(batch, Vec::new());
            self.library.send(LibraryCommand::LoadBatch(batch));
        }
    }

    pub fn resume_batch(&mut self, batch: i64) {
        self.history.message = None;
        self.library.send(LibraryCommand::ResumeBatch(batch));
    }

    pub fn undo_batch(&mut self, batch: i64) {
        self.history.message = None;
        self.library.send(LibraryCommand::UndoBatch(batch));
    }

    // --- collections -----------------------------------------------------------

    fn save_collections(&mut self, list: Vec<Collection>, label: &str) {
        self.collections = list.clone();
        self.recompute_collections();
        self.library.send(LibraryCommand::SaveCollections {
            list,
            label: label.to_owned(),
        });
    }

    /// A new collection holding the selection.
    pub fn new_collection(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let mut list = self.collections.clone();
        list.push(Collection {
            id: shoebox_core::collections::new_id(),
            name: name.to_owned(),
            members: self.selected_paths(),
            updated: events::now_ms(),
        });
        self.save_collections(list, &format!("collection: {name}"));
    }

    /// Add the selection to a collection (or take it out).
    pub(crate) fn collection_add_selection(&mut self, id: &str, add: bool) {
        let stacks = self.selected_stacks();
        self.collection_add(id, &stacks, add);
    }

    /// Add stacks to a collection (or take them out).
    pub(crate) fn collection_add(&mut self, id: &str, stacks: &[usize], add: bool) {
        let mut list = self.collections.clone();
        let Some(c) = list.iter_mut().find(|c| c.id == id) else {
            return;
        };
        let files: HashSet<PathBuf> = stacks
            .iter()
            .flat_map(|&s| self.catalog.stacks[s].all())
            .map(|i| self.catalog.photos[i].path.clone())
            .collect();
        c.members.retain(|p| !files.contains(p));
        if add {
            c.members.extend(stacks.iter().map(|&s| {
                self.catalog.photos[self.catalog.stacks[s].primary]
                    .path
                    .clone()
            }));
        }
        c.updated = events::now_ms();
        let label = format!("collection: {}", c.name);
        self.save_collections(list, &label);
        self.refilter();
    }

    pub(crate) fn rename_collection(&mut self, id: &str, name: &str) {
        let mut list = self.collections.clone();
        if let Some(c) = list.iter_mut().find(|c| c.id == id)
            && !name.trim().is_empty()
        {
            c.name = name.trim().to_owned();
            c.updated = events::now_ms();
            self.save_collections(list, &format!("rename collection: {}", name.trim()));
        }
    }

    pub(crate) fn delete_collection(&mut self, id: &str) {
        let Some(gone) = self.collections.iter().find(|c| c.id == id).cloned() else {
            return;
        };
        let list: Vec<Collection> = self
            .collections
            .iter()
            .filter(|c| c.id != id)
            .cloned()
            .collect();
        if self.selected_collection.as_deref() == Some(id) {
            self.selected_collection = None;
            self.refilter();
        }
        self.save_collections(list, &format!("delete collection: {}", gone.name));
    }

    pub(crate) fn select_collection(&mut self, id: Option<String>) {
        if id.is_some() {
            self.new_source();
            self.folders.clear();
            self.selected_event = None;
        }
        self.selected_collection = id;
        self.refilter();
        self.scroll_to_current = true;
    }

    /// Create a folder (the New Folder dialog's action; tests).
    pub fn new_folder(&self, dir: PathBuf) {
        self.library.send(LibraryCommand::NewFolder(dir));
    }

    pub fn set_import_source(&mut self, dir: PathBuf) {
        self.import.source = Some(dir);
        self.import.volume = None;
        self.import.plan = None;
    }

    /// The import plan: (copies, left out as already in the library), once
    /// planned.
    pub fn import_counts(&self) -> Option<(usize, usize)> {
        match &self.import.plan {
            Some(Ok(p)) => Some((
                p.copies.len(),
                shoebox_core::import::Plan::left_out(&p.skipped),
            )),
            _ => None,
        }
    }

    /// How many of the import plan's photos have their previews loaded
    /// (tests).
    pub fn import_previews(&self) -> usize {
        let Some(Ok(plan)) = &self.import.plan else {
            return 0;
        };
        plan.photos
            .iter()
            .filter(|p| {
                self.textures.contains(&Key {
                    photo: crate::jobs::photo_id(&p.src.path),
                    size: Size::Small,
                })
            })
            .count()
    }

    /// Is this photo in the cloud only, and has its metadata been read
    /// (tests)?
    pub fn cloud_state(&self, file_name: &str) -> Option<(bool, bool)> {
        self.catalog
            .photos
            .iter()
            .find(|p| p.path.file_name().is_some_and(|n| n == file_name))
            .map(|p| (p.cloud, p.meta.exif.capture_time.is_some()))
    }

    /// The import's folders and what each name becomes (tests).
    pub fn import_folders(&self) -> Vec<(String, shoebox_core::import::FolderUse)> {
        self.import
            .folders
            .iter()
            .map(|f| (f.rel.display().to_string(), f.kind))
            .collect()
    }

    pub fn import_message(&self) -> Option<&str> {
        self.import.message.as_deref()
    }

    /// The organise dry-run table: Ok(number of files) or the refusal.
    pub fn org_preview(&self) -> Option<Result<usize, String>> {
        self.org
            .preview
            .as_ref()
            .map(|p| p.as_ref().map(|p| p.moves.len()).map_err(Clone::clone))
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The faces outlined on a photo, by file name: (name, is the person
    /// chosen), sorted (tests).
    pub fn faces_drawn(&self, file: &str) -> Vec<(String, bool)> {
        self.catalog
            .photos
            .iter()
            .position(|p| p.path.file_name().is_some_and(|n| n == file))
            .map(|i| crate::views::faces_to_draw(self, self.ids[i]))
            .unwrap_or_default()
            .into_iter()
            .map(|(_, name, me)| (name, me))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The error shown in the status bar, if any (tests).
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Journal batches: (id, label, status), oldest first.
    pub fn batches(&self) -> Vec<(i64, String, String)> {
        self.history
            .batches
            .iter()
            .map(|b| (b.id, b.label.clone(), b.status.clone()))
            .collect()
    }

    /// Select photos by file name (tests).
    pub fn select_names(&mut self, names: &[&str]) {
        self.selection = self
            .catalog
            .photos
            .iter()
            .filter(|p| {
                p.path
                    .file_name()
                    .is_some_and(|n| names.iter().any(|x| n == *x))
            })
            .map(|p| photo_id(&p.path))
            .collect();
        self.current = self.selection.iter().next().copied();
        self.scroll_to_current = true;
    }

    /// Timeline days as (title, stacks), then undated stacks (tests).
    pub fn timeline_days(&self) -> Option<(Vec<(String, usize)>, usize)> {
        let l = self.timeline.layout.as_ref()?;
        Some((
            l.timeline
                .days
                .iter()
                .map(|d| (d.date.long(), d.stacks.len()))
                .collect(),
            l.timeline.undated.len(),
        ))
    }

    /// Photos placed on the map (tests).
    pub fn located_count(&self) -> usize {
        self.map.located()
    }

    /// File name of the current photo (tests).
    pub fn current_name(&self) -> Option<String> {
        self.current.and_then(|c| self.name_of(c))
    }

    /// How many photos (stacks) a collection holds (tests).
    pub fn collection_count(&self, name: &str) -> usize {
        self.collections
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| self.collection_members.get(&c.id))
            .map_or(0, |m| m.len())
    }

    /// Collection names (tests).
    pub fn collection_names(&self) -> Vec<String> {
        self.collections.iter().map(|c| c.name.clone()).collect()
    }

    // --- changes -----------------------------------------------------------

    /// The unseen change shown on a cell: for a collapsed stack, any member's
    /// (another app's change first, then the latest).
    pub(crate) fn cell_change(&self, idx: usize) -> Option<&shoebox_core::changes::Change> {
        let st = self.stack_of(idx);
        let collapsed = st.primary == idx && !self.expanded.contains(&self.ids[idx]);
        let photos = &self.catalog.photos;
        let mine = || photos[idx].change.as_ref();
        if !collapsed {
            return mine();
        }
        st.all()
            .filter_map(|i| photos[i].change.as_ref())
            .max_by_key(|c| (c.by_other(), c.at_ms))
    }

    /// Clear unseen changes on these stacks (shown at once, saved in the
    /// background).
    pub(crate) fn mark_stacks_seen(&mut self, stacks: &[usize]) {
        let members: Vec<usize> = stacks
            .iter()
            .flat_map(|&s| self.catalog.stacks[s].all())
            .filter(|&i| self.catalog.photos[i].change.is_some())
            .collect();
        if members.is_empty() {
            return;
        }
        let catalog = Arc::make_mut(&mut self.catalog);
        let mut paths = Vec::new();
        for i in members {
            catalog.photos[i].change = None;
            paths.push(catalog.photos[i].path.clone());
        }
        self.library.send(LibraryCommand::MarkSeen(paths));
        if self.filter.unseen_changes {
            self.refilter();
        }
    }

    /// Mark as Seen on the selection (or the current photo).
    pub(crate) fn mark_selection_seen(&mut self) {
        let stacks = self.selected_stacks();
        self.mark_stacks_seen(&stacks);
    }

    /// Does the selection (or current photo) have unseen changes?
    pub(crate) fn selection_has_changes(&self) -> bool {
        self.selected_stacks().iter().any(|&s| {
            self.catalog.stacks[s]
                .all()
                .any(|i| self.catalog.photos[i].change.is_some())
        })
    }

    // --- marks -------------------------------------------------------------

    /// Photos a mark applies to: the selection (or the current photo). A
    /// collapsed stack's primary stands for the whole stack when the
    /// "marks to all stack members" setting is on.
    fn mark_targets(&self) -> Vec<usize> {
        let mut base: Vec<usize> = self
            .shown
            .iter()
            .filter(|t| self.selection.contains(&t.id))
            .map(|t| t.idx)
            .collect();
        // A file and its copy both selected: marked once.
        base.dedup();
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
        // Reference folders are read-only: mark the rest, and say so.
        let (targets, skipped): (Vec<usize>, Vec<usize>) = targets
            .into_iter()
            .partition(|&i| !self.catalog.is_reference(i));
        if !skipped.is_empty() {
            self.notice = Some(reference_notice(skipped.len()));
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
        // Compare and Survey mark in place.
        let in_place = matches!(self.view, View::Compare | View::Survey);
        if !in_place
            && (self.auto_advance || (self.display.is_some() && crate::platform::caps_lock()))
        {
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

    /// The catalog index of a tile's file (a copy's tile: its photo's).
    pub(crate) fn index(&self, id: PhotoId) -> Option<usize> {
        self.index_of.get(&id).copied().or_else(|| {
            let (path, _) = self.copy_tiles.get(&id)?;
            self.index_of.get(&crate::jobs::photo_id(path)).copied()
        })
    }

    /// The virtual copy a tile stands for (None: the file itself).
    pub(crate) fn copy_of(&self, id: PhotoId) -> Option<&str> {
        self.copy_tiles.get(&id).map(|(_, c)| c.as_str())
    }

    /// A copy's details, by its tile's id.
    pub(crate) fn copy_info(&self, id: PhotoId) -> Option<&crate::jobs::CopyInfo> {
        let (path, copy) = self.copy_tiles.get(&id)?;
        self.copies.get(path)?.iter().find(|c| c.id == *copy)
    }

    // --- loading -----------------------------------------------------------

    /// Ask for an image this frame (no-op if loaded or failed).
    /// Photos' edits changed (or were listed): show their previews, made
    /// first if need be.
    /// A photo's edited settings as the catalog last said (its master's
    /// edit), if it has one.
    pub fn edit_settings(&self, id: PhotoId) -> Option<shoebox_develop::edit::Settings> {
        let i = self.index(id)?;
        let json = self.edited_json.get(&self.catalog.photos[i].path)?;
        let doc = shoebox_develop::edit::EditDoc::from_json(json).ok()?;
        Some(doc.settings().clone())
    }

    fn note_edits(&mut self, list: Vec<crate::jobs::EditInfo>) {
        for info in list {
            let id = crate::jobs::photo_id(&info.path);
            if info.key.is_some() {
                self.edited_json
                    .insert(info.path.clone(), info.json.clone());
            } else {
                self.edited_json.remove(&info.path);
            }
            let before = self.edited.get(&info.path).cloned();
            if before == info.key {
                continue;
            }
            // What shows of it changes: load it again.
            self.textures.retain_photos(|p| *p != id);
            self.histograms.remove(&id);
            let Some(key) = info.key else {
                self.edited.remove(&info.path);
                continue;
            };
            self.edited.insert(info.path.clone(), key.clone());
            if self.previews_ready.contains(&key) {
                continue;
            }
            self.render_preview(id, &info.json, key);
        }
    }

    /// Render an edit's preview (for tile `id`) in the background.
    fn render_preview(&mut self, id: PhotoId, json: &str, key: String) {
        let Some(idx) = self.index(id) else { return };
        let Ok(doc) = shoebox_develop::edit::EditDoc::from_json(json) else {
            return;
        };
        let p = &self.catalog.photos[idx];
        self.developer.preview(crate::develop::PreviewJob {
            id,
            path: p.path.clone(),
            lens: crate::develop::shot(p),
            format: p.format,
            orientation: p.meta.orientation(),
            settings: doc.settings().clone(),
            key,
            target: match self.target {
                shoebox_decode::Target::DisplayP3 => shoebox_develop::cpu::Target::DisplayP3,
                _ => shoebox_develop::cpu::Target::Srgb,
            },
        });
    }

    /// Photos' virtual copies listed or changed: their tiles, and previews
    /// made where need be.
    fn note_copies(&mut self, list: Vec<(PathBuf, Vec<crate::jobs::CopyInfo>)>) {
        for (path, copies) in list {
            let photo = crate::jobs::photo_id(&path);
            for old in self.copies.remove(&path).unwrap_or_default() {
                let id = crate::jobs::copy_id(photo, &old.id);
                let now = copies.iter().find(|c| c.id == old.id);
                if now.is_none_or(|c| c.key != old.key) {
                    self.textures.retain_photos(|p| *p != id);
                }
                if now.is_none() {
                    // Removed: its tile goes, and the photo stands for it.
                    self.copy_tiles.remove(&id);
                    self.selection.remove(&id);
                    if self.current == Some(id) {
                        self.current = Some(photo);
                    }
                }
            }
            for c in &copies {
                let id = crate::jobs::copy_id(photo, &c.id);
                self.copy_tiles.insert(id, (path.clone(), c.id.clone()));
                if let Some(key) = &c.key
                    && !self.previews_ready.contains(key)
                {
                    self.render_preview(id, &c.json, key.clone());
                }
            }
            if !copies.is_empty() {
                self.copies.insert(path, copies);
            }
        }
        self.refilter();
    }

    /// An edit's preview is ready: photos showing that edit load it.
    pub(crate) fn preview_ready(&mut self, id: PhotoId, key: String) {
        let current = match self.copy_info(id) {
            Some(c) => c.key.as_ref(),
            None => self
                .index(id)
                .and_then(|i| self.edited.get(&self.catalog.photos[i].path)),
        };
        if current == Some(&key) {
            self.textures.retain_photos(|p| *p != id);
            self.histograms.remove(&id);
        }
        self.previews_ready.insert(key);
    }

    /// Ask for tile `id`'s image this frame (no-op if loaded or failed).
    pub(crate) fn want(&mut self, id: PhotoId, size: Size, priority: Priority) {
        self.want_job(id, size, priority, false, false);
    }

    /// Like [`Self::want`] for the photo being opened: one in the cloud only
    /// is downloaded (elsewhere only its cached thumbnails show).
    pub(crate) fn want_open(&mut self, id: PhotoId, size: Size) {
        self.want_job(id, size, Priority::Visible, true, false);
    }

    /// The photo being opened as it was before its edit, under
    /// [`crate::jobs::original_id`].
    pub(crate) fn want_original(&mut self, id: PhotoId, size: Size) {
        self.want_job(id, size, Priority::Visible, true, true);
    }

    /// Whether this tile shows an edit (a copy's: one that changes
    /// something).
    pub(crate) fn is_edited(&self, id: PhotoId) -> bool {
        match self.copy_info(id) {
            Some(c) => c.key.is_some(),
            None => self
                .index(id)
                .is_some_and(|i| self.edited.contains_key(&self.catalog.photos[i].path)),
        }
    }

    fn want_job(
        &mut self,
        id: PhotoId,
        size: Size,
        priority: Priority,
        open: bool,
        original: bool,
    ) {
        let Some(idx) = self.index(id) else { return };
        // A copy shows its own edit's preview; the file, the master's.
        let edit = match self.copy_info(id) {
            Some(c) => c.key.clone(),
            None => self.edited.get(&self.catalog.photos[idx].path).cloned(),
        };
        let key = Key {
            photo: if original {
                crate::jobs::original_id(id)
            } else {
                id
            },
            size,
        };
        if self.textures.contains(&key) || self.textures.failed.contains_key(&key) {
            return;
        }
        let p = &self.catalog.photos[idx];
        if p.cloud && open {
            self.fetching.insert(key.photo);
        }
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
                edited: edit
                    .filter(|k| !original && self.previews_ready.contains(k))
                    .map(|k| self.developer.preview_path(&k)),
                cloud: p.cloud && !open,
                keep: true,
            },
        ));
    }

    /// Quitting (the window's close button, or Quit in the menu) while an
    /// import copies asks first; stopping it then quits once it's stopped.
    fn tick_quit(&mut self, ctx: &egui::Context) {
        let menu_quit = crate::menu::take_quit() || std::mem::take(&mut self.quit_asked);
        let closing = ctx.input(|i| i.viewport().close_requested());
        if self.import.copying.is_some() {
            if closing {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            if (menu_quit || closing) && !self.quit_when_stopped {
                self.confirm_quit = true;
            }
        } else if menu_quit || std::mem::take(&mut self.quit_when_stopped) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Quit as the menu's Quit does (tests).
    #[doc(hidden)]
    pub fn ask_quit(&mut self) {
        self.quit_asked = true;
    }

    /// Whether Shoebox is asking about stopping an import to quit (tests).
    pub fn asking_to_quit(&self) -> bool {
        self.confirm_quit
    }

    /// Stop the import and quit once it has stopped.
    pub(crate) fn stop_import_and_quit(&mut self) {
        self.confirm_quit = false;
        self.quit_when_stopped = true;
        self.cancel_import();
    }

    /// Eject the card or disk the last import came from.
    pub(crate) fn eject_import_volume(&mut self) {
        if let Some(v) = self.import.volume.clone() {
            self.library.send(LibraryCommand::Eject(v));
        }
    }

    /// Act on a folder picked in the folder picker.
    fn tick_picker(&mut self, ctx: &egui::Context) {
        use crate::picker::FolderFor;
        let Some((purpose, p)) = self.picker.poll(ctx) else {
            return;
        };
        match purpose {
            FolderFor::AddRoot => {
                self.scanning = true;
                self.command(LibraryCommand::AddRoot(p));
            }
            FolderFor::ImportSource => {
                self.import.source = Some(p);
                self.import.volume = None;
                self.import.plan = None;
            }
            FolderFor::MoveSelection => self.move_selection_to(p.canonicalize().unwrap_or(p)),
        }
    }

    /// Ask for a preview of a photo an import will copy (not cached on
    /// disk: the source isn't in the library).
    pub(crate) fn want_preview(&mut self, p: &shoebox_core::import::SourceFile) {
        let key = Key {
            photo: crate::jobs::photo_id(&p.path),
            size: Size::Small,
        };
        if self.textures.contains(&key) || self.textures.failed.contains_key(&key) {
            return;
        }
        self.wanted.push((
            key,
            Priority::Visible,
            Job {
                path: p.path.clone(),
                format: p.format,
                size: p.size,
                mtime_ns: p.mtime_ns,
                orientation: p.orientation,
                target: self.target,
                edited: None,
                cloud: false,
                keep: false,
            },
        ));
    }

    /// Download these photos' cloud-only files (all of each stack).
    pub(crate) fn download(&mut self, ids: &[PhotoId]) {
        let mut paths = Vec::new();
        for &id in ids {
            let Some(i) = self.index(id) else { continue };
            for j in self.stack_of(i).all() {
                let p = &self.catalog.photos[j];
                if p.cloud && !paths.contains(&p.path) {
                    paths.push(p.path.clone());
                }
            }
        }
        if !paths.is_empty() {
            self.download_progress = Some((0, paths.len()));
            self.library.send(LibraryCommand::Download(paths));
        }
    }

    /// How many of these photos (with their stacks) are in the cloud only.
    pub(crate) fn cloud_in(&self, ids: &[PhotoId]) -> usize {
        ids.iter()
            .filter_map(|&id| self.index(id))
            .filter(|&i| self.stack_of(i).all().any(|j| self.catalog.photos[j].cloud))
            .count()
    }

    // --- selection ---------------------------------------------------------

    /// Click with modifiers, as in Lightroom: plain = select one,
    /// Cmd = toggle, Shift = range from the anchor.
    pub(crate) fn click(&mut self, pos_in_shown: usize, modifiers: egui::Modifiers) {
        let Some(id) = self.shown.get(pos_in_shown).map(|t| t.id) else {
            return;
        };
        if modifiers.command {
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
            self.anchor = Some(id);
        } else if modifiers.shift
            && let Some(a) = self
                .anchor
                .and_then(|a| self.shown.iter().position(|t| t.id == a))
        {
            let (lo, hi) = (a.min(pos_in_shown), a.max(pos_in_shown));
            self.selection = self.shown[lo..=hi].iter().map(|t| t.id).collect();
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
            .and_then(|c| self.shown.iter().position(|t| t.id == c))
            .map_or(0, |p| {
                (p as isize + delta).clamp(0, self.shown.len() as isize - 1) as usize
            });
        let id = self.shown[pos].id;
        self.current = Some(id);
        self.selection = BTreeSet::from([id]);
        self.anchor = Some(id);
        self.loupe = LoupeState::default();
        self.scroll_to_current = true;
    }

    /// Reveal the selection (in display order), or the current photo if
    /// nothing is selected.
    pub(crate) fn reveal_selection(&self) {
        let mut paths: Vec<&Path> = self
            .shown
            .iter()
            .filter(|t| self.selection.contains(&t.id))
            .map(|t| self.catalog.photos[t.idx].path.as_path())
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
        let Some(id) = self.shown.get(pos_in_shown).map(|t| t.id) else {
            return;
        };
        if !self.selection.contains(&id) {
            self.click(pos_in_shown, egui::Modifiers::NONE);
        }
    }

    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    pub(crate) fn current_pos(&self) -> Option<usize> {
        let c = self.current?;
        self.shown.iter().position(|t| t.id == c)
    }

    // --- input -------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if crate::menu::take_open_settings() {
            self.open_settings();
        }
        if let Some(size) = crate::theme::size_key(ctx, self.settings.text_size) {
            let mut s = self.settings.clone();
            s.text_size = size;
            self.save_settings(s);
        }
        let comma = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Comma));
        if comma && !self.settings_ui.open {
            self.open_settings();
        } else if self.settings_ui.open {
            // The window takes the keyboard: Escape (or ⌘,) closes it.
            if comma || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.settings_ui.open = false;
            }
            return;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let input = ctx.input(|i| i.clone());
        let m = input.modifiers;
        let pressed = |k| input.key_pressed(k);
        if pressed(egui::Key::Tab) {
            // Tab is the panels' key here, not egui's move-focus (which
            // would land in the search box and take the next Tab).
            ctx.memory_mut(|mem| mem.move_focus(egui::FocusDirection::None));
            if m.shift {
                let show = !(self.left_open || self.right_open || self.bars_open);
                (self.left_open, self.right_open, self.bars_open) = (show, show, show);
            } else {
                let show = !(self.left_open || self.right_open);
                (self.left_open, self.right_open) = (show, show);
            }
        }
        // ⇧⌘C copies Develop's settings, ⇧⌘V pastes them (egui may send
        // these as Copy and Paste events rather than key presses).
        let copy = input.events.iter().any(|e| matches!(e, egui::Event::Copy));
        let paste = input
            .events
            .iter()
            .any(|e| matches!(e, egui::Event::Paste(_)));
        if m.command && m.shift && (pressed(egui::Key::C) || copy) {
            if self.view == View::Develop {
                views::presets::start_copy(self);
            }
            return;
        }
        if m.command && m.shift && (pressed(egui::Key::V) || paste) {
            views::presets::paste(self);
            return;
        }
        if m.command && m.shift && pressed(egui::Key::E) {
            if m.alt {
                views::export::repeat(self);
            } else {
                self.export.open = true;
            }
            return;
        }
        if m.command && self.view == View::Develop {
            if pressed(egui::Key::CloseBracket) {
                views::develop::turn(self, true);
            }
            if pressed(egui::Key::OpenBracket) {
                views::develop::turn(self, false);
            }
        }
        if m.command {
            if pressed(egui::Key::Z) {
                if self.view == View::Develop {
                    // Develop's ⌘Z walks the edit's own history.
                    views::develop::undo(self, m.shift);
                } else if m.shift {
                    self.redo();
                } else {
                    self.undo();
                }
            }
            if pressed(egui::Key::E) {
                self.new_event_from_selection(None);
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
                self.selection = self.shown.iter().map(|t| t.id).collect();
            }
            if pressed(egui::Key::D) {
                self.selection.clear();
            }
            if pressed(egui::Key::R) {
                self.reveal_selection();
            }
            if pressed(egui::Key::Backspace) {
                self.reject_selection();
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
            if self.view == View::Develop {
                // Develop's S is soft proofing.
                self.develop.proof = !self.develop.proof;
            } else {
                self.toggle_stack();
            }
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
        } else if pressed(egui::Key::Escape) && self.view == View::Loupe && !ctx.any_popup_open() {
            // Back to the grid (or wherever the loupe was opened from);
            // an open menu takes the first Escape.
            self.set_view(self.back_view);
        }
        if pressed(egui::Key::Enter) && self.current.is_some() && self.view != View::Develop {
            self.set_view(View::Loupe);
        }
        if pressed(egui::Key::Backslash) && self.view == View::Loupe {
            self.loupe.original = !self.loupe.original;
        }
        if pressed(egui::Key::D) && self.view != View::Develop {
            self.set_view(View::Develop);
        }
        if self.view == View::Develop {
            use views::develop::Compare;
            if pressed(egui::Key::Backslash) {
                let c = &mut self.develop.compare;
                *c = if *c == Compare::Original {
                    Compare::Edited
                } else {
                    Compare::Original
                };
            }
            if pressed(egui::Key::Y) {
                let c = &mut self.develop.compare;
                *c = if *c == Compare::Split {
                    Compare::Edited
                } else {
                    Compare::Split
                };
            }
            if pressed(egui::Key::J) {
                self.develop.warnings = !self.develop.warnings;
            }
            if pressed(egui::Key::W) {
                self.develop.picking = !self.develop.picking;
            }
            if pressed(egui::Key::Q) {
                let on = !self.develop.spots.active;
                views::spots::set_active(self, on);
            }
            if pressed(egui::Key::Backspace)
                && self.develop.spots.active
                && let Some(i) = self.develop.spots.selected
            {
                views::spots::remove(self, i);
            }
            if pressed(egui::Key::O) && self.develop.local.selected.is_some() {
                let t = &mut self.develop.local;
                t.show_mask = !t.show_mask;
            }
            if (pressed(egui::Key::Z) || pressed(egui::Key::Space)) && self.develop.crop.is_none() {
                // Fit ↔ 100%, about the centre.
                self.develop.zoom = match self.develop.zoom {
                    Some(_) => None,
                    None => Some(egui::pos2(0.5, 0.5)),
                };
            }
            if pressed(egui::Key::R) && m.shift {
                views::develop::toggle_reference(self);
            } else if pressed(egui::Key::R) {
                if self.develop.crop.is_some() {
                    views::develop::finish_crop(self, true);
                } else {
                    views::develop::start_crop(self);
                }
            }
            if self.develop.crop.is_some() {
                // The crop takes Enter and Escape.
                if pressed(egui::Key::Enter) {
                    views::develop::finish_crop(self, true);
                }
                if pressed(egui::Key::Escape) {
                    views::develop::finish_crop(self, false);
                }
            } else if pressed(egui::Key::Escape)
                && self.develop.spots.active
                && !ctx.any_popup_open()
            {
                // Escape puts the spot tool down first.
                views::spots::set_active(self, false);
            } else if pressed(egui::Key::Escape)
                && self.develop.local.selected.is_some()
                && !ctx.any_popup_open()
            {
                // Escape puts the mask down first.
                self.develop.local.selected = None;
                self.develop.local.picking = false;
            } else if pressed(egui::Key::Escape) && !self.fullscreen && !ctx.any_popup_open() {
                self.set_view(self.back_view);
            }
        }
        if pressed(egui::Key::Escape) && self.dupes.open.is_some() {
            self.close_group();
        }
        if self.dupes.open.is_some() {
            if pressed(egui::Key::CloseBracket) {
                self.step_group(1);
            }
            if pressed(egui::Key::OpenBracket) {
                self.step_group(-1);
            }
        }
        let cols = self.grid_cols as isize;
        match self.view {
            View::Compare => {
                if pressed(egui::Key::ArrowRight) {
                    self.step_candidate(1);
                }
                if pressed(egui::Key::ArrowLeft) {
                    self.step_candidate(-1);
                }
                if pressed(egui::Key::ArrowUp) {
                    self.swap_compare();
                }
                if pressed(egui::Key::Space) || pressed(egui::Key::Z) {
                    let c = &mut self.compare;
                    c.zoom = if c.zoom > 1.0 {
                        1.0
                    } else {
                        let ppp = ctx.pixels_per_point();
                        (1.0 / (c.select_fit * ppp)).max(1.0)
                    };
                    c.center = egui::pos2(0.5, 0.5);
                }
                return;
            }
            View::Survey => {
                let delta = isize::from(pressed(egui::Key::ArrowRight))
                    - isize::from(pressed(egui::Key::ArrowLeft));
                if delta != 0 && !self.survey.is_empty() {
                    let pos = self
                        .current
                        .and_then(|c| self.survey.iter().position(|&x| x == c))
                        .map_or(0, |p| {
                            (p as isize + delta).clamp(0, self.survey.len() as isize - 1) as usize
                        });
                    self.activate(self.survey[pos]);
                }
                return;
            }
            View::Duplicates | View::Import | View::History => return,
            _ => {}
        }
        if pressed(egui::Key::ArrowRight) {
            self.step(1);
        }
        if pressed(egui::Key::ArrowLeft) {
            self.step(-1);
        }
        if self.view == View::Grid {
            // Up and Down follow the laid-out rows (days start new ones).
            let vertical = isize::from(pressed(egui::Key::ArrowDown))
                - isize::from(pressed(egui::Key::ArrowUp));
            if vertical != 0 {
                match self.current_pos().and_then(|p| {
                    let rows = &self.grid.rows;
                    let r = rows.iter().position(|&(s, e)| (s..e).contains(&p))?;
                    let t = r.checked_add_signed(vertical).filter(|&t| t < rows.len())?;
                    let (s, e) = rows[t];
                    Some((s + (p - rows[r].0)).min(e - 1) as isize - p as isize)
                }) {
                    Some(d) => self.step(d),
                    None if self.grid.rows.is_empty() => self.step(vertical * cols),
                    None => {}
                }
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
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(d) = &self.display {
            d.ensure();
        }
        self.textures.begin_frame();
        crate::theme::apply(&ctx, self.settings.text_size);
        self.apply_events(&ctx);
        self.tick_semantic(&ctx);
        if let Some(id) = self.show_in_all_request.take() {
            self.start_show_in_all(id);
        }
        self.tick_people();
        self.tick_picker(&ctx);
        self.tick_quit(&ctx);
        self.tick_idle(&ctx);
        crate::updater::tick(&ctx, crate::jobs::working());
        self.handle_keys(&ctx);
        views::develop::poll(self);
        if self.view == View::Develop {
            views::develop::tick(self);
        }
        self.wanted.clear();
        self.ppl.hovered = None; // set again by the info panel while pointed at
        self.fold_stacks_left();

        let mut bars = self.bars_open;
        egui::Panel::top("topbar")
            .exact_size(38.0)
            .show_collapsible(ui, &mut bars, |ui| panels::topbar(self, ui));
        egui::Panel::bottom("status")
            .show_collapsible(ui, &mut bars, |ui| panels::status(self, ui));
        self.bars_open = bars;

        // Reviewing a duplicate group: its files are all on screen already.
        let mut film = self.view.has_filmstrip() && self.bars_open && self.dupes.open.is_none();
        egui::Panel::bottom("filmstrip")
            .exact_size(92.0)
            .resizable(false)
            .show_collapsible(ui, &mut film, |ui| views::filmstrip::show(self, ui));

        let mut left = self.left_open;
        egui::Panel::left("library")
            .resizable(true)
            .default_size(260.0)
            .show_collapsible(ui, &mut left, |ui| {
                if self.view == View::Develop {
                    views::develop::history(self, ui);
                } else {
                    panels::library(self, ui);
                }
            });
        self.left_open = left;
        let mut right = self.right_open;
        egui::Panel::right("info")
            .resizable(true)
            .default_size(280.0)
            .show_collapsible(ui, &mut right, |ui| {
                if self.view == View::Develop {
                    views::develop::controls(self, ui);
                } else {
                    panels::info(self, ui);
                }
            });
        self.right_open = right;
        // Over and under the photos only (added after the side panels):
        // the filters narrow what's in the middle.
        let mut filters = self.bars_open && panels::filter_strip_shown(self);
        egui::Panel::top("filterbar")
            .show_collapsible(ui, &mut filters, |ui| panels::filter_strip(self, ui));
        let mut bars = self.bars_open;
        egui::Panel::bottom("viewbar")
            .exact_size(34.0)
            .show_collapsible(ui, &mut bars, |ui| panels::viewbar(self, ui));

        // Viewing a stack in the loupe clears its change badge.
        if self.view == View::Loupe
            && let Some(i) = self.current.and_then(|c| self.index(c))
        {
            let s = self.catalog.stack_of[i];
            self.mark_stacks_seen(&[s]);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            if self.settings.roots.is_empty() && !self.scanning {
                panels::empty_library(self, ui);
                return;
            }
            match self.view {
                View::Grid => views::grid::show(self, ui),
                View::Masonry => views::masonry::show(self, ui),
                View::Loupe => views::loupe::show(self, ui),
                View::Compare => views::compare::show(self, ui),
                View::Survey => views::survey::show(self, ui),
                View::Duplicates => views::duplicates::show(self, ui),
                View::Import => views::files::import(self, ui),
                View::History => views::files::history(self, ui),
                View::Timeline => views::timeline::show(self, ui),
                View::Map => views::map::show(self, ui),
                View::Develop => views::develop::show(self, ui, frame.wgpu_render_state()),
            }
        });
        views::duplicates::apply_dialog(self, &ctx);
        views::files::dialogs(self, &ctx);
        views::settings::show(self, &ctx);
        views::export::dialog(self, &ctx);
        views::presets::copy_window(self, &ctx);
        self.scroll_to_current = false;

        let wanted = std::mem::take(&mut self.wanted);
        self.loader.demand(wanted);
    }
}

pub(crate) fn build_tree(catalog: &Catalog) -> Vec<FolderNode> {
    let mut roots: Vec<FolderNode> = catalog
        .roots
        .iter()
        .map(|root| FolderNode {
            path: root.path.clone(),
            name: root.path.file_name().map_or_else(
                || root.path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            ..Default::default()
        })
        .collect();
    for (si, stack) in catalog.stacks.iter().enumerate() {
        let stamp = si + 1;
        for i in stack.all() {
            let p = &catalog.photos[i];
            let Some(mut cur) = roots.get_mut(p.root) else {
                continue;
            };
            cur.count_stack(stamp);
            let mut parts: Vec<&str> = p.rel.split('/').collect();
            parts.pop(); // the file name
            for part in parts {
                if !cur.children.contains_key(part) {
                    let node = FolderNode {
                        path: cur.path.join(part),
                        name: part.to_owned(),
                        ..Default::default()
                    };
                    cur.children.insert(part.to_owned(), node);
                }
                let Some(next) = cur.children.get_mut(part) else {
                    break;
                };
                cur = next;
                cur.count_stack(stamp);
            }
        }
    }
    roots
}

/// Menu label for [`reveal`] on this platform.
#[cfg(target_os = "macos")]
pub(crate) const REVEAL_LABEL: &str = "Reveal in Finder";
#[cfg(not(target_os = "macos"))]
pub(crate) const REVEAL_LABEL: &str = "Show in File Manager";

/// Open a file in its default app (a movie in QuickTime Player).
pub(crate) fn open_file(path: &Path) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(path).spawn();
    #[cfg(not(target_os = "macos"))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = result {
        tracing::warn!("open {}: {e}", path.display());
    }
}

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

/// The event editor's fields.
/// See `ShoeboxApp::undo_at`: (next undo made at, next redo undone at).
#[derive(Debug, Default, Clone, Copy)]
struct UndoTimes {
    library: (Option<i64>, Option<i64>),
    people: (Option<i64>, Option<i64>),
}

#[derive(Debug, Clone, Default)]
pub struct EventEdit {
    /// None for a new event.
    pub id: Option<String>,
    pub parent: Option<String>,
    pub name: String,
    /// Photos a new event starts with.
    pub photos: Vec<PathBuf>,
    pub error: Option<String>,
}

/// What the status bar says when photos in reference folders were left
/// unchanged.
/// The card or disk `p` is on, when it's one that can be ejected (mounted
/// under `/Volumes` on macOS, `/media` or `/run/media` on Linux).
pub(crate) fn ejectable_volume(p: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let depth = p.starts_with("/Volumes").then_some(3);
    #[cfg(not(target_os = "macos"))]
    let depth = if p.starts_with("/run/media") {
        Some(5)
    } else if p.starts_with("/media") {
        Some(4)
    } else {
        None
    };
    let parts: Vec<_> = p.components().collect();
    let depth = depth?;
    (parts.len() >= depth).then(|| parts[..depth].iter().collect())
}

pub(crate) fn reference_notice(n: usize) -> String {
    if n == 1 {
        "1 photo is in a reference folder (read-only), so it was left as it is".to_owned()
    } else {
        format!("{n} photos are in reference folders (read-only), so they were left as they are")
    }
}

/// Local date and time, `16 Oct 2026 14:02`.
pub(crate) fn local_time_ms(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%-d %b %Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| "—".into())
}

/// Local date and time of a file modification time in nanoseconds.
pub(crate) fn local_time_ns(ns: i64) -> String {
    local_time_ms(ns / 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::ejectable_volume;
    use std::path::Path;

    #[test]
    fn ejectable_volumes() {
        #[cfg(target_os = "macos")]
        let (card, inside) = ("/Volumes/NIKON Z F  ", "/Volumes/NIKON Z F  /DCIM/100NCZ_F");
        #[cfg(not(target_os = "macos"))]
        let (card, inside) = ("/media/me/NIKON", "/media/me/NIKON/DCIM/100NCZ_F");
        assert_eq!(
            ejectable_volume(Path::new(inside)).as_deref(),
            Some(Path::new(card))
        );
        assert_eq!(
            ejectable_volume(Path::new(card)).as_deref(),
            Some(Path::new(card))
        );
        assert_eq!(ejectable_volume(Path::new("/Users/me/Pictures")), None);
        assert_eq!(ejectable_volume(Path::new("/Volumes")), None);
    }
}
