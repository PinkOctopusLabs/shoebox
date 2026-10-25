//! Loupe: one image, fit to the window. Shows the 1024 px thumbnail at once,
//! then the full decode. Space/Z toggles fit and 100%; pinch or ⌘-scroll
//! zooms about the cursor; scroll or drag pans; arrows move (handled in app).
//! Each photo opens at Fit. \ shows an edited photo as it was before. A
//! movie plays here (`movie`).

use eframe::egui::{self, Color32, Rect, pos2, vec2};

use crate::app::{ShoeboxApp, Zoom};
use crate::jobs::{Priority, Size};

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let Some((id, idx)) = app.current.and_then(|id| app.index(id).map(|i| (id, i))) else {
        ui.centered_and_justified(|ui| ui.weak("Select a photo"));
        return;
    };
    // A photo in the cloud only is downloaded as it opens.
    app.want_open(id, Size::Large);
    app.want_open(id, Size::Full);
    if let Some(pos) = app.current_pos() {
        for n in [pos + 1, pos.wrapping_sub(1)] {
            if let Some(t) = app.shown.get(n).copied() {
                app.want(t.id, Size::Large, Priority::Near);
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

    let original = app.loupe.original && app.is_edited(id);
    if original {
        app.want_original(id, Size::Large);
        app.want_original(id, Size::Full);
    }
    let photo = &app.catalog.photos[idx];
    let before = original
        .then(|| app.textures.best(crate::jobs::original_id(id), Size::Full))
        .flatten();
    app.loupe.drawn = None;
    let tex = match before {
        Some(t) => {
            app.loupe.drawn = Some((true, t.size()));
            Some(t)
        }
        // The edit stays until the original arrives.
        None => app.textures.best(id, Size::Full).inspect(|t| {
            app.loupe.drawn = Some((false, t.size()));
        }),
    };
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
    let (cloud, is_movie) = (photo.cloud, photo.format.is_movie());
    // A movie's frames once it plays, over its poster until then.
    let playing =
        is_movie && super::movie::draw(app.loupe.movie.as_deref_mut(), ui, &painter, target);
    match tex {
        _ if playing => {}
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
                .map_or(
                    if cloud {
                        "Downloading from the cloud…".to_owned()
                    } else {
                        "Loading…".to_owned()
                    },
                    |(_, e)| format!("Can't display: {e}"),
                );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(14.0),
                Color32::GRAY,
            );
        }
    }
    if original {
        let failed = [Size::Large, Size::Full].iter().any(|&size| {
            app.textures.failed.contains_key(&crate::jobs::Key {
                photo: crate::jobs::original_id(id),
                size,
            })
        });
        let text = if failed {
            "Original can't be shown: showing the edit"
        } else {
            "Original"
        };
        super::develop::chip(&painter, rect.left_top() + vec2(10.0, 10.0), text);
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
    if is_movie {
        super::movie::play_button(app, ui, rect);
        super::movie::controls(app, ui, rect);
    }
}
