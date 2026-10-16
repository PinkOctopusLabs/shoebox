//! Centre views.

pub mod filmstrip;
pub mod grid;
pub mod loupe;
pub mod masonry;

use eframe::egui::{self, Color32, Rect, Stroke, StrokeKind, pos2, vec2};

use crate::app::ShoeboxApp;
use crate::jobs::{Priority, Size};

pub const GAP: f32 = 6.0;

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
    let resp = ui.interact(rect, ui.scope_id().with(("cell", id)), egui::Sense::click());
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
    // File-type badge, bottom left.
    let label = photo.format.label();
    let font = egui::FontId::monospace(9.5);
    let galley = painter.layout_no_wrap(label.to_owned(), font, Color32::WHITE);
    let badge = Rect::from_min_size(
        pos2(inner.left() + 3.0, inner.bottom() - galley.size().y - 5.0),
        galley.size() + vec2(6.0, 2.0),
    );
    painter.rect_filled(badge, 2.0, Color32::from_black_alpha(150));
    painter.galley(badge.min + vec2(3.0, 1.0), galley, Color32::WHITE);

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
    if resp.hovered() {
        resp.clone().on_hover_text(photo.rel.clone());
    }
    resp
}

/// Handle a cell's clicks: select, and double-click opens the loupe.
pub fn handle_click(app: &mut ShoeboxApp, ui: &egui::Ui, resp: &egui::Response, pos: usize) {
    if resp.double_clicked() {
        app.click(pos, egui::Modifiers::NONE);
        app.set_view(crate::app::View::Loupe);
    } else if resp.clicked() {
        app.click(pos, ui.input(|i| i.modifiers));
    }
}
