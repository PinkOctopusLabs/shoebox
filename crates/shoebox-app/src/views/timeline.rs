//! Timeline (T): stacks grouped by month and day, under a heat strip of
//! shooting days. Click a day (in the strip or its header) to jump to it in
//! Grid; click a thumbnail to select it, double-click to open the loupe.
//! Follows the grid's filters, folder, event, collection and sort order.
//! Virtualised: only visible rows are laid out.

use std::collections::{BTreeMap, HashMap};

use eframe::egui::{self, Color32, Rect, RichText, Sense, pos2, vec2};
use shoebox_core::timeline::{Date, MONTHS, Timeline, WEEKDAYS, heat_levels};

use super::{GAP, cell, handle_click};
use crate::app::{ShoeboxApp, View};
use crate::views::duplicates::plural;

const MONTH_H: f32 = 40.0;
const CELL: f32 = 11.0;
const CELL_GAP: f32 = 2.0;

/// A value per stack index (a Vec, not a HashMap: built over every stack
/// whenever the view changes).
struct ByStack(Vec<usize>);

impl ByStack {
    const NONE: usize = usize::MAX;

    fn new(stacks: usize) -> Self {
        Self(vec![Self::NONE; stacks])
    }

    fn insert(&mut self, s: usize, v: usize) {
        if let Some(slot) = self.0.get_mut(s) {
            *slot = v;
        }
    }

    fn get(&self, s: &usize) -> Option<&usize> {
        self.0.get(*s).filter(|&&v| v != Self::NONE)
    }
}

/// Built from `shown`; rebuilt when it changes.
pub struct Layout {
    shown_gen: u64,
    pub timeline: Timeline,
    /// Position in `shown` of each stack's primary.
    pos_of_stack: ByStack,
    /// Day index of each stack.
    day_of_stack: ByStack,
    /// Event names per day index.
    events: HashMap<usize, Vec<String>>,
    counts: BTreeMap<Date, usize>,
}

/// The view's state.
#[derive(Default)]
pub struct State {
    pub layout: Option<Layout>,
    /// The heat strip's year (None: the current photo's, else the latest).
    pub year: Option<i32>,
}

#[derive(Clone, Copy)]
enum Row {
    Month(i32, u8),
    Day(usize),
    Undated,
}

fn build(app: &ShoeboxApp) -> Layout {
    let mut stacks = Vec::new();
    let mut pos_of_stack = ByStack::new(app.catalog.stacks.len());
    for (pos, &i) in app.shown.iter().enumerate() {
        let s = app.catalog.stack_of[i];
        if app.catalog.stacks[s].primary == i {
            stacks.push(s);
            pos_of_stack.insert(s, pos);
        }
    }
    let timeline = Timeline::build(&app.catalog, &stacks);
    let mut day_of_stack = ByStack::new(app.catalog.stacks.len());
    for (k, d) in timeline.days.iter().enumerate() {
        for &s in &d.stacks {
            day_of_stack.insert(s, k);
        }
    }
    let mut events: HashMap<usize, Vec<String>> = HashMap::new();
    for e in &app.events {
        let Some(members) = app.event_members.get(&e.id) else {
            continue;
        };
        let mut days: Vec<usize> = members
            .iter()
            .filter_map(|s| day_of_stack.get(s).copied())
            .collect();
        days.sort_unstable();
        days.dedup();
        for d in days {
            events.entry(d).or_default().push(e.name.clone());
        }
    }
    let counts = timeline.counts();
    Layout {
        shown_gen: app.shown_gen,
        timeline,
        pos_of_stack,
        day_of_stack,
        events,
        counts,
    }
}

/// Jump to a day in Grid: its first stack becomes the current photo.
fn jump_to(app: &mut ShoeboxApp, pos: usize) {
    if pos < app.shown.len() {
        app.click(pos, egui::Modifiers::NONE);
        app.set_view(View::Grid);
    }
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if app
        .timeline
        .layout
        .as_ref()
        .is_none_or(|l| l.shown_gen != app.shown_gen)
    {
        app.timeline.layout = Some(build(app));
    }
    let Some(layout) = app.timeline.layout.take() else {
        return;
    };
    heat_strip(app, ui, &layout);
    ui.separator();
    body(app, ui, &layout);
    app.timeline.layout = Some(layout);
}

/// The year's shooting days: weeks across, weekdays down.
fn heat_strip(app: &mut ShoeboxApp, ui: &mut egui::Ui, layout: &Layout) {
    let years = layout.timeline.years();
    let Some(&latest) = years.last() else {
        ui.weak("No dated photos in this view.");
        return;
    };
    let current_year = app
        .current
        .and_then(|c| app.index(c))
        .map(|i| app.catalog.stack_of[i])
        .and_then(|s| layout.day_of_stack.get(&s))
        .map(|&d| layout.timeline.days[d].date.y);
    let mut year = app.timeline.year.or(current_year).unwrap_or(latest);
    if !years.contains(&year) {
        year = latest;
    }
    ui.horizontal(|ui| {
        let i = years.iter().position(|&y| y == year).unwrap_or(0);
        if ui
            .add_enabled(i > 0, egui::Button::new("◀").small())
            .clicked()
        {
            app.timeline.year = Some(years[i - 1]);
        }
        ui.label(RichText::new(year.to_string()).strong());
        if ui
            .add_enabled(i + 1 < years.len(), egui::Button::new("▶").small())
            .clicked()
        {
            app.timeline.year = Some(years[i + 1]);
        }
        let in_year: Vec<(&Date, &usize)> = layout
            .counts
            .range(
                Date {
                    y: year,
                    m: 1,
                    d: 1,
                }..=Date {
                    y: year,
                    m: 12,
                    d: 31,
                },
            )
            .collect();
        let photos: usize = in_year.iter().map(|(_, n)| **n).sum();
        ui.weak(format!(
            "{} · {}",
            plural(in_year.len(), "shooting day"),
            plural(photos, "photo")
        ));
    });

    let level = heat_levels(&layout.counts);
    let accent = ui.visuals().selection.stroke.color;
    let empty = ui.visuals().faint_bg_color;
    let bg = ui.visuals().extreme_bg_color;
    let shade = |l: u8| match l {
        0 => empty,
        l => {
            let f = [0.0, 0.3, 0.5, 0.75, 1.0][usize::from(l)];

            Color32::from_rgb(
                (f32::from(bg.r()) + (f32::from(accent.r()) - f32::from(bg.r())) * f) as u8,
                (f32::from(bg.g()) + (f32::from(accent.g()) - f32::from(bg.g())) * f) as u8,
                (f32::from(bg.b()) + (f32::from(accent.b()) - f32::from(bg.b())) * f) as u8,
            )
        }
    };
    let jan1 = Date {
        y: year,
        m: 1,
        d: 1,
    };
    let first = jan1.days() - jan1.weekday() as i64; // the Monday on or before 1 Jan
    let dec31 = Date {
        y: year,
        m: 12,
        d: 31,
    }
    .days();
    let weeks = ((dec31 - first) / 7 + 1) as usize;
    let step = CELL + CELL_GAP;
    let label_w = 28.0;
    let (rect, resp) = ui.allocate_exact_size(
        vec2(label_w + weeks as f32 * step, 14.0 + 7.0 * step + 16.0),
        Sense::click(),
    );
    let painter = ui.painter_at(rect);
    let origin = rect.min + vec2(label_w, 14.0);
    for (r, name) in WEEKDAYS.iter().enumerate().filter(|(r, _)| r % 2 == 0) {
        painter.text(
            pos2(rect.min.x, origin.y + r as f32 * step + CELL / 2.0),
            egui::Align2::LEFT_CENTER,
            *name,
            egui::FontId::proportional(9.0),
            ui.visuals().weak_text_color(),
        );
    }
    let day_rect = |n: i64| {
        let k = n - first;
        let (w, wd) = ((k / 7) as f32, (k % 7) as f32);
        Rect::from_min_size(origin + vec2(w * step, wd * step), vec2(CELL, CELL))
    };
    for n in jan1.days()..=dec31 {
        let date = Date::from_days(n);
        if date.d == 1 {
            // Above the grid, at the week the month starts in.
            let x = day_rect(n).left();
            painter.text(
                pos2(x, origin.y - 3.0),
                egui::Align2::LEFT_BOTTOM,
                &MONTHS[usize::from(date.m) - 1][..3],
                egui::FontId::proportional(9.0),
                ui.visuals().weak_text_color(),
            );
        }
        let count = layout.counts.get(&date).copied().unwrap_or(0);
        painter.rect_filled(day_rect(n), 2.0, shade(level(count)));
    }
    // Hover and click: which day is under the pointer?
    let at = |p: egui::Pos2| -> Option<Date> {
        let rel = p - origin;
        if rel.x < 0.0 || rel.y < 0.0 {
            return None;
        }
        let (w, wd) = ((rel.x / step) as i64, (rel.y / step) as i64);
        if wd > 6 {
            return None;
        }
        let n = first + w * 7 + wd;
        let date = Date::from_days(n);
        (date.y == year && day_rect(n).contains(p)).then_some(date)
    };
    if let Some(date) = resp.hover_pos().and_then(at) {
        let count = layout.counts.get(&date).copied().unwrap_or(0);
        painter.rect_stroke(
            day_rect(date.days()).expand(1.0),
            2.0,
            egui::Stroke::new(1.0, ui.visuals().strong_text_color()),
            egui::StrokeKind::Outside,
        );
        let text = if count == 0 {
            date.long()
        } else {
            format!("{} · {}", date.long(), plural(count, "photo"))
        };
        resp.clone().on_hover_text_at_pointer(text);
    }
    if resp.clicked()
        && let Some(date) = resp.interact_pointer_pos().and_then(at)
        && let Some(day) = layout.timeline.days.iter().find(|d| d.date == date)
        && let Some(&pos) = day.stacks.first().and_then(|s| layout.pos_of_stack.get(s))
    {
        jump_to(app, pos);
    }
    // A small legend.
    let legend = pos2(rect.max.x - 5.0 * step - 70.0, origin.y + 7.0 * step + 4.0);
    painter.text(
        legend,
        egui::Align2::LEFT_TOP,
        "Less",
        egui::FontId::proportional(9.0),
        ui.visuals().weak_text_color(),
    );
    for l in 0..5u8 {
        painter.rect_filled(
            Rect::from_min_size(
                legend + vec2(24.0 + f32::from(l) * step, 0.0),
                vec2(CELL - 2.0, CELL - 2.0),
            ),
            2.0,
            shade(l),
        );
    }
    painter.text(
        legend + vec2(26.0 + 5.0 * step, 0.0),
        egui::Align2::LEFT_TOP,
        "More",
        egui::FontId::proportional(9.0),
        ui.visuals().weak_text_color(),
    );
}

/// Month headers and day rows, virtualised.
fn body(app: &mut ShoeboxApp, ui: &mut egui::Ui, layout: &Layout) {
    let thumb = (app.thumb_size * 0.6).clamp(72.0, 180.0);
    let day_h = thumb + 34.0;
    let mut rows = Vec::new();
    let mut month = None;
    for (k, d) in layout.timeline.days.iter().enumerate() {
        if month != Some((d.date.y, d.date.m)) {
            month = Some((d.date.y, d.date.m));
            rows.push(Row::Month(d.date.y, d.date.m));
        }
        rows.push(Row::Day(k));
    }
    if !layout.timeline.undated.is_empty() {
        rows.push(Row::Undated);
    }
    let height = |r: &Row| match r {
        Row::Month(..) => MONTH_H,
        Row::Day(_) | Row::Undated => day_h,
    };
    let mut tops = Vec::with_capacity(rows.len() + 1);
    let mut y = 0.0;
    for r in &rows {
        tops.push(y);
        y += height(r);
    }
    let total = y;

    // Month totals for the headers.
    let mut month_totals: HashMap<(i32, u8), (usize, usize)> = HashMap::new();
    for d in &layout.timeline.days {
        let e = month_totals.entry((d.date.y, d.date.m)).or_default();
        e.0 += d.stacks.len();
        e.1 += 1;
    }

    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if app.scroll_to_current
        && let Some(s) = app
            .current
            .and_then(|c| app.index(c))
            .map(|i| app.catalog.stack_of[i])
    {
        let row = match layout.day_of_stack.get(&s) {
            Some(&d) => rows
                .iter()
                .position(|r| matches!(r, Row::Day(k) if *k == d)),
            None => rows.iter().position(|r| matches!(r, Row::Undated)),
        };
        if let Some(r) = row {
            area = area.vertical_scroll_offset((tops[r] - MONTH_H).max(0.0));
        }
    }
    area.show_viewport(ui, |ui, viewport| {
        ui.set_min_height(total);
        let origin = ui.min_rect().min;
        let width = ui.available_width();
        let first = tops.partition_point(|&t| t + day_h < viewport.min.y);
        for (k, row) in rows.iter().enumerate().skip(first) {
            let top = tops[k];
            if top > viewport.max.y {
                break;
            }
            let rect = Rect::from_min_size(origin + vec2(0.0, top), vec2(width, height(row)));
            match *row {
                Row::Month(y, m) => {
                    let (n, days) = month_totals.get(&(y, m)).copied().unwrap_or_default();
                    let painter = ui.painter_at(rect);
                    painter.text(
                        rect.left_bottom() + vec2(0.0, -8.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("{} {y}", MONTHS[usize::from(m) - 1]),
                        egui::FontId::proportional(18.0),
                        ui.visuals().strong_text_color(),
                    );
                    painter.text(
                        rect.right_bottom() + vec2(-4.0, -9.0),
                        egui::Align2::RIGHT_BOTTOM,
                        format!("{} · {}", plural(n, "photo"), plural(days, "day")),
                        egui::FontId::proportional(12.0),
                        ui.visuals().weak_text_color(),
                    );
                }
                Row::Day(d) => {
                    let day = &layout.timeline.days[d];
                    let title = day.date.long();
                    let events = layout.events.get(&d).cloned().unwrap_or_default();
                    day_row(app, ui, rect, thumb, &title, &day.stacks, &events, layout);
                }
                Row::Undated => {
                    let stacks = layout.timeline.undated.clone();
                    day_row(app, ui, rect, thumb, "Undated", &stacks, &[], layout);
                }
            }
        }
    });
}

/// "Fri 1 May 2026 · 12 photos [Japan Trip]" and a strip of thumbnails.
#[allow(clippy::too_many_arguments)]
fn day_row(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    rect: Rect,
    thumb: f32,
    title: &str,
    stacks: &[usize],
    events: &[String],
    layout: &Layout,
) {
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min, vec2(rect.width(), 26.0))),
    );
    child.horizontal(|ui| {
        let first = stacks
            .first()
            .and_then(|s| layout.pos_of_stack.get(s))
            .copied();
        let r = ui
            .add(egui::Button::new(RichText::new(title).strong()).frame(false))
            .on_hover_text("Show this day in Grid");
        if r.clicked()
            && let Some(pos) = first
        {
            jump_to(app, pos);
        }
        ui.weak(plural(stacks.len(), "photo"));
        for e in events {
            ui.label(
                RichText::new(format!(" {e} "))
                    .small()
                    .background_color(ui.visuals().selection.bg_fill.gamma_multiply(0.5)),
            );
        }
    });
    let strip = Rect::from_min_size(rect.min + vec2(0.0, 28.0), vec2(rect.width(), thumb));
    let fit = (((strip.width() + GAP) / (thumb + GAP)).floor() as usize).max(1);
    let more = stacks.len() > fit;
    let n = if more { fit - 1 } else { stacks.len() };
    for (k, s) in stacks.iter().take(n).enumerate() {
        let Some(&pos) = layout.pos_of_stack.get(s) else {
            continue;
        };
        let r = Rect::from_min_size(
            strip.min + vec2(k as f32 * (thumb + GAP), 0.0),
            vec2(thumb, thumb),
        );
        let resp = cell(app, ui, r, pos, false);
        handle_click(app, ui, &resp, pos);
    }
    if more {
        let r = Rect::from_min_size(
            strip.min + vec2(n as f32 * (thumb + GAP), 0.0),
            vec2(thumb, thumb),
        );
        let resp = ui.interact(r, ui.scope_id().with(("more", stacks[0])), Sense::click());
        let painter = ui.painter_at(r);
        painter.rect_filled(r, 3.0, ui.visuals().faint_bg_color);
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            format!("+{}", stacks.len() - n),
            egui::FontId::proportional(18.0),
            ui.visuals().text_color(),
        );
        if resp.on_hover_text("Show this day in Grid").clicked()
            && let Some(&pos) = stacks.first().and_then(|s| layout.pos_of_stack.get(s))
        {
            jump_to(app, pos);
        }
    }
}
