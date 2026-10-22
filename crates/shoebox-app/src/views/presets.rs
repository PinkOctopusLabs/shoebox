//! Develop's presets (the left panel: hover to preview, click to apply,
//! then an Amount) and copying settings from one photo to others (⇧⌘C,
//! ⇧⌘V).

use std::collections::BTreeSet;

use eframe::egui::{self, RichText};
use shoebox_develop::edit::Settings;
use shoebox_develop::preset::{Part, Preset};

use super::develop::{edit, heading};
use crate::app::ShoeboxApp;
use crate::jobs::LibraryCommand;

/// The "Save preset" window's fields.
pub struct Saving {
    pub name: String,
    pub group: String,
    pub parts: BTreeSet<Part>,
}

/// The preset last applied: the settings before it, its amount, and the
/// history position after it (another change hides the Amount slider,
/// since re-applying would undo that change).
#[derive(Clone)]
pub struct Applied {
    pub preset: Preset,
    pub before: Settings,
    pub amount: f64,
    pub at: usize,
}

fn copied_parts() -> BTreeSet<Part> {
    Part::ALL
        .into_iter()
        .filter(|p| p.copied_by_default())
        .collect()
}

/// The presets list.
pub fn panel(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    app.develop.preview = None;
    let st = &app.develop;
    let Some(doc) = &st.doc else { return };
    let current = doc.settings().clone();
    let at = doc.at();
    let read_only = st.read_only;
    ui.horizontal(|ui| {
        heading(ui, "PRESETS");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui
                .add_enabled(!read_only, egui::Button::new("+").small())
                .on_hover_text("Save these settings as a preset");
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, !read_only, "New preset")
            });
            if r.clicked() {
                let n = app.develop.looks.len() + 1;
                app.develop.saving_look = Some(Saving {
                    name: format!("Preset {n}"),
                    group: String::new(),
                    parts: copied_parts(),
                });
            }
        });
    });
    // Favourites first, then by group (ungrouped first), then by name.
    let mut order: Vec<usize> = (0..app.develop.looks.len()).collect();
    order.sort_by_key(|&i| {
        let p = &app.develop.looks[i];
        (!p.favourite, p.group.clone(), p.name.to_lowercase())
    });
    if order.is_empty() {
        ui.weak("None yet");
    }
    let mut last_group: Option<String> = None;
    let mut apply = None;
    let mut favourite = None;
    let mut remove = None;
    for i in order {
        let p = &app.develop.looks[i];
        let group = if p.favourite {
            "Favourites".to_owned()
        } else {
            p.group.clone()
        };
        if last_group.as_ref() != Some(&group) && !group.is_empty() {
            ui.label(RichText::new(&group).small().weak());
        }
        last_group = Some(group);
        let label = if p.favourite {
            format!("★ {}", p.name)
        } else {
            p.name.clone()
        };
        let r = ui.add_enabled(!read_only, egui::Button::selectable(false, label));
        let parts: Vec<&str> = p.parts().iter().map(|x| x.label()).collect();
        let r = r.on_hover_text(parts.join(", "));
        if r.hovered() {
            app.develop.preview = Some(p.apply(&current, 1.0));
        }
        if r.clicked() {
            apply = Some(i);
        }
        r.context_menu(|ui| {
            let fav = if p.favourite {
                "Remove from favourites"
            } else {
                "Add to favourites"
            };
            if ui.button(fav).clicked() {
                favourite = Some(i);
            }
            if ui.button("Delete").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = apply {
        let preset = app.develop.looks[i].clone();
        let before = current.clone();
        let label = format!("Preset {}", preset.name);
        let p = preset.clone();
        edit(app, "preset", label, |s| *s = p.apply(&before, 1.0));
        let at = app.develop.doc.as_ref().map_or(0, |d| d.at());
        app.develop.applied = Some(Applied {
            preset,
            before: current.clone(),
            amount: 1.0,
            at,
        });
    }
    if let Some(i) = favourite {
        let p = &mut app.develop.looks[i];
        p.favourite = !p.favourite;
        app.developer.save_looks(app.develop.looks.clone());
    }
    if let Some(i) = remove {
        app.develop.looks.remove(i);
        app.develop.applied = None;
        app.developer.save_looks(app.develop.looks.clone());
    }
    // The Amount of the preset just applied (until something else changes).
    if let Some(a) = app.develop.applied.clone() {
        if a.at != at && apply.is_none() {
            app.develop.applied = None;
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&a.preset.name).small());
                let mut v = a.amount * 100.0;
                let r = ui.add(
                    egui::Slider::new(&mut v, 0.0..=200.0)
                        .suffix("%")
                        .fixed_decimals(0),
                );
                if r.changed() {
                    let amount = v / 100.0;
                    let label = format!("Preset {} {v:.0}%", a.preset.name);
                    let (p, before) = (a.preset.clone(), a.before.clone());
                    edit(app, "preset-amount", label, |s| {
                        *s = p.apply(&before, amount);
                    });
                    let at = app.develop.doc.as_ref().map_or(0, |d| d.at());
                    if let Some(x) = &mut app.develop.applied {
                        x.amount = amount;
                        x.at = at;
                    }
                }
            });
        }
    }
    save_window(app, ui.ctx(), &current);
}

fn parts_checklist(ui: &mut egui::Ui, parts: &mut BTreeSet<Part>) {
    egui::Grid::new("parts").num_columns(2).show(ui, |ui| {
        for (k, p) in Part::ALL.into_iter().enumerate() {
            let mut on = parts.contains(&p);
            if ui.checkbox(&mut on, p.label()).changed() {
                if on {
                    parts.insert(p);
                } else {
                    parts.remove(&p);
                }
            }
            if k % 2 == 1 {
                ui.end_row();
            }
        }
    });
    ui.horizontal(|ui| {
        if ui.small_button("All").clicked() {
            *parts = Part::ALL.into_iter().collect();
        }
        if ui.small_button("None").clicked() {
            parts.clear();
        }
    });
}

fn save_window(app: &mut ShoeboxApp, ctx: &egui::Context, current: &Settings) {
    let Some(saving) = &mut app.develop.saving_look else {
        return;
    };
    let mut open = true;
    let mut done = None;
    egui::Window::new("Save preset")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::Grid::new("save-look").num_columns(2).show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut saving.name);
                ui.end_row();
                ui.label("Group");
                ui.add(egui::TextEdit::singleline(&mut saving.group).hint_text("(none)"));
                ui.end_row();
            });
            ui.label("Settings it carries:");
            parts_checklist(ui, &mut saving.parts);
            ui.horizontal(|ui| {
                let ok = !saving.name.trim().is_empty() && !saving.parts.is_empty();
                if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
    if done == Some(true) {
        let parts: Vec<Part> = saving.parts.iter().copied().collect();
        let mut p = Preset::from_settings(saving.name.trim(), current, &parts);
        p.group = saving.group.trim().to_owned();
        // A preset with the same name is replaced.
        app.develop.looks.retain(|l| l.name != p.name);
        app.develop.looks.push(p);
        app.developer.save_looks(app.develop.looks.clone());
    }
    if !open || done.is_some() {
        app.develop.saving_look = None;
    }
}

/// ⇧⌘C in Develop: choose what to copy.
pub fn start_copy(app: &mut ShoeboxApp) {
    if app.develop.doc.is_some() {
        app.develop.copying = Some(copied_parts());
    }
}

/// The "Copy settings" window.
pub fn copy_window(app: &mut ShoeboxApp, ctx: &egui::Context) {
    let Some(parts) = &mut app.develop.copying else {
        return;
    };
    let mut open = true;
    let mut done = None;
    egui::Window::new("Copy settings")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            parts_checklist(ui, parts);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!parts.is_empty(), egui::Button::new("Copy"))
                    .clicked()
                {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
    if done == Some(true)
        && let Some(doc) = &app.develop.doc
    {
        let list: Vec<Part> = parts.iter().copied().collect();
        app.settings_clipboard = Some(Preset::from_settings("Copied", doc.settings(), &list));
        app.notice = Some("Settings copied. Shift-⌘V pastes them onto the selected photos.".into());
    }
    if !open || done.is_some() {
        app.develop.copying = None;
    }
}

/// ⇧⌘V: the copied settings onto the selection (else the current photo).
/// The photo open in Develop changes as an ordinary step; the others go to
/// the library worker as one batch. Reference photos are left alone.
pub fn paste(app: &mut ShoeboxApp) {
    let Some(preset) = app.settings_clipboard.clone() else {
        app.notice =
            Some("Nothing copied yet: Shift-⌘C in Develop copies a photo's settings.".into());
        return;
    };
    let sel = app.selected_in_order();
    let ids = if sel.is_empty() {
        app.current.into_iter().collect()
    } else {
        sel
    };
    // The tile open in Develop (a copy's, when it's a copy).
    let open = app.develop.doc.as_ref().and(app.current);
    let mut others = Vec::new();
    let mut skipped = 0;
    let mut here = false;
    for id in ids {
        let Some(i) = app.index(id) else { continue };
        if app.catalog.is_reference(i) || app.catalog.photos[i].format.is_movie() {
            skipped += 1;
            continue;
        }
        if Some(id) == open && app.view == crate::app::View::Develop {
            here = true;
            continue;
        }
        let p = &app.catalog.photos[i];
        let copy = app.copy_of(id).unwrap_or(shoebox_core::edits::MAIN);
        others.push((
            app.catalog.roots[p.root].path.clone(),
            p.rel.clone(),
            copy.to_owned(),
            preset.clone(),
        ));
    }
    if here && !app.develop.read_only {
        let p = preset.clone();
        edit(app, "paste", "Paste settings".into(), |s| {
            *s = p.apply(s, 1.0)
        });
    }
    let n = others.len() + usize::from(here);
    if !others.is_empty() {
        app.command(LibraryCommand::ApplyPreset {
            photos: others,
            label: "Paste settings".into(),
        });
    }
    app.notice = Some(match (n, skipped) {
        (0, _) => "Nothing to paste onto (reference photos are read-only).".into(),
        (n, 0) => format!(
            "Pasted settings onto {n} photo{}.",
            if n == 1 { "" } else { "s" }
        ),
        (n, s) => format!(
            "Pasted settings onto {n} photo{}; {s} in reference folders left alone.",
            if n == 1 { "" } else { "s" }
        ),
    });
}

/// Match Total Exposures (EDITING-SPEC, Tools beyond PhotoLab): the
/// selection's Exposure set so each comes out as bright as the current
/// photo, from how much light each let in (shutter, ISO, aperture) and the
/// current photo's own Exposure. Photos without those in their EXIF, and
/// reference photos, are left alone.
pub fn match_exposures(app: &mut ShoeboxApp) {
    let Some(current) = app.current else { return };
    let Some(ci) = app.index(current) else { return };
    let Some(target) = app.catalog.photos[ci].meta.exif.captured_stops() else {
        app.notice = Some(
            "This photo's EXIF doesn't say its shutter speed, ISO and aperture, so there's \
             nothing to match to."
                .into(),
        );
        return;
    };
    // The current photo's own Exposure (from Develop if it's open there).
    let open = app
        .develop
        .doc
        .as_ref()
        .filter(|_| app.develop.photo == Some(current));
    let ev = match open {
        Some(d) => d.settings().modules.exposure.ev,
        None => app
            .edit_settings(current)
            .map_or(0.0, |s| s.modules.exposure.ev),
    };
    let mut photos = Vec::new();
    let mut skipped = 0;
    for id in app.selected_in_order() {
        if id == current {
            continue;
        }
        let Some(i) = app.index(id) else { continue };
        let p = &app.catalog.photos[i];
        let Some(stops) = p.meta.exif.captured_stops() else {
            skipped += 1;
            continue;
        };
        if app.catalog.is_reference(i) || p.format.is_movie() {
            skipped += 1;
            continue;
        }
        // Brighter captures need less Exposure.
        let v = ((ev + target - stops) * 100.0).round() / 100.0;
        let mut s = shoebox_develop::edit::Settings::default();
        s.modules.exposure.ev = v.clamp(-5.0, 5.0);
        let preset = shoebox_develop::preset::Preset::from_settings(
            "Match total exposures",
            &s,
            &[shoebox_develop::preset::Part::Exposure],
        );
        let copy = app.copy_of(id).unwrap_or(shoebox_core::edits::MAIN);
        photos.push((
            app.catalog.roots[p.root].path.clone(),
            p.rel.clone(),
            copy.to_owned(),
            preset,
        ));
    }
    let n = photos.len();
    if n > 0 {
        app.command(LibraryCommand::ApplyPreset {
            photos,
            label: "Match total exposures".into(),
        });
    }
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    app.notice = Some(match (n, skipped) {
        (0, 0) => "Select the photos to match to this one first.".into(),
        (0, s) => format!(
            "Nothing matched: {s} photo{} without exposure data or read-only.",
            plural(s)
        ),
        (n, 0) => format!(
            "Matched the exposure of {n} photo{} to this one.",
            plural(n)
        ),
        (n, s) => format!(
            "Matched the exposure of {n} photo{} to this one; {s} without exposure data or read-only left alone.",
            plural(n)
        ),
    });
}
