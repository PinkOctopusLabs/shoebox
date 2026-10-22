//! SAM 2.1 on a drawn picture: a click on each of two objects gives that
//! object (and not the other), a negative click takes away. Needs the
//! checkpoint: `SHOEBOX_SAM_MODEL=<folder with sam2.1_hiera_tiny.pt>`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_ml::sam::{Click, FILE, Sam};

/// An orange ball left, a blue square right, on a grey ground; and which
/// pixels are which.
fn picture(w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
    let mut px = Vec::with_capacity(w * h * 3);
    let mut truth = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - w as f32 * 0.3, y as f32 - h as f32 * 0.5);
            let r = (dx * dx + dy * dy).sqrt() / (h as f32 * 0.22);
            let square = (x as f32 - w as f32 * 0.72).abs() < h as f32 * 0.18
                && (y as f32 - h as f32 * 0.5).abs() < h as f32 * 0.18;
            if r < 1.0 {
                let s = 1.0 - 0.45 * r * r;
                px.extend([(235.0 * s) as u8, (125.0 * s) as u8, (35.0 * s) as u8]);
                truth.push(1);
            } else if square {
                px.extend([40, 70, 200]);
                truth.push(2);
            } else {
                let n = ((x * 7 + y * 13) % 11) as u8;
                px.extend([140 + n, 140 + n, 135 + n]);
                truth.push(0);
            }
        }
    }
    (px, truth)
}

fn iou(mask: &[f32], truth: &[u8], label: u8) -> f32 {
    let (mut i, mut u) = (0.0, 0.0);
    for (m, t) in mask.iter().zip(truth) {
        let (a, b) = (*m > 0.5, *t == label);
        if a && b {
            i += 1.0;
        }
        if a || b {
            u += 1.0;
        }
    }
    i / f32::max(u, 1.0)
}

#[test]
#[ignore = "needs the SAM 2.1 checkpoint (SHOEBOX_SAM_MODEL)"]
fn clicks_find_objects() {
    let Ok(dir) = std::env::var("SHOEBOX_SAM_MODEL") else {
        eprintln!("skipping: SHOEBOX_SAM_MODEL not set");
        return;
    };
    let device = shoebox_ml::device();
    let t = std::time::Instant::now();
    let sam = Sam::load(&std::path::Path::new(&dir).join(FILE), &device).unwrap();
    eprintln!("load {:.2} s", t.elapsed().as_secs_f32());
    let (w, h) = (900, 600);
    let (px, truth) = picture(w, h);
    let t = std::time::Instant::now();
    let e = sam.embed(&px, w, h).unwrap();
    eprintln!("embed {:.2} s", t.elapsed().as_secs_f32());
    let ball = Click {
        x: 270.0,
        y: 300.0,
        add: true,
    };
    let t = std::time::Instant::now();
    let m = sam.predict(&e, &[ball]).unwrap();
    eprintln!("predict {:.3} s", t.elapsed().as_secs_f32());
    let (ib, is) = (iou(&m, &truth, 1), iou(&m, &truth, 2));
    eprintln!("ball click: ball IoU {ib:.3}, square IoU {is:.3}");
    assert!(ib > 0.85 && is < 0.05);
    let square = Click {
        x: 648.0,
        y: 300.0,
        add: true,
    };
    let m = sam.predict(&e, &[square]).unwrap();
    let (ib, is) = (iou(&m, &truth, 1), iou(&m, &truth, 2));
    eprintln!("square click: ball IoU {ib:.3}, square IoU {is:.3}");
    assert!(is > 0.85 && ib < 0.05);
    // Both, then the ball taken away again.
    let both = sam.predict(&e, &[ball, square]).unwrap();
    let covered = |m: &[f32], label| iou(m, &truth, label);
    eprintln!("both: {:.3} {:.3}", covered(&both, 1), covered(&both, 2));
    let less = sam
        .predict(
            &e,
            &[
                ball,
                square,
                Click {
                    x: 270.0,
                    y: 300.0,
                    add: false,
                },
            ],
        )
        .unwrap();
    eprintln!(
        "minus the ball: {:.3} {:.3}",
        covered(&less, 1),
        covered(&less, 2)
    );
    assert!(covered(&less, 1) < covered(&both, 1));
}
