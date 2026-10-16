//! Filmstrip under Loupe, Compare and Survey: the current image is
//! highlighted; click to jump. Virtualised horizontally.

use eframe::egui::{self, Rect, pos2, vec2};

use super::{GAP, cell};
use crate::app::ShoeboxApp;

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let h = ui.available_height() - 4.0;
    let step = h + GAP;
    let n = app.shown.len();
    let mut area = egui::ScrollArea::horizontal().auto_shrink([false, false]);
    if app.scroll_to_current
        && let Some(pos) = app.current_pos()
    {
        area = area.horizontal_scroll_offset(
            (pos as f32 * step - ui.available_width() / 2.0 + h / 2.0).max(0.0),
        );
    }
    area.show_viewport(ui, |ui, viewport| {
        ui.set_min_size(vec2(n as f32 * step, h));
        let origin = ui.min_rect().min.to_vec2();
        let first = (viewport.min.x / step).floor().max(0.0) as usize;
        let last = ((viewport.max.x / step).ceil() as usize).min(n);
        for pos in first..last {
            let r = Rect::from_min_size(pos2(pos as f32 * step, 2.0), vec2(h, h)).translate(origin);
            let resp = cell(app, ui, r, pos, false);
            if resp.clicked() {
                app.click(pos, egui::Modifiers::NONE);
            }
            super::photo_menu(app, &resp, pos);
        }
    });
}
