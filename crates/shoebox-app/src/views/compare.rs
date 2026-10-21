//! Compare (C): the select and a candidate side by side, zoom and pan
//! synced. Arrows step the candidate, Up makes it the select, Space/Z
//! toggles fit and 100%. Click a side to mark it. In duplicate review the
//! candidates are the group's files, with per-file decisions under each.

use eframe::egui::{self, Color32, Rect, RichText, Stroke, StrokeKind, vec2};

use super::{draw_zoomed, zoom_pan};
use crate::app::ShoeboxApp;
use crate::jobs::PhotoId;

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if app.dupes.open.is_some() {
        super::duplicates::group_bar(app, ui);
    }
    let (Some(a), Some(b)) = (app.compare.select, app.compare.candidate) else {
        ui.centered_and_justified(|ui| ui.weak("Select two photos to compare"));
        return;
    };
    let footer = if app.dupes.open.is_some() { 74.0 } else { 26.0 };
    let full = ui.available_rect_before_wrap();
    let half = (full.width() - 6.0) / 2.0;
    let panes = [
        (
            a,
            "Select",
            Rect::from_min_size(full.min, vec2(half, full.height())),
        ),
        (
            b,
            "Candidate",
            Rect::from_min_size(full.min + vec2(half + 6.0, 0.0), vec2(half, full.height())),
        ),
    ];
    ui.allocate_rect(full, egui::Sense::hover());
    for (id, role, area) in panes {
        pane(app, ui, id, role, area, footer);
    }
}

fn pane(app: &mut ShoeboxApp, ui: &mut egui::Ui, id: PhotoId, role: &str, area: Rect, footer: f32) {
    let Some(idx) = app.index(id) else { return };
    let img_rect = Rect::from_min_max(area.min, area.max - vec2(0.0, footer));
    let resp = ui.interact(
        img_rect,
        ui.scope_id().with(("compare", role)),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(img_rect);
    painter.rect_filled(img_rect, 0.0, Color32::from_gray(18));
    let (zoom, center) = (app.compare.zoom, app.compare.center);
    let (target, fit) = draw_zoomed(app, &painter, img_rect, id, zoom, center);
    if role == "Select" {
        app.compare.select_fit = fit;
    }
    let mut z = app.compare.zoom;
    let mut c = app.compare.center;
    zoom_pan(ui, &resp, target, &mut z, &mut c);
    (app.compare.zoom, app.compare.center) = (z, c);
    if resp.clicked() {
        app.activate(id);
    }
    if resp.double_clicked() {
        app.compare.zoom = if app.compare.zoom > 1.0 {
            1.0
        } else {
            1.0 / (fit * ui.ctx().pixels_per_point())
        };
        app.compare.center = egui::pos2(0.5, 0.5);
    }
    let active = app.current == Some(id);
    if active {
        painter.rect_stroke(
            img_rect.shrink(1.0),
            0.0,
            Stroke::new(2.0, ui.visuals().selection.stroke.color),
            StrokeKind::Inside,
        );
    }
    let pct = fit * zoom * ui.ctx().pixels_per_point() * 100.0;
    painter.text(
        img_rect.left_top() + vec2(10.0, 10.0),
        egui::Align2::LEFT_TOP,
        format!("{role} · {pct:.0}%"),
        egui::FontId::proportional(12.0),
        Color32::from_gray(190),
    );
    super::mark_badges(&app.catalog.photos[idx].marks, &painter, img_rect);

    let foot = Rect::from_min_max(egui::pos2(area.min.x, img_rect.max.y), area.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(foot.shrink2(vec2(6.0, 4.0))));
    let p = app.catalog.photos[idx].clone();
    let name = p
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    child.horizontal(|ui| {
        ui.label(RichText::new(name).strong());
        if let (Some(w), Some(h)) = (p.meta.width, p.meta.height) {
            ui.weak(format!("{w} × {h}"));
        }
    });
    if app.dupes.open.is_some() {
        super::duplicates::entry_controls(app, &mut child, &p.path);
    }
}
