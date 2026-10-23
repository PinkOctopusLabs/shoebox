//! Dates (SPEC "Dates"): Set date… for the selection (a dry run, then one
//! batch), and Tasks → Find dates: clues for the undated photos, found on a
//! thread of its own (`shoebox_core::dating`), grouped for review.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use eframe::egui::{self, Color32, Rect, RichText, pos2, vec2};
use shoebox_core::cache::Cache;
use shoebox_core::catalog::{Catalog, Photo};
use shoebox_core::dating::{self, Clue, Confidence};
use shoebox_core::embeddings::{self, Vector};
use shoebox_core::hash::Analysis;
use shoebox_core::photos::ledger::Ledgers;
use shoebox_core::sidecar::{Precision, SidecarDate};

use crate::app::ShoeboxApp;
use crate::jobs::{LibraryCommand, Priority, Size, photo_id};
use crate::views::duplicates::plural;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The long edge pictures are read for text at.
const TEXT_EDGE: u32 = 2048;

/// Thumbnails shown in a group before "+N".
const STRIP: usize = 12;
const THUMB: f32 = 96.0;

#[derive(Default)]
pub struct State {
    /// Set date…'s dry run, while it's open.
    pub set: Option<SetDate>,
    /// Clues found so far, by photo.
    clues: HashMap<PathBuf, Vec<Clue>>,
    finder: Option<Finder>,
    /// What's typed in each group's date field (by group key).
    typed: HashMap<String, String>,
    skipped: HashSet<String>,
    /// The groups and counts, kept until the clues or the catalog change.
    view: Option<Grouped>,
    /// The catalog clues were last looked for in (new photos need theirs).
    looked_in: usize,
    /// How many dated photos have the duplicate finder's hashes, of how
    /// many (the same-picture clue compares with those only).
    hashed: Option<(usize, usize)>,
    /// Bumped whenever clues arrive (the picture pass replaces a photo's).
    found: usize,
}

/// What the view shows, worked out once per change.
struct Grouped {
    /// The clues (their [`State::found`]) and the catalog it was made from.
    clues: usize,
    catalog: usize,
    groups: Vec<Group>,
    undated: usize,
    cloud: usize,
    references: usize,
}

/// The dry run of a date write: the photos, the date each is suggested
/// (Find dates), and a date typed for all of them.
pub struct SetDate {
    pub photos: Vec<PathBuf>,
    pub suggested: Vec<Option<SidecarDate>>,
    pub text: String,
}

/// What the clue thread sends.
enum Found {
    /// Dated photos with hashes to compare with, of how many.
    Hashed {
        hashed: usize,
        dated: usize,
    },
    Clues(PathBuf, Vec<Clue>),
    /// A second pass over this many photos (their pictures).
    More(usize),
}

/// The clue thread: what it has done of how many.
struct Finder {
    rx: mpsc::Receiver<Found>,
    done: usize,
    total: usize,
}

// ---- Dates as text ----------------------------------------------------

/// `12 March 2019, 10:15:30`, `12 March 2019`, `March 2019` or `2019`.
pub fn date_text(d: &SidecarDate) -> String {
    let t = &d.time;
    let num = |r: std::ops::Range<usize>| t.get(r).and_then(|s| s.parse::<usize>().ok());
    let (Some(y), Some(m), Some(day)) = (num(0..4), num(5..7), num(8..10)) else {
        return t.clone();
    };
    let month = MONTHS.get(m.wrapping_sub(1)).copied().unwrap_or("?");
    match d.precision {
        Precision::Year => format!("{y}"),
        Precision::Month => format!("{month} {y}"),
        Precision::Day => format!("{day} {month} {y}"),
        Precision::Moment => format!("{day} {month} {y}, {}", t.get(11..).unwrap_or("")),
    }
}

/// A photo's date as the Info panel and the dry run show it.
pub fn meta_date_text(meta: &shoebox_core::meta::Meta) -> String {
    let x = &meta.exif;
    let Some(t) = &x.capture_time else {
        return "Undated".into();
    };
    if let Some(d) = meta.sidecar_date()
        && d.precision != Precision::Moment
    {
        return format!("{} (approximate)", date_text(d));
    }
    let mut s = t.replacen(':', "-", 2);
    if let Some(ss) = &x.subsec {
        s.push('.');
        s.push_str(ss);
    }
    if let Some(o) = &x.offset {
        s.push(' ');
        s.push_str(o);
    }
    s
}

/// A date as typed: `2019-03-12 10:15:30` (or `10:15`), `2019-03-12`,
/// `2019-03`, `2019`; `:` between the date's parts too.
pub fn parse_typed(s: &str) -> Option<SidecarDate> {
    let s = s.trim();
    // Minutes without seconds.
    if s.len() == 16 && s.is_ascii() && s.as_bytes()[13] == b':' {
        return SidecarDate::parse(&format!("{s}:00"));
    }
    SidecarDate::parse(s)
}

// ---- Set date… --------------------------------------------------------

/// Set date… for the selection.
pub fn open_set_date(app: &mut ShoeboxApp) {
    let photos = app.selected_paths();
    if photos.is_empty() {
        return;
    }
    let text = match photos.as_slice() {
        // One photo: start from its date.
        [one] => app
            .catalog
            .photos
            .iter()
            .find(|p| &p.path == one)
            .and_then(|p| {
                let x = &p.meta.exif;
                let t = x.capture_time.as_ref()?;
                Some(match p.meta.sidecar_date() {
                    Some(d) if d.precision != Precision::Moment => d.to_xmp(),
                    _ => t.replacen(':', "-", 2),
                })
            })
            .unwrap_or_default(),
        _ => String::new(),
    };
    app.dates.set = Some(SetDate {
        suggested: vec![None; photos.len()],
        photos,
        text,
    });
}

/// Set date…'s dialog: the date, and each photo's date now and after.
pub fn dialog(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let Some(mut set) = app.dates.set.take() else {
        return;
    };
    let by_path: HashMap<&Path, usize> = app
        .catalog
        .photos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.path.as_path(), i))
        .collect();
    let typed = parse_typed(&set.text);
    let mut keep = true;
    let mut go = false;
    egui::Modal::new(egui::Id::unique("set-date")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.heading(format!("Set date of {}", plural(set.photos.len(), "photo")));
        ui.weak(
            "Written to each photo's sidecar (the image files aren't changed), as one step \
             ⌘Z takes back.",
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Date");
            let r = ui.add(
                egui::TextEdit::singleline(&mut set.text)
                    .hint_text(if set.suggested.iter().any(Option::is_some) {
                        "as suggested, or type one for all"
                    } else {
                        "2019-03-12 10:15, 2019-03-12, 2019-03 or 2019"
                    })
                    .desired_width(260.0),
            );
            if set.suggested.iter().all(Option::is_none) {
                r.request_focus();
            }
            match (&typed, set.text.trim().is_empty()) {
                (Some(d), _) => {
                    let what = match d.precision {
                        Precision::Moment => date_text(d),
                        _ => format!("{} (approximate)", date_text(d)),
                    };
                    ui.weak(what);
                }
                (None, true) => {}
                (None, false) => {
                    ui.colored_label(ui.visuals().warn_fg_color, "Not a date");
                }
            }
        });
        if typed
            .as_ref()
            .is_some_and(|d| d.precision != Precision::Moment)
        {
            ui.weak(
                "A day, month or year is kept as just that, and sorts at its middle. It \
                 dates undated photos only: one with a date of its own keeps it.",
            );
        }
        ui.add_space(6.0);
        let mut writes = 0;
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("set-date-rows")
                    .num_columns(3)
                    .striped(true)
                    .spacing(vec2(12.0, 3.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Photo").strong());
                        ui.label(RichText::new("Now").strong());
                        ui.label(RichText::new("After").strong());
                        ui.end_row();
                        for (k, path) in set.photos.iter().enumerate() {
                            let date = date_for(&set, k, typed.as_ref());
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.label(name);
                            let Some(&i) = by_path.get(path.as_path()) else {
                                ui.weak("gone");
                                ui.label("");
                                ui.end_row();
                                continue;
                            };
                            let p = &app.catalog.photos[i];
                            ui.label(meta_date_text(&p.meta));
                            let after = if app.catalog.is_reference(i) {
                                RichText::new("left out: a reference folder").weak()
                            } else if let Some(d) = &date {
                                if d.precision != Precision::Moment
                                    && p.meta.exif.capture_time.is_some()
                                    && p.meta.sidecar_date().is_none()
                                {
                                    RichText::new("keeps its own date").weak()
                                } else {
                                    writes += 1;
                                    RichText::new(match d.precision {
                                        Precision::Moment => date_text(d),
                                        _ => format!("{} (approximate)", date_text(d)),
                                    })
                                }
                            } else {
                                RichText::new("—").weak()
                            };
                            ui.label(after);
                            ui.end_row();
                        }
                    });
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = format!("Set {}", plural(writes, "date"));
            if ui
                .add_enabled(writes > 0, egui::Button::new(label))
                .clicked()
            {
                go = true;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                keep = false;
            }
        });
    });
    if go {
        let dates: Vec<(PathBuf, SidecarDate)> = set
            .photos
            .iter()
            .enumerate()
            .filter_map(|(k, p)| Some((p.clone(), date_for(&set, k, typed.as_ref())?)))
            .collect();
        let label = format!("Set date of {}", plural(dates.len(), "photo"));
        app.command(LibraryCommand::SetDates { dates, label });
    } else if keep {
        app.dates.set = Some(set);
    }
}

/// The date photo `k` gets: the one typed, else its suggestion.
fn date_for(set: &SetDate, k: usize, typed: Option<&SidecarDate>) -> Option<SidecarDate> {
    typed
        .cloned()
        .or_else(|| set.suggested.get(k).cloned().flatten())
}

// ---- Find dates -------------------------------------------------------

/// A group of undated photos sharing a suggestion.
struct Group {
    key: String,
    /// Their folder, shortened.
    folder: String,
    photos: Vec<PathBuf>,
    /// Each photo's best clue, if any (same order as `photos`).
    best: Vec<Option<Clue>>,
    /// Other suggestions, each with how many photos it's for.
    others: Vec<(Clue, usize)>,
}

/// The undated photos that can be dated (not in reference folders).
fn undated(app: &ShoeboxApp) -> Vec<usize> {
    (0..app.catalog.photos.len())
        .filter(|&i| {
            app.catalog.photos[i].meta.exif.capture_time.is_none() && !app.catalog.is_reference(i)
        })
        .collect()
}

/// Start finding clues for the undated photos not looked at yet.
fn start(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let todo: Vec<usize> = undated(app)
        .into_iter()
        .filter(|&i| !app.dates.clues.contains_key(&app.catalog.photos[i].path))
        .collect();
    if todo.is_empty() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let total = todo.len();
    let ctx = ctx.clone();
    let job = Job {
        catalog: app.catalog.clone(),
        cache_db: app.cache_db.clone(),
        embeddings_db: app.embeddings_db.clone(),
        people: app.ppl.view.clone(),
        todo,
    };
    let spawned = std::thread::Builder::new()
        .name("find-dates".into())
        .spawn(move || find(&job, &tx, &ctx));
    match spawned {
        Ok(_) => app.dates.finder = Some(Finder { rx, done: 0, total }),
        Err(e) => tracing::warn!("find dates: {e}"),
    }
}

/// What the clue thread works from.
struct Job {
    catalog: std::sync::Arc<Catalog>,
    cache_db: PathBuf,
    embeddings_db: PathBuf,
    people: Option<std::sync::Arc<crate::people::PeopleView>>,
    /// The photos to find clues for (catalog indices).
    todo: Vec<usize>,
}

/// The clues for the photos of `job`, in two passes. First their own and
/// the library's (iCloud Photos records, the hashes the duplicate finder
/// cached, read from the cache beside the library's connection; an
/// undated photo not hashed yet is hashed here, unless it's in the cloud
/// only). Then, for those without a sure date, the picture's: its text
/// (Vision), and the hints of the photos it looks like (search's
/// embeddings, read beside the search thread's connection) and of the same
/// people. Each pass sends each photo's clues; the second's replace the
/// first's.
fn find(job: &Job, tx: &mpsc::Sender<Found>, ctx: &egui::Context) {
    let (catalog, todo) = (&*job.catalog, &job.todo);
    let roots: Vec<PathBuf> = catalog.roots.iter().map(|r| r.path.clone()).collect();
    let ledgers = Ledgers::load(&roots);
    let assets = ledgers
        .iter()
        .filter_map(|(_, r, e)| {
            let root = roots.get(r)?;
            Some(e.files.iter().map(|f| (f.kind, f.path(root))).collect())
        })
        .collect();
    let cache = match Cache::open_read_only(&job.cache_db) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("find dates: no cached hashes: {e}");
            None
        }
    };
    let ids: Vec<Option<i64>> = roots
        .iter()
        .map(|r| cache.as_ref().and_then(|c| c.find_root(r).ok().flatten()))
        .collect();
    let cached = |p: &Photo| -> Option<Analysis> {
        let id = (*ids.get(p.root)?)?;
        cache
            .as_ref()?
            .lookup(id, &p.rel, p.size, p.mtime_ns)
            .ok()
            .flatten()
    };
    let hashed: Vec<(usize, Analysis)> = catalog
        .photos
        .iter()
        .enumerate()
        .filter(|(_, p)| p.meta.exif.capture_time.is_some())
        .filter_map(|(i, p)| Some((i, cached(p)?)))
        .collect();
    let dated = catalog
        .photos
        .iter()
        .filter(|p| p.meta.exif.capture_time.is_some())
        .count();
    let library = dating::Library::new(catalog, assets, hashed);
    let _ = tx.send(Found::Hashed {
        hashed: library.hashed(),
        dated,
    });
    tracing::info!(
        "find dates: {} undated, {} dated photos hashed to compare with",
        todo.len(),
        library.hashed()
    );
    // Each photo's clues as found, before ranking (ranking twice would
    // count agreement twice).
    let mut found: Vec<(usize, Vec<Clue>)> = Vec::with_capacity(todo.len());
    for (n, &i) in todo.iter().enumerate() {
        let p = &catalog.photos[i];
        let mut c = dating::clues(&p.path, Path::new(&p.rel), p.format, p.cloud);
        let own = cached(p).or_else(|| {
            (!p.cloud)
                .then(|| shoebox_core::hash::analyze_path(&p.path).ok())
                .flatten()
        });
        c.extend(library.clues(i, own.as_ref()));
        let mut ranked = c.clone();
        dating::rank(&mut ranked);
        if tx.send(Found::Clues(p.path.clone(), ranked)).is_err() {
            return;
        }
        found.push((i, c));
        if n % 16 == 0 {
            ctx.request_repaint();
        }
    }

    // The picture's clues, for those without a sure date yet.
    let sure = |c: &[Clue]| {
        let mut r = c.to_vec();
        dating::rank(&mut r);
        r.iter()
            .find(|c| !c.source.is_hint())
            .is_some_and(|c| c.confidence == Confidence::High)
    };
    found.retain(|(_, c)| !sure(c));
    if found.is_empty() {
        ctx.request_repaint();
        return;
    }
    if tx.send(Found::More(found.len())).is_err() {
        return;
    }
    let by_path: HashMap<&Path, usize> = catalog
        .photos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.path.as_path(), i))
        .collect();
    let vectors: HashMap<usize, Vector> =
        match embeddings::read_rows(&job.embeddings_db, "image", shoebox_ml::MODEL_ID) {
            Ok(rows) => rows
                .iter()
                .filter_map(|(path, p)| {
                    Some((*by_path.get(path.as_path())?, Vector::from_payload(p)))
                })
                .collect(),
            Err(e) => {
                tracing::warn!("find dates: no search embeddings: {e}");
                HashMap::new()
            }
        };
    let people: HashMap<usize, Vec<usize>> = match &job.people {
        Some(v) => {
            let ids: HashMap<crate::jobs::PhotoId, usize> = catalog
                .photos
                .iter()
                .enumerate()
                .map(|(i, p)| (photo_id(&p.path), i))
                .collect();
            v.by_photo
                .iter()
                .filter_map(|(id, groups)| Some((*ids.get(id)?, groups.clone())))
                .collect()
        }
        None => HashMap::new(),
    };
    tracing::info!(
        "find dates: the pictures of {} photos ({} with search embeddings, {} with people)",
        found.len(),
        vectors.len(),
        people.len()
    );
    let library = library.with_pictures(vectors, people);
    for (n, (i, mut c)) in found.into_iter().enumerate() {
        let p = &catalog.photos[i];
        let text = if p.cloud || p.format.is_movie() {
            Vec::new()
        } else {
            shoebox_decode::render_path(
                p.format,
                &p.path,
                Some(TEXT_EDGE),
                p.meta.orientation(),
                shoebox_decode::Target::Srgb,
            )
            .map_err(|e| tracing::warn!("find dates: {}: {e}", p.path.display()))
            .ok()
            .and_then(|img| {
                shoebox_ml::vision::text(&img.px, img.width as usize, img.height as usize)
                    .map_err(|e| tracing::warn!("find dates: text in {}: {e}", p.path.display()))
                    .ok()
            })
            .unwrap_or_default()
        };
        c.extend(library.picture_clues(i, &text));
        dating::rank(&mut c);
        if tx.send(Found::Clues(p.path.clone(), c)).is_err() {
            return;
        }
        if n % 8 == 0 {
            ctx.request_repaint();
        }
    }
    ctx.request_repaint();
}

/// Take in what the thread has found.
fn poll(app: &mut ShoeboxApp) {
    let Some(f) = &mut app.dates.finder else {
        return;
    };
    let mut ended = false;
    loop {
        match f.rx.try_recv() {
            Ok(Found::Clues(path, c)) => {
                f.done += 1;
                app.dates.found += 1;
                app.dates.clues.insert(path, c);
            }
            Ok(Found::Hashed { hashed, dated }) => app.dates.hashed = Some((hashed, dated)),
            Ok(Found::More(n)) => f.total += n,
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                ended = true;
                break;
            }
        }
    }
    if ended {
        app.dates.finder = None;
    }
}

/// The same-picture clue compares with the dated photos Duplicates has
/// hashed: say how many are, and how to hash the rest.
fn duplicates_note(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let Some((hashed, dated)) = app.dates.hashed else {
        return;
    };
    if dated == 0 || hashed >= dated {
        return;
    }
    ui.add_space(4.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let have = if hashed == 0 {
            "none of the dated photos has been hashed yet".to_owned()
        } else {
            format!("{hashed} of {dated} dated photos have been hashed")
        };
        ui.add(
            egui::Label::new(format!(
                "Finding the same picture dated elsewhere (such as a recovered preview and its \
                 NEF) uses the hashes Duplicates makes, and {have}. Run Duplicates once (it \
                 reads every photo, which takes a while in a large library), then Look again \
                 here."
            ))
            .wrap(),
        );
        if ui.button("Open Duplicates").clicked() {
            app.set_view(crate::app::View::Duplicates);
        }
    });
}

/// Forget the clues found and look again.
fn look_again(app: &mut ShoeboxApp) {
    app.dates.clues.clear();
    app.dates.view = None;
    app.dates.hashed = None;
    app.dates.looked_in = 0;
}

/// How many photos have been looked at for clues.
pub fn looked_at(app: &ShoeboxApp) -> usize {
    app.dates.clues.len()
}

/// How far finding clues has got, for Activity.
pub fn progress(app: &ShoeboxApp) -> Option<(usize, usize)> {
    app.dates.finder.as_ref().map(|f| (f.done, f.total))
}

/// The undated photos by folder and best suggestion (the day of it, so a
/// day's file names group together), clueless ones by folder.
fn groups(app: &ShoeboxApp) -> Vec<Group> {
    let mut by: BTreeMap<String, Group> = BTreeMap::new();
    for i in undated(app) {
        let p = &app.catalog.photos[i];
        let Some(clues) = app.dates.clues.get(&p.path) else {
            continue;
        };
        // Its folder in its root, after the root's name.
        let root = app
            .catalog
            .roots
            .get(p.root)
            .and_then(|r| r.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folder = match Path::new(&p.rel).parent() {
            Some(d) if !d.as_os_str().is_empty() => format!("{root}/{}", d.display()),
            _ => root,
        };
        // Hints are never the suggestion Accept takes.
        let best = clues.iter().find(|c| !c.source.is_hint()).cloned();
        let what = best.as_ref().map_or_else(
            || "none".to_owned(),
            |c| {
                let n = match c.date.precision {
                    Precision::Moment | Precision::Day => 10,
                    Precision::Month => 7,
                    Precision::Year => 4,
                };
                format!("{:?} {}", c.source, c.date.time.get(..n).unwrap_or(""))
            },
        );
        let key = format!("{folder}\u{0}{what}");
        let g = by.entry(key.clone()).or_insert_with(|| Group {
            key,
            folder,
            photos: Vec::new(),
            best: Vec::new(),
            others: Vec::new(),
        });
        g.photos.push(p.path.clone());
        g.best.push(best);
        // Others that say something else than its best.
        let first = g.best.last().cloned().flatten();
        for c in clues
            .iter()
            .filter(|c| first.as_ref().is_none_or(|b| b.date != c.date))
        {
            let same = |o: &Clue| o.date == c.date && o.source == c.source;
            match g.others.iter_mut().find(|(o, _)| same(o)) {
                Some((_, n)) => *n += 1,
                None => g.others.push((c.clone(), 1)),
            }
        }
    }
    let mut out: Vec<Group> = by.into_values().collect();
    // Surest suggestions first; no clues last.
    out.sort_by_key(|g| {
        std::cmp::Reverse(
            g.best
                .first()
                .and_then(|b| b.as_ref())
                .map_or(0, |b| b.confidence as u8 + 1),
        )
    });
    for g in &mut out {
        g.others
            .sort_by(|a, b| b.0.confidence.cmp(&a.0.confidence).then(b.1.cmp(&a.1)));
        g.others.truncate(4);
    }
    out
}

fn confidence_text(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "sure",
        Confidence::Medium => "likely",
        Confidence::Low => "a guess",
    }
}

fn clue_text(c: &Clue) -> String {
    let d = match c.date.precision {
        Precision::Moment => date_text(&c.date),
        _ => format!("{} (approximate)", date_text(&c.date)),
    };
    format!("{d} · {}", confidence_text(c.confidence))
}

/// The groups and counts, made again when the clues or the catalog change.
fn grouped(app: &mut ShoeboxApp) -> Grouped {
    let catalog = std::sync::Arc::as_ptr(&app.catalog) as usize;
    let clues = app.dates.found;
    match app.dates.view.take() {
        Some(v) if v.clues == clues && v.catalog == catalog => v,
        _ => {
            let all = undated(app);
            let photos = &app.catalog.photos;
            Grouped {
                clues,
                catalog,
                groups: groups(app),
                undated: all.len(),
                cloud: all.iter().filter(|&&i| photos[i].cloud).count(),
                references: (0..photos.len())
                    .filter(|&i| {
                        photos[i].meta.exif.capture_time.is_none() && app.catalog.is_reference(i)
                    })
                    .count(),
            }
        }
    }
}

/// Tasks → Find dates.
pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    poll(app);
    let catalog = std::sync::Arc::as_ptr(&app.catalog) as usize;
    if app.dates.finder.is_none() && app.dates.looked_in != catalog {
        app.dates.looked_in = catalog;
        start(app, ui.ctx());
    }
    let view = grouped(app);
    let (cloud, references) = (view.cloud, view.references);
    ui.horizontal_wrapped(|ui| {
        let n = view.undated;
        let s = if n == 1 { "" } else { "s" };
        ui.label(RichText::new(format!("{n} undated photo{s}")).strong());
        if let Some((done, total)) = progress(app) {
            ui.spinner();
            ui.weak(format!("looking for clues · {done} of {total}"));
        }
        if cloud > 0 {
            ui.weak(format!(
                "· {cloud} in the cloud only: just their names and folders give clues until \
                 they're here"
            ));
        }
        if references > 0 {
            ui.weak(format!(
                "· {} in reference folders left out",
                plural(references, "photo")
            ));
        }
        if app.dates.finder.is_none()
            && ui
                .small_button("Look again")
                .on_hover_text(
                    "Look for clues again, with what's new in the library (such as hashes \
                     from Duplicates)",
                )
                .clicked()
        {
            look_again(app);
        }
    });
    ui.weak(
        "Each group's suggestion comes from clues in the files, their names and folders, the \
         rest of the library (the same picture or frame, the camera's sequence, the photos \
         beside them) and the pictures (dates in them; photos they look like and of the same \
         people, as hints to pick yourself). Accept dates each photo by its own suggestion; a date typed in \
         dates them all. Nothing is written without the dry run.",
    );
    duplicates_note(app, ui);
    ui.separator();
    if view.undated == 0 {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| ui.weak("Every photo has a date."));
        app.dates.view = Some(view);
        return;
    }
    let shown: Vec<&Group> = view
        .groups
        .iter()
        .filter(|g| !app.dates.skipped.contains(&g.key))
        .collect();
    let skipped = view.groups.len() - shown.len();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for g in shown {
                group(app, ui, g);
            }
            if skipped > 0 {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.weak(format!("{} skipped", plural(skipped, "group")));
                    if ui.small_button("Show again").clicked() {
                        app.dates.skipped.clear();
                    }
                });
            }
        });
    app.dates.view = Some(view);
}

fn group(app: &mut ShoeboxApp, ui: &mut egui::Ui, g: &Group) {
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&g.folder).strong());
        ui.weak(format!("· {}", plural(g.photos.len(), "photo")));
    });
    let best = g.best.iter().flatten().next();
    match best {
        Some(c) => {
            ui.horizontal_wrapped(|ui| {
                let text = if g.photos.len() > 1 && c.date.precision == Precision::Moment {
                    // Each its own time: say the day.
                    let day = SidecarDate {
                        precision: Precision::Day,
                        ..c.date.clone()
                    };
                    format!(
                        "{}, each at its own time · {}",
                        date_text(&day),
                        confidence_text(c.confidence)
                    )
                } else {
                    clue_text(c)
                };
                ui.label(RichText::new(text).color(ui.visuals().strong_text_color()));
                ui.weak(format!("· {}", c.describe()));
            });
        }
        None => {
            ui.weak("No clues: type a date if you know it.");
        }
    }
    let hints = scene_hints(app, g);
    if !hints.is_empty() {
        ui.weak(format!("Hints: {}", hints.join(" · ")));
    }
    strip(app, ui, g);
    ui.horizontal_wrapped(|ui| {
        let typed = app.dates.typed.entry(g.key.clone()).or_default();
        ui.add(
            egui::TextEdit::singleline(typed)
                .hint_text("a date: 2019-03-12, 2019-03 or 2019")
                .desired_width(240.0),
        );
        let typed = typed.clone();
        if !typed.trim().is_empty() {
            match parse_typed(&typed) {
                Some(d) => {
                    if ui.button(format!("Set {}…", date_text(&d))).clicked() {
                        app.dates.set = Some(SetDate {
                            photos: g.photos.clone(),
                            suggested: vec![None; g.photos.len()],
                            text: typed.clone(),
                        });
                    }
                }
                None => {
                    ui.colored_label(ui.visuals().warn_fg_color, "Not a date");
                }
            }
        } else if best.is_some()
            && ui
                .button("Accept…")
                .on_hover_text("Each photo by its own suggestion, after the dry run")
                .clicked()
        {
            app.dates.set = Some(SetDate {
                photos: g.photos.clone(),
                suggested: g
                    .best
                    .iter()
                    .map(|b| b.as_ref().map(|c| c.date.clone()))
                    .collect(),
                text: String::new(),
            });
        }
        for (c, n) in &g.others {
            let hint = format!("{} ({})", c.describe(), plural(*n, "photo"));
            if ui
                .small_button(format!("{}…", clue_text(c)))
                .on_hover_text(hint)
                .clicked()
            {
                app.dates.set = Some(SetDate {
                    photos: g.photos.clone(),
                    suggested: vec![Some(c.date.clone()); g.photos.len()],
                    text: String::new(),
                });
            }
        }
        if let Some(c) = best
            && ui
                .small_button("Show beside…")
                .on_hover_text("Open Grid at the photos taken nearest this date, to compare")
                .clicked()
        {
            show_beside(app, &c.date);
        }
        if ui.small_button("Skip").clicked() {
            app.dates.skipped.insert(g.key.clone());
        }
    });
    ui.add_space(4.0);
    ui.separator();
}

/// Open Grid (all photos, by capture time) at the dated photo taken
/// nearest `date`.
pub fn show_beside(app: &mut ShoeboxApp, date: &SidecarDate) {
    let Some(target) = shoebox_core::exif::capture_seconds(&date.time, None) else {
        return;
    };
    // The catalog is sorted by capture time, the dated first.
    let photos = &app.catalog.photos;
    let dated = photos.partition_point(|p| p.meta.capture_seconds().is_some());
    let at = photos[..dated].partition_point(|p| p.meta.capture_seconds() < Some(target));
    let gap = |i: usize| {
        photos
            .get(i)
            .and_then(|p| p.meta.capture_seconds())
            .map_or(f64::INFINITY, |t| (t - target).abs())
    };
    let nearest = [at.checked_sub(1), (at < dated).then_some(at)]
        .into_iter()
        .flatten()
        .min_by(|&a, &b| gap(a).total_cmp(&gap(b)));
    if let Some(i) = nearest {
        app.show_in_all(app.ids[i]);
    }
}

/// What search's tags on a group's photos say about when (a Christmas
/// tree, snow), as words.
fn scene_hints(app: &ShoeboxApp, g: &Group) -> Vec<&'static str> {
    let Some(tags) = &app.sem.tags else {
        return Vec::new();
    };
    let mut out: Vec<&'static str> = Vec::new();
    for path in &g.photos {
        let Some(t) = tags.by_photo.get(&photo_id(path)) else {
            continue;
        };
        for (label, _) in t {
            let hint = tags
                .names
                .get(usize::from(*label))
                .and_then(|n| dating::scene_hint(n));
            if let Some(h) = hint
                && !out.contains(&h)
            {
                out.push(h);
            }
        }
    }
    out
}

/// A group's thumbnails; a click selects the photo (its Info shows).
fn strip(app: &mut ShoeboxApp, ui: &mut egui::Ui, g: &Group) {
    ui.horizontal(|ui| {
        for path in g.photos.iter().take(STRIP) {
            let (rect, resp) = ui.allocate_exact_size(vec2(THUMB, THUMB), egui::Sense::click());
            // Only what's on screen asks for its thumbnail.
            if !ui.is_rect_visible(rect) {
                continue;
            }
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
            let id = photo_id(path);
            let size = super::size_for(THUMB, ui.ctx().pixels_per_point());
            app.want(id, size, Priority::Visible);
            if size == Size::Large {
                app.want(id, Size::Small, Priority::Visible);
            }
            match app.textures.best(id, size) {
                Some(t) => {
                    let [w, h] = t.size();
                    let target = super::fit(rect.shrink(2.0), w as f32 / h as f32);
                    painter.image(
                        t.id(),
                        target,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                None => {
                    painter.rect_filled(rect.shrink(2.0), 2.0, ui.visuals().extreme_bg_color);
                }
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
            let resp = resp.on_hover_text(&name);
            if resp.clicked()
                && let Some(idx) = app.index(id)
            {
                app.activate(app.ids[idx]);
            }
        }
        if g.photos.len() > STRIP {
            ui.weak(format!("+{}", g.photos.len() - STRIP));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_dates() {
        let d = parse_typed("2019-03-12 10:15").unwrap();
        assert_eq!(
            (d.time.as_str(), d.precision),
            ("2019:03:12 10:15:00", Precision::Moment)
        );
        assert_eq!(date_text(&d), "12 March 2019, 10:15:00");
        let d = parse_typed(" 2019-03 ").unwrap();
        assert_eq!(date_text(&d), "March 2019");
        assert_eq!(date_text(&parse_typed("2019").unwrap()), "2019");
        assert_eq!(
            date_text(&parse_typed("2019:03:12").unwrap()),
            "12 March 2019"
        );
        assert!(parse_typed("2019-13").is_none());
        assert!(parse_typed("March").is_none());
    }
}
