//! Centre views.

pub mod compare;
pub mod crop;
pub mod develop;
pub mod duplicates;
pub mod export;
pub mod files;
pub mod filmstrip;
pub mod grid;
pub mod look;
pub mod loupe;
pub mod map;
pub mod masonry;
pub mod presets;
pub mod settings;
pub mod survey;
pub mod timeline;

use eframe::egui::{self, Color32, Rect, Stroke, StrokeKind, pos2, vec2};

use crate::app::ShoeboxApp;
use crate::jobs::{PhotoId, Priority, Size};

pub const GAP: f32 = 4.0;

/// Photos being dragged (onto a folder to move them, or a collection).
pub struct DragPhotos(pub Vec<crate::jobs::PhotoId>);

/// The thumbnail size to load for a cell of `px` points.
pub fn size_for(px: f32, ppp: f32) -> Size {
    if px * ppp > 300.0 {
        Size::Large
    } else {
        Size::Small
    }
}

/// Fit `aspect` (w/h) inside `rect`, centred.
pub fn fit(rect: Rect, aspect: f32) -> Rect {
    let a = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.5
    };
    let (w, h) = if rect.width() / rect.height() > a {
        (rect.height() * a, rect.height())
    } else {
        (rect.width(), rect.width() / a)
    };
    Rect::from_center_size(rect.center(), vec2(w, h))
}

/// Full-resolution size of tile `id` in image pixels: an edit's, its
/// preview's (a crop changes the shape); else the file's metadata, else the
/// best texture, else 3:2.
pub fn full_size(app: &mut ShoeboxApp, id: PhotoId) -> egui::Vec2 {
    let texture = |app: &mut ShoeboxApp| {
        app.textures
            .best(id, Size::Full)
            .map(|t| vec2(t.size()[0] as f32, t.size()[1] as f32))
    };
    if app.is_edited(id)
        && let Some(v) = texture(app)
    {
        return v;
    }
    if let Some(i) = app.index(id)
        && let (Some(w), Some(h)) = (
            app.catalog.photos[i].meta.width,
            app.catalog.photos[i].meta.height,
        )
    {
        return vec2(w as f32, h as f32);
    }
    texture(app).unwrap_or(vec2(3.0, 2.0))
}

/// Draw tile `id` into `rect`: `zoom` over fit (1 = fit), with image point
/// `center` (fractions) at the middle of `rect`. Loads what it needs
/// (the 1024 px thumbnail, and the full image once zoomed or large).
/// Returns where the image went and the fit scale (points per image pixel).
pub fn draw_zoomed(
    app: &mut ShoeboxApp,
    painter: &egui::Painter,
    rect: Rect,
    id: PhotoId,
    zoom: f32,
    center: egui::Pos2,
) -> (Rect, f32) {
    app.want(id, Size::Large, Priority::Visible);
    let ppp = painter.ctx().pixels_per_point();
    let full = full_size(app, id);
    let fit = (rect.width() / full.x).min(rect.height() / full.y);
    if zoom > 1.01 || rect.width().max(rect.height()) * ppp > 1100.0 {
        app.want(id, Size::Full, Priority::Visible);
    }
    let size = full * fit * zoom;
    let target = Rect::from_min_size(
        rect.center() - vec2(center.x * size.x, center.y * size.y),
        size,
    );
    match app.textures.best(id, Size::Full) {
        Some(t) => {
            painter.image(
                t.id(),
                target,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            let msg = app
                .textures
                .failed
                .iter()
                .find(|(k, _)| k.photo == id)
                .map_or("Loading…".to_owned(), |(_, e)| {
                    format!("Can't display: {e}")
                });
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(13.0),
                Color32::GRAY,
            );
        }
    }
    (target, fit)
}

/// Zoom and pan shared by Compare's panes: update `zoom` and `center` from
/// scroll/pinch about the cursor and from dragging over `resp`.
pub fn zoom_pan(
    ui: &egui::Ui,
    resp: &egui::Response,
    target: Rect,
    zoom: &mut f32,
    center: &mut egui::Pos2,
) {
    if let Some(cursor) = resp.hover_pos() {
        let (pinch, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y));
        let factor = pinch * (scroll * 0.0025).exp();
        if (factor - 1.0).abs() > 1e-4 {
            let new = (*zoom * factor).clamp(1.0, 40.0);
            // Keep the image point under the cursor where it is.
            let p = (cursor - target.min) / target.size();
            let size = target.size() * (new / *zoom);
            let min = cursor - p * size;
            let rect_center = resp.rect.center();
            *center = pos2(
                (rect_center.x - min.x) / size.x,
                (rect_center.y - min.y) / size.y,
            );
            *zoom = new;
        }
    }
    if resp.dragged() && *zoom > 1.0 {
        let d = resp.drag_delta() / target.size();
        *center -= d;
    }
    if *zoom <= 1.0 {
        *zoom = 1.0;
        *center = pos2(0.5, 0.5);
    }
    *center = pos2(center.x.clamp(0.0, 1.0), center.y.clamp(0.0, 1.0));
}

/// Draw one photo cell: image (or placeholder), file-type badge, selection.
/// Returns the click response. `fill` stretches the image to the cell (masonry).
pub fn cell(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    rect: Rect,
    pos: usize,
    fill: bool,
) -> egui::Response {
    // A menu action earlier this frame (say, taking a photo out of the
    // collection being shown) can shrink `shown` under the loop drawing it.
    let Some(crate::app::Tile { idx, id }) = app.shown.get(pos).copied() else {
        return ui.interact(
            rect,
            ui.scope_id().with(("gone", pos)),
            egui::Sense::hover(),
        );
    };
    // One of the file's virtual copies, not the file itself.
    let copy = app.copy_info(id).map(|c| c.name.clone());
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("cell", id)),
        egui::Sense::click_and_drag(),
    );
    if resp.drag_started() {
        // Drag the selection if this photo is in it, else just this one
        // (files: a copy's tile stands for its file).
        let ids: Vec<crate::jobs::PhotoId> = if app.selection.contains(&id) {
            app.selection.iter().copied().collect()
        } else {
            vec![id]
        };
        let mut files: Vec<crate::jobs::PhotoId> = ids
            .into_iter()
            .filter_map(|id| app.index(id).map(|i| app.ids[i]))
            .collect();
        files.sort_unstable();
        files.dedup();
        resp.dnd_set_drag_payload(DragPhotos(files));
    }
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    let size = size_for(rect.width().max(rect.height()), ui.ctx().pixels_per_point());
    app.want(id, size, Priority::Visible);
    if size == Size::Large {
        app.want(id, Size::Small, Priority::Visible); // something to show meanwhile
    }

    let selected = app.selection.contains(&id);
    let current = app.current == Some(id);
    if !fill {
        // Tiles a step lighter than the background, selected ones lighter
        // still (as in Lightroom).
        let bg = match (visuals.dark_mode, selected) {
            (true, false) => Color32::from_gray(38),
            (true, true) => Color32::from_gray(66),
            (false, false) => Color32::from_gray(222),
            (false, true) => Color32::from_gray(196),
        };
        painter.rect_filled(rect, 3.0, bg);
    }
    let photo = &app.catalog.photos[idx];
    let cell_inner = if fill { rect } else { rect.shrink(4.0) };
    // Badges and marks sit on the image itself, not the cell around it.
    let inner = match app.textures.best(id, size) {
        Some(tex) => {
            let [w, h] = tex.size();
            let target = if fill {
                cell_inner
            } else {
                fit(cell_inner, w as f32 / h as f32)
            };
            painter.image(
                tex.id(),
                target,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            target
        }
        None => {
            let target = if fill {
                cell_inner
            } else {
                fit(cell_inner, photo.meta.aspect().unwrap_or(1.5))
            };
            painter.rect_filled(target, 2.0, visuals.extreme_bg_color);
            if photo.cloud {
                painter.text(
                    target.center(),
                    egui::Align2::CENTER_CENTER,
                    crate::icons::CLOUD,
                    egui::FontId::proportional(24.0),
                    visuals.weak_text_color(),
                );
            } else if app.textures.failed.keys().any(|k| k.photo == id) {
                painter.text(
                    target.center(),
                    egui::Align2::CENTER_CENTER,
                    "⚠",
                    egui::FontId::proportional(18.0),
                    visuals.warn_fg_color,
                );
            }
            target
        }
    };
    // Faces outlined (the view bar's toggle), the person chosen in amber.
    face_boxes(app, &painter, id, inner, false);
    // Badges. Bottom left: file type, or the stack's formats and size.
    let st = app.stack_of(idx);
    let expanded = app.expanded.contains(&app.ids[st.primary]);
    let is_movie = |i: usize| app.catalog.photos[i].format.is_movie();
    let live = st.primary == idx && !is_movie(idx) && st.all().any(is_movie);
    let label = if let Some(name) = &copy {
        // A virtual copy: the file's type and the copy's name.
        format!("{} · {name}", photo.format.label())
    } else if live && st.len() == 2 {
        // A Live Photo: the picture and its short video.
        "LIVE".to_owned()
    } else if st.len() > 1 && st.primary == idx && !expanded {
        format!("{} · {}", st.badge(&app.catalog.photos), st.len())
    } else if st.variants.contains(&idx) {
        format!("{} copy", photo.format.label())
    } else if is_movie(idx) {
        format!("▶ {}", photo.format.label())
    } else {
        photo.format.label().to_owned()
    };
    // Edited in Develop (any of the stack): the tile shows the edit. A
    // folded stack counts its virtual copies: "EDITED +2".
    let edited = st
        .all()
        .any(|i| app.edited.contains_key(&app.catalog.photos[i].path));
    let copies: usize = if copy.is_none() && st.primary == idx && !expanded {
        st.all()
            .filter_map(|i| app.copies.get(&app.catalog.photos[i].path))
            .map(Vec::len)
            .sum()
    } else {
        0
    };
    let label = match (copy.is_some(), edited, copies) {
        (true, _, _) => label,
        (false, true, 0) => format!("{label} · EDITED"),
        (false, true, n) => format!("{label} · EDITED +{n}"),
        (false, false, 0) => label,
        (false, false, 1) => format!("{label} · +1 COPY"),
        (false, false, n) => format!("{label} · +{n} COPIES"),
    };
    // In the cloud only (the whole stack, as it's what opening fetches).
    let label = if st.all().any(|i| app.catalog.photos[i].cloud) {
        format!("{} {label}", crate::icons::CLOUD)
    } else {
        label
    };
    let marks = photo.marks.clone();
    let font = egui::FontId::monospace(9.5);
    let galley = painter.layout_no_wrap(label, font, Color32::WHITE);
    let badge = Rect::from_min_size(
        pos2(inner.left() + 4.0, inner.bottom() - galley.size().y - 6.0),
        galley.size() + vec2(6.0, 2.0),
    );
    painter.rect_filled(badge, 2.0, Color32::from_black_alpha(150));
    painter.galley(badge.min + vec2(3.0, 1.0), galley, Color32::WHITE);

    // Rejects are dimmed, as in Lightroom.
    if marks.pick == Some(shoebox_core::marks::Pick::Reject) {
        painter.rect_filled(inner, 0.0, Color32::from_black_alpha(140));
    }
    // Top left: flag.
    let flag = match marks.pick {
        Some(shoebox_core::marks::Pick::Pick) => Some(("✔", Color32::from_rgb(80, 200, 120))),
        Some(shoebox_core::marks::Pick::Reject) => Some(("✖", Color32::from_rgb(235, 80, 80))),
        None => None,
    };
    if let Some((glyph, colour)) = flag {
        let r = Rect::from_min_size(inner.left_top() + vec2(4.0, 6.0), vec2(16.0, 16.0));
        painter.rect_filled(r, 3.0, Color32::from_black_alpha(160));
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(11.0),
            colour,
        );
    }
    // Bottom right: rating.
    if marks.rating > 0 {
        let stars = "★".repeat(usize::from(marks.rating));
        let g = painter.layout_no_wrap(
            stars,
            egui::FontId::proportional(10.0),
            Color32::from_rgb(250, 210, 80),
        );
        let r = Rect::from_min_size(
            pos2(
                inner.right() - g.size().x - 9.0,
                inner.bottom() - g.size().y - 5.0,
            ),
            g.size() + vec2(6.0, 2.0),
        );
        painter.rect_filled(r, 2.0, Color32::from_black_alpha(150));
        painter.galley(r.min + vec2(3.0, 1.0), g, Color32::WHITE);
    }
    // Top right: changed since you last viewed it.
    if let Some(ch) = app.cell_change(idx) {
        let c = egui::pos2(inner.right() - 9.0, inner.top() + 9.0);
        painter.circle_filled(c, 5.5, Color32::from_black_alpha(160));
        painter.circle_filled(c, 4.0, change_colour(ch));
        let r = Rect::from_center_size(c, vec2(14.0, 14.0));
        let tip = describe_change(ch);
        ui.interact(r, ui.scope_id().with(("change", id)), egui::Sense::hover())
            .on_hover_text(tip);
    }
    // Colour label: a dot at the top left (after the flag, if any).
    if let Some(l) = marks.label {
        let x = if flag.is_some() { 30.0 } else { 12.0 };
        let c = inner.left_top() + vec2(x, 14.0);
        painter.circle_filled(c, 6.0, Color32::from_black_alpha(160));
        painter.circle_filled(c, 4.5, label_colour(l));
    }

    if selected || current {
        let color = if current {
            visuals.selection.stroke.color
        } else {
            visuals.selection.bg_fill
        };
        painter.rect_stroke(
            rect.shrink(1.0),
            3.0,
            Stroke::new(if current { 2.5 } else { 1.5 }, color),
            StrokeKind::Inside,
        );
    }
    let name = photo
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A copy's tile is named for the copy too: "DSC_0042.NEF (Copy 1)".
    let name = match &copy {
        Some(c) => format!("{name} ({c})"),
        None => name,
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
    if resp.hovered() {
        resp.clone().on_hover_text(photo.rel.clone());
    }
    resp
}

/// The photos' scroll area (grid, masonry, timeline), at offset `jump_to`
/// when set. egui keeps last frame's scroll limit and any scroll still
/// under way (a flick, a bounce at the end): a jump past that limit, into
/// photos just shown, would spring back to it. So a jump starts afresh.
pub fn photo_scroll(ui: &egui::Ui, jump_to: Option<f32>) -> egui::ScrollArea {
    let area = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("photos");
    match jump_to {
        Some(y) => {
            // The id `id_salt("photos")` gives the area.
            let id = ui.make_persistent_id(egui::IdSalt::new("photos"));
            egui::scroll_area::State::default().store(ui.ctx(), id);
            area.vertical_scroll_offset(y)
        }
        None => area,
    }
}

pub(crate) const SHOW_IN_ALL: &str = "Show in All Photos";

/// The photo context menu (right-click).
pub fn photo_menu(app: &mut ShoeboxApp, resp: &egui::Response, pos: usize) {
    if resp.secondary_clicked() {
        app.secondary_click(pos);
    }
    resp.context_menu(|ui| {
        let n = app.selection.len();
        let label = if n > 1 {
            format!("{} ({n} photos)", crate::app::REVEAL_LABEL)
        } else {
            crate::app::REVEAL_LABEL.to_owned()
        };
        if ui.button(label).clicked() {
            app.reveal_selection();
            ui.close();
        }
        if (app.narrowed() || app.view_name() != crate::app::View::Grid)
            && ui
                .button(SHOW_IN_ALL)
                .on_hover_text("All photos in the grid, with the ones taken around this one")
                .clicked()
        {
            if let Some(t) = app.shown.get(pos) {
                app.show_in_all(t.id);
            }
            ui.close();
        }
        // A virtual copy's tile: delete the copy (never the file).
        let tile = app.shown.get(pos).copied();
        if let Some(t) = tile
            && let Some(copy) = app.copy_of(t.id).map(str::to_owned)
            && !app.catalog.is_reference(t.idx)
            && ui
                .button("Delete Copy")
                .on_hover_text(
                    "Move this copy's edit to the Trash (⌘Z brings it back); the photo stays",
                )
                .clicked()
        {
            let file = app.ids[t.idx];
            develop::remove_copy(app, file, &copy);
            ui.close();
        }
        if ui
            .button("Export…")
            .on_hover_text("Export with presets (Shift-⌘E)")
            .clicked()
        {
            app.export.open = true;
            ui.close();
        }
        let ids: Vec<crate::jobs::PhotoId> = app.selection.iter().copied().collect();
        let cloud = app.cloud_in(&ids);
        if cloud > 0 {
            let label = if cloud > 1 {
                format!("Download from Cloud ({cloud} photos)")
            } else {
                "Download from Cloud".to_owned()
            };
            if ui
                .button(label)
                .on_hover_text("Fetch the files from the cloud drive so Shoebox can read them")
                .clicked()
            {
                app.download(&ids);
                ui.close();
            }
        }
        if app.settings.semantic_search
            && ui
                .add_enabled(
                    app.sem.status.ready(),
                    egui::Button::new("Find Similar Photos"),
                )
                .on_disabled_hover_text("Semantic search is getting ready (see Search in Settings)")
                .clicked()
        {
            if let Some(t) = app.shown.get(pos) {
                app.find_similar(app.ids[t.idx]);
            }
            ui.close();
        }
        if let Some(name) = app.ppl.shown_group().and_then(|g| g.name.clone())
            && ui
                .button(format!("Not {name}"))
                .on_hover_text("This photo's face isn't them")
                .clicked()
        {
            if let Some(t) = app.shown.get(pos) {
                app.not_person(app.ids[t.idx]);
            }
            ui.close();
        }
        ui.separator();
        crate::panels::add_to_event_menu(app, ui, |app| app.selected_stacks());
        crate::panels::remove_from_event_menu(app, ui);
        if ui.button("New Event from Selection…  (⌘E)").clicked() {
            app.new_event_from_selection(None);
            ui.close();
        }
        crate::panels::add_to_collection_menu(app, ui, |app| app.selected_stacks());
        crate::panels::remove_from_collection_menu(app, ui);
        if ui.button("New Collection from Selection…").clicked() {
            app.collection_edit = Some((None, String::new()));
            app.left_open = true;
            app.collapsed.remove("Collections");
            ui.close();
        }
        ui.separator();
        if ui.button("Move to Folder…").clicked() {
            let start = app
                .selected_paths()
                .first()
                .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
            app.picker.open(
                crate::picker::FolderFor::MoveSelection,
                "Move to folder",
                start.as_deref(),
            );
            ui.close();
        }
        if ui.button("Rename…").clicked() {
            app.start_rename();
            ui.close();
        }
        if ui.button("Move to _rejected  (⌘Backspace)").clicked() {
            app.reject_selection();
            ui.close();
        }
        let rejects = app.rejected_shown().len();
        if rejects > 0
            && ui
                .button(format!("Move all {rejects} rejected to _rejected"))
                .clicked()
        {
            app.reject_all_rejected();
            ui.close();
        }
        if ui.button("Move to Trash…").clicked() {
            let photos = app.selected_paths();
            app.request_org(crate::jobs::OrgRequest::Trash { photos });
            ui.close();
        }
        if app.selection_has_changes() && ui.button("Mark as Seen").clicked() {
            app.mark_selection_seen();
            ui.close();
        }
    });
}

/// Handle a cell's clicks: select, and double-click opens the loupe.
pub fn handle_click(app: &mut ShoeboxApp, ui: &egui::Ui, resp: &egui::Response, pos: usize) {
    if resp.double_clicked() {
        app.click(pos, egui::Modifiers::NONE);
        app.set_view(crate::app::View::Loupe);
    } else if resp.clicked() {
        app.click(pos, ui.input(|i| i.modifiers));
    }
    photo_menu(app, resp, pos);
}

/// Flag, rating and colour label in a strip at the bottom left of `rect`
/// (Compare and Survey, where photos are large).
pub fn mark_badges(marks: &shoebox_core::marks::Marks, painter: &egui::Painter, rect: Rect) {
    use shoebox_core::marks::Pick;
    let mut parts: Vec<(String, Color32)> = Vec::new();
    match marks.pick {
        Some(Pick::Pick) => parts.push(("✔".into(), Color32::from_rgb(80, 200, 120))),
        Some(Pick::Reject) => parts.push(("✖".into(), Color32::from_rgb(235, 80, 80))),
        None => {}
    }
    if marks.rating > 0 {
        parts.push((
            "★".repeat(usize::from(marks.rating)),
            Color32::from_rgb(250, 210, 80),
        ));
    }
    let mut x = rect.left() + 8.0;
    let y = rect.bottom() - 26.0;
    for (text, colour) in parts {
        let g = painter.layout_no_wrap(text, egui::FontId::proportional(13.0), colour);
        let r = Rect::from_min_size(pos2(x, y), g.size() + vec2(10.0, 4.0));
        painter.rect_filled(r, 3.0, Color32::from_black_alpha(160));
        painter.galley(r.min + vec2(5.0, 2.0), g, colour);
        x = r.right() + 4.0;
    }
    if let Some(l) = marks.label {
        let r = Rect::from_min_size(pos2(x, y + 2.0), vec2(16.0, 16.0));
        painter.rect_filled(r, 3.0, label_colour(l));
    }
}

/// Display colour of a colour label.
pub fn label_colour(l: shoebox_core::marks::Label) -> Color32 {
    use shoebox_core::marks::Label;
    match l {
        Label::Red => Color32::from_rgb(229, 72, 77),
        Label::Yellow => Color32::from_rgb(245, 217, 10),
        Label::Green => Color32::from_rgb(70, 167, 88),
        Label::Blue => Color32::from_rgb(62, 99, 221),
        Label::Purple => Color32::from_rgb(142, 78, 198),
    }
}

/// Badge colour: another app's change stands out more than Shoebox's own.
pub fn change_colour(c: &shoebox_core::changes::Change) -> Color32 {
    if c.by_other() {
        Color32::from_rgb(255, 149, 0)
    } else {
        Color32::from_rgb(100, 170, 255)
    }
}

/// "Sidecar changed by another app, 16 Oct 2026 14:02".
pub fn describe_change(c: &shoebox_core::changes::Change) -> String {
    use shoebox_core::changes::By;
    let who = match &c.by {
        By::Other => "another app".to_owned(),
        By::Shoebox { batch, label } if label.is_empty() => format!("Shoebox (batch {batch})"),
        By::Shoebox { batch, label } => format!("Shoebox: {label} (batch {batch})"),
    };
    format!(
        "{} changed by {who}, {}",
        c.what.describe(),
        crate::app::local_time_ms(c.at_ms)
    )
}

/// The faces to outline on a photo: (x, y, width, height as fractions of
/// the upright image, name, is the person chosen in People or pointed at
/// in the info panel). Every recognised face while the view bar's face
/// toggle is on, whatever People shows; with it off, only a face whose name
/// is pointed at.
pub(crate) fn faces_to_draw(
    app: &ShoeboxApp,
    id: crate::jobs::PhotoId,
) -> Vec<([f32; 4], String, bool)> {
    let Some(view) = app.ppl.view.as_ref() else {
        return Vec::new();
    };
    let all = !app.settings.hide_face_boxes;
    let shown = app.ppl.shown_indices();
    let hovered = app.ppl.hovered.as_ref().and_then(|k| view.find(k));
    let me = |g| shown.contains(&g) || hovered == Some(g);
    view.boxes
        .get(&id)
        .into_iter()
        .flatten()
        .filter(|&&(_, g)| all || hovered == Some(g))
        .map(|&(r, g)| (r, app.ppl.label(&view.groups[g]), me(g)))
        .collect()
}

/// The person being shown: amber, which stands out on most photos (the
/// selection blue is close to white on a dark theme).
const FACE_HIGHLIGHT: Color32 = Color32::from_rgb(255, 190, 40);

/// Faces on a photo drawn at `target` (see [`faces_to_draw`]): the person
/// chosen outlined in amber, others faintly; with `names` (the loupe),
/// names under them.
pub fn face_boxes(
    app: &ShoeboxApp,
    painter: &egui::Painter,
    id: crate::jobs::PhotoId,
    target: Rect,
    names: bool,
) {
    for ([x, y, w, h], name, me) in faces_to_draw(app, id) {
        // Detector boxes hug the face; give it a little room.
        let r = Rect::from_min_size(
            target.min + vec2(x, y) * target.size(),
            vec2(w, h) * target.size(),
        )
        .expand2(vec2(w, h) * target.size() * 0.12);
        if r.width() < 6.0 {
            continue; // a face too small on a tile to box usefully
        }
        let (stroke, fill, text) = if me {
            (
                Stroke::new(2.0, FACE_HIGHLIGHT),
                FACE_HIGHLIGHT,
                Color32::BLACK,
            )
        } else {
            (
                Stroke::new(1.0, Color32::from_white_alpha(150)),
                Color32::from_black_alpha(160),
                Color32::WHITE,
            )
        };
        if me {
            // A dark edge keeps it visible on light photos.
            painter.rect_stroke(
                r,
                4.0,
                Stroke::new(3.5, Color32::from_black_alpha(140)),
                StrokeKind::Outside,
            );
        }
        painter.rect_stroke(r, 4.0, stroke, StrokeKind::Outside);
        if names && r.width() >= 24.0 {
            let galley = painter.layout_no_wrap(name, egui::FontId::proportional(11.0), text);
            // Under the face, or inside its box when there's no room below.
            let size = galley.size() + vec2(8.0, 2.0);
            let below = r.bottom() + 4.0;
            let y = if below + size.y <= target.bottom() {
                below
            } else {
                r.bottom() - size.y - 4.0
            };
            let tag = Rect::from_min_size(pos2(r.center().x - size.x / 2.0, y), size);
            painter.rect_filled(tag, 3.0, fill);
            painter.galley(tag.min + vec2(4.0, 1.0), galley, text);
        }
    }
}
