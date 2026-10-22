//! The scene model on a drawn landscape: a blue sky over green grass, a
//! grey-brown hill between. Needs the checkpoint:
//! `SHOEBOX_SKY_MODEL=<folder with upernet_convnext_base_22k_640x640.pth>`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_ml::scene::{FILE, Part, SceneModel};

/// Sky above a third of the height, a hill's slope across the middle,
/// grass below; noise on all of it as a photo has.
fn picture(w: usize, h: usize) -> Vec<u8> {
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let n = (((x * 7919 + y * 104_729) % 23) as f32 - 11.0) * 1.5;
            let ridge = 0.38 + 0.1 * (fx * 6.0).sin() * (1.0 - fx);
            let p = if fy < ridge {
                // Deeper blue overhead, paler at the horizon.
                let t = fy / ridge;
                [70.0 + 110.0 * t, 130.0 + 80.0 * t, 225.0 + 20.0 * t]
            } else if fy < 0.6 {
                [110.0 + n, 100.0 + n, 85.0 + n]
            } else {
                let g = n * 2.0;
                [70.0 + g, 135.0 + g, 45.0 + g * 0.5]
            };
            px.extend(p.map(|v| v.clamp(0.0, 255.0) as u8));
        }
    }
    px
}

#[test]
#[ignore = "needs the scene checkpoint (SHOEBOX_SKY_MODEL)"]
fn finds_sky_and_grass() {
    let Ok(dir) = std::env::var("SHOEBOX_SKY_MODEL") else {
        eprintln!("skipping: SHOEBOX_SKY_MODEL not set");
        return;
    };
    let device = shoebox_ml::device();
    let t = std::time::Instant::now();
    let model = SceneModel::load(&std::path::Path::new(&dir).join(FILE), &device).unwrap();
    eprintln!("load {:.2} s", t.elapsed().as_secs_f32());
    let (w, h) = (1500, 1000);
    let px = picture(w, h);
    for run in 0..2 {
        let t = std::time::Instant::now();
        let s = model.segment(&px, w, h).unwrap();
        eprintln!("segment ({run}) {:.2} s", t.elapsed().as_secs_f32());
        assert_eq!((s.width, s.height), (240, 160));
        // Mean probability over a box (fractions of the picture).
        let mean = |p: Part, x0: f32, y0: f32, x1: f32, y1: f32| {
            let v = s.part(p);
            let (mut sum, mut n) = (0.0, 0.0);
            for y in (y0 * s.height as f32) as usize..(y1 * s.height as f32) as usize {
                for x in (x0 * s.width as f32) as usize..(x1 * s.width as f32) as usize {
                    sum += v[y * s.width + x];
                    n += 1.0;
                }
            }
            sum / n
        };
        let sky = mean(Part::Sky, 0.1, 0.02, 0.9, 0.2);
        let below = mean(Part::Sky, 0.1, 0.7, 0.9, 0.98);
        let green =
            mean(Part::Vegetation, 0.1, 0.7, 0.9, 0.98) + mean(Part::Ground, 0.1, 0.7, 0.9, 0.98);
        eprintln!("sky above {sky:.2}, below {below:.2}; grass or ground below {green:.2}");
        for p in Part::ALL {
            eprintln!(
                "  {p:?}: top {:.2}, bottom {:.2}",
                mean(p, 0.0, 0.0, 1.0, 0.25),
                mean(p, 0.0, 0.7, 1.0, 1.0)
            );
        }
        assert!(sky > 0.8, "sky {sky}");
        assert!(below < 0.05, "no sky in the grass: {below}");
        assert!(green > 0.6, "grass {green}");
    }
}
