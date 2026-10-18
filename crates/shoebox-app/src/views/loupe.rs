//! Loupe: one image, fit to the window. Shows the 1024 px thumbnail at once,
//! then the full decode. Space/Z toggles fit and 100%; pinch or ⌘-scroll
//! zooms about the cursor; scroll or drag pans; arrows move (handled in app).
//! Each photo opens at Fit.

use eframe::egui::{self, Color32, Rect, RichText, pos2, vec2};

use crate::app::{ShoeboxApp, Zoom};
use crate::jobs::{Priority, Size};

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let Some((id, idx)) = app.current.and_then(|id| app.index(id).map(|i| (id, i))) else {
        ui.centered_and_justified(|ui| ui.weak("Select a photo"));
        return;
    };
    app.want(idx, Size::Large, Priority::Visible);
    app.want(idx, Size::Full, Priority::Visible);
    if let Some(pos) = app.current_pos() {
        for n in [pos + 1, pos.wrapping_sub(1)] {
            if let Some(&j) = app.shown.get(n) {
                app.want(j, Size::Large, Priority::Near);
            }
        }
    }

    if app.loupe.photo != Some(id) {
        app.loupe = crate::app::LoupeState {
            photo: Some(id),
            ..Default::default()
        };
    }
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
    if let Some(pos) = app.current_pos() {
        super::photo_menu(app, &resp, pos);
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(18));

    let photo = &app.catalog.photos[idx];
    let tex = app.textures.best(id, Size::Full);
    // Full-resolution size in image pixels: from metadata, else the texture.
    let full = match (photo.meta.width, photo.meta.height, &tex) {
        (Some(w), Some(h), _) => vec2(w as f32, h as f32),
        (_, _, Some(t)) => vec2(t.size()[0] as f32, t.size()[1] as f32),
        _ => vec2(3.0, 2.0),
    };
    let ppp = ui.ctx().pixels_per_point();
    let fit = (rect.width() / full.x).min(rect.height() / full.y);
    let mut scale = match app.loupe.zoom {
        Zoom::Fit => fit,
        Zoom::Scale(s) => s / ppp,
    };

    // Zoom about the cursor: pinch, or ⌘-scroll (egui turns that into a
    // zoom). A plain scroll pans a zoomed image and never zooms, so the
    // grid's scroll momentum can't zoom a photo as it opens.
    if let Some(cursor) = resp.hover_pos() {
        let (factor, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if app.loupe.zoom != Zoom::Fit {
            app.loupe.pan += scroll;
        }
        if (factor - 1.0).abs() > 1e-4 {
            let new = (scale * factor).clamp(fit.min(0.02), 8.0 / ppp);
            let c = cursor - rect.center();
            app.loupe.pan = (app.loupe.pan - c) * (new / scale) + c;
            scale = new;
            app.loupe.zoom = Zoom::Scale(scale * ppp);
        }
    }
    if resp.dragged() && app.loupe.zoom != Zoom::Fit {
        app.loupe.pan += resp.drag_delta();
    }
    if resp.double_clicked() {
        app.loupe.zoom = match app.loupe.zoom {
            Zoom::Fit => Zoom::Scale(1.0),
            Zoom::Scale(_) => Zoom::Fit,
        };
        app.loupe.pan = egui::Vec2::ZERO;
    }
    if app.loupe.zoom == Zoom::Fit {
        app.loupe.pan = egui::Vec2::ZERO;
    }
    // Keep the image from leaving the window entirely.
    let shown = full * scale;
    let slack = ((shown - rect.size()) / 2.0).max(vec2(0.0, 0.0));
    app.loupe.pan = app.loupe.pan.clamp(-slack, slack);

    let target = Rect::from_center_size(rect.center() + app.loupe.pan, shown);
    match tex {
        Some(t) => {
            painter.image(
                t.id(),
                target,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            // Faces outlined and named (the view bar's toggle), the person
            // chosen in People standing out.
            super::face_boxes(app, &painter, id, target, true);
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
                egui::FontId::proportional(14.0),
                Color32::GRAY,
            );
        }
    }
    let pct = scale * ppp * 100.0;
    let label = if app.loupe.zoom == Zoom::Fit {
        format!("Fit ({pct:.0}%)")
    } else {
        format!("{pct:.0}%")
    };
    painter.text(
        rect.right_top() + vec2(-10.0, 10.0),
        egui::Align2::RIGHT_TOP,
        label,
        egui::FontId::proportional(12.0),
        Color32::from_gray(170),
    );
    // A movie shows its poster frame; playing it is QuickTime's job.
    let photo = &app.catalog.photos[idx];
    if photo.format.is_movie() {
        let path = photo.path.clone();
        let at = rect.center_bottom() + vec2(0.0, -40.0);
        let area = egui::Area::new(ui.scope_id().with("movie"))
            .fixed_pos(at - vec2(120.0, 14.0))
            .order(egui::Order::Foreground);
        area.show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Video (poster frame)").color(Color32::from_gray(200)));
                if ui.button("▶ Open in QuickTime Player").clicked() {
                    crate::app::open_file(&path);
                }
            });
        });
    }
}
