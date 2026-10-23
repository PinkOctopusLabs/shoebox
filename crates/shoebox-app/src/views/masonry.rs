//! Masonry: true aspect ratios in equal-width columns; each photo goes into
//! the shortest column, in date order. Or (a setting) justified rows: equal
//! height within a row, rows filling the width, strict left-to-right order.
//! Layout comes from metadata, so it's ready before any thumbnail decodes.
//! Virtualised: only visible cells draw.

use eframe::egui::{self, Rect, pos2, vec2};

use super::{GAP, cell, handle_click};
use crate::app::ShoeboxApp;

#[derive(Default)]
pub struct Layout {
    key: (u64, u32, u32, bool),
    /// Cell rects in content coordinates, by position in `shown`.
    rects: Vec<Rect>,
    height: f32,
    /// The first photo in view last frame.
    anchor: Option<super::Anchor>,
}

impl Layout {
    fn update(
        &mut self,
        aspects: impl Iterator<Item = f32>,
        generation: u64,
        width: f32,
        target: f32,
        justified: bool,
    ) {
        let key = (
            generation,
            width.round() as u32,
            target.round() as u32,
            justified,
        );
        if key == self.key && !self.rects.is_empty() {
            return;
        }
        self.key = key;
        if justified {
            (self.rects, self.height) = justify(aspects, width, target);
            return;
        }
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

/// Justified rows: photos at `target` height fill a row until it's wider
/// than `width`, then the row is scaled to fit exactly. The last row keeps
/// the target height.
fn justify(aspects: impl Iterator<Item = f32>, width: f32, target: f32) -> (Vec<Rect>, f32) {
    let aspects: Vec<f32> = aspects.map(|a| a.clamp(0.2, 5.0)).collect();
    let mut rects = Vec::with_capacity(aspects.len());
    let mut y = 0.0;
    let mut start = 0;
    while start < aspects.len() {
        let mut end = start;
        let mut sum = 0.0;
        while end < aspects.len() {
            sum += aspects[end];
            end += 1;
            let gaps = GAP * (end - start - 1) as f32;
            if sum * target + gaps >= width {
                break;
            }
        }
        let gaps = GAP * (end - start - 1) as f32;
        let full = sum * target + gaps >= width;
        // Scale a full row to the width (never taller than 1.5× target, for
        // a lone panorama the row is just wide).
        let h = if full {
            ((width - gaps) / sum).min(target * 1.5)
        } else {
            target
        };
        let mut x = 0.0;
        for &a in &aspects[start..end] {
            let w = a * h;
            rects.push(Rect::from_min_size(pos2(x, y), vec2(w, h)));
            x += w + GAP;
        }
        y += h + GAP;
        start = end;
    }
    (rects, y)
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let width = ui.available_width();
    let aspects: Vec<f32> = app
        .shown
        .iter()
        .map(|t| app.catalog.photos[t.idx].meta.aspect().unwrap_or(1.5))
        .collect();
    let mut layout = std::mem::take(&mut app.masonry);
    layout.update(
        aspects.into_iter(),
        app.shown_gen,
        width,
        app.thumb_size,
        app.settings.justified_rows,
    );
    let jump = app
        .scroll_to_current
        .then(|| app.current_pos().and_then(|p| layout.rects.get(p)))
        .flatten()
        .map(|r| (r.center().y - ui.available_height() / 2.0).max(0.0))
        .or_else(|| app.latest_jump(layout.height, ui.available_height()))
        .or_else(|| {
            super::Anchor::keep(layout.anchor, app.shown_gen, |pos| {
                layout.rects.get(pos).map(|r| r.min.y)
            })
        });
    let area = super::photo_scroll(ui, jump);
    let generation = app.shown_gen;
    let mut anchor = None;
    area.show_viewport(ui, |ui, viewport| {
        ui.set_min_size(vec2(width, layout.height));
        let origin = ui.min_rect().min.to_vec2();
        for (pos, r) in layout.rects.iter().enumerate() {
            if !r.intersects(viewport) {
                continue;
            }
            if anchor.is_none() {
                anchor = Some(super::Anchor::at(generation, pos, r.min.y, viewport));
            }
            let rect = r.translate(origin);
            let resp = cell(app, ui, rect, pos, true);
            handle_click(app, ui, &resp, pos);
        }
    });
    layout.anchor = anchor;
    app.masonry = layout;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn justified_rows_fill_the_width() {
        // Width 1000, target 200: three 1.5 photos (300 px each + gaps)
        // don't fill a row, four do.
        let (r, _) = justify([1.5, 1.5, 1.5, 1.5, 1.5, 0.75].into_iter(), 1000.0, 200.0);
        let row1 = &r[..4];
        assert!(row1.iter().all(|x| (x.top() - 0.0).abs() < 1e-3));
        let right = row1[3].right();
        assert!((right - 1000.0).abs() < 0.5, "fills the width: {right}");
        let h = row1[0].height();
        assert!(h < 200.0 && row1.iter().all(|x| (x.height() - h).abs() < 1e-3));
        // The last row isn't stretched.
        assert!((r[4].height() - 200.0).abs() < 1e-3);
        assert!(r[4].top() > row1[0].bottom());
        assert!(r[5].left() > r[4].right(), "strict left-to-right order");
    }
}
