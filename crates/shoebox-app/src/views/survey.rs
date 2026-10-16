//! Survey (N): 2–9 photos tiled as large as they fit, for picking the best
//! of a burst. Click a tile to make it the one marks apply to (rate and flag
//! in place, by key or under the tile); its X takes it out of the survey.
//! In duplicate review the tiles are the group's files, with decisions.

use eframe::egui::{self, Color32, Rect, RichText, Stroke, StrokeKind, pos2, vec2};

use super::draw_zoomed;
use crate::app::ShoeboxApp;

/// Columns × rows for `n` tiles of aspect `a` in `area`, largest tiles first.
pub fn layout(n: usize, area: egui::Vec2, a: f32) -> (usize, usize) {
    let mut best = (1, n.max(1), 0.0f32);
    for cols in 1..=n.max(1) {
        let rows = n.div_ceil(cols);
        let (w, h) = (area.x / cols as f32, area.y / rows as f32);
        let tile = if w / h > a { h * a * h } else { w * w / a };
        if tile > best.2 {
            best = (cols, rows, tile);
        }
    }
    (best.0, best.1)
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if app.dupes.open.is_some() {
        super::duplicates::group_bar(app, ui);
    }
    // Drop photos that have gone (moved, or the catalog changed).
    let survey: Vec<_> = app
        .survey
        .iter()
        .copied()
        .filter(|&id| app.index(id).is_some())
        .collect();
    if survey.len() < 2 {
        ui.centered_and_justified(|ui| {
            ui.weak("Select 2 to 9 photos in the grid, then press N to survey them")
        });
        return;
    }
    let reviewing = app.dupes.open.is_some();
    let footer = if reviewing { 74.0 } else { 44.0 };
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, egui::Sense::hover());
    let (cols, rows) = layout(survey.len(), area.size(), 1.5);
    let cell = vec2(area.width() / cols as f32, area.height() / rows as f32);
    let mut remove = None;
    for (k, &id) in survey.iter().enumerate() {
        let Some(idx) = app.index(id) else { continue };
        let (c, r) = (k % cols, k / cols);
        let tile = Rect::from_min_size(area.min + vec2(c as f32 * cell.x, r as f32 * cell.y), cell)
            .shrink(4.0);
        let img = Rect::from_min_max(tile.min, tile.max - vec2(0.0, footer));
        let resp = ui.interact(
            img,
            ui.scope_id().with(("survey", id)),
            egui::Sense::click(),
        );
        let painter = ui.painter_at(tile);
        painter.rect_filled(tile, 4.0, Color32::from_gray(24));
        draw_zoomed(app, &painter, img.shrink(4.0), idx, 1.0, pos2(0.5, 0.5));
        super::mark_badges(&app.catalog.photos[idx].marks, &painter, img);
        if resp.clicked() {
            app.activate(id);
        }
        if resp.double_clicked() {
            app.activate(id);
            app.set_view(crate::app::View::Loupe);
        }
        if app.current == Some(id) {
            painter.rect_stroke(
                tile.shrink(1.0),
                4.0,
                Stroke::new(2.5, ui.visuals().selection.stroke.color),
                StrokeKind::Inside,
            );
        }
        // The X takes the photo out of the survey (outside review only: a
        // group's files stay together).
        if !reviewing {
            let x = Rect::from_min_size(img.right_top() + vec2(-26.0, 6.0), vec2(20.0, 20.0));
            let xr = ui.interact(
                x,
                ui.scope_id().with(("survey-x", id)),
                egui::Sense::click(),
            );
            painter.rect_filled(
                x,
                4.0,
                Color32::from_black_alpha(if xr.hovered() { 220 } else { 150 }),
            );
            painter.text(
                x.center(),
                egui::Align2::CENTER_CENTER,
                "✖",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            if xr.on_hover_text("Remove from survey").clicked() {
                remove = Some(id);
            }
        }

        let foot = Rect::from_min_max(pos2(tile.min.x, img.max.y), tile.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(foot.shrink2(vec2(6.0, 2.0))));
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
        if reviewing {
            super::duplicates::entry_controls(app, &mut child, &p.path);
        } else {
            rate_in_place(app, &mut child, id, &p.marks);
        }
    }
    if let Some(id) = remove {
        app.survey_remove(id);
    }
}

/// Stars and flags under a tile: clicking marks that photo.
fn rate_in_place(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    id: crate::jobs::PhotoId,
    marks: &shoebox_core::marks::Marks,
) {
    use shoebox_core::marks::Pick;
    ui.horizontal(|ui| {
        for r in 1..=5u8 {
            let on = marks.rating >= r;
            let star = RichText::new("★").color(if on {
                Color32::from_rgb(250, 210, 80)
            } else {
                Color32::from_gray(90)
            });
            if ui.add(egui::Button::new(star).frame(false)).clicked() {
                app.activate(id);
                app.set_rating(if marks.rating == r { 0 } else { r });
            }
        }
        ui.separator();
        for (pick, glyph, hint) in [
            (Pick::Pick, "✔", "Pick (P)"),
            (Pick::Reject, "✖", "Reject (X)"),
        ] {
            let on = marks.pick == Some(pick);
            if ui.selectable_label(on, glyph).on_hover_text(hint).clicked() {
                app.activate(id);
                app.set_pick(if on { None } else { Some(pick) });
            }
        }
    });
}
