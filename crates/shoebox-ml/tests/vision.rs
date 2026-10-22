//! Vision's subject and person masks (macOS): the mask is the image's
//! size, and a lone object on a plain ground is found as the subject.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_ml::vision::{Find, segment};

/// A shaded orange ball on a plain blue-grey ground.
fn ball(w: usize, h: usize) -> Vec<u8> {
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - w as f32 * 0.5, y as f32 - h as f32 * 0.55);
            let r = (dx * dx + dy * dy).sqrt() / (h as f32 * 0.28);
            if r < 1.0 {
                let shade = 1.0 - 0.5 * r * r;
                px.extend([
                    (240.0 * shade) as u8,
                    (120.0 * shade) as u8,
                    (30.0 * shade) as u8,
                ]);
            } else {
                px.extend([120, 130, 140]);
            }
        }
    }
    px
}

#[test]
fn subject_and_person_masks() {
    let (w, h) = (640, 480);
    let img = ball(w, h);
    let r = segment(&img, w, h, Find::Subject);
    if cfg!(not(target_os = "macos")) {
        assert!(r.is_err());
        return;
    }
    let m = r.unwrap();
    assert_eq!((m.width, m.height, m.data.len()), (w, h, w * h));
    let at = |x: usize, y: usize| m.data[y * w + x];
    eprintln!("subject: centre {} corner {}", at(320, 264), at(10, 10));
    // Vision may find nothing in a drawing; when it finds something, it's
    // the ball.
    if m.data.iter().any(|&v| v > 0) {
        assert!(at(320, 264) > 200 && at(10, 10) < 50);
    }
    let p = segment(&img, w, h, Find::Person).unwrap();
    assert_eq!((p.width, p.height), (w, h));
}
