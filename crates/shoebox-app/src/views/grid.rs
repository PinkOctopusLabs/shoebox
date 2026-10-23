//! Virtualised grid: only visible rows are laid out. With day headers on
//! (and sorted by capture time), each day starts a new row under a heading.

use eframe::egui::{self, Rect, pos2, vec2};

use super::{GAP, cell, handle_click};
use crate::app::ShoeboxApp;

pub fn columns_for(size: f32, width: f32) -> usize {
    (((width + GAP) / (size + GAP)).floor() as usize).max(1)
}

const HEADER: f32 = 30.0;

#[derive(Debug, Clone, PartialEq)]
enum Line {
    /// A day heading.
    Header(String),
    /// Positions `start..end` of `shown`.
    Row(usize, usize),
}

#[derive(Default)]
pub struct Layout {
    key: (u64, u32, u32, bool),
    /// Each line and its top, in content coordinates.
    lines: Vec<(f32, Line)>,
    height: f32,
    cell: f32,
    /// Rows as (start, end) positions, for Up and Down.
    pub rows: Vec<(usize, usize)>,
}

impl Layout {
    /// The line holding position `pos`.
    fn line_of(&self, pos: usize) -> Option<&(f32, Line)> {
        self.lines
            .iter()
            .find(|(_, l)| matches!(l, Line::Row(s, e) if (*s..*e).contains(&pos)))
    }
}

/// "Saturday 25 April 2026" from an EXIF date ("2026:04:25 …").
fn day_heading(day: &str) -> String {
    let parse = || -> Option<jiff::civil::Date> {
        let y = day.get(0..4)?.parse().ok()?;
        let m = day.get(5..7)?.parse().ok()?;
        let d = day.get(8..10)?.parse().ok()?;
        jiff::civil::Date::new(y, m, d).ok()
    };
    match parse() {
        Some(d) => d.strftime("%A %-d %B %Y").to_string(),
        None => "Undated".to_owned(),
    }
}

/// Lay out `n` cells in `cols` columns, starting a new row (under a
/// heading) wherever `day` changes, if `days` is given.
fn lines(n: usize, cols: usize, cell: f32, days: Option<&[String]>) -> (Vec<(f32, Line)>, f32) {
    let mut out = Vec::new();
    let mut y = 0.0;
    let mut start = 0;
    while start < n {
        let mut end = (start + cols).min(n);
        if let Some(days) = days {
            if start == 0 || days[start] != days[start - 1] {
                let count = days[start..]
                    .iter()
                    .take_while(|d| **d == days[start])
                    .count();
                let text = format!(
                    "{}  ·  {}",
                    day_heading(&days[start]),
                    crate::views::duplicates::plural(count, "photo")
                );
                out.push((y, Line::Header(text)));
                y += HEADER;
            }
            // A row never runs into the next day.
            if let Some(k) = (start + 1..end).find(|&k| days[k] != days[start]) {
                end = k;
            }
        }
        out.push((y, Line::Row(start, end)));
        y += cell + GAP;
        start = end;
    }
    (out, y)
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    let width = ui.available_width() - ui.spacing().scroll.bar_width;
    let cols = columns_for(app.thumb_size, width);
    // Stretch cells so the columns fill the width.
    let size = ((width - GAP * (cols as f32 - 1.0)) / cols as f32).floor();
    app.grid_cols = cols;
    let headers = app.day_headers_shown();
    let mut layout = std::mem::take(&mut app.grid);
    let key = (app.shown_gen, width.round() as u32, size as u32, headers);
    if layout.key != key || layout.lines.is_empty() {
        let days: Option<Vec<String>> = headers.then(|| {
            app.shown
                .iter()
                .map(|t| app.capture_day(t.idx).unwrap_or_default())
                .collect()
        });
        let (lines, height) = lines(app.shown.len(), cols, size, days.as_deref());
        layout.rows = lines
            .iter()
            .filter_map(|(_, l)| match l {
                Line::Row(s, e) => Some((*s, *e)),
                Line::Header(_) => None,
            })
            .collect();
        layout = Layout {
            key,
            lines,
            height,
            cell: size,
            rows: layout.rows,
        };
    }
    // Bring the current row into view (roughly centred).
    let jump = app
        .scroll_to_current
        .then(|| app.current_pos().and_then(|p| layout.line_of(p)))
        .flatten()
        .map(|(y, _)| (y - ui.available_height() / 2.0 + layout.cell / 2.0).max(0.0))
        .or_else(|| app.latest_jump(layout.height, ui.available_height()));
    let area = super::photo_scroll(ui, jump);
    area.show_viewport(ui, |ui, viewport| {
        ui.set_min_size(vec2(width, layout.height));
        let origin = ui.min_rect().min.to_vec2();
        // Lines are in order: skip to the first one in view.
        let first = layout
            .lines
            .partition_point(|(y, _)| *y + layout.cell + GAP < viewport.min.y);
        for (y, line) in &layout.lines[first..] {
            if *y > viewport.max.y {
                break;
            }
            match line {
                Line::Header(text) => {
                    let rect =
                        Rect::from_min_size(pos2(0.0, *y), vec2(width, HEADER)).translate(origin);
                    ui.painter().text(
                        pos2(rect.left() + 4.0, rect.center().y + 2.0),
                        egui::Align2::LEFT_CENTER,
                        text,
                        egui::FontId::proportional(14.0),
                        ui.visuals().strong_text_color(),
                    );
                }
                Line::Row(start, end) => {
                    for pos in *start..*end {
                        let x = (pos - start) as f32 * (layout.cell + GAP);
                        let rect = Rect::from_min_size(pos2(x, *y), vec2(layout.cell, layout.cell))
                            .translate(origin);
                        let resp = cell(app, ui, rect, pos, false);
                        handle_click(app, ui, &resp, pos);
                    }
                }
            }
        }
    });
    app.grid = layout;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_break_at_days() {
        let d = |s: &str| s.to_owned();
        let days = [
            d("2026:04:25 10:00"),
            d("2026:04:25 11:00"),
            d("2026:04:25 12:00"),
            d("2026:04:26 09:00"),
            d(""),
        ];
        let days: Vec<String> = days
            .iter()
            .map(|s| s.get(..10).unwrap_or("").to_owned())
            .collect();
        let (l, _) = lines(5, 2, 100.0, Some(&days));
        let kinds: Vec<Line> = l.into_iter().map(|(_, l)| l).collect();
        assert_eq!(
            kinds,
            [
                Line::Header("Saturday 25 April 2026  ·  3 photos".into()),
                Line::Row(0, 2),
                Line::Row(2, 3),
                Line::Header("Sunday 26 April 2026  ·  1 photo".into()),
                Line::Row(3, 4),
                Line::Header("Undated  ·  1 photo".into()),
                Line::Row(4, 5),
            ]
        );
        let (plain, h) = lines(5, 2, 100.0, None);
        assert_eq!(plain.len(), 3);
        assert_eq!(h, 3.0 * (100.0 + GAP));
    }
}
