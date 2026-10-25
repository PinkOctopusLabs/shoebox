//! Playing a movie in the loupe (AVFoundation through `crate::video`): the
//! poster frame until it plays, then its frames; a bar with play/pause, the
//! time, a scrubber, sound on or off, volume and Open in QuickTime Player. Space
//! plays and pauses. It stops when the loupe shows something else.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, Rect, RichText, pos2, vec2};

use crate::app::ShoeboxApp;
use crate::video::{Player, Status};

/// Frames at most this long: enough for the loupe on a 5K screen, and a
/// 4K clip's frames stay cheap to hand to the GPU.
const MAX_EDGE: u32 = 2560;

/// The movie playing (or paused) in the loupe.
pub struct Playback {
    pub path: PathBuf,
    player: Result<Player, String>,
    texture: Option<egui::TextureHandle>,
    turns: u8,
}

impl Playback {
    pub fn is_playing(&self) -> bool {
        self.player.as_ref().is_ok_and(Player::is_playing)
    }

    /// Why it can't play, if it can't.
    pub fn error(&self) -> Option<String> {
        match &self.player {
            Err(e) => Some(e.clone()),
            Ok(p) => match p.status() {
                Status::Failed(e) => Some(e),
                _ => None,
            },
        }
    }
}

fn open(app: &ShoeboxApp, path: &Path) -> Playback {
    let player = Player::open(path, MAX_EDGE, app.target);
    if let Ok(p) = &player {
        p.set_muted(app.movie_muted);
        p.set_volume(app.movie_volume);
    }
    Playback {
        path: path.to_path_buf(),
        turns: player.as_ref().map_or(0, Player::quarter_turns),
        player,
        texture: None,
    }
}

/// The current photo's path, if it's a movie that's here (not in the
/// cloud only: the loupe downloads it first).
fn current_movie(app: &ShoeboxApp) -> Option<(PathBuf, bool)> {
    let i = app.index(app.current?)?;
    let p = &app.catalog.photos[i];
    p.format.is_movie().then(|| (p.path.clone(), p.cloud))
}

/// Whether the loupe shows a movie (Space plays it rather than zooming).
pub fn showing(app: &ShoeboxApp) -> bool {
    current_movie(app).is_some()
}

/// Play or pause the current movie (Space, the button, a click on it).
pub fn toggle(app: &mut ShoeboxApp) {
    let Some((path, cloud)) = current_movie(app) else {
        return;
    };
    if cloud {
        return;
    }
    if app.loupe.movie.as_ref().is_none_or(|m| m.path != path) {
        app.loupe.movie = Some(Box::new(open(app, &path)));
    }
    let Some(Ok(p)) = app.loupe.movie.as_ref().map(|m| &m.player) else {
        return;
    };
    if p.is_playing() {
        p.pause();
    } else {
        // From the start again once it's played to the end.
        if p.duration().is_some_and(|d| p.time() >= d - 0.05) {
            p.seek(0.0);
        }
        p.play();
    }
}

/// Take the newest frame, if there is one, into the texture.
fn update(ctx: &egui::Context, m: &mut Playback) {
    let Ok(p) = &m.player else { return };
    if let Some(f) = p.frame() {
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
        match &mut m.texture {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => {
                m.texture = Some(ctx.load_texture("movie", img, egui::TextureOptions::LINEAR));
            }
        }
    }
}

/// Draw the movie's frame upright in `target`, if it has one yet (else the
/// loupe keeps its poster). Returns whether it drew.
pub fn draw(
    movie: Option<&mut Playback>,
    ui: &egui::Ui,
    painter: &egui::Painter,
    target: Rect,
) -> bool {
    let Some(m) = movie else {
        return false;
    };
    update(ui.ctx(), m);
    if m.is_playing() {
        ui.ctx().request_repaint();
    }
    let Some(t) = &m.texture else { return false };
    // The stored frame's corners (TL, TR, BR, BL), each placed `turns`
    // quarter turns clockwise round the target's.
    let uv = [
        pos2(0.0, 0.0),
        pos2(1.0, 0.0),
        pos2(1.0, 1.0),
        pos2(0.0, 1.0),
    ];
    let at = [
        target.left_top(),
        target.right_top(),
        target.right_bottom(),
        target.left_bottom(),
    ];
    let mut mesh = egui::Mesh::with_texture(t.id());
    for (i, pos) in at.into_iter().enumerate() {
        let k = (i + 4 - usize::from(m.turns % 4)) % 4;
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv: uv[k],
            color: Color32::WHITE,
        });
    }
    mesh.indices = vec![0, 1, 2, 0, 2, 3];
    painter.add(egui::Shape::mesh(mesh));
    true
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub(crate) fn clock(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The bar under a movie in the loupe (`rect`: the loupe).
pub fn controls(app: &mut ShoeboxApp, ui: &egui::Ui, rect: Rect) {
    let Some((path, cloud)) = current_movie(app) else {
        return;
    };
    let width = (rect.width() - 40.0).clamp(240.0, 720.0);
    let area = egui::Area::new(ui.scope_id().with("movie-controls"))
        .fixed_pos(pos2(rect.center().x - width / 2.0, rect.bottom() - 56.0))
        .order(egui::Order::Foreground);
    area.show(ui.ctx(), |ui| {
        egui::Frame::new()
            .fill(Color32::from_black_alpha(190))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(width - 20.0);
                ui.horizontal(|ui| bar(app, ui, &path, cloud));
            });
    });
}

fn bar(app: &mut ShoeboxApp, ui: &mut egui::Ui, path: &Path, cloud: bool) {
    let movie = app.loupe.movie.as_deref().filter(|m| m.path == path);
    let playing = movie.is_some_and(Playback::is_playing);
    let (time, duration) = movie
        .and_then(|m| m.player.as_ref().ok())
        .map_or((0.0, None), |p| (p.time(), p.duration()));
    let error = movie.and_then(Playback::error);

    let icon = if playing {
        crate::icons::PAUSE
    } else {
        crate::icons::PLAY
    };
    let tip = if cloud {
        "In the cloud only: it plays once it's downloaded"
    } else if playing {
        "Pause (Space)"
    } else {
        "Play (Space)"
    };
    let play = ui
        .add_enabled(
            !cloud && error.is_none(),
            egui::Button::new(RichText::new(icon).size(16.0)).frame(false),
        )
        .on_hover_text(tip)
        .on_disabled_hover_text(tip);
    if play.clicked() {
        toggle(app);
    }
    let style = |s: String| RichText::new(s).monospace().color(Color32::from_gray(210));
    ui.label(style(clock(time)));

    // Sound, time left and QuickTime at the right; the scrubber between.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui
            .small_button("Open in QuickTime Player")
            .on_hover_text("Play it in QuickTime Player instead")
            .clicked()
        {
            if let Some(p) = app
                .loupe
                .movie
                .as_ref()
                .and_then(|m| m.player.as_ref().ok())
            {
                p.pause();
            }
            crate::app::open_file(path);
        }
        // Volume, then the speaker to its left (laid out right to left).
        let mut volume = app.movie_volume;
        ui.spacing_mut().slider_width = 70.0;
        let slider = ui
            .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
            .on_hover_text("Volume");
        slider.widget_info(|| egui::WidgetInfo::slider(true, f64::from(volume), "Volume"));
        if slider.changed() {
            app.movie_volume = volume;
            // Turning it up turns the sound on.
            if volume > 0.0 {
                app.movie_muted = false;
            }
            if let Some(p) = app
                .loupe
                .movie
                .as_ref()
                .and_then(|m| m.player.as_ref().ok())
            {
                p.set_volume(volume);
                p.set_muted(app.movie_muted);
            }
        }
        let muted = app.movie_muted || app.movie_volume == 0.0;
        let sound = if muted {
            crate::icons::SOUND_OFF
        } else {
            crate::icons::SOUND_ON
        };
        if ui
            .add(egui::Button::new(RichText::new(sound).size(16.0)).frame(false))
            .on_hover_text(if muted { "Sound on" } else { "Sound off" })
            .clicked()
        {
            app.movie_muted = !muted;
            if !app.movie_muted && app.movie_volume == 0.0 {
                app.movie_volume = 0.5;
            }
            if let Some(p) = app
                .loupe
                .movie
                .as_ref()
                .and_then(|m| m.player.as_ref().ok())
            {
                p.set_muted(app.movie_muted);
                p.set_volume(app.movie_volume);
            }
        }
        ui.label(style(duration.map_or_else(|| "–:––".to_owned(), clock)));
        if let Some(e) = &error {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Can't play it here: {e}"),
            );
            return;
        }
        let mut at = time;
        ui.spacing_mut().slider_width = ui.available_width() - 8.0;
        let scrub = ui.add_enabled(
            duration.is_some(),
            egui::Slider::new(&mut at, 0.0..=duration.unwrap_or(1.0)).show_value(false),
        );
        if scrub.changed()
            && let Some(p) = app
                .loupe
                .movie
                .as_ref()
                .and_then(|m| m.player.as_ref().ok())
        {
            p.seek(at);
        }
    });
}

/// A large play button in the middle of a movie that hasn't started.
pub fn play_button(app: &mut ShoeboxApp, ui: &egui::Ui, rect: Rect) {
    let Some((path, cloud)) = current_movie(app) else {
        return;
    };
    if cloud || app.loupe.movie.as_ref().is_some_and(|m| m.path == path) {
        return;
    }
    let area = egui::Area::new(ui.scope_id().with("movie-play"))
        .fixed_pos(rect.center() - vec2(32.0, 32.0))
        .order(egui::Order::Foreground);
    area.show(ui.ctx(), |ui| {
        let (r, resp) = ui.allocate_exact_size(vec2(64.0, 64.0), egui::Sense::click());
        let fill = if resp.hovered() {
            Color32::from_black_alpha(220)
        } else {
            Color32::from_black_alpha(160)
        };
        ui.painter().circle_filled(r.center(), 32.0, fill);
        ui.painter().text(
            r.center() + vec2(2.0, 0.0),
            egui::Align2::CENTER_CENTER,
            crate::icons::PLAY,
            egui::FontId::proportional(30.0),
            Color32::WHITE,
        );
        let resp = resp.on_hover_text("Play (Space)");
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Play"));
        if resp.clicked() {
            toggle(app);
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn clock() {
        assert_eq!(super::clock(0.0), "0:00");
        assert_eq!(super::clock(65.4), "1:05");
        assert_eq!(super::clock(3_725.0), "1:02:05");
    }
}
