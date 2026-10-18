//! Toolbar, status bar, library tree and info panel.

use eframe::egui::{self, Color32, RichText, pos2, vec2};

use shoebox_core::marks::{Label, Pick};

use crate::app::{FolderNode, ShoeboxApp, View, reveal};
use crate::jobs::LibraryCommand;

/// The bar over the photos (as Lightroom's Library Filter): what's shown
/// (scope, search, filters).
pub fn filter_strip(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.add_space(2.0);
    filter_bar(app, ui);
    ui.add_space(2.0);
}

/// The bar under the photos (as in Lightroom): views, layout, then sort and
/// thumbnail size at the right.
pub fn viewbar(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let before = (app.sort, app.sort_desc);
    // Size for the thumbnail views; sort wherever photos are in order.
    let sized = matches!(app.view, View::Grid | View::Masonry | View::Timeline);
    let sorted = !matches!(app.view, View::Duplicates | View::Import | View::History);
    let right = |app: &mut ShoeboxApp, ui: &mut egui::Ui| {
        panel_toggle(ui, &mut app.right_open, false);
        if sized {
            ui.spacing_mut().slider_width = 80.0;
            ui.add(egui::Slider::new(&mut app.thumb_size, 96.0..=512.0).show_value(false))
                .on_hover_text("Thumbnail size");
        }
        if sized && sorted {
            ui.separator();
        }
        if sorted && app.semantic_query().is_some() {
            ui.weak("Best match first");
        } else if sorted {
            sort_controls(app, ui);
        }
    };
    let mut wrap = false;
    ui.horizontal(|ui| {
        panel_toggle(ui, &mut app.left_open, true);
        for (v, name, key) in View::ALL {
            let hint = format!("{name} ({})", key.name());
            if ui
                .selectable_label(app.view == v, name)
                .on_hover_text(hint)
                .clicked()
            {
                app.set_view(v);
            }
        }
        if ui
            .selectable_label(app.view == View::Map, "Map")
            .on_hover_text("Photos with a location")
            .clicked()
        {
            app.set_view(View::Map);
        }
        // Sort and size at the right when there's room, else on a second line.
        if ui.available_width() >= 300.0 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                right(app, ui)
            });
        } else {
            wrap = true;
        }
    });
    if wrap {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(app, ui)
        });
    }
    if before != (app.sort, app.sort_desc) {
        app.refilter();
        app.scroll_to_current = true;
    }
}

/// Background work and undo/redo (laid out right to left).
fn work_and_undo(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if ui
        .add_enabled(app.can_redo, egui::Button::new("Redo"))
        .on_hover_text("Redo (Shift+⌘Z)")
        .clicked()
    {
        app.command(LibraryCommand::Redo);
    }
    if ui
        .add_enabled(app.can_undo, egui::Button::new("Undo"))
        .on_hover_text("Undo (⌘Z)")
        .clicked()
    {
        app.command(LibraryCommand::Undo);
    }
    ui.separator();
    let busy = app.loader.busy();
    if let Some((done, total)) = app.scan_progress {
        ui.label(format!("Reading {done}/{total}"));
        ui.add(egui::Spinner::new());
    } else if app.scanning {
        ui.label("Scanning…");
        ui.add(egui::Spinner::new());
    } else if busy > 0 {
        ui.label(format!("{busy} thumbnails"));
        ui.add(egui::Spinner::new());
    } else if let Some(task) = semantic_task(&app.sem.status) {
        ui.label(task)
            .on_hover_text("Semantic search (see Search in Settings)");
        ui.add(egui::Spinner::new());
    } else if let Some(task) = people_task(&app.ppl.status) {
        ui.label(task)
            .on_hover_text("People (see People in Settings)");
        ui.add(egui::Spinner::new());
    }
}

fn filter_bar(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let before = (
        app.filter.clone(),
        app.sort,
        app.sort_desc,
        app.sem.by_content,
    );
    ui.horizontal_wrapped(|ui| {
        scope_chips(app, ui);
        search_box(app, ui);
        egui::ComboBox::from_id_salt("f-rating")
            .selected_text(if app.filter.min_rating == 0 {
                "Any rating".to_owned()
            } else {
                format!("★ ≥ {}", app.filter.min_rating)
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.filter.min_rating, 0, "Any rating");
                for r in 1..=5u8 {
                    ui.selectable_value(&mut app.filter.min_rating, r, format!("★ ≥ {r}"));
                }
            });
        use shoebox_core::query::{FlagFilter, LabelFilter, LabelName};
        egui::ComboBox::from_id_salt("f-flag")
            .selected_text(match app.filter.flag {
                FlagFilter::Any => "Any flag",
                FlagFilter::Picked => "Picked",
                FlagFilter::Rejected => "Rejected",
                FlagFilter::Unflagged => "Unflagged",
                FlagFilter::NotRejected => "Not rejected",
            })
            .show_ui(ui, |ui| {
                for (f, name) in [
                    (FlagFilter::Any, "Any flag"),
                    (FlagFilter::Picked, "Picked"),
                    (FlagFilter::Rejected, "Rejected"),
                    (FlagFilter::Unflagged, "Unflagged"),
                    (FlagFilter::NotRejected, "Not rejected"),
                ] {
                    ui.selectable_value(&mut app.filter.flag, f, name);
                }
            });
        let label_name = |l: LabelFilter| match l {
            LabelFilter::Any => "Any label".to_owned(),
            LabelFilter::None => "No label".to_owned(),
            LabelFilter::Is(n) => format!("{n:?}"),
        };
        egui::ComboBox::from_id_salt("f-label")
            .selected_text(label_name(app.filter.label))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut app.filter.label, LabelFilter::Any, "Any label");
                ui.selectable_value(&mut app.filter.label, LabelFilter::None, "No label");
                for l in Label::ALL {
                    let n = LabelName::from(l);
                    ui.selectable_value(
                        &mut app.filter.label,
                        LabelFilter::Is(n),
                        label_name(LabelFilter::Is(n)),
                    );
                }
            });
        filters_button(app, ui);
        if !app.filter.is_empty() && ui.small_button("Clear").clicked() {
            app.filter = shoebox_core::query::Filter::default();
        }
        // How this view lays the photos out (not a view of its own).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            layout_toggle(app, ui);
        });
    });
    if before
        != (
            app.filter.clone(),
            app.sort,
            app.sort_desc,
            app.sem.by_content,
        )
    {
        app.refilter();
        app.scroll_to_current = true;
    }
}

/// The search box, the "Content" switch for semantic search, or what
/// "Find Similar" is showing.
fn search_box(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if let Some((id, path)) = app.sem.similar.clone() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if ui
            .link(format!("Photos like {name}"))
            .on_hover_text("Show that photo")
            .clicked()
        {
            app.activate(id);
            app.scroll_to_current = true;
        }
        if ui
            .small_button("✖")
            .on_hover_text("Show all photos")
            .clicked()
        {
            app.clear_similar();
        }
        return;
    }
    if let Some(g) = app.ppl.shown_group().cloned() {
        person_bar(app, ui, &g);
        return;
    }
    if let Some(l) = app.sem.tag {
        let name = app
            .sem
            .tags
            .as_ref()
            .and_then(|t| t.names.get(l as usize).cloned())
            .unwrap_or_default();
        ui.label(format!("Tagged \u{201c}{name}\u{201d}"));
        if ui
            .small_button("✖")
            .on_hover_text("Show all photos")
            .clicked()
        {
            app.clear_tag();
        }
        return;
    }
    if app.settings.semantic_search {
        let ready = app.sem.status.ready();
        ui.add_enabled_ui(ready, |ui| {
            ui.toggle_value(&mut app.sem.by_content, "Content")
                .on_hover_text("Search by what's in the photos: describe it, in any language")
                .on_disabled_hover_text(
                    "Semantic search is getting ready (see Search in Settings)",
                );
        });
    }
    let hint = if app.sem.by_content {
        "Describe the photo, e.g. dog on a beach (/)"
    } else {
        "Search names and keywords (/)"
    };
    let search = ui.add(
        egui::TextEdit::singleline(&mut app.filter.search)
            .hint_text(hint)
            .desired_width(if app.sem.by_content { 260.0 } else { 200.0 }),
    );
    if app.focus_search {
        search.request_focus();
        app.focus_search = false;
    }
    if app.semantic_query().is_some() {
        if app.sem.searching {
            ui.add(egui::Spinner::new());
        } else if let Some(e) = &app.sem.error {
            ui.colored_label(ui.visuals().warn_fg_color, e);
        }
    }
}

/// Sort order and direction (laid out right to left).
fn sort_controls(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if ui
        .small_button(if app.sort_desc { "⏷" } else { "⏶" })
        .on_hover_text(if app.sort_desc {
            "Descending (click to reverse)"
        } else {
            "Ascending (click to reverse)"
        })
        .clicked()
    {
        app.sort_desc = !app.sort_desc;
    }
    egui::ComboBox::from_id_salt("sort")
        .selected_text(
            shoebox_core::query::Sort::ALL
                .iter()
                .find(|(s, _)| *s == app.sort)
                .map_or("", |(_, n)| *n),
        )
        .show_ui(ui, |ui| {
            for (s, n) in shoebox_core::query::Sort::ALL {
                ui.selectable_value(&mut app.sort, s, n);
            }
        });
    ui.weak("Sort");
}

/// How many of the less-used filters (in the Filters pop-over) are set.
fn extra_filters(f: &shoebox_core::query::Filter) -> usize {
    [
        !f.types.is_empty(),
        f.camera.is_some(),
        f.lens.is_some(),
        f.nef_missing,
        f.undated,
        f.changed_within_days.is_some(),
        f.unseen_changes,
    ]
    .iter()
    .filter(|&&on| on)
    .count()
}

/// "Filters ▾": file type, camera, lens, NEF missing, undated, changed.
fn filters_button(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let n = extra_filters(&app.filter);
    let text = if n == 0 {
        "Filters ⏷".to_owned()
    } else {
        format!("Filters ({n}) ⏷")
    };
    let btn = ui.add(egui::Button::new(text).selected(n > 0));
    egui::Popup::from_toggle_button_response(&btn)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(260.0);
            ui.weak("File type");
            ui.horizontal(|ui| {
                for t in ["NEF", "JPG", "TIF", "PNG", "HEIC", "MOV", "MP4"] {
                    let mut on = app.filter.types.iter().any(|x| x == t);
                    if ui.toggle_value(&mut on, t).changed() {
                        if on {
                            app.filter.types.push(t.to_owned());
                        } else {
                            app.filter.types.retain(|x| x != t);
                        }
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                option_combo(
                    ui,
                    "f-camera",
                    "Any camera",
                    &app.cameras.clone(),
                    &mut app.filter.camera,
                );
                option_combo(
                    ui,
                    "f-lens",
                    "Any lens",
                    &app.lenses.clone(),
                    &mut app.filter.lens,
                );
            });
            ui.add_space(4.0);
            ui.checkbox(&mut app.filter.nef_missing, "NEF missing")
                .on_hover_text("Photos without a RAW file");
            ui.checkbox(&mut app.filter.undated, "Undated")
                .on_hover_text("No capture time");
            ui.checkbox(&mut app.filter.unseen_changes, "Unseen changes")
                .on_hover_text("Changed since you last viewed them");
            let changed_name = |d: Option<u32>| match d {
                None => "Changed any time".to_owned(),
                Some(1) => "Changed today".to_owned(),
                Some(d) => format!("Changed in {d} days"),
            };
            egui::ComboBox::from_id_salt("f-changed")
                .selected_text(changed_name(app.filter.changed_within_days))
                .show_ui(ui, |ui| {
                    for d in [None, Some(1), Some(7), Some(30)] {
                        ui.selectable_value(
                            &mut app.filter.changed_within_days,
                            d,
                            changed_name(d),
                        );
                    }
                });
        });
}

fn option_combo(
    ui: &mut egui::Ui,
    id: &str,
    any: &str,
    values: &[String],
    value: &mut Option<String>,
) {
    if values.is_empty() {
        return;
    }
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.clone().unwrap_or_else(|| any.to_owned()))
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, any);
            for v in values {
                ui.selectable_value(value, Some(v.clone()), v);
            }
        });
}

pub fn status(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(format!(
            "{} selected / {} shown / {} total",
            app.selection.len(),
            app.shown.len(),
            app.catalog.photos.len()
        ));
        if let Some(p) = app.current.and_then(|c| app.photo(c)) {
            ui.separator();
            ui.label(RichText::new(short_path(&p.path)).weak())
                .on_hover_text(p.path.display().to_string());
        }
        let interrupted = app.history.interrupted();
        if interrupted > 0 {
            ui.separator();
            if ui
                .link(
                    RichText::new("An operation was interrupted: review in History")
                        .color(ui.visuals().warn_fg_color),
                )
                .clicked()
            {
                app.set_view(View::History);
            }
        }
        if let Some(n) = app.notice.clone() {
            ui.separator();
            ui.label(RichText::new(n).color(Color32::from_rgb(80, 200, 120)));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            work_and_undo(app, ui);
            if let Some(e) = app.error.clone() {
                if ui.small_button("×").clicked() {
                    app.error = None;
                }
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
        });
    });
}

pub(crate) fn pick_folder() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .set_title("Add a folder to the library")
        .pick_folder()
}

pub fn library(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    // A scroll bar beside the rows, not over their counts and + buttons.
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| library_contents(app, ui));
}

fn library_contents(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let (open, add) = section(app, ui, "Library", Some(("Add a folder", true)));
    if add && let Some(p) = pick_folder() {
        app.scanning = true;
        app.command(LibraryCommand::AddRoot(p));
    }
    if open {
        let all_selected = app.folders.is_empty()
            && app.selected_event.is_none()
            && app.selected_collection.is_none();
        if row(
            ui,
            all_selected,
            "All photos",
            Some(app.catalog.stacks.len()),
        )
        .clicked()
        {
            app.show_everything();
        }
        let tree = std::mem::take(&mut app.tree);
        for (i, root) in tree.iter().enumerate() {
            let reference = app
                .settings
                .roots
                .iter()
                .any(|r| r.path == root.path && r.reference);
            folder_node(app, ui, root, Some(reference), i == 0);
        }
        app.tree = tree;
    }
    events_panel(app, ui);
    collections_panel(app, ui);
    smart_filters(app, ui);
    people_panel(app, ui);
    things_panel(app, ui);
    let (open, _) = section(app, ui, "Tasks", None);
    if !open {
        return;
    }
    let groups = app.dupes.review.as_ref().map(|_| app.dupes.visible().len());
    let label = match groups {
        Some(n) => format!("Duplicates  {n}"),
        None => "Duplicates".to_owned(),
    };
    if row(ui, app.view == View::Duplicates, label, None)
        .on_hover_text("Find and review duplicate files")
        .clicked()
    {
        app.set_view(View::Duplicates);
    }
    if row(ui, app.view == View::Import, "Import…", None)
        .on_hover_text("Copy photos from a card or folder into the library")
        .clicked()
    {
        app.set_view(View::Import);
    }
    let interrupted = app.history.interrupted();
    let label = if interrupted > 0 {
        RichText::new(format!("History  ⚠ {interrupted} interrupted"))
            .color(ui.visuals().warn_fg_color)
    } else {
        RichText::new("History")
    };
    if row(ui, app.view == View::History, label, None)
        .on_hover_text("Every file operation, with undo")
        .clicked()
    {
        app.command(LibraryCommand::LoadHistory);
        app.set_view(View::History);
    }
    if row(ui, app.settings_ui.open, "Settings…", None)
        .on_hover_text(format!(
            "{} ,",
            if cfg!(target_os = "macos") {
                "⌘"
            } else {
                "Ctrl"
            }
        ))
        .clicked()
    {
        app.open_settings();
    }
}

fn collections_panel(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let (open, add) = section(
        app,
        ui,
        "Collections",
        Some((
            "New collection from the selection",
            app.collection_edit.is_none(),
        )),
    );
    if add {
        app.collection_edit = Some((None, String::new()));
    }
    if !open {
        return;
    }
    if let Some((id, name)) = app.collection_edit.as_mut() {
        let r = ui.add(egui::TextEdit::singleline(name).hint_text("Name, then Enter"));
        r.request_focus();
        if r.lost_focus() {
            let (id, name) = (id.clone(), name.clone());
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                match id {
                    Some(id) => app.rename_collection(&id, &name),
                    None => app.new_collection(&name),
                }
            }
            app.collection_edit = None;
        }
    }
    let mut delete = None;
    for c in app.collections.clone() {
        let n = app.collection_members.get(&c.id).map_or(0, |m| m.len());
        let selected = app.selected_collection.as_deref() == Some(c.id.as_str());
        let r = row(ui, selected, c.name.as_str(), Some(n));
        if r.clicked() {
            app.select_collection(if selected { None } else { Some(c.id.clone()) });
        }
        // Drop photos on a collection to add them.
        if let Some(ids) = r.dnd_release_payload::<crate::views::DragPhotos>() {
            app.selection = ids.0.iter().copied().collect();
            app.collection_add_selection(&c.id, true);
        }
        r.context_menu(|ui| {
            if ui.button("Add selected photos").clicked() {
                app.collection_add_selection(&c.id, true);
                ui.close();
            }
            if ui.button("Remove selected photos").clicked() {
                app.collection_add_selection(&c.id, false);
                ui.close();
            }
            if ui.button("Rename…").clicked() {
                app.collection_edit = Some((Some(c.id.clone()), c.name.clone()));
                ui.close();
            }
            if ui.button("Delete collection (photos untouched)").clicked() {
                delete = Some(c.id.clone());
                ui.close();
            }
        });
    }
    if let Some(id) = delete {
        app.delete_collection(&id);
    }
}

fn events_panel(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let (open, add) = section(
        app,
        ui,
        "Events",
        Some((
            "New event from the selection (⌘E), or type a date range",
            app.event_edit.is_none(),
        )),
    );
    if add {
        app.new_event_from_selection(None);
    }
    if !open {
        return;
    }
    event_editor(app, ui);
    let events = app.events.clone();
    let ids: std::collections::HashSet<&str> = events.iter().map(|e| e.id.as_str()).collect();
    // Top-level events (no parent, or a parent that no longer exists), by year.
    let mut years: std::collections::BTreeMap<String, Vec<&shoebox_core::events::Event>> =
        Default::default();
    for e in events
        .iter()
        .filter(|e| e.parent.as_deref().is_none_or(|p| !ids.contains(p)))
    {
        years
            .entry(e.start.get(..4).unwrap_or("Undated").to_owned())
            .or_default()
            .push(e);
    }
    for (year, list) in years.iter().rev() {
        egui::CollapsingHeader::new(year)
            .id_salt(("event-year", year))
            .default_open(true)
            .show(ui, |ui| {
                for e in list {
                    event_node(app, ui, e, &events);
                }
            });
    }
}

fn event_editor(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let mut save = false;
    let mut cancel = false;
    let Some(ed) = app.event_edit.as_mut() else {
        return;
    };
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(if ed.id.is_some() {
            "Edit event"
        } else {
            "New event"
        });
        let name =
            ui.add(egui::TextEdit::singleline(&mut ed.name).hint_text("Name, e.g. Japan Trip"));
        if ed.name.is_empty() && !name.has_focus() {
            name.request_focus();
        }
        ui.add(egui::TextEdit::singleline(&mut ed.start).hint_text("Start YYYY-MM-DD [HH:MM]"));
        ui.add(egui::TextEdit::singleline(&mut ed.end).hint_text("End YYYY-MM-DD [HH:MM]"));
        if let Some(err) = &ed.error {
            ui.colored_label(ui.visuals().warn_fg_color, err);
        }
        ui.horizontal(|ui| {
            save = ui.button("Save").clicked()
                || (name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            cancel = ui.button("Cancel").clicked();
        });
    });
    if save {
        app.save_event_edit();
    }
    if cancel {
        app.event_edit = None;
    }
}

fn event_node(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    e: &shoebox_core::events::Event,
    all: &[shoebox_core::events::Event],
) {
    let members = app.event_members.get(&e.id).cloned().unwrap_or_default();
    let selected = app.selected_event.as_deref() == Some(e.id.as_str());
    let id = ui.make_persistent_id(("event", &e.id));
    let state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let header = state.show_header(ui, |ui| {
        let r = row(ui, selected, e.name.as_str(), Some(members.len())).on_hover_text(format!(
            "{} to {}\nDrop photos here to add them",
            e.start, e.end
        ));
        if r.clicked() {
            app.select_event(if selected { None } else { Some(e.id.clone()) });
        }
        // Drop photos on an event to add them.
        if r.dnd_hover_payload::<crate::views::DragPhotos>().is_some() {
            ui.painter().rect_stroke(
                r.rect.expand(1.0),
                3.0,
                egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Outside,
            );
        }
        if let Some(ids) = r.dnd_release_payload::<crate::views::DragPhotos>() {
            let mut stacks: Vec<usize> = ids
                .0
                .iter()
                .filter_map(|id| app.index(*id))
                .map(|i| app.catalog.stack_of[i])
                .collect();
            stacks.sort_unstable();
            stacks.dedup();
            app.event_include(&e.id, &stacks, true);
        }
        r.context_menu(|ui| {
            if ui.button("Edit…").clicked() {
                app.edit_event(&e.id);
                ui.close();
            }
            if ui.button("Add selected photos").clicked() {
                app.event_include_selection(&e.id, true);
                ui.close();
            }
            if ui.button("Exclude selected photos").clicked() {
                app.event_include_selection(&e.id, false);
                ui.close();
            }
            if ui.button("New sub-event from selection").clicked() {
                app.new_event_from_selection(Some(e.id.clone()));
                ui.close();
            }
            if ui.button("Delete event (photos untouched)").clicked() {
                app.delete_event(&e.id);
                ui.close();
            }
        });
    });
    header.body(|ui| {
        for child in all
            .iter()
            .filter(|c| c.parent.as_deref() == Some(e.id.as_str()))
        {
            event_node(app, ui, child, all);
        }
        // The day folders the event covers.
        let mut days: Vec<std::path::PathBuf> = members
            .iter()
            .filter_map(|&s| {
                app.catalog.photos[app.catalog.stacks[s].primary]
                    .path
                    .parent()
                    .map(std::path::Path::to_path_buf)
            })
            .collect();
        days.sort();
        days.dedup();
        for d in days {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let sel = app.folders == [d.as_path()];
            if ui.selectable_label(sel, format!("📁 {name}")).clicked() {
                app.set_folders(vec![d.clone()]);
            }
        }
    });
}

/// The search bar while a person's photos are shown: their name (or a field
/// to name them), the keyword button, and ✖.
fn person_bar(app: &mut ShoeboxApp, ui: &mut egui::Ui, g: &crate::people::Group) {
    use crate::people::GroupKey;
    let label = app.ppl.label(g);
    ui.label(format!("Person: {label}"));
    let hint = if g.name.is_some() {
        "Rename"
    } else {
        "Name this person"
    };
    let edit = ui.add(
        egui::TextEdit::singleline(&mut app.ppl.name_input)
            .hint_text(hint)
            .desired_width(150.0),
    );
    if edit.lost_focus()
        && ui.input(|i| i.key_pressed(egui::Key::Enter))
        && !app.ppl.name_input.trim().is_empty()
    {
        let name = std::mem::take(&mut app.ppl.name_input);
        app.name_person(g.key.clone(), name);
    }
    if let (Some(name), GroupKey::Person(id)) = (&g.name, &g.key) {
        let keyword = format!("People|{name}");
        if ui
            .button(format!("Add keyword to {} photos", g.photos.len()))
            .on_hover_text(format!(
                "Adds {} to these photos' sidecars (undoable)",
                keyword.replace('|', " › ")
            ))
            .clicked()
        {
            app.keyword_shown(&keyword);
        }
        if ui
            .small_button("Forget")
            .on_hover_text("Forget this name (the faces stay, unnamed)")
            .clicked()
        {
            app.forget_person(id.clone());
        }
    }
    if ui
        .small_button("✖")
        .on_hover_text("Show all photos")
        .clicked()
    {
        app.clear_person();
    }
}

/// People found in the library (named first), with photo counts.
fn people_panel(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if !app.settings.people {
        return;
    }
    let Some(view) = app.ppl.view.clone() else {
        return;
    };
    if view.groups.is_empty() {
        return;
    }
    let (open, _) = section(app, ui, "People", None);
    if !open {
        return;
    }
    let all = app.ppl.all;
    let mut chosen = None;
    for g in view.groups.iter().take(if all { usize::MAX } else { 8 }) {
        let sel = app.ppl.showing.as_ref() == Some(&g.key);
        let text = if g.name.is_some() {
            RichText::new(app.ppl.label(g))
        } else {
            RichText::new(app.ppl.label(g)).italics()
        };
        if row(ui, sel, text, Some(g.photos.len()))
            .on_hover_text("Click to see their photos and name them")
            .clicked()
        {
            chosen = Some(if sel { None } else { Some(g.key.clone()) });
        }
    }
    if view.groups.len() > 8
        && ui
            .link(if all {
                "Show fewer".to_owned()
            } else {
                format!("Show all {}", view.groups.len())
            })
            .clicked()
    {
        app.ppl.all = !all;
    }
    match chosen {
        Some(Some(k)) => app.show_person(k),
        Some(None) => app.clear_person(),
        None => {}
    }
}

/// Suggested tags across the library (semantic search), with counts.
fn things_panel(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if !app.settings.semantic_search {
        return;
    }
    let Some(tags) = app.sem.tags.clone() else {
        return;
    };
    if tags.counts.is_empty() {
        return;
    }
    let mut chosen = None;
    let (open, _) = section(app, ui, "Things", None);
    if !open {
        return;
    }
    let all = app.sem.things_all;
    for &(l, n) in tags.counts.iter().take(if all { usize::MAX } else { 8 }) {
        let name = tags.names[l as usize].as_str();
        let sel = app.sem.tag == Some(l);
        if row(ui, sel, name, Some(n))
            .on_hover_text("Photos Shoebox suggests this tag for")
            .clicked()
        {
            chosen = Some(if sel { None } else { Some(l) });
        }
    }
    if tags.counts.len() > 8
        && ui
            .link(if all {
                "Show fewer".to_owned()
            } else {
                format!("Show all {}", tags.counts.len())
            })
            .clicked()
    {
        app.sem.things_all = !all;
    }
    match chosen {
        Some(Some(l)) => app.show_tag(l),
        Some(None) => app.clear_tag(),
        None => {}
    }
}

fn smart_filters(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let can_save = !app.filter.is_empty() && app.new_smart_filter.is_none();
    let (open, add) = section(
        app,
        ui,
        "Smart filters",
        Some(("Save the current filter", can_save)),
    );
    if add {
        app.new_smart_filter = Some(String::new());
    }
    if !open {
        return;
    }
    if let Some(name) = app.new_smart_filter.as_mut() {
        let r = ui.add(egui::TextEdit::singleline(name).hint_text("Name, then Enter"));
        r.request_focus();
        let done = r.lost_focus();
        if done && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty() {
            let mut settings = app.settings.clone();
            settings
                .smart_filters
                .push(shoebox_core::query::SmartFilter {
                    name: name.trim().to_owned(),
                    filter: app.filter.clone(),
                });
            app.settings = settings.clone();
            app.command(LibraryCommand::SaveSettings(Box::new(settings)));
        }
        if done {
            app.new_smart_filter = None;
        }
    }
    // Built in: everything changed in the last week, newest first.
    let recent = shoebox_core::query::recently_changed();
    let on = app.filter == recent;
    if row(ui, on, "Recently changed", None)
        .on_hover_text("Image or sidecar modified in the last 7 days")
        .clicked()
    {
        if on {
            app.filter = shoebox_core::query::Filter::default();
            app.sort = shoebox_core::query::Sort::CaptureTime;
        } else {
            app.filter = recent;
            app.sort = shoebox_core::query::Sort::Modified;
        }
        app.sort_desc = false;
        app.refilter();
        app.scroll_to_current = true;
    }
    let mut delete = None;
    for (i, sf) in app.settings.smart_filters.clone().iter().enumerate() {
        let on = app.filter == sf.filter;
        let r = row(ui, on, sf.name.as_str(), None);
        if r.clicked() {
            app.filter = if on {
                shoebox_core::query::Filter::default()
            } else {
                sf.filter.clone()
            };
            app.refilter();
            app.scroll_to_current = true;
        }
        r.context_menu(|ui| {
            if ui.button("Delete smart filter").clicked() {
                delete = Some(i);
                ui.close();
            }
        });
    }
    if let Some(i) = delete {
        let mut settings = app.settings.clone();
        settings.smart_filters.remove(i);
        app.settings = settings.clone();
        app.command(LibraryCommand::SaveSettings(Box::new(settings)));
    }
}

/// One folder row; `root` is Some(is_reference) for library roots.
fn folder_node(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    node: &FolderNode,
    root: Option<bool>,
    open: bool,
) {
    let selected = app.folders.contains(&node.path);
    let name = if root == Some(true) {
        format!("{}  🔒", node.name)
    } else {
        node.name.clone()
    };
    let id = ui.make_persistent_id(("folder", &node.path));
    let state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
    let header = state.show_header(ui, |ui| {
        let r = row(ui, selected, name, Some(node.count))
            .on_hover_text("⌘-click to show several folders");
        if r.clicked() {
            let add = ui.input(|i| i.modifiers.command || i.modifiers.shift);
            app.click_folder(&node.path, add);
        }
        // Drop photos on a folder to move them there.
        if r.dnd_hover_payload::<crate::views::DragPhotos>().is_some() {
            ui.painter().rect_stroke(
                r.rect.expand(1.0),
                3.0,
                egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Outside,
            );
        }
        if let Some(ids) = r.dnd_release_payload::<crate::views::DragPhotos>() {
            let photos = app.photos_for(&ids.0);
            app.request_org(crate::jobs::OrgRequest::MoveTo {
                photos,
                folder: node.path.clone(),
            });
        }
        r.context_menu(|ui| {
            if ui.button("Reveal in Finder").clicked() {
                reveal(&node.path);
                ui.close();
            }
            if ui.button("New Folder…").clicked() {
                app.org.new_folder = Some((node.path.clone(), String::new()));
                ui.close();
            }
            ui.separator();
            // The folders shown (this one, if it isn't among them).
            let scope = |app: &mut ShoeboxApp| {
                if !app.folders.contains(&node.path) {
                    app.set_folders(vec![node.path.clone()]);
                }
                app.shown_stacks()
            };
            let what = if selected && app.folders.len() > 1 {
                format!("Photos in these {} folders", app.folders.len())
            } else {
                "Photos in this folder".to_owned()
            };
            ui.weak(what);
            if ui.button("New Event from These Photos…").clicked() {
                let stacks = scope(app);
                app.new_event_from(&stacks, None);
                ui.close();
            }
            add_to_event_menu(app, ui, |app| scope(app));
            ui.separator();
            if !app.selection.is_empty()
                && ui
                    .button(format!(
                        "Move {} here…",
                        crate::views::duplicates::plural(app.selection.len(), "selected photo")
                    ))
                    .clicked()
            {
                app.move_selection_to(node.path.clone());
                ui.close();
            }
            if ui.button("Rescan").clicked() {
                app.scanning = true;
                app.command(LibraryCommand::Rescan);
                ui.close();
            }
            if let Some(reference) = root {
                let text = if reference {
                    "Unmark as Reference"
                } else {
                    "Mark as Reference"
                };
                if ui.button(text).clicked() {
                    app.scanning = true;
                    app.command(LibraryCommand::SetReference(node.path.clone(), !reference));
                    ui.close();
                }
                if ui.button("Remove from library (files untouched)").clicked() {
                    app.remove_root(node.path.clone());
                    ui.close();
                }
            }
        });
    });
    header.body(|ui| {
        for child in node.children.values() {
            folder_node(app, ui, child, None, false);
        }
    });
}

/// The right panel: histogram, the photo's details, and organising, each a
/// folding section.
pub fn info(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let Some(id) = app.current else {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.weak("No photo selected");
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Click a photo to see its details")
                            .small()
                            .weak(),
                    );
                });
                return;
            };
            let Some(idx) = app.index(id) else { return };
            app.want(idx, crate::jobs::Size::Small, crate::jobs::Priority::Near);
            let (open, _) = section(app, ui, "Histogram", None);
            if open && let Some(h) = app.histograms.get(&id) {
                histogram(ui, h);
            }
            let (open, _) = section(app, ui, "Info", None);
            if open {
                details(app, ui, id, idx);
            }
            let (open, _) = section(app, ui, "Organise", None);
            if open {
                organise(app, ui);
            }
        });
}

fn details(app: &mut ShoeboxApp, ui: &mut egui::Ui, id: crate::jobs::PhotoId, idx: usize) {
    let photo = app.catalog.photos[idx].clone();
    let x = &photo.meta.exif;
    // Rows with nothing to say are left out.
    let opt = |v: Option<String>| v.unwrap_or_default();
    let rows: Vec<(&str, String)> = vec![
        (
            "Captured",
            opt(x.capture_time.as_ref().map(|t| {
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
            })),
        ),
        (
            "Camera",
            opt(match (&x.make, &x.model) {
                (Some(make), Some(model))
                    if model.starts_with(make.split(' ').next().unwrap_or("")) =>
                {
                    Some(model.clone())
                }
                (Some(make), Some(model)) => Some(format!("{make} {model}")),
                (a, b) => a.clone().or_else(|| b.clone()),
            }),
        ),
        ("Lens", opt(x.lens.clone())),
        ("Shutter", opt(x.exposure.clone())),
        ("Aperture", opt(x.fnumber.map(|f| format!("f/{f:.1}")))),
        ("ISO", opt(x.iso.map(|i| i.to_string()))),
        ("Focal length", opt(x.focal.map(|f| format!("{f:.0} mm")))),
        (
            "Dimensions",
            opt(photo
                .meta
                .width
                .zip(photo.meta.height)
                .map(|(w, h)| format!("{w} × {h}"))),
        ),
        ("File size", human_size(photo.size)),
        ("Image modified", crate::app::local_time_ns(photo.mtime_ns)),
        (
            "Sidecar modified",
            opt(photo.sidecar_mtime_ns().map(crate::app::local_time_ns)),
        ),
        (
            "Shutter count",
            // A TIFF export has no MakerNote; its NEF or JPG does.
            opt(x
                .shutter_count
                .or_else(|| {
                    app.stack_of(idx)
                        .all()
                        .find_map(|i| app.catalog.photos[i].meta.exif.shutter_count)
                })
                .map(group_thousands)),
        ),
    ];
    egui::Grid::new("info-grid")
        .num_columns(2)
        .spacing(vec2(10.0, 4.0))
        .show(ui, |ui| {
            for (k, v) in rows.into_iter().filter(|(_, v)| !v.is_empty()) {
                ui.label(RichText::new(k).weak());
                ui.label(v);
                ui.end_row();
            }
        });
    if let Some(ch) = &photo.change {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot.center(), 4.0, crate::views::change_colour(ch));
            ui.add(egui::Label::new(crate::views::describe_change(ch)).wrap());
        });
        ui.horizontal(|ui| {
            if ui.small_button("Mark as Seen").clicked() {
                app.mark_selection_seen();
            }
            if let shoebox_core::changes::By::Shoebox { batch, .. } = ch.by
                && ui.small_button("Show in History").clicked()
            {
                app.show_batch(batch);
            }
        });
    }
    if let Some((lat, lon)) = photo.location() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Location").weak());
            ui.label(format!(
                "{:.5}° {}, {:.5}° {}",
                lat.abs(),
                if lat < 0.0 { "S" } else { "N" },
                lon.abs(),
                if lon < 0.0 { "W" } else { "E" }
            ));
            if ui.small_button("Show on Map").clicked() {
                crate::views::map::show_on_map(app, id);
            }
        });
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Path").weak());
        if ui
            .small_button(crate::app::REVEAL_LABEL)
            .on_hover_text("Show the file in its folder")
            .clicked()
        {
            reveal(&photo.path);
        }
    });
    ui.add(egui::Label::new(short_path(&photo.path)).wrap())
        .on_hover_text(photo.path.display().to_string());

    let siblings = match &app.siblings_of {
        Some((sid, s)) if *sid == id => s.clone(),
        _ => {
            let s = app.catalog.siblings(idx);
            app.siblings_of = Some((id, s.clone()));
            s
        }
    };
    if siblings.len() > 1 {
        ui.add_space(6.0);
        ui.label(RichText::new("Stack members").weak());
        for j in siblings {
            let p = &app.catalog.photos[j];
            let name = p
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.label(if j == idx {
                RichText::new(name).strong()
            } else {
                RichText::new(name)
            });
        }
    }
}

fn organise(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let Some(m) = app.current_marks() else { return };
    let n = app.selection.len().max(1);
    if n > 1 {
        ui.weak(format!("Changes apply to {n} selected photos"));
    }
    ui.checkbox(&mut app.auto_advance, "Auto-advance")
        .on_hover_text("Move to the next photo after marking (or hold Caps Lock)");
    ui.horizontal(|ui| {
        ui.label(RichText::new("Rating").weak());
        for r in 1..=5u8 {
            let star = if m.rating >= r { "★" } else { "☆" };
            let b = ui.add(egui::Button::new(RichText::new(star).size(16.0)).frame(false));
            if b.on_hover_text(format!("{r} ({r})")).clicked() {
                app.set_rating(if m.rating == r { 0 } else { r });
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Flag").weak());
        if ui
            .selectable_label(m.pick == Some(Pick::Pick), "✔ Pick")
            .on_hover_text("P")
            .clicked()
        {
            app.set_pick(if m.pick == Some(Pick::Pick) {
                None
            } else {
                Some(Pick::Pick)
            });
        }
        if ui
            .selectable_label(m.pick == Some(Pick::Reject), "✖ Reject")
            .on_hover_text("X")
            .clicked()
        {
            app.set_pick(if m.pick == Some(Pick::Reject) {
                None
            } else {
                Some(Pick::Reject)
            });
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Label").weak());
        for (l, key) in [
            (Label::Red, "6"),
            (Label::Yellow, "7"),
            (Label::Green, "8"),
            (Label::Blue, "9"),
            (Label::Purple, ""),
        ] {
            let (r, painter) = ui.allocate_painter(vec2(18.0, 18.0), egui::Sense::click());
            let c = crate::views::label_colour(l);
            painter.rect_filled(r.rect.shrink(2.0), 3.0, c);
            if m.label == Some(l) {
                painter.rect_stroke(
                    r.rect,
                    3.0,
                    egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
                    egui::StrokeKind::Inside,
                );
            }
            let hint = if key.is_empty() {
                l.name().to_owned()
            } else {
                format!("{} ({key})", l.name())
            };
            if r.on_hover_text(hint).clicked() {
                app.toggle_label(l);
            }
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new("Keywords").weak());
    ui.horizontal_wrapped(|ui| {
        for k in m.keywords.clone() {
            let shown = k.replace('|', " › ");
            if ui
                .small_button(format!("{shown}  ×"))
                .on_hover_text("Remove")
                .clicked()
            {
                app.remove_keyword(&k);
            }
        }
    });
    if app.settings.people
        && let Some(cur) = app.current
    {
        let people = app.people_in(cur);
        if !people.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("People").weak());
                for (key, label) in people {
                    if ui
                        .small_button(label)
                        .on_hover_text("Show their photos")
                        .clicked()
                    {
                        app.show_person(key);
                    }
                }
            });
        }
    }
    if app.settings.semantic_search
        && let Some(cur) = app.current
    {
        let suggested = app.suggested_tags(cur);
        if !suggested.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Suggested").weak())
                    .on_hover_text("Tags from semantic search; click to add as a keyword");
                for t in suggested {
                    if ui.small_button(format!("+ {t}")).clicked() {
                        app.add_keyword(&t);
                    }
                }
            });
        }
    }
    let edit = ui.add(
        egui::TextEdit::singleline(&mut app.keyword_input)
            .hint_text("Add keyword (⌘K), e.g. Places > Japan > Kyoto")
            .desired_width(f32::INFINITY),
    );
    if app.focus_keywords {
        edit.request_focus();
        app.focus_keywords = false;
    }
    let typed = app.keyword_input.trim().to_lowercase();
    let suggestions: Vec<String> = if typed.is_empty() {
        Vec::new()
    } else {
        app.all_keywords
            .iter()
            .filter(|(k, _)| k.to_lowercase().contains(&typed) && !m.keywords.contains(k))
            .take(6)
            .map(|(k, _)| k.clone())
            .collect()
    };
    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let k = std::mem::take(&mut app.keyword_input);
        app.add_keyword(&k);
        app.focus_keywords = true; // keep typing
    }
    for sug in suggestions {
        if ui.small_button(sug.replace('|', " › ")).clicked() {
            app.add_keyword(&sug);
            app.keyword_input.clear();
        }
    }
}

fn human_size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / f64::from(1u32 << 30)),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / f64::from(1u32 << 20)),
        b => format!("{:.0} KB", b as f64 / 1024.0),
    }
}

fn histogram(ui: &mut egui::Ui, h: &[[u32; 256]; 4]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 70.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    // Ignore the extreme bins when scaling, so clipping doesn't flatten the rest.
    let max = h[..3]
        .iter()
        .flat_map(|c| c[1..255].iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let colours = [
        Color32::from_rgba_unmultiplied(230, 60, 60, 110),
        Color32::from_rgba_unmultiplied(60, 200, 80, 110),
        Color32::from_rgba_unmultiplied(70, 110, 240, 110),
    ];
    for (c, colour) in colours.into_iter().enumerate() {
        let pts: Vec<egui::Pos2> = std::iter::once(pos2(rect.left(), rect.bottom()))
            .chain((0..256).map(|i| {
                let x = rect.left() + rect.width() * i as f32 / 255.0;
                let y = rect.bottom() - rect.height() * (h[c][i] as f32 / max).min(1.0);
                pos2(x, y)
            }))
            .chain(std::iter::once(pos2(rect.right(), rect.bottom())))
            .collect();
        for w in pts.windows(2) {
            let quad = vec![
                w[0],
                w[1],
                pos2(w[1].x, rect.bottom()),
                pos2(w[0].x, rect.bottom()),
            ];
            painter.add(egui::Shape::convex_polygon(
                quad,
                colour,
                egui::Stroke::NONE,
            ));
        }
    }
}

pub fn empty_library(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.heading("No library folders yet");
        ui.add_space(8.0);
        ui.label("Add a folder of photos. Shoebox only reads it; your files stay where they are.");
        ui.add_space(12.0);
        if ui.button("Add folder…").clicked()
            && let Some(p) = pick_folder()
        {
            app.scanning = true;
            app.command(LibraryCommand::AddRoot(p));
        }
    });
}

/// What semantic search is doing in the background, for the toolbar.
fn semantic_task(s: &crate::semantic::Status) -> Option<String> {
    use crate::semantic::ModelState as M;
    match &s.model {
        M::Downloading { done, total } => Some(format!(
            "Downloading search model {}%",
            done * 100 / (*total).max(1)
        )),
        M::Verifying | M::Converting => Some("Preparing search model".to_owned()),
        M::Ready if s.indexing() => Some(format!(
            "Indexing for search {}/{}",
            s.indexed + s.skipped,
            s.total
        )),
        _ => None,
    }
}

/// What People is doing in the background, for the toolbar.
fn people_task(s: &crate::semantic::Status) -> Option<String> {
    use crate::semantic::ModelState as M;
    match &s.model {
        M::Downloading { done, total } => Some(format!(
            "Downloading face models {}%",
            done * 100 / (*total).max(1)
        )),
        M::Verifying | M::Converting => Some("Preparing face models".to_owned()),
        M::Ready if s.indexing() => Some(format!(
            "Finding faces {}/{}",
            s.indexed + s.skipped,
            s.total
        )),
        _ => None,
    }
}

/// A list row in the left panel: the name, and a count in muted text at the
/// right. Its accessible label is "name  count".
pub(crate) fn row(
    ui: &mut egui::Ui,
    selected: bool,
    name: impl Into<RichText>,
    count: Option<usize>,
) -> egui::Response {
    let name: RichText = name.into();
    let label = match count {
        Some(n) => format!("{}  {n}", name.text()),
        None => name.text().to_owned(),
    };
    let height = ui.spacing().interact_size.y;
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&resp, selected);
        if selected || resp.hovered() || resp.has_focus() {
            ui.painter().rect_filled(rect, 4.0, visuals.weak_bg_fill);
        }
        let pad = 6.0;
        // A selected row shows how to undo it: hover reveals ✖ for the count.
        let count = if selected && resp.hovered() {
            None
        } else {
            count
        };
        if selected && resp.hovered() {
            let g = ui.painter().layout_no_wrap(
                "✖".to_owned(),
                egui::FontId::proportional(11.0),
                visuals.text_color(),
            );
            let pos = pos2(
                rect.right() - pad - g.size().x,
                rect.center().y - g.size().y / 2.0,
            );
            ui.painter().galley(pos, g, visuals.text_color());
        }
        let count_w = count.map_or(0.0, |n| {
            let g = ui.painter().layout_no_wrap(
                n.to_string(),
                egui::FontId::proportional(12.0),
                ui.visuals().weak_text_color(),
            );
            let w = g.size().x;
            let pos = pos2(rect.right() - pad - w, rect.center().y - g.size().y / 2.0);
            ui.painter().galley(pos, g, ui.visuals().weak_text_color());
            w + 8.0
        });
        let galley = egui::WidgetText::from(name).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            (rect.width() - 2.0 * pad - count_w).max(10.0),
            egui::TextStyle::Body,
        );
        let pos = pos2(rect.left() + pad, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, visuals.text_color());
    }
    resp
}

/// A section heading in the left panel, folding its contents when clicked,
/// with an optional "+" at the right. Returns (open, "+" clicked).
fn section(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    title: &'static str,
    add: Option<(&str, bool)>,
) -> (bool, bool) {
    let open = !app.collapsed.contains(title);
    let mut added = false;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let arrow = if open { "⏷" } else { "⏵" };
        let text = RichText::new(format!("{arrow} {}", title.to_uppercase()))
            .size(11.0)
            .strong()
            .color(ui.visuals().weak_text_color());
        if ui
            .add(egui::Label::new(text).sense(egui::Sense::click()))
            .on_hover_text(if open { "Hide" } else { "Show" })
            .clicked()
        {
            if open {
                app.collapsed.insert(title);
            } else {
                app.collapsed.remove(title);
            }
        }
        if let Some((hint, enabled)) = add {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                added = ui
                    .add_enabled(enabled, egui::Button::new("+").small().frame(false))
                    .on_hover_text(hint)
                    .clicked();
            });
        }
    });
    (open, added)
}

/// What's narrowing the grid (a folder, event, collection or smart filter),
/// each as a chip with a ✖ that clears it.
fn scope_chips(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let mut chips: Vec<(String, u8)> = Vec::new();
    let name = |f: &std::path::Path| {
        f.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match app.folders.as_slice() {
        [] => {}
        [one] => chips.push((format!("📁 {}", name(one)), 0)),
        many => chips.push((format!("📁 {} folders", many.len()), 0)),
    }
    if let Some(e) = app
        .selected_event
        .as_ref()
        .and_then(|id| app.events.iter().find(|e| &e.id == id))
    {
        chips.push((format!("Event: {}", e.name), 1));
    }
    if let Some(c) = app
        .selected_collection
        .as_ref()
        .and_then(|id| app.collections.iter().find(|c| &c.id == id))
    {
        chips.push((format!("Collection: {}", c.name), 2));
    }
    let smart = if app.filter == shoebox_core::query::recently_changed() {
        Some("Recently changed".to_owned())
    } else {
        app.settings
            .smart_filters
            .iter()
            .find(|sf| !app.filter.is_empty() && sf.filter == app.filter)
            .map(|sf| sf.name.clone())
    };
    if let Some(name) = smart {
        chips.push((name, 3));
    }
    let scoped = !chips.is_empty();
    for (text, kind) in chips {
        let r = ui
            .add(egui::Button::new(format!("{text}  ✖")).selected(true))
            .on_hover_text("Clear");
        if r.clicked() {
            match kind {
                0 => app.set_folders(Vec::new()),
                1 => app.select_event(None),
                2 => app.select_collection(None),
                _ => {
                    app.filter = shoebox_core::query::Filter::default();
                    app.sort = shoebox_core::query::Sort::CaptureTime;
                    app.sort_desc = false;
                }
            }
        }
    }
    // How many photos the chosen folders, event or filter hold.
    if scoped {
        let n = app.shown.len();
        ui.label(RichText::new(format!("{n} photo{}", if n == 1 { "" } else { "s" })).weak());
    }
}

/// Grid: day headings on or off. Masonry: columns or justified rows.
fn layout_toggle(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let mut s = app.settings.clone();
    match app.view {
        View::Grid => {
            ui.checkbox(&mut s.day_headers, "Group by day")
                .on_hover_text("A heading for each day (when sorted by capture time)");
        }
        View::Masonry => {
            ui.checkbox(&mut s.justified_rows, "Justified rows")
                .on_hover_text("Rows of equal height in date order, instead of columns");
        }
        _ => {}
    }
    if s != app.settings {
        app.save_settings(s);
        app.refilter();
    }
}

/// 12345 → "12,345".
fn group_thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A path with the home folder as `~` (the full path goes in a tooltip).
pub(crate) fn short_path(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A button that folds a side panel away or back (Tab does both).
fn panel_toggle(ui: &mut egui::Ui, open: &mut bool, left: bool) {
    let (glyph, side) = match (left, *open) {
        (true, true) => ("◀", "Hide the library panel"),
        (true, false) => ("▶", "Show the library panel"),
        (false, true) => ("▶", "Hide the info panel"),
        (false, false) => ("◀", "Show the info panel"),
    };
    let r = ui
        .add(egui::Button::new(RichText::new(glyph).size(10.0)).frame(false))
        .on_hover_text(format!("{side} (Tab: both)"));
    if r.clicked() {
        *open = !*open;
        // Keep Tab for the panels, not for moving focus from this button.
        r.surrender_focus();
    }
}

/// "Add to Event" with every event (sub-events indented); `stacks` gives
/// the photos when one is chosen.
pub(crate) fn add_to_event_menu(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    stacks: impl FnOnce(&mut ShoeboxApp) -> Vec<usize>,
) {
    let events = app.events.clone();
    let mut chosen = None;
    ui.add_enabled_ui(!events.is_empty(), |ui| {
        ui.menu_button("Add to Event", |ui| {
            fn level(
                e: &shoebox_core::events::Event,
                all: &[shoebox_core::events::Event],
            ) -> usize {
                let mut n = 0;
                let mut p = e.parent.as_deref();
                while let Some(id) = p.filter(|_| n < 8) {
                    n += 1;
                    p = all
                        .iter()
                        .find(|x| x.id == id)
                        .and_then(|x| x.parent.as_deref());
                }
                n
            }
            // Newest first, each followed by its sub-events.
            let mut order: Vec<&shoebox_core::events::Event> = Vec::new();
            fn walk<'a>(
                parent: Option<&str>,
                all: &'a [shoebox_core::events::Event],
                out: &mut Vec<&'a shoebox_core::events::Event>,
            ) {
                let ids: std::collections::HashSet<&str> =
                    all.iter().map(|e| e.id.as_str()).collect();
                let mut kids: Vec<&shoebox_core::events::Event> = all
                    .iter()
                    .filter(|e| match parent {
                        Some(p) => e.parent.as_deref() == Some(p),
                        None => e.parent.as_deref().is_none_or(|p| !ids.contains(p)),
                    })
                    .collect();
                kids.sort_by(|a, b| b.start.cmp(&a.start));
                for k in kids {
                    out.push(k);
                    if out.len() < all.len() {
                        walk(Some(&k.id), all, out);
                    }
                }
            }
            walk(None, &events, &mut order);
            for e in order {
                let indent = "    ".repeat(level(e, &events));
                if ui.button(format!("{indent}{}", e.name)).clicked() {
                    chosen = Some(e.id.clone());
                    ui.close();
                }
            }
        })
    })
    .response
    .on_disabled_hover_text("No events yet");
    if let Some(id) = chosen {
        let stacks = stacks(app);
        app.event_include(&id, &stacks, true);
    }
}
