//! Tasks → Duplicates: groups as thumbnail strips with distance, dimensions
//! and paths. Decide per file (keep, move to `_duplicates/`, add to the
//! stack as a variant, or not a duplicate), or let auto-select propose;
//! open a group to look closely (Compare for two files, Survey for more).
//! Apply shows every move first and runs them as one undoable batch.

use std::path::Path;

use eframe::egui::{self, Color32, Rect, RichText, pos2, vec2};
use shoebox_core::dupes::{self, Level, MAX_THRESHOLD};
use shoebox_core::review::AutoSelect;

use crate::jobs::ApplyStage;
use shoebox_core::review::{Decision, Entry, Review};
use shoebox_core::scan::Role;

use crate::app::{DupeScope, ShoeboxApp};
use crate::jobs::{Priority, Size, photo_id};

/// A row's height besides its thumbnails: the heading and the text under
/// each one.
const ROW_TEXT: f32 = 114.0;

/// "1 file", "3 files".
pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// "0.4 s", "12 s", "3 min", "5 h", "2 days": a gap between capture times.
fn span(secs: f64) -> String {
    match secs {
        s if s < 10.0 && s.fract() != 0.0 => format!("{s:.1} s"),
        s if s < 10.0 => format!("{s} s"),
        s if s < 90.0 => format!("{} s", s.round()),
        s if s < 5400.0 => format!("{} min", (s / 60.0).round()),
        s if s < 172_800.0 => format!("{} h", (s / 3600.0).round()),
        s => format!("{} days", (s / 86_400.0).round()),
    }
}

/// [`plural`] with thousands grouped: "17,964 files".
fn plural_n(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{} {word}s", group_digits(n))
    }
}

pub fn decision_name(d: Decision) -> &'static str {
    match d {
        Decision::Undecided => "Undecided",
        Decision::Keep => "Keep",
        Decision::Move => "Move to _duplicates",
        Decision::Variant => "Add as variant",
        Decision::Ignore => "Not a duplicate",
    }
}

fn decision_colour(d: Decision) -> Color32 {
    match d {
        Decision::Undecided => Color32::from_gray(150),
        Decision::Keep => Color32::from_rgb(80, 200, 120),
        Decision::Move => Color32::from_rgb(255, 149, 0),
        Decision::Variant => Color32::from_rgb(100, 170, 255),
        Decision::Ignore => Color32::from_gray(120),
    }
}

/// How a file matches its closest copy: "Same image data as x.nef",
/// "Near-identical · 95%" (how alike: `Match::percent`), with how far apart
/// they were taken ("taken 2.0 s apart") or "unconfirmed" when it
/// isn't proposed for moving.
pub fn match_text(r: &Review, keeper: usize, e: &Entry) -> String {
    let Some((other, m)) = e.best else {
        return "Kept copy".to_owned();
    };
    let level = match m.level {
        Level::Exact => "Identical file".to_owned(),
        Level::ImageData => "Same image data".to_owned(),
        Level::NearIdentical => "Near-identical".to_owned(),
        Level::Similar => "Similar".to_owned(),
    };
    let mut s = format!("{level} · {}%", m.percent());
    if other != keeper {
        let name = r.files[other]
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        s.push_str(&format!(" as {name}"));
    }
    if m.rotation != 0 {
        s.push_str(&format!(" · {}°", m.rotation));
    }
    if let Some(g) = e.gap {
        s.push_str(&format!(" · taken {} apart", span(g)));
    } else if !e.certain {
        s.push_str(" · no capture time to confirm");
    }
    s
}

/// The review entry of `path` in the open group: (review, position, entry).
fn open_entry(app: &ShoeboxApp, path: &Path) -> Option<(std::sync::Arc<Review>, usize)> {
    let r = app.dupes.review.clone()?;
    let g = r.groups.get(app.dupes.open?)?;
    let k = g
        .entries
        .iter()
        .position(|e| r.files[e.file].path == path)?;
    Some((r, k))
}

/// The decision buttons for one file (`k`: position in its group).
fn decision_buttons(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    path: &Path,
    reference: bool,
    k: usize,
) {
    let now = app.decision(path);
    for d in [
        Decision::Keep,
        Decision::Move,
        Decision::Variant,
        Decision::Ignore,
    ] {
        let short = match d {
            Decision::Keep => "Keep",
            Decision::Move => "Move",
            Decision::Variant => "Variant",
            Decision::Ignore => "Not dupe",
            Decision::Undecided => "",
        };
        // Reference files never move; the kept copy can't be its own variant.
        let enabled = !(reference && d == Decision::Move) && !(k == 0 && d == Decision::Variant);
        let text = RichText::new(short).color(if now == d {
            decision_colour(d)
        } else {
            ui.visuals().text_color()
        });
        let r = ui
            .add_enabled(enabled, egui::Button::selectable(now == d, text))
            .on_hover_text(decision_name(d));
        if r.clicked() {
            app.set_decision(path, if now == d { Decision::Undecided } else { d });
        }
    }
}

/// Under a Compare pane or Survey tile while reviewing a group.
pub fn entry_controls(app: &mut ShoeboxApp, ui: &mut egui::Ui, path: &Path) {
    let Some((r, k)) = open_entry(app, path) else {
        return;
    };
    let Some(g) = app.dupes.open.and_then(|g| r.groups.get(g)) else {
        return;
    };
    let e = &g.entries[k];
    let f = &r.files[e.file];
    let reference = f.role == Role::Reference;
    ui.horizontal(|ui| {
        if reference {
            ui.label(RichText::new("REF 🔒").strong())
                .on_hover_text("In a reference folder: never moved");
        }
        ui.weak(match_text(&r, g.entries[0].file, e));
    });
    ui.horizontal(|ui| decision_buttons(app, ui, &f.path.clone(), reference, k));
}

/// Above Compare/Survey while reviewing: which group, stepping, back.
pub fn group_bar(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let Some(g) = app.dupes.open else {
        return;
    };
    let visible = app.dupes.visible();
    let pos = visible.iter().position(|&x| x == g).unwrap_or(0);
    ui.horizontal(|ui| {
        if ui.button("‹ All groups").on_hover_text("Esc").clicked() {
            app.close_group();
        }
        ui.separator();
        if ui
            .add_enabled(pos > 0, egui::Button::new("◀"))
            .on_hover_text("Previous group ([)")
            .clicked()
        {
            app.step_group(-1);
        }
        ui.label(format!("Group {} of {}", pos + 1, visible.len()));
        if ui
            .add_enabled(pos + 1 < visible.len(), egui::Button::new("▶"))
            .on_hover_text("Next group (])")
            .clicked()
        {
            app.step_group(1);
        }
        ui.separator();
        if ui
            .button("Auto-select")
            .on_hover_text(auto_hint(app.dupes.auto))
            .clicked()
        {
            app.auto_select(Some(g));
        }
        apply_button(app, ui);
    });
    ui.add_space(4.0);
}

/// A slider over match distances (0 to [`MAX_THRESHOLD`] bits) shown as
/// the percentage matches show, stricter to the right; with `hundred`, one
/// more step at the right end, -1: 100% (identical files and the same
/// image data only).
fn percent_slider(ui: &mut egui::Ui, value: &mut i32, hundred: bool) -> egui::Response {
    let top = if hundred { -1 } else { 0 };
    ui.add(
        egui::Slider::new(value, MAX_THRESHOLD as i32..=top)
            .custom_formatter(|v, _| match v as i32 {
                d if d < 0 => "100%".to_owned(),
                d => format!("{}%", dupes::percent(d as u32)),
            })
            .custom_parser(move |t| {
                let want: f64 = t.trim().trim_end_matches('%').trim().parse().ok()?;
                // The strictest step at or below the percentage typed.
                if want >= 100.0 && hundred {
                    return Some(-1.0);
                }
                (0..=MAX_THRESHOLD)
                    .rev()
                    .find(|&d| f64::from(dupes::percent(d)) >= want)
                    .or(Some(0))
                    .map(f64::from)
            }),
    )
}

/// "Auto-select copies ≥ 100% taken within the same moment".
fn auto_controls(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.label("Auto-select copies ≥");
    let rule = &mut app.dupes.auto;
    let mut v = rule.up_to.map_or(-1, |d| d as i32);
    percent_slider(ui, &mut v, true).on_hover_text(
        "How alike a copy must be for Auto-select to mark it to move. \
         100%: only identical files and the same image data",
    );
    rule.up_to = u32::try_from(v).ok();
    {
        ui.label("taken within");
        egui::ComboBox::from_id_salt("dupes-within")
            .selected_text(within_name(rule.within))
            .show_ui(ui, |ui| {
                for w in WITHINS {
                    ui.selectable_value(&mut rule.within, w, within_name(w));
                }
            })
            .response
            .on_hover_text(
                "Below 100%, mark only copies taken this close in time to the copy they \
                 match (or with no capture time to tell). Burst frames are a second or so \
                 apart and look alike (often over 95%) but are different photos; a copy \
                 whose clock or time zone was off can be hours apart. At 100% it \
                 doesn't matter: identical copies are marked whenever they were taken",
            );
    }
    ui.separator();
}

/// The gaps "taken within" offers (None: any time).
const WITHINS: [Option<f64>; 9] = [
    Some(0.0),
    Some(0.5),
    Some(1.0),
    Some(2.0),
    Some(5.0),
    Some(60.0),
    Some(3600.0),
    Some(86_400.0),
    None,
];

/// "the same moment", "2 s", "1 h", "any time".
pub(crate) fn within_name(w: Option<f64>) -> String {
    match w {
        Some(0.0) => "the same moment".to_owned(),
        Some(s) => span(s),
        None => "any time".to_owned(),
    }
}

fn auto_hint(rule: AutoSelect) -> String {
    let keep = "Keep the reference copy (else NEF > TIFF > HEIC > JPG, then the largest)";
    match rule.up_to {
        None => format!(
            "{keep}; move identical files and copies with the same image data (100%); leave the rest undecided"
        ),
        Some(d) => format!(
            "{keep}; move identical copies, and copies {}% alike or more{}; leave the rest undecided",
            dupes::percent(d),
            match rule.within {
                Some(_) => format!(" taken within {} of their copy", within_name(rule.within)),
                None => String::new(),
            }
        ),
    }
}

/// While decisions are applied: the stage, a bar, how many of how many and
/// the time left. Nothing can be decided meanwhile.
fn applying_panel(app: &ShoeboxApp, ui: &mut egui::Ui) {
    let Some((stage, done, total, since)) = app.dupes.applying else {
        return;
    };
    let (what, unit) = match stage {
        ApplyStage::Checking => (
            "Checking the files are unchanged since they were found",
            "files read",
        ),
        ApplyStage::Moving => ("Moving to _duplicates", "files and sidecars moved"),
    };
    ui.add_space(4.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new());
            ui.strong(what);
        });
        let fraction = if total == 0 {
            0.0
        } else {
            done as f32 / total as f32
        };
        ui.add(egui::ProgressBar::new(fraction).desired_width(ui.available_width().min(520.0)));
        let elapsed = since.elapsed().as_secs_f64();
        let left = (done > 0 && elapsed > 3.0).then(|| {
            crate::panels::time_left(elapsed * (total - done.min(total)) as f64 / done as f64)
        });
        ui.weak(format!(
            "{} of {} {unit}{}",
            group_digits(done),
            group_digits(total),
            left.map(|l| format!(" · {l}")).unwrap_or_default()
        ));
        ui.weak(
            "One batch: ⌘Z puts everything back. Then the library is read again and the \
             duplicates found again.",
        );
    });
}

/// 17964 → "17,964".
fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn apply_button(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let n = app.marked_to_move();
    let other = app
        .dupes
        .decisions
        .values()
        .any(|d| matches!(d, Decision::Variant | Decision::Ignore));
    let label = if n > 0 {
        format!("Apply… ({n} to move)")
    } else {
        "Apply…".to_owned()
    };
    if ui
        .add_enabled(
            (n > 0 || other) && !app.dupes.running && app.dupes.applying.is_none(),
            egui::Button::new(label),
        )
        .on_hover_text("Shows every change before anything moves")
        .clicked()
    {
        app.request_apply();
    }
}

/// The list of groups.
/// "In: All photos ▾": where the next find looks. A folder, event or
/// collection compares only its own photos with each other.
fn scope_combo(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let mut choices = vec![DupeScope::All];
    // The folders shown in the grid, and one chosen from a folder's menu.
    if let DupeScope::Folder(f) = &app.dupes.scope
        && !app.folders.contains(f)
    {
        choices.push(app.dupes.scope.clone());
    }
    choices.extend(app.folders.iter().cloned().map(DupeScope::Folder));
    let events = choices.len();
    choices.extend(app.events.iter().map(|e| DupeScope::Event(e.id.clone())));
    let collections = choices.len();
    choices.extend(
        app.collections
            .iter()
            .map(|c| DupeScope::Collection(c.id.clone())),
    );
    let mut scope = app.dupes.scope.clone();
    ui.label("In");
    egui::ComboBox::from_id_salt("dupe-scope")
        .selected_text(app.scope_name(&scope))
        .height(400.0)
        .show_ui(ui, |ui| {
            for (k, c) in choices.iter().enumerate() {
                if k == events && events < collections {
                    ui.separator();
                    ui.weak("Events");
                }
                if k == collections && collections < choices.len() {
                    ui.separator();
                    ui.weak("Collections");
                }
                let name = match c {
                    DupeScope::Folder(_) => format!("📁 {}", app.scope_name(c)),
                    _ => app.scope_name(c),
                };
                ui.selectable_value(&mut scope, c.clone(), name);
            }
        })
        .response
        .on_hover_text(
            "Where to look: a folder, event or collection compares only its own photos with each other",
        );
    app.dupes.scope = scope;
    ui.separator();
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.heading("Duplicates");
        ui.separator();
        scope_combo(app, ui);
        ui.label("Show matches ≥");
        let mut v = app.dupes.config.threshold as i32;
        percent_slider(ui, &mut v, false).on_hover_text(
            "How alike two files must look to be listed. Identical files and the same \
             image data (100%) are always found; below that, a resized, recompressed \
             or edited copy. Lower finds more, and more look-alikes that aren't copies \
             (burst frames, the same view)",
        );
        app.set_dupe_threshold(u32::try_from(v).unwrap_or(0));
        let label = if app.dupes.review.is_some() {
            "Find again"
        } else {
            "Find duplicates"
        };
        if ui
            .add_enabled(
                !app.dupes.running && app.dupes.applying.is_none(),
                egui::Button::new(label),
            )
            .clicked()
        {
            app.find_duplicates();
        }
        if app.dupes.running {
            ui.add(egui::Spinner::new());
            match (app.dupes.progress, app.scan_progress) {
                (Some((d, t)), _) if d < t => ui.label(format!("Hashing {d}/{t}")),
                // After applying: the library is read again, then searched.
                (None, Some((d, t))) => ui.label(format!("Reading the library {d}/{t}")),
                _ => ui.label("Matching…"),
            };
        }
    });
    let Some(r) = app.dupes.review.clone() else {
        if !app.dupes.running {
            ui.add_space(20.0);
            ui.weak(
                "Finds identical files, the same image data (a carved preview of a NEF), \
                 and resized or recompressed copies. Nothing moves until you apply.",
            );
        }
        return;
    };
    let visible = app.dupes.visible();
    let reference_only = r.groups.iter().filter(|g| g.reference_only(&r)).count();
    ui.horizontal_wrapped(|ui| {
        let files: usize = visible.iter().map(|&g| r.groups[g].entries.len()).sum();
        let within = match &app.dupes.found_in {
            Some(s) if *s != DupeScope::All => format!(" in {}", app.scope_name(s)),
            _ => String::new(),
        };
        ui.label(format!(
            "{} · {}{within}",
            plural(visible.len(), "group"),
            plural(files, "file")
        ));
        if reference_only > 0 {
            ui.checkbox(
                &mut app.dupes.show_reference_only,
                format!(
                    "Show {} of reference files only",
                    plural(reference_only, "group")
                ),
            )
            .on_hover_text(
                "Matches inside the reference folders (such as a NEF and its export): nothing there can move",
            );
        }
        egui::ComboBox::from_id_salt("dupes-order")
            .selected_text(if app.dupes.least_alike_first {
                "Least alike first"
            } else {
                "Most alike first"
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.dupes.least_alike_first, false, "Most alike first");
                ui.selectable_value(&mut app.dupes.least_alike_first, true, "Least alike first");
            })
            .response
            .on_hover_text("Each group by its least alike match");
    });
    // Auto-select and applying: a row of their own.
    ui.horizontal_wrapped(|ui| {
        auto_controls(app, ui);
        if ui
            .button("Auto-select all")
            .on_hover_text(auto_hint(app.dupes.auto))
            .clicked()
        {
            app.auto_select(None);
        }
        if ui.button("Clear decisions").clicked() {
            app.dupes.decisions.clear();
        }
        apply_button(app, ui);
        if let Some(m) = &app.dupes.message {
            ui.separator();
            ui.label(RichText::new(m).color(Color32::from_rgb(80, 200, 120)));
        }
    });
    applying_panel(app, ui);
    ui.separator();
    let cloud = app.dupes.review.as_ref().map_or(0, |r| r.stats.cloud);
    if cloud > 0 {
        ui.weak(format!(
            "{} in the cloud only were left out (checking them would download them). Download them first to include them.",
            plural(cloud, "photo")
        ));
    }
    let visible = app.dupes.visible();
    if visible.is_empty() {
        ui.add_space(20.0);
        ui.weak("No duplicates to review.");
        return;
    }
    let row_h = app.dupes.thumb().y + ROW_TEXT;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_h, visible.len(), |ui, range| {
            for row in range {
                group_row(app, ui, &r, visible[row], row);
            }
        });
}

fn group_row(app: &mut ShoeboxApp, ui: &mut egui::Ui, r: &Review, gi: usize, row: usize) {
    let g = &r.groups[gi];
    let row_h = app.dupes.thumb().y + ROW_TEXT;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), egui::Sense::hover());
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 4.0))));
    let ui = &mut ui;
    ui.horizontal(|ui| {
        let summary = g
            .summary()
            .map(|m| match m.level {
                Level::Exact => "identical files".to_owned(),
                Level::ImageData => "same image data".to_owned(),
                Level::NearIdentical => format!("near-identical (≥ {}%)", m.percent()),
                Level::Similar => format!("similar (≥ {}%)", m.percent()),
            })
            .unwrap_or_default();
        ui.label(RichText::new(format!("{}", row + 1)).strong());
        ui.label(format!("{} files · {summary}", g.entries.len()));
        if g.has_reference {
            ui.weak("· has a reference copy");
        }
        if ui
            .small_button("Auto-select")
            .on_hover_text(auto_hint(app.dupes.auto))
            .clicked()
        {
            app.auto_select(Some(gi));
        }
        let open = if g.entries.len() == 2 {
            "Open in Compare"
        } else {
            "Open in Survey"
        };
        if ui.small_button(open).clicked() {
            app.open_group(gi);
        }
    });
    egui::ScrollArea::horizontal()
        .id_salt(("dupe-strip", gi))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (k, e) in g.entries.iter().enumerate() {
                    entry_tile(app, ui, r, gi, k, e);
                }
            });
        });
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 1.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}

fn entry_tile(app: &mut ShoeboxApp, ui: &mut egui::Ui, r: &Review, gi: usize, k: usize, e: &Entry) {
    let f = &r.files[e.file];
    let decision = app.decision(&f.path);
    ui.vertical(|ui| {
        let thumb = app.dupes.thumb();
        ui.set_width(thumb.x);
        let (rect, resp) = ui.allocate_exact_size(thumb, egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
        let id = photo_id(&f.path);
        if app.index(id).is_some() {
            let size = super::size_for(thumb.x, ui.ctx().pixels_per_point());
            app.want(id, size, Priority::Visible);
            if size == Size::Large {
                app.want(id, Size::Small, Priority::Visible); // meanwhile
            }
            match app.textures.best(id, size) {
                Some(t) => {
                    let [w, h] = t.size();
                    let target = super::fit(rect.shrink(3.0), w as f32 / h as f32);
                    painter.image(
                        t.id(),
                        target,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                None => {
                    painter.rect_filled(rect.shrink(3.0), 2.0, ui.visuals().extreme_bg_color);
                }
            }
        }
        if decision == Decision::Move || decision == Decision::Ignore {
            painter.rect_filled(rect, 3.0, Color32::from_black_alpha(110));
        }
        // Decision strip along the bottom of the thumbnail.
        painter.rect_filled(
            Rect::from_min_max(pos2(rect.left(), rect.bottom() - 4.0), rect.max),
            0.0,
            decision_colour(decision),
        );
        let name = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
        if resp.clicked() {
            app.open_group(gi);
            if let Some(idx) = app.index(id) {
                app.activate(app.ids[idx]);
            }
        }
        let reference = f.role == Role::Reference;
        let mut head = format!("{} ", f.format.label());
        if let (Some(w), Some(h)) = (f.width, f.height) {
            head.push_str(&format!("{w}×{h}"));
        }
        if reference {
            head.push_str(" · REF 🔒");
        }
        ui.label(RichText::new(head).small().strong());
        ui.label(
            RichText::new(match_text(r, r.groups[gi].entries[0].file, e))
                .small()
                .weak(),
        )
        .on_hover_text(match_text(r, r.groups[gi].entries[0].file, e));
        let root = r.roots.get(f.root).map(|x| x.path.as_path());
        let shown_path = root
            .and_then(|rp| rp.file_name())
            .map(|n| format!("{}/{}", n.to_string_lossy(), f.rel))
            .unwrap_or_else(|| f.path.display().to_string());
        ui.add(egui::Label::new(RichText::new(shown_path).small()).truncate())
            .on_hover_text(f.path.display().to_string());
        ui.menu_button(
            RichText::new(decision_name(decision))
                .small()
                .color(decision_colour(decision)),
            |ui| {
                for d in [
                    Decision::Undecided,
                    Decision::Keep,
                    Decision::Move,
                    Decision::Variant,
                    Decision::Ignore,
                ] {
                    let enabled =
                        !(reference && d == Decision::Move) && !(k == 0 && d == Decision::Variant);
                    if ui
                        .add_enabled(
                            enabled,
                            egui::Button::selectable(decision == d, decision_name(d)),
                        )
                        .clicked()
                    {
                        app.set_decision(&f.path, d);
                        ui.close();
                    }
                }
            },
        );
    });
}

/// A file the dry run moves: its name and folder, how alike it is to the
/// copy kept, and that copy.
#[derive(Clone)]
struct MoveRow {
    name: String,
    folder: String,
    level: Level,
    percent: u8,
    kept: String,
}

/// The files marked to move among those listed, by folder, each with the
/// copy that stays.
fn move_rows(app: &ShoeboxApp, short: &dyn Fn(&Path) -> String) -> Vec<MoveRow> {
    let Some(r) = &app.dupes.review else {
        return Vec::new();
    };
    let mut rows: Vec<MoveRow> = r
        .groups
        .iter()
        .flat_map(|g| g.entries.iter().map(move |e| (g, e)))
        .filter(|(_, e)| app.dupes.decisions.get(&r.files[e.file].path) == Some(&Decision::Move))
        .filter_map(|(g, e)| {
            let path = &r.files[e.file].path;
            let (other, m) = e.best.or(e.to_keeper.map(|m| (g.entries[0].file, m)))?;
            Some(MoveRow {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                folder: path.parent().map(short).unwrap_or_default(),
                level: m.level,
                percent: m.percent(),
                kept: short(&r.files[other].path),
            })
        })
        .collect();
    rows.sort_by(|a, b| (&a.folder, &a.name).cmp(&(&b.folder, &b.name)));
    rows
}

/// The dry run: what applying does, in numbers, then every file to move
/// beside the copy that stays. Sized to the window, the buttons always in
/// view.
pub fn apply_dialog(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let Some(preview) = app.dupes.preview.clone() else {
        return;
    };
    let roots: Vec<std::path::PathBuf> = app
        .dupes
        .review
        .as_ref()
        .map(|r| r.roots.iter().map(|x| x.path.clone()).collect())
        .unwrap_or_default();
    let short = |p: &Path| -> String {
        roots
            .iter()
            .find_map(|r| {
                p.strip_prefix(r).ok().map(|rel| {
                    let name = r.file_name().unwrap_or_default().to_string_lossy();
                    if rel.as_os_str().is_empty() {
                        name.into_owned()
                    } else {
                        format!("{name}/{}", rel.display())
                    }
                })
            })
            .unwrap_or_else(|| p.display().to_string())
    };
    // Worked out once per dry run (thousands of rows).
    let rows_id = egui::Id::unique("dupes-apply-rows");
    let rows: std::sync::Arc<Vec<MoveRow>> = match ctx.data(|d| d.get_temp(rows_id)) {
        Some(r) => r,
        None => {
            let r = std::sync::Arc::new(move_rows(app, &short));
            ctx.data_mut(|d| d.insert_temp(rows_id, r.clone()));
            r
        }
    };
    let screen = ctx.content_rect();
    let width = (screen.width() - 96.0).clamp(320.0, 980.0);
    let list_height = (screen.height() - 380.0).max(120.0);
    let mut close = false;
    let mut confirm = false;
    egui::Modal::new(egui::Id::unique("dupes-apply")).show(ctx, |ui| {
        ui.set_width(width);
        match &preview {
            Err(e) => {
                ui.heading("Can't apply");
                ui.add(egui::Label::new(e).wrap());
                if ui.button("OK").clicked() {
                    close = true;
                }
            }
            Ok(p) => {
                let files = p
                    .moves
                    .iter()
                    .filter(|m| {
                        !m.src
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
                    })
                    .count();
                let sidecars = p.moves.len() - files;
                ui.heading(if files > 0 {
                    format!("Move {} to _duplicates?", plural_n(files, "file"))
                } else {
                    "Apply duplicate decisions?".to_owned()
                });
                ui.add_space(4.0);
                if files > 0 {
                    let kinds = [
                        (Level::Exact, "identical files (100%)"),
                        (Level::ImageData, "the same image data (100%)"),
                        (Level::NearIdentical, "near-identical"),
                        (Level::Similar, "similar"),
                    ];
                    let parts: Vec<String> = kinds
                        .iter()
                        .filter_map(|&(l, name)| {
                            let n = rows.iter().filter(|r| r.level == l).count();
                            (n > 0).then(|| format!("{} {name}", group_digits(n)))
                        })
                        .collect();
                    ui.label(parts.join(" · "));
                    let lowest = rows.iter().map(|r| r.percent).min().unwrap_or(100);
                    if lowest < 100 {
                        ui.weak(format!(
                            "The least alike is {lowest}%: check the list if that's lower than you meant."
                        ));
                    }
                    // The first move, as an example of where they go.
                    let example = p
                        .moves
                        .first()
                        .map(|m| format!(" ({} goes to {})", short(&m.src), short(&m.dst)))
                        .unwrap_or_default();
                    ui.add(
                        egui::Label::new(format!(
                            "Each goes to _duplicates in its own library folder, keeping its \
                             path{example}{}. Nothing is deleted, and the copy kept stays where \
                             it is.",
                            if sidecars > 0 {
                                format!(", with its sidecar ({} in all)", group_digits(sidecars))
                            } else {
                                String::new()
                            }
                        ))
                        .wrap(),
                    );
                }
                if p.variants > 0 {
                    ui.label(format!(
                        "Add {} to stacks as variants.",
                        plural_n(p.variants, "file")
                    ));
                }
                if p.ignores > 0 {
                    ui.label(format!(
                        "Remember {} as not duplicates.",
                        plural_n(p.ignores, "pair")
                    ));
                }
                if !rows.is_empty() {
                    ui.add_space(6.0);
                    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
                    let pct_w = 48.0;
                    let col = |ui: &egui::Ui| ((ui.available_width() - pct_w - 16.0) / 2.0).max(80.0);
                    let cell = |ui: &mut egui::Ui, w: f32, text: RichText| {
                        ui.allocate_ui_with_layout(
                            vec2(w, row_h),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_width(w);
                                ui.add(egui::Label::new(text).truncate())
                            },
                        );
                    };
                    ui.horizontal(|ui| {
                        let w = col(ui);
                        cell(ui, w, RichText::new("File to move").strong());
                        cell(ui, pct_w, RichText::new("Alike").strong());
                        cell(ui, w, RichText::new("Copy kept").strong());
                    });
                    egui::ScrollArea::vertical()
                        .max_height(list_height)
                        .auto_shrink([false, true])
                        .show_rows(ui, row_h, rows.len(), |ui, range| {
                            for r in &rows[range] {
                                ui.horizontal(|ui| {
                                    let w = col(ui);
                                    // The name first: a long folder is what gets cut.
                                    cell(
                                        ui,
                                        w,
                                        RichText::new(format!("{}  ·  {}", r.name, r.folder)).small(),
                                    );
                                    cell(ui, pct_w, RichText::new(format!("{}%", r.percent)).small());
                                    cell(ui, w, RichText::new(&r.kept).small().weak());
                                })
                                .response
                                .on_hover_text(format!(
                                    "{}/{}\nkept: {}",
                                    r.folder, r.name, r.kept
                                ));
                            }
                        });
                }
                ui.add_space(8.0);
                ui.weak("One batch: ⌘Z (or `shoebox undo`) puts everything back.");
                ui.horizontal(|ui| {
                    let go = if files > 0 {
                        format!("Move {}", plural_n(files, "file"))
                    } else {
                        "Apply".to_owned()
                    };
                    if ui.button(RichText::new(go).strong()).clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
        }
    });
    if confirm {
        app.confirm_apply();
    } else if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.dupes.preview = None;
    }
    if app.dupes.preview.is_none() {
        ctx.data_mut(|d| d.remove::<std::sync::Arc<Vec<MoveRow>>>(rows_id));
    }
}
