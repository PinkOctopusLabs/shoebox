//! Centre views.

pub mod compare;
pub mod duplicates;
pub mod files;
pub mod filmstrip;
pub mod grid;
pub mod loupe;
pub mod masonry;
pub mod survey;

use eframe::egui::{self, Color32, Rect, Stroke, StrokeKind, pos2, vec2};

use crate::app::ShoeboxApp;
use crate::jobs::{Priority, Size};

pub const GAP: f32 = 6.0;

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

/// Full-resolution size of photo `idx` in image pixels (metadata, else the
/// best texture, else 3:2).
pub fn full_size(app: &mut ShoeboxApp, idx: usize) -> egui::Vec2 {
    let p = &app.catalog.photos[idx];
    if let (Some(w), Some(h)) = (p.meta.width, p.meta.height) {
        return vec2(w as f32, h as f32);
    }
    let id = app.ids[idx];
    match app.textures.best(id, Size::Full) {
        Some(t) => vec2(t.size()[0] as f32, t.size()[1] as f32),
        None => vec2(3.0, 2.0),
    }
}

/// Draw photo `idx` into `rect`: `zoom` over fit (1 = fit), with image point
/// `center` (fractions) at the middle of `rect`. Loads what it needs
/// (the 1024 px thumbnail, and the full image once zoomed or large).
/// Returns where the image went and the fit scale (points per image pixel).
pub fn draw_zoomed(
    app: &mut ShoeboxApp,
    painter: &egui::Painter,
    rect: Rect,
    idx: usize,
    zoom: f32,
    center: egui::Pos2,
) -> (Rect, f32) {
    let id = app.ids[idx];
    app.want(idx, Size::Large, Priority::Visible);
    let ppp = painter.ctx().pixels_per_point();
    let full = full_size(app, idx);
    let fit = (rect.width() / full.x).min(rect.height() / full.y);
    if zoom > 1.01 || rect.width().max(rect.height()) * ppp > 1100.0 {
        app.want(idx, Size::Full, Priority::Visible);
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
    let idx = app.shown[pos];
    let id = app.ids[idx];
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("cell", id)),
        egui::Sense::click_and_drag(),
    );
    if resp.drag_started() {
        // Drag the selection if this photo is in it, else just this one.
        let ids: Vec<crate::jobs::PhotoId> = if app.selection.contains(&id) {
            app.selection.iter().copied().collect()
        } else {
            vec![id]
        };
        resp.dnd_set_drag_payload(DragPhotos(ids));
    }
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    let size = size_for(rect.width().max(rect.height()), ui.ctx().pixels_per_point());
    app.want(idx, size, Priority::Visible);
    if size == Size::Large {
        app.want(idx, Size::Small, Priority::Visible); // something to show meanwhile
    }

    let selected = app.selection.contains(&id);
    let current = app.current == Some(id);
    let bg = if selected {
        visuals.selection.bg_fill.gamma_multiply(0.35)
    } else {
        visuals.faint_bg_color
    };
    if !fill {
        painter.rect_filled(rect, 3.0, bg);
    }
    let photo = &app.catalog.photos[idx];
    let inner = if fill { rect } else { rect.shrink(6.0) };
    match app.textures.best(id, size) {
        Some(tex) => {
            let [w, h] = tex.size();
            let target = if fill {
                inner
            } else {
                fit(inner, w as f32 / h as f32)
            };
            painter.image(
                tex.id(),
                target,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            let target = if fill {
                inner
            } else {
                fit(inner, photo.meta.aspect().unwrap_or(1.5))
            };
            painter.rect_filled(target, 2.0, visuals.extreme_bg_color);
            if app.textures.failed.keys().any(|k| k.photo == id) {
                painter.text(
                    target.center(),
                    egui::Align2::CENTER_CENTER,
                    "⚠",
                    egui::FontId::proportional(18.0),
                    visuals.warn_fg_color,
                );
            }
        }
    }
    // Badges. Bottom left: file type, or the stack's formats and size.
    let st = app.stack_of(idx);
    let expanded = app.expanded.contains(&app.ids[st.primary]);
    let label = if st.len() > 1 && st.primary == idx && !expanded {
        format!("{} · {}", st.badge(&app.catalog.photos), st.len())
    } else if st.variants.contains(&idx) {
        format!("{} copy", photo.format.label())
    } else {
        photo.format.label().to_owned()
    };
    let marks = photo.marks.clone();
    let font = egui::FontId::monospace(9.5);
    let galley = painter.layout_no_wrap(label, font, Color32::WHITE);
    let badge = Rect::from_min_size(
        pos2(inner.left() + 3.0, inner.bottom() - galley.size().y - 5.0),
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
        let r = Rect::from_min_size(inner.left_top() + vec2(3.0, 3.0), vec2(16.0, 16.0));
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
    // Colour label: a strip along the top of the cell.
    if let Some(l) = marks.label {
        // Inset, so the selection border doesn't cover it.
        let strip = Rect::from_min_size(
            rect.left_top() + vec2(4.0, 4.0),
            vec2(rect.width() - 8.0, 4.0),
        );
        painter.rect_filled(strip, 2.0, label_colour(l));
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
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
    if resp.hovered() {
        resp.clone().on_hover_text(photo.rel.clone());
    }
    resp
}

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
        ui.separator();
        if ui.button("Move to Folder…").clicked() {
            let start = app
                .selected_paths()
                .first()
                .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
            let mut dialog = rfd::FileDialog::new().set_title("Move to folder");
            if let Some(d) = start {
                dialog = dialog.set_directory(d);
            }
            if let Some(folder) = dialog.pick_folder() {
                app.move_selection_to(folder.canonicalize().unwrap_or(folder));
            }
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
