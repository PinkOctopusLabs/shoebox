//! Develop's crop tool: a rectangle over the whole (rotated) frame, dragged
//! by its corners, edges or middle, held to an aspect if one is chosen, and
//! never allowed outside the rotated photo. Rectangles are fractions of the
//! frame, `[left, top, right, bottom]`, as the edit stores them.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, pos2};

/// The aspect choices: name (as stored) and width ÷ height. "original" is
/// the photo's own.
pub const ASPECTS: [(&str, &str); 11] = [
    ("free", "Free"),
    ("original", "Original"),
    ("1:1", "1 : 1"),
    ("3:2", "3 : 2"),
    ("2:3", "2 : 3"),
    ("4:3", "4 : 3"),
    ("3:4", "3 : 4"),
    ("16:9", "16 : 9"),
    ("9:16", "9 : 16"),
    ("5:4", "5 : 4"),
    ("4:5", "4 : 5"),
];

/// Width ÷ height in pixels for an aspect, on a `w`×`h` frame.
pub fn pixel_ratio(aspect: &str, w: f64, h: f64) -> Option<f64> {
    match aspect {
        "free" => None,
        "original" => Some(w / h),
        s => {
            let (a, b) = s.split_once(':')?;
            let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
            (a > 0.0 && b > 0.0).then(|| a / b)
        }
    }
}

/// The same ratio in frame fractions (width fraction ÷ height fraction).
fn frac_ratio(aspect: &str, w: f64, h: f64) -> Option<f64> {
    pixel_ratio(aspect, w, h).map(|r| r * h / w)
}

/// Whether `rect` lies inside the photo once it's rotated by `degrees` in a
/// `w`×`h` frame (the frame is the photo's own size).
pub fn inside(rect: [f64; 4], w: f64, h: f64, degrees: f64) -> bool {
    let a = degrees.to_radians();
    let (sin, cos) = (a.sin(), a.cos());
    let eps = 1e-6;
    [
        (rect[0], rect[1]),
        (rect[2], rect[1]),
        (rect[0], rect[3]),
        (rect[2], rect[3]),
    ]
    .iter()
    .all(|&(fx, fy)| {
        let (px, py) = ((fx - 0.5) * w, (fy - 0.5) * h);
        // Back into the photo, as the render's geometry map does.
        let u = cos * px + sin * py;
        let v = -sin * px + cos * py;
        u.abs() <= w / 2.0 + eps && v.abs() <= h / 2.0 + eps
    })
}

/// The largest rectangle of `aspect` centred in `rect` (a new aspect
/// chosen).
pub fn fit_aspect(rect: [f64; 4], aspect: &str, w: f64, h: f64) -> [f64; 4] {
    let Some(r) = frac_ratio(aspect, w, h) else {
        return rect;
    };
    let (cx, cy) = ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0);
    let (mut rw, mut rh) = (rect[2] - rect[0], rect[3] - rect[1]);
    if rw / rh > r {
        rw = rh * r;
    } else {
        rh = rw / r;
    }
    [cx - rw / 2.0, cy - rh / 2.0, cx + rw / 2.0, cy + rh / 2.0]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    Move,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// The smallest crop side, as a fraction of the frame.
const MIN: f64 = 0.02;

/// `start` dragged by `d` (frame fractions) on `handle`, held to the
/// aspect ratio `r` (frame fractions) if any.
pub fn dragged(handle: Handle, start: [f64; 4], d: (f64, f64), r: Option<f64>) -> [f64; 4] {
    let [mut l, mut t, mut rt, mut b] = start;
    match handle {
        Handle::Move => {
            let (w, h) = (rt - l, b - t);
            l = (l + d.0).clamp(0.0, 1.0 - w);
            t = (t + d.1).clamp(0.0, 1.0 - h);
            return [l, t, l + w, t + h];
        }
        Handle::Left | Handle::TopLeft | Handle::BottomLeft => l = (l + d.0).min(rt - MIN),
        Handle::Right | Handle::TopRight | Handle::BottomRight => rt = (rt + d.0).max(l + MIN),
        _ => {}
    }
    match handle {
        Handle::Top | Handle::TopLeft | Handle::TopRight => t = (t + d.1).min(b - MIN),
        Handle::Bottom | Handle::BottomLeft | Handle::BottomRight => b = (b + d.1).max(t + MIN),
        _ => {}
    }
    let Some(r) = r else {
        return [l, t, rt, b];
    };
    // Held to the aspect: corners follow the width, the opposite corner
    // fixed; edges keep the other side centred.
    let (w, h) = (rt - l, b - t);
    match handle {
        Handle::Left | Handle::Right => {
            let nh = w / r;
            let cy = (t + b) / 2.0;
            [l, cy - nh / 2.0, rt, cy + nh / 2.0]
        }
        Handle::Top | Handle::Bottom => {
            let nw = h * r;
            let cx = (l + rt) / 2.0;
            [cx - nw / 2.0, t, cx + nw / 2.0, b]
        }
        Handle::TopLeft => [l, b - w / r, rt, b],
        Handle::TopRight => [l, b - w / r, rt, b],
        Handle::BottomLeft => [l, t, rt, t + w / r],
        _ => [l, t, rt, t + w / r],
    }
}

/// A crop being drawn.
#[derive(Debug, Clone)]
pub struct CropEdit {
    pub rect: [f64; 4],
    pub aspect: String,
    drag: Option<(Handle, [f64; 4], Pos2)>,
}

impl CropEdit {
    pub fn new(rect: [f64; 4], aspect: &str) -> Self {
        Self {
            rect,
            aspect: aspect.to_owned(),
            drag: None,
        }
    }

    /// Choose an aspect: the crop becomes the largest of it that fits.
    pub fn set_aspect(&mut self, aspect: &str, w: f64, h: f64, degrees: f64) {
        self.aspect = aspect.to_owned();
        let mut r = fit_aspect(self.rect, aspect, w, h);
        // Shrink about the centre until it's inside the rotated photo.
        for _ in 0..60 {
            if inside(r, w, h, degrees) {
                break;
            }
            let (cx, cy) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
            r = [
                cx + (r[0] - cx) * 0.97,
                cy + (r[1] - cy) * 0.97,
                cx + (r[2] - cx) * 0.97,
                cy + (r[3] - cy) * 0.97,
            ];
        }
        self.rect = r;
    }

    /// Draw over `target` (where the whole frame is shown) and take drags.
    /// `w`×`h` is the frame in pixels, `degrees` the rotation.
    pub fn ui(&mut self, ui: &mut egui::Ui, target: Rect, w: f64, h: f64, degrees: f64) {
        let to_screen = |fx: f64, fy: f64| {
            pos2(
                target.left() + fx as f32 * target.width(),
                target.top() + fy as f32 * target.height(),
            )
        };
        let crop = Rect::from_min_max(
            to_screen(self.rect[0], self.rect[1]),
            to_screen(self.rect[2], self.rect[3]),
        );
        let resp = ui.interact(target, ui.scope_id().with("crop"), egui::Sense::drag());
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let grab = 14.0;
            let near = |a: f32, b: f32| (a - b).abs() <= grab;
            let (l, r, t, b) = (
                near(p.x, crop.left()),
                near(p.x, crop.right()),
                near(p.y, crop.top()),
                near(p.y, crop.bottom()),
            );
            let within_x = p.x > crop.left() - grab && p.x < crop.right() + grab;
            let within_y = p.y > crop.top() - grab && p.y < crop.bottom() + grab;
            let handle = match (l, r, t, b) {
                (true, _, true, _) => Some(Handle::TopLeft),
                (_, true, true, _) => Some(Handle::TopRight),
                (true, _, _, true) => Some(Handle::BottomLeft),
                (_, true, _, true) => Some(Handle::BottomRight),
                (true, ..) if within_y => Some(Handle::Left),
                (_, true, ..) if within_y => Some(Handle::Right),
                (_, _, true, _) if within_x => Some(Handle::Top),
                (.., true) if within_x => Some(Handle::Bottom),
                _ if crop.contains(p) => Some(Handle::Move),
                _ => None,
            };
            self.drag = handle.map(|h| (h, self.rect, p));
        }
        if let Some((handle, start, from)) = self.drag
            && let Some(p) = resp.interact_pointer_pos()
        {
            let d = (
                f64::from((p.x - from.x) / target.width()),
                f64::from((p.y - from.y) / target.height()),
            );
            let next = dragged(handle, start, d, frac_ratio(&self.aspect, w, h));
            let in_frame = next.iter().all(|v| (-1e-9..=1.0 + 1e-9).contains(v));
            // Stay inside the photo: a step that would leave it is ignored.
            if in_frame && inside(next, w, h, degrees) {
                self.rect = next;
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
        }

        // Darken outside the crop; thirds inside; handles at the corners.
        let painter = ui.painter_at(target);
        let shade = Color32::from_black_alpha(150);
        let crop = Rect::from_min_max(
            to_screen(self.rect[0], self.rect[1]),
            to_screen(self.rect[2], self.rect[3]),
        );
        for r in [
            Rect::from_min_max(target.min, pos2(target.right(), crop.top())),
            Rect::from_min_max(pos2(target.left(), crop.bottom()), target.max),
            Rect::from_min_max(
                pos2(target.left(), crop.top()),
                pos2(crop.left(), crop.bottom()),
            ),
            Rect::from_min_max(
                pos2(crop.right(), crop.top()),
                pos2(target.right(), crop.bottom()),
            ),
        ] {
            painter.rect_filled(r, 0.0, shade);
        }
        let thin = Stroke::new(1.0, Color32::from_white_alpha(110));
        for i in 1..3 {
            let f = i as f32 / 3.0;
            let x = crop.left() + crop.width() * f;
            let y = crop.top() + crop.height() * f;
            painter.line_segment([pos2(x, crop.top()), pos2(x, crop.bottom())], thin);
            painter.line_segment([pos2(crop.left(), y), pos2(crop.right(), y)], thin);
        }
        painter.rect_stroke(
            crop,
            0.0,
            Stroke::new(1.5, Color32::WHITE),
            egui::StrokeKind::Middle,
        );
        for c in [
            crop.left_top(),
            crop.right_top(),
            crop.left_bottom(),
            crop.right_bottom(),
        ] {
            painter.rect_filled(
                Rect::from_center_size(c, egui::vec2(10.0, 10.0)),
                1.0,
                Color32::WHITE,
            );
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspects() {
        assert_eq!(pixel_ratio("3:2", 6000.0, 4000.0), Some(1.5));
        assert_eq!(pixel_ratio("original", 6000.0, 4000.0), Some(1.5));
        assert_eq!(pixel_ratio("free", 6000.0, 4000.0), None);
        // 1:1 on a 3:2 frame is narrower in fractions.
        let r = fit_aspect([0.0, 0.0, 1.0, 1.0], "1:1", 6000.0, 4000.0);
        let (pw, ph) = ((r[2] - r[0]) * 6000.0, (r[3] - r[1]) * 4000.0);
        assert!(
            (pw - ph).abs() < 1e-6 && (ph - 4000.0).abs() < 1e-6,
            "{r:?}"
        );
    }

    #[test]
    fn inside_the_rotated_photo() {
        assert!(inside([0.0, 0.0, 1.0, 1.0], 600.0, 400.0, 0.0));
        assert!(!inside([0.0, 0.0, 1.0, 1.0], 600.0, 400.0, 5.0));
        let m = shoebox_develop::cpu::max_crop(600.0, 400.0, 5.0);
        assert!(inside(m, 600.0, 400.0, 5.0));
        assert!(!inside([m[0] - 0.02, m[1], m[2], m[3]], 600.0, 400.0, 5.0));
    }

    #[test]
    fn drags() {
        let s = [0.2, 0.2, 0.8, 0.8];
        let near = |a: [f64; 4], b: [f64; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9);
        assert!(near(
            dragged(Handle::Move, s, (0.1, -0.1), None),
            [0.3, 0.1, 0.9, 0.7]
        ));
        // Moving stops at the frame's edge.
        assert!((dragged(Handle::Move, s, (0.5, 0.0), None)[2] - 1.0).abs() < 1e-9);
        assert!(near(
            dragged(Handle::Left, s, (-0.1, 0.0), None),
            [0.1, 0.2, 0.8, 0.8]
        ));
        // A corner held to a ratio keeps it.
        let r = dragged(Handle::BottomRight, s, (0.1, 0.3), Some(1.0));
        assert!(((r[2] - r[0]) - (r[3] - r[1])).abs() < 1e-9, "{r:?}");
        // Never smaller than the minimum, never inside out.
        let r = dragged(Handle::Right, s, (-0.9, 0.0), None);
        assert!(r[2] > r[0]);
    }
}
