//! Map: photos with a GPS position as thumbnail markers on OpenStreetMap
//! tiles (downloaded as needed, cached on disk). Nearby photos group into
//! one marker with a count. Click a marker to select its photos;
//! double-click zooms in on a group, or opens a single photo in the loupe.
//! Follows the grid's filters, folder, event and collection.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use walkers::{HttpOptions, HttpTiles, MapMemory, Position, lat_lon, sources::OpenStreetMap};

use crate::app::{ShoeboxApp, View};
use crate::jobs::{PhotoId, Priority, Size};
use crate::views::duplicates::plural;

/// Markers closer than this (points) group together.
const CELL: f32 = 60.0;
const MARKER: f32 = 48.0;

/// A photo on the map: its position in `shown` and coordinates.
#[derive(Clone, Copy)]
struct Point {
    pos: usize,
    lat: f64,
    lon: f64,
}

pub struct State {
    /// None in tests (no network) or until the map is first shown.
    tiles: Option<HttpTiles>,
    /// Where tiles are cached; None = no tiles at all.
    pub tile_cache: Option<PathBuf>,
    memory: MapMemory,
    points: Vec<Point>,
    /// `shown_gen` the points were built for.
    built: Option<u64>,
    /// Fit the view to the points on the next frame.
    fit: bool,
    /// Centre on this photo on the next frame ("Show on Map").
    focus: Option<PhotoId>,
}

impl State {
    pub fn new(tile_cache: Option<PathBuf>) -> Self {
        Self {
            tiles: None,
            tile_cache,
            memory: MapMemory::default(),
            points: Vec::new(),
            built: None,
            fit: true,
            focus: None,
        }
    }

    /// Photos on the map (tests).
    pub fn located(&self) -> usize {
        self.points.len()
    }
}

/// Web Mercator: (0..1, 0..1) across the world.
fn mercator(lat: f64, lon: f64) -> (f64, f64) {
    let lat = lat.clamp(-85.0, 85.0).to_radians();
    (
        (lon + 180.0) / 360.0,
        (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0,
    )
}

fn inverse_mercator(x: f64, y: f64) -> (f64, f64) {
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y))
        .sinh()
        .atan()
        .to_degrees();
    (lat, x * 360.0 - 180.0)
}

/// Centre and zoom that fit every point in a view of `size` points.
fn fit(points: &[Point], size: egui::Vec2) -> Option<(Position, f64)> {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in points {
        let (x, y) = mercator(p.lat, p.lon);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    if points.is_empty() {
        return None;
    }
    let (lat, lon) = inverse_mercator((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    // A tile is 256 points; leave a margin of one marker on each side.
    let w = f64::from((size.x - 2.0 * MARKER).max(64.0));
    let h = f64::from((size.y - 2.0 * MARKER).max(64.0));
    let span = |d: f64, px: f64| {
        if d <= 0.0 {
            17.0
        } else {
            (px / (d * 256.0)).log2()
        }
    };
    let zoom = span(x1 - x0, w).min(span(y1 - y0, h)).clamp(2.0, 16.0);
    Some((lat_lon(lat, lon), zoom))
}

fn build(app: &mut ShoeboxApp) {
    let mut points = Vec::new();
    for (pos, &i) in app.shown.iter().enumerate() {
        let st = &app.catalog.stacks[app.catalog.stack_of[i]];
        if st.primary != i {
            continue; // an expanded stack's other members
        }
        // The primary's position, else any member's (a JPG may have GPS
        // when its export doesn't).
        let gps = st.all().find_map(|m| app.catalog.photos[m].location());
        if let Some((lat, lon)) = gps {
            points.push(Point { pos, lat, lon });
        }
    }
    app.map.points = points;
    app.map.built = Some(app.shown_gen);
}

/// What the person did to a marker this frame.
enum Action {
    Select(Vec<usize>),
    ZoomInto(Position),
    Open(usize),
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    if app.map.built != Some(app.shown_gen) {
        build(app);
        app.map.fit = true;
    }
    if app.map.tiles.is_none()
        && let Some(dir) = app.map.tile_cache.clone()
    {
        let options = HttpOptions {
            cache: Some(dir),
            user_agent: Some(walkers::HeaderValue::from_static(concat!(
                "Shoebox/",
                env!("CARGO_PKG_VERSION"),
                " (desktop photo library)"
            ))),
            ..HttpOptions::default()
        };
        app.map.tiles = Some(HttpTiles::with_options(
            OpenStreetMap,
            options,
            ui.ctx().clone(),
        ));
    }

    let total = app
        .shown
        .iter()
        .filter(|&&i| app.catalog.stacks[app.catalog.stack_of[i]].primary == i)
        .count();
    ui.horizontal(|ui| {
        ui.label(format!(
            "{} of {} have a location",
            plural(app.map.points.len(), "photo"),
            total
        ));
        if ui.button("Fit all").clicked() {
            app.map.fit = true;
        }
        if app.map.tile_cache.is_none() {
            ui.weak("(map tiles off)");
        }
    });

    let rect = ui.available_rect_before_wrap();
    if app.map.fit {
        if let Some((centre, zoom)) = fit(&app.map.points, rect.size()) {
            app.map.memory.center_at(centre);
            let _ = app.map.memory.set_zoom(zoom);
        }
        app.map.fit = false;
    }
    if let Some(id) = app.map.focus.take()
        && let Some((lat, lon)) = app.index(id).and_then(|i| app.catalog.photos[i].location())
    {
        app.map.memory.center_at(lat_lon(lat, lon));
        let _ = app.map.memory.set_zoom(15.0);
    }

    let mut tiles = app.map.tiles.take();
    let mut memory = std::mem::take(&mut app.map.memory);
    let centre = memory.detached().unwrap_or_else(|| lat_lon(0.0, 0.0));
    let action = {
        let map = walkers::Map::new(
            tiles.as_mut().map(|t| t as &mut dyn walkers::Tiles),
            &mut memory,
            centre,
        )
        .zoom_with_ctrl(false)
        .double_click_to_zoom(false);
        map.show(ui, |ui, _resp, projector, _memory| {
            markers(app, ui, projector)
        })
        .inner
    };
    match action {
        Some(Action::Select(positions)) => {
            app.selection = positions.iter().map(|&p| app.ids[app.shown[p]]).collect();
            if let Some(&p) = positions.first() {
                app.current = Some(app.ids[app.shown[p]]);
            }
        }
        Some(Action::ZoomInto(at)) => {
            memory.center_at(at);
            let z = memory.zoom();
            let _ = memory.set_zoom((z + 2.0).min(18.0));
        }
        Some(Action::Open(p)) => {
            app.click(p, egui::Modifiers::NONE);
            app.set_view(View::Loupe);
        }
        None => {}
    }
    app.map.tiles = tiles;
    app.map.memory = memory;

    // OpenStreetMap's licence asks for this credit on the map.
    if app.map.tile_cache.is_some() {
        let credit = Rect::from_min_max(rect.max - vec2(230.0, 20.0), rect.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(credit));
        child
            .painter()
            .rect_filled(credit, 0.0, Color32::from_black_alpha(140));
        child.horizontal(|ui| {
            ui.add_space(6.0);
            ui.hyperlink_to(
                RichText::new("© OpenStreetMap contributors").small(),
                "https://www.openstreetmap.org/copyright",
            );
        });
    }
}

/// Group the visible points by screen cell and draw one marker per group.
fn markers(
    app: &mut ShoeboxApp,
    ui: &mut egui::Ui,
    projector: &walkers::Projector,
) -> Option<Action> {
    let clip = ui.max_rect();
    let mut groups: HashMap<(i32, i32), Vec<(Point, egui::Pos2)>> = HashMap::new();
    for p in &app.map.points {
        let v = projector.project(lat_lon(p.lat, p.lon));
        let at = pos2(v.x, v.y);
        if !clip.expand(MARKER).contains(at) {
            continue;
        }
        let key = ((at.x / CELL).floor() as i32, (at.y / CELL).floor() as i32);
        groups.entry(key).or_default().push((*p, at));
    }
    let mut groups: Vec<Vec<(Point, egui::Pos2)>> = groups.into_values().collect();
    // Draw bigger groups last (on top), in a stable order.
    groups.sort_by_key(|g| (g.len(), g[0].0.pos));
    let painter = ui.painter_at(clip);
    let mut action = None;
    for g in groups {
        let (first, at) = g[0];
        let n = g.len();
        let r = Rect::from_center_size(at, vec2(MARKER, MARKER));
        let idx = app.shown[first.pos];
        let id = app.ids[idx];
        app.want(idx, Size::Small, Priority::Visible);
        // A pin's point under the marker, a white frame, the thumbnail.
        painter.circle_filled(at + vec2(0.0, MARKER / 2.0 + 3.0), 3.0, Color32::WHITE);
        painter.rect_filled(r.expand(2.0), 5.0, Color32::WHITE);
        match app.textures.best(id, Size::Small) {
            Some(t) => {
                // Crop to a square.
                let [w, h] = t.size();
                let (w, h) = (w as f32, h as f32);
                let uv = if w > h {
                    let m = (1.0 - h / w) / 2.0;
                    Rect::from_min_max(pos2(m, 0.0), pos2(1.0 - m, 1.0))
                } else {
                    let m = (1.0 - w / h) / 2.0;
                    Rect::from_min_max(pos2(0.0, m), pos2(1.0, 1.0 - m))
                };
                painter.image(t.id(), r, uv, Color32::WHITE);
            }
            None => {
                painter.rect_filled(r, 3.0, Color32::from_gray(60));
            }
        }
        let selected = g
            .iter()
            .any(|(p, _)| app.selection.contains(&app.ids[app.shown[p.pos]]));
        if selected {
            painter.rect_stroke(
                r.expand(3.0),
                6.0,
                Stroke::new(3.0, ui.visuals().selection.stroke.color),
                StrokeKind::Outside,
            );
        }
        if n > 1 {
            let text = if n > 999 {
                "999+".to_owned()
            } else {
                n.to_string()
            };
            let galley =
                painter.layout_no_wrap(text, egui::FontId::proportional(11.0), Color32::WHITE);
            let badge = Rect::from_min_size(
                pos2(r.right() - galley.size().x - 4.0, r.top() - 8.0),
                galley.size() + vec2(8.0, 3.0),
            );
            painter.rect_filled(badge, 7.0, Color32::from_rgb(230, 70, 70));
            painter.galley(badge.min + vec2(4.0, 1.5), galley, Color32::WHITE);
        }
        let resp = ui.interact(r, ui.scope_id().with(("marker", first.pos)), Sense::click());
        let name = app.catalog.photos[idx]
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
        let resp = resp.on_hover_text(if n == 1 {
            name
        } else {
            format!(
                "{} here (click to select, double-click to zoom in)",
                plural(n, "photo")
            )
        });
        if resp.double_clicked() {
            action = Some(if n == 1 {
                Action::Open(first.pos)
            } else {
                let (lat, lon) = g
                    .iter()
                    .fold((0.0, 0.0), |(a, b), (p, _)| (a + p.lat, b + p.lon));
                Action::ZoomInto(lat_lon(lat / n as f64, lon / n as f64))
            });
        } else if resp.clicked() {
            action = Some(Action::Select(g.iter().map(|(p, _)| p.pos).collect()));
        }
    }
    action
}

/// "Show on Map" for a photo: switch to the map centred on it.
pub fn show_on_map(app: &mut ShoeboxApp, id: PhotoId) {
    app.map.focus = Some(id);
    app.set_view(View::Map);
}
