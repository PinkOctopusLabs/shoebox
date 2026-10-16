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
