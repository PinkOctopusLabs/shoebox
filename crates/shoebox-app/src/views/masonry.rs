//! Masonry: true aspect ratios in equal-width columns; each photo goes into
//! the shortest column, in date order. Layout comes from metadata, so it's
//! ready before any thumbnail decodes. Virtualised: only visible cells draw.

use eframe::egui::{self, Rect, pos2, vec2};

use super::{GAP, cell, handle_click};
use crate::app::ShoeboxApp;

#[derive(Default)]
pub struct Layout {
    key: (u64, u32, u32),
    /// Cell rects in content coordinates, by position in `shown`.
    rects: Vec<Rect>,
    height: f32,
}

impl Layout {
    fn update(
        &mut self,
        aspects: impl Iterator<Item = f32>,
        generation: u64,
        width: f32,
        target: f32,
    ) {
        let key = (generation, width.round() as u32, target.round() as u32);
        if key == self.key && !self.rects.is_empty() {
            return;
        }
        self.key = key;
        let cols = (((width + GAP) / (target + GAP)).floor() as usize).max(1);
        let col_w = (width - GAP * (cols as f32 - 1.0)) / cols as f32;
        let mut heights = vec![0.0f32; cols];
        self.rects = aspects
            .map(|a| {
                let (c, _) = heights
                    .iter()
                    .enumerate()
                    .min_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap_or((0, &0.0));
                let h = col_w / a.clamp(0.2, 5.0);
                let r =
                    Rect::from_min_size(pos2(c as f32 * (col_w + GAP), heights[c]), vec2(col_w, h));
                heights[c] += h + GAP;
                r
            })
            .collect();
        self.height = heights.into_iter().fold(0.0, f32::max);
    }
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let width = ui.available_width();
    let aspects: Vec<f32> = app
        .shown
        .iter()
        .map(|&i| app.catalog.photos[i].meta.aspect().unwrap_or(1.5))
        .collect();
    let mut layout = std::mem::take(&mut app.masonry);
    layout.update(aspects.into_iter(), app.shown_gen, width, app.thumb_size);
    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if app.scroll_to_current
        && let Some(r) = app.current_pos().and_then(|p| layout.rects.get(p))
    {
        area = area.vertical_scroll_offset((r.center().y - ui.available_height() / 2.0).max(0.0));
    }
    area.show_viewport(ui, |ui, viewport| {
        ui.set_min_size(vec2(width, layout.height));
        let origin = ui.min_rect().min.to_vec2();
        for (pos, r) in layout.rects.iter().enumerate() {
            if !r.intersects(viewport) {
                continue;
            }
            let rect = r.translate(origin);
            let resp = cell(app, ui, rect, pos, true);
            handle_click(app, ui, &resp, pos);
        }
    });
    app.masonry = layout;
}
