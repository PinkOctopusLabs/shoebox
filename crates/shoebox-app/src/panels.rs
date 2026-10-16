//! Toolbar, status bar, library tree and info panel.

use eframe::egui::{self, Color32, RichText, pos2, vec2};

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
        if let Some(e) = app.error.clone() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
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
    ui.label(RichText::new("Events").strong());
    ui.weak("Arrive in M3");
    ui.label(RichText::new("Collections").strong());
    ui.weak("Arrive in M5");
    ui.label(RichText::new("Smart filters").strong());
    ui.weak("Arrive in M3");
    ui.separator();
    ui.label(RichText::new("Tasks").strong());
    ui.weak("Duplicates review (M4), Import and History (M5)");
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
        r.context_menu(|ui| {
            if ui.button("Reveal in Finder").clicked() {
                reveal(&node.path);
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
    ui.heading("Organise");
    ui.separator();
    ui.weak("Ratings, flags, labels and keywords arrive in M3.");
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

pub fn not_yet(view: View, ui: &mut egui::Ui) {
    let (name, when) = match view {
        View::Compare => ("Compare", "M4"),
        View::Survey => ("Survey", "M4"),
        _ => ("Timeline", "M6"),
    };
    ui.centered_and_justified(|ui| ui.weak(format!("{name} view arrives in {when}")));
}
