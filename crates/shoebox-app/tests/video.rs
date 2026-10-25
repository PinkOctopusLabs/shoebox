//! Movie playback through AVFoundation, on clips ffmpeg makes (skipped
//! without it): frames in the right place and colour, turned as the track
//! says, the clock running while it plays.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use shoebox_app::video::{Frame, Player, Status, run_main_loop};
use shoebox_decode::Target;

/// A 2 s, 320×240 H.264 clip, blue with a red top-left quarter, turned
/// `rotation` degrees by its track (as a phone held upright records).
fn clip(dir: &Path, name: &str, rotation: Option<i32>) -> Option<PathBuf> {
    let plain = dir.join("plain.mov");
    if !plain.exists() {
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=blue:s=320x240:d=2:r=30,drawbox=x=0:y=0:w=160:h=120:c=red:t=fill")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&plain)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            eprintln!("no ffmpeg with libx264: skipping");
            return None;
        }
    }
    let Some(degrees) = rotation else {
        return Some(plain);
    };
    let out = dir.join(name);
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-display_rotation:v:0"])
        .arg(degrees.to_string())
        .arg("-i")
        .arg(&plain)
        .args(["-c", "copy"])
        .arg(&out)
        .output()
        .is_ok_and(|o| o.status.success());
    ok.then_some(out)
}

fn ready(p: &Player) {
    let start = Instant::now();
    loop {
        match p.status() {
            Status::Ready => return,
            Status::Failed(e) => panic!("{e}"),
            Status::Loading => {}
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never ready");
        run_main_loop(0.01);
    }
}

fn frame(p: &Player) -> Frame {
    let start = Instant::now();
    loop {
        if let Some(f) = p.frame() {
            return f;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no frame");
        run_main_loop(0.01);
    }
}

fn px(f: &Frame, x: usize, y: usize) -> [u8; 3] {
    let i = (y * f.width + x) * 4;
    [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2]]
}

fn red(p: [u8; 3]) -> bool {
    p[0] > 180 && p[1] < 80 && p[2] < 80
}

fn blue(p: [u8; 3]) -> bool {
    p[2] > 180 && p[0] < 80 && p[1] < 80
}

fn plays_a_movie() {
    let t = tempfile::tempdir().unwrap();
    let Some(path) = clip(t.path(), "plain.mov", None) else {
        return;
    };
    let p = Player::open(&path, 2560, Target::Srgb).unwrap();
    p.set_muted(true);
    ready(&p);
    let d = p.duration().unwrap();
    assert!((d - 2.0).abs() < 0.1, "{d}");
    assert_eq!(p.quarter_turns(), 0);
    assert!(!p.is_playing());

    p.seek(1.0);
    let f = frame(&p);
    assert_eq!((f.width, f.height), (320, 240));
    assert!(red(px(&f, 40, 30)), "{:?}", px(&f, 40, 30));
    assert!(blue(px(&f, 280, 200)), "{:?}", px(&f, 280, 200));

    p.seek(0.0);
    p.play();
    assert!(p.is_playing());
    run_main_loop(0.5);
    assert!(p.time() > 0.2, "the clock runs: {}", p.time());
    p.pause();
    assert!(!p.is_playing());
}

/// Frames come as stored, smaller when asked; the turn to make them
/// upright comes from the track.
fn says_how_to_turn_a_rotated_movie() {
    let t = tempfile::tempdir().unwrap();
    // ffmpeg's display rotation is anticlockwise: -90 is a quarter turn
    // clockwise, as an iPhone held upright records.
    let Some(path) = clip(t.path(), "turned.mov", Some(-90)) else {
        return;
    };
    let p = Player::open(&path, 160, Target::DisplayP3).unwrap();
    p.set_muted(true);
    ready(&p);
    assert_eq!(p.quarter_turns(), 1);
    p.seek(0.5);
    let f = frame(&p);
    assert_eq!((f.width, f.height), (160, 120), "stored, not turned");
    assert!(red(px(&f, 20, 15)), "{:?}", px(&f, 20, 15));
    // A clockwise quarter turn puts the stored top left at the top right,
    // where AVFoundation's own upright poster has it.
    let poster = shoebox_decode::render_path(
        shoebox_decode::Format::Mov,
        &path,
        Some(160),
        1,
        Target::Srgb,
    )
    .unwrap();
    assert_eq!((poster.width, poster.height), (120, 160));
    let at = |x: u32, y: u32| {
        let i = ((y * poster.width + x) * 3) as usize;
        [poster.px[i], poster.px[i + 1], poster.px[i + 2]]
    };
    assert!(red(at(100, 20)), "{:?}", at(100, 20));
    assert!(blue(at(20, 140)), "{:?}", at(20, 140));
}

fn a_file_that_is_not_a_movie_fails() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("broken.mov");
    std::fs::write(&path, b"not a movie").unwrap();
    match Player::open(&path, 2560, Target::Srgb) {
        Err(e) => assert!(!e.is_empty()),
        Ok(p) => {
            let start = Instant::now();
            while p.status() == Status::Loading {
                assert!(start.elapsed() < Duration::from_secs(10));
                run_main_loop(0.01);
            }
            assert!(matches!(p.status(), Status::Failed(_)));
        }
    }
}

fn main() {
    let tests: [(&str, fn()); 3] = [
        ("plays_a_movie", plays_a_movie),
        (
            "says_how_to_turn_a_rotated_movie",
            says_how_to_turn_a_rotated_movie,
        ),
        (
            "a_file_that_is_not_a_movie_fails",
            a_file_that_is_not_a_movie_fails,
        ),
    ];
    for (name, test) in tests {
        test();
        println!("test {name} ... ok");
    }
}
