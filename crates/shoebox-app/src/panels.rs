//! Toolbar, status bar, library tree and info panel.

use eframe::egui::{self, Color32, RichText, pos2, vec2};

use shoebox_core::marks::{Label, Pick};

use crate::app::{FolderNode, ShoeboxApp, View, reveal};
use crate::jobs::LibraryCommand;

pub fn toolbar(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
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
        ui.separator();
        ui.label("Size");
        ui.add(egui::Slider::new(&mut app.thumb_size, 96.0..=512.0).show_value(false));
        ui.separator();
        ui.toggle_value(&mut app.auto_advance, "Auto-advance")
            .on_hover_text("Move to the next photo after marking (or hold Caps Lock)");
        if ui
            .add_enabled(app.can_undo, egui::Button::new("Undo"))
            .on_hover_text("Undo (⌘Z)")
            .clicked()
        {
            app.command(LibraryCommand::Undo);
        }
        if ui
            .add_enabled(app.can_redo, egui::Button::new("Redo"))
            .on_hover_text("Redo (Shift+⌘Z)")
            .clicked()
        {
            app.command(LibraryCommand::Redo);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let busy = app.loader.busy();
            if let Some((done, total)) = app.scan_progress {
                ui.add(egui::Spinner::new());
                ui.label(format!("Reading {done}/{total}"));
            } else if app.scanning {
                ui.add(egui::Spinner::new());
                ui.label("Scanning…");
            } else if busy > 0 {
                ui.add(egui::Spinner::new());
                ui.label(format!("{busy} thumbnails"));
            }
        });
    });
    filter_bar(app, ui);
}

fn filter_bar(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let before = (app.filter.clone(), app.sort, app.sort_desc);
    ui.horizontal_wrapped(|ui| {
        let search = ui.add(
            egui::TextEdit::singleline(&mut app.filter.search)
                .hint_text("Search names and keywords (/)")
                .desired_width(200.0),
        );
        if app.focus_search {
            search.request_focus();
            app.focus_search = false;
        }
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
        for t in ["NEF", "JPG", "TIF", "PNG", "HEIC"] {
            let mut on = app.filter.types.iter().any(|x| x == t);
            if ui.toggle_value(&mut on, t).changed() {
                if on {
                    app.filter.types.push(t.to_owned());
                } else {
                    app.filter.types.retain(|x| x != t);
                }
            }
        }
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
        ui.toggle_value(&mut app.filter.nef_missing, "NEF missing");
        ui.toggle_value(&mut app.filter.undated, "Undated");
        let changed_name = |d: Option<u32>| match d {
            None => "Any time".to_owned(),
            Some(1) => "Changed today".to_owned(),
            Some(d) => format!("Changed in {d} days"),
        };
        egui::ComboBox::from_id_salt("f-changed")
            .selected_text(changed_name(app.filter.changed_within_days))
            .show_ui(ui, |ui| {
                for d in [None, Some(1), Some(7), Some(30)] {
                    ui.selectable_value(&mut app.filter.changed_within_days, d, changed_name(d));
                }
            });
        ui.toggle_value(&mut app.filter.unseen_changes, "Unseen changes")
            .on_hover_text("Changed since you last viewed them");
        ui.separator();
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
        if ui
            .small_button(if app.sort_desc { "Desc" } else { "Asc" })
            .on_hover_text("Reverse order")
            .clicked()
        {
            app.sort_desc = !app.sort_desc;
        }
        if !app.filter.is_empty() && ui.small_button("Clear").clicked() {
            app.filter = shoebox_core::query::Filter::default();
        }
    });
    if before != (app.filter.clone(), app.sort, app.sort_desc) {
        app.refilter();
        app.scroll_to_current = true;
    }
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
            ui.label(RichText::new(p.path.display().to_string()).weak());
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
        if let Some(e) = app.error.clone() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").clicked() {
                    app.error = None;
                }
                ui.colored_label(ui.visuals().warn_fg_color, e);
            });
        }
    });
}

fn pick_folder() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .set_title("Add a folder to the library")
        .pick_folder()
}

pub fn library(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading("Library");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("+").on_hover_text("Add a folder").clicked()
                && let Some(p) = pick_folder()
            {
                app.scanning = true;
                app.command(LibraryCommand::AddRoot(p));
            }
        });
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .max_height(ui.available_height() * 0.6)
        .show(ui, |ui| {
            let all_selected = app.folder.is_none();
            if ui
                .selectable_label(
                    all_selected,
                    format!("All photos  {}", app.catalog.photos.len()),
                )
                .clicked()
            {
                app.set_folder(None);
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
        });
    ui.separator();
    events_panel(app, ui);
    collections_panel(app, ui);
    smart_filters(app, ui);
    ui.separator();
    ui.label(RichText::new("Tasks").strong());
    let groups = app.dupes.review.as_ref().map(|_| app.dupes.visible().len());
    let label = match groups {
        Some(n) => format!("Duplicates  {n}"),
        None => "Duplicates".to_owned(),
    };
    if ui
        .selectable_label(app.view == View::Duplicates, label)
        .on_hover_text("Find and review duplicate files")
        .clicked()
    {
        app.set_view(View::Duplicates);
    }
    if ui
        .selectable_label(app.view == View::Import, "Import…")
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
    if ui
        .selectable_label(app.view == View::History, label)
        .on_hover_text("Every file operation, with undo")
        .clicked()
    {
        app.command(LibraryCommand::LoadHistory);
        app.set_view(View::History);
    }
}

fn collections_panel(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Collections").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    app.collection_edit.is_none(),
                    egui::Button::new("+").small(),
                )
                .on_hover_text("New collection from the selection")
                .clicked()
            {
                app.collection_edit = Some((None, String::new()));
            }
        });
    });
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
        let r = ui.selectable_label(selected, format!("{}  {n}", c.name));
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
    ui.horizontal(|ui| {
        ui.label(RichText::new("Events").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(app.event_edit.is_none(), egui::Button::new("+").small())
                .on_hover_text("New event from the selection (⌘E), or type a date range")
                .clicked()
            {
                app.new_event_from_selection(None);
            }
        });
    });
    event_editor(app, ui);
    if app.selected_event.is_some() && ui.small_button("Show all photos").clicked() {
        app.select_event(None);
    }
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
        let r = ui
            .selectable_label(selected, format!("{}  {}", e.name, members.len()))
            .on_hover_text(format!("{} to {}", e.start, e.end));
        if r.clicked() {
            app.select_event(if selected { None } else { Some(e.id.clone()) });
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
            let sel = app.folder.as_deref() == Some(d.as_path());
            if ui.selectable_label(sel, format!("📁 {name}")).clicked() {
                app.selected_event = None;
                app.set_folder(Some(d.clone()));
            }
        }
    });
}

fn smart_filters(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Smart filters").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_save = !app.filter.is_empty() && app.new_smart_filter.is_none();
            if ui
                .add_enabled(can_save, egui::Button::new("+").small())
                .on_hover_text("Save the current filter")
                .clicked()
            {
                app.new_smart_filter = Some(String::new());
            }
        });
    });
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
    if ui
        .selectable_label(app.filter == recent, "Recently changed")
        .on_hover_text("Image or sidecar modified in the last 7 days")
        .clicked()
    {
        app.filter = recent;
        app.sort = shoebox_core::query::Sort::Modified;
        app.sort_desc = false;
        app.refilter();
        app.scroll_to_current = true;
    }
    let mut delete = None;
    for (i, sf) in app.settings.smart_filters.clone().iter().enumerate() {
        let r = ui.selectable_label(app.filter == sf.filter, &sf.name);
        if r.clicked() {
            app.filter = sf.filter.clone();
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
    let selected = app.folder.as_deref() == Some(node.path.as_path());
    let mut label = format!("{}  {}", node.name, node.count);
    if root == Some(true) {
        label.push_str("  🔒");
    }
    let id = ui.make_persistent_id(("folder", &node.path));
    let state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
    let header = state.show_header(ui, |ui| {
        let r = ui.selectable_label(selected, label);
        if r.clicked() {
            app.set_folder(Some(node.path.clone()));
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
                    app.scanning = true;
                    if app
                        .folder
                        .as_ref()
                        .is_some_and(|f| f.starts_with(&node.path))
                    {
                        app.set_folder(None);
                    }
                    app.command(LibraryCommand::RemoveRoot(node.path.clone()));
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

pub fn info(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.heading("Info");
    ui.separator();
    let Some(id) = app.current else {
        ui.weak("No photo selected");
        return;
    };
    let Some(idx) = app.index(id) else { return };
    app.want(idx, crate::jobs::Size::Small, crate::jobs::Priority::Near);
    if let Some(h) = app.histograms.get(&id) {
        histogram(ui, h);
    }
    let photo = app.catalog.photos[idx].clone();
    let x = &photo.meta.exif;
    let opt = |v: Option<String>| v.unwrap_or_else(|| "—".into());
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
        ("Shutter count", "—".into()),
    ];
    egui::Grid::new("info-grid")
        .num_columns(2)
        .spacing(vec2(10.0, 4.0))
        .show(ui, |ui| {
            for (k, v) in rows {
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
    ui.add_space(6.0);
    ui.label(RichText::new("Path").weak());
    ui.add(egui::Label::new(photo.path.display().to_string()).wrap());

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

    ui.add_space(10.0);
    organise(app, ui);
}

fn organise(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.heading("Organise");
    ui.separator();
    let Some(m) = app.current_marks() else { return };
    let n = app.selection.len().max(1);
    if n > 1 {
        ui.weak(format!("Changes apply to {n} selected photos"));
    }
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

pub fn not_yet(_view: View, ui: &mut egui::Ui) {
    ui.centered_and_justified(|ui| ui.weak("Timeline view arrives in M6"));
}
