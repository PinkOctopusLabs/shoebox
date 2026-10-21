//! Tasks → Duplicates: groups as thumbnail strips with distance, dimensions
//! and paths. Decide per file (keep, move to `_duplicates/`, add to the
//! stack as a variant, or not a duplicate), or let auto-select propose;
//! open a group to look closely (Compare for two files, Survey for more).
//! Apply shows every move first and runs them as one undoable batch.

use std::path::Path;

use eframe::egui::{self, Color32, Rect, RichText, pos2, vec2};
use shoebox_core::dupes::Level;
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
/// "Near-identical · 3", with "other exposure" or "unconfirmed" when it
/// isn't proposed for moving.
pub fn match_text(r: &Review, keeper: usize, e: &Entry) -> String {
    let Some((other, m)) = e.best else {
        return "Kept copy".to_owned();
    };
    let level = match m.level {
        Level::Exact => "Identical file".to_owned(),
        Level::ImageData => "Same image data".to_owned(),
        Level::NearIdentical => format!("Near-identical · {}", m.distance),
        Level::Similar => format!("Similar · {}", m.distance),
    };
    let mut s = level;
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
    if !e.same_exposure {
        s.push_str(" · other exposure");
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
            .on_hover_text("Keep the reference copy (else NEF > TIFF > JPG, then the largest); move other copies taken at the same moment")
            .clicked()
        {
            app.auto_select(Some(g));
        }
        apply_button(app, ui);
    });
    ui.add_space(4.0);
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
            (n > 0 || other) && !app.dupes.running,
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
        let cfg = &mut app.dupes.config;
        ui.label("Similar ≤");
        ui.add(egui::Slider::new(&mut cfg.threshold, 0..=16))
            .on_hover_text("Largest perceptual distance (bits) that counts as a match");
        ui.label("Near-identical ≤");
        ui.add(egui::Slider::new(&mut cfg.near_identical, 0..=8));
        cfg.near_identical = cfg.near_identical.min(cfg.threshold);
        let label = if app.dupes.review.is_some() {
            "Find again"
        } else {
            "Find duplicates"
        };
        if ui
            .add_enabled(!app.dupes.running, egui::Button::new(label))
            .clicked()
        {
            app.find_duplicates();
        }
        if app.dupes.running {
            ui.add(egui::Spinner::new());
            match app.dupes.progress {
                Some((d, t)) if d < t => ui.label(format!("Hashing {d}/{t}")),
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
        ui.separator();
        if ui
            .button("Auto-select all")
            .on_hover_text("Keep the reference copy (else NEF > TIFF > JPG, then the largest); move the other copies taken at the same moment or with no capture time to tell; leave other exposures (burst frames) undecided")
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
                Level::NearIdentical => format!("near-identical (≤ {})", m.distance),
                Level::Similar => format!("similar (≤ {})", m.distance),
            })
            .unwrap_or_default();
        ui.label(RichText::new(format!("{}", row + 1)).strong());
        ui.label(format!("{} files · {summary}", g.entries.len()));
        if g.has_reference {
            ui.weak("· has a reference copy");
        }
        if ui.small_button("Auto-select").clicked() {
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

/// The dry-run table: every move (sidecars included) before anything runs.
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
                    format!(
                        "{}/{}",
                        r.file_name().unwrap_or_default().to_string_lossy(),
                        rel.display()
                    )
                })
            })
            .unwrap_or_else(|| p.display().to_string())
    };
    let mut close = false;
    let mut confirm = false;
    egui::Modal::new(egui::Id::unique("dupes-apply")).show(ctx, |ui| {
        ui.set_width(760.0);
        match &preview {
            Err(e) => {
                ui.heading("Can't apply");
                ui.label(e);
                if ui.button("OK").clicked() {
                    close = true;
                }
            }
            Ok(p) => {
                ui.heading("Apply duplicate decisions");
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
                let mut lines = Vec::new();
                if files > 0 {
                    lines.push(format!(
                        "Move {} to _duplicates{}",
                        plural(files, "file"),
                        if sidecars > 0 {
                            format!(" (with {})", plural(sidecars, "sidecar"))
                        } else {
                            String::new()
                        }
                    ));
                }
                if p.variants > 0 {
                    lines.push(format!(
                        "Add {} to stacks as variants",
                        plural(p.variants, "file")
                    ));
                }
                if p.ignores > 0 {
                    lines.push(format!(
                        "Remember {} as not duplicates",
                        plural(p.ignores, "pair")
                    ));
                }
                for l in lines {
                    ui.label(l);
                }
                if !p.moves.is_empty() {
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            egui::Grid::new("dupe-moves")
                                .striped(true)
                                .num_columns(3)
                                .show(ui, |ui| {
                                    for m in &p.moves {
                                        ui.label(RichText::new(short(&m.src)).monospace().small());
                                        ui.weak("▶");
                                        ui.label(RichText::new(short(&m.dst)).monospace().small());
                                        ui.end_row();
                                    }
                                });
                        });
                }
                ui.add_space(8.0);
                ui.weak("One batch: ⌘Z (or `shoebox undo`) puts everything back.");
                ui.horizontal(|ui| {
                    let go = if files > 0 {
                        format!("Move {}", plural(files, "file"))
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
}
