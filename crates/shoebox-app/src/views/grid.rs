//! Virtualised grid: only visible rows are laid out.

use eframe::egui::{self, vec2};

use super::{GAP, cell, handle_click};
use crate::app::ShoeboxApp;

pub fn columns_for(size: f32, width: f32) -> usize {
    (((width + GAP) / (size + GAP)).floor() as usize).max(1)
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let width = ui.available_width() - ui.spacing().scroll.bar_width;
    let cols = columns_for(app.thumb_size, width);
    // Stretch cells so the columns fill the width.
    let size = ((width - GAP * (cols as f32 - 1.0)) / cols as f32).floor();
    app.grid_cols = cols;
    let n = app.shown.len();
    let rows = n.div_ceil(cols);
    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if app.scroll_to_current
        && let Some(pos) = app.current_pos()
    {
        // Bring the current row into view (roughly centred).
        let row = (pos / cols) as f32;
        let y = (row * (size + GAP) - ui.available_height() / 2.0 + size / 2.0).max(0.0);
        area = area.vertical_scroll_offset(y);
    }
    ui.spacing_mut().item_spacing = vec2(GAP, GAP);
    area.show_rows(ui, size, rows, |ui, range| {
        for row in range {
            ui.horizontal(|ui| {
                for c in 0..cols {
                    let pos = row * cols + c;
                    if pos >= n {
                        break;
                    }
                    let (rect, _) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
                    let resp = cell(app, ui, rect, pos, false);
                    handle_click(app, ui, &resp, pos);
                }
            });
        }
    });
}
