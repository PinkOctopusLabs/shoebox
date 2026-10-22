//! The depth model on a drawn scene: a floor running away to a horizon
//! under a sky, a ball standing on the floor near the camera. Needs the
//! checkpoint: `SHOEBOX_DEPTH_MODEL=<folder with depth_anything_v2_vits.pth>`.
//! With `SHOEBOX_DEPTH_PHOTOS=<folder of PNGs>` it also times real photos
//! and writes each depth map into
//! `SHOEBOX_DEPTH_OUT`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_ml::depth::{DepthModel, FILE};

/// A chequered floor in perspective below the horizon (a third down),
/// sky above, and a red ball on the floor in the lower left.
fn picture(w: usize, h: usize) -> Vec<u8> {
    let horizon = h as f32 / 3.0;
    let (bx, by, br) = (w as f32 * 0.3, h as f32 * 0.72, h as f32 * 0.16);
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32, y as f32);
            let d = ((fx - bx).powi(2) + (fy - by).powi(2)).sqrt();
            let p = if d < br {
                // Shaded from the upper left.
                let l = 1.0 - 0.6 * ((fx - bx + br * 0.4).hypot(fy - by + br * 0.4) / (2.0 * br));
                [220.0 * l, 40.0 * l, 30.0 * l]
            } else if fy < horizon {
                let t = fy / horizon;
                [90.0 + 100.0 * t, 140.0 + 80.0 * t, 230.0 + 15.0 * t]
            } else {
                // Floor squares shrink towards the horizon.
                let z = 40.0 / (fy - horizon + 1.0);
                let u = (fx - w as f32 / 2.0) * z / 40.0;
                let v = z * 4.0;
                let c = ((u.floor() as i64 + v.floor() as i64) & 1) == 0;
                let fog = (z / 4.0).min(1.0);
                let base = if c { 200.0 } else { 70.0 };
                let g = base + (150.0 - base) * fog;
                [g, g * 0.95, g * 0.85]
            };
            px.extend(p.map(|v| v.clamp(0.0, 255.0) as u8));
        }
    }
    px
}

fn mean(d: &shoebox_ml::depth::Depth, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (mut s, mut n) = (0.0, 0.0);
    for y in (y0 * d.height as f32) as usize..(y1 * d.height as f32) as usize {
        for x in (x0 * d.width as f32) as usize..(x1 * d.width as f32) as usize {
            s += d.data[y * d.width + x];
            n += 1.0;
        }
    }
    s / n
}

#[test]
#[ignore = "needs the depth checkpoint (SHOEBOX_DEPTH_MODEL)"]
fn near_is_larger_than_far() {
    let Ok(dir) = std::env::var("SHOEBOX_DEPTH_MODEL") else {
        eprintln!("skipping: SHOEBOX_DEPTH_MODEL not set");
        return;
    };
    let device = shoebox_ml::device();
    let t = std::time::Instant::now();
    let model = DepthModel::load(&std::path::Path::new(&dir).join(FILE), &device).unwrap();
    eprintln!("load {:.2} s", t.elapsed().as_secs_f32());
    let (w, h) = (1500, 1000);
    let px = picture(w, h);
    for run in 0..2 {
        let t = std::time::Instant::now();
        let d = model.estimate(&px, w, h).unwrap();
        eprintln!("estimate ({run}) {:.2} s", t.elapsed().as_secs_f32());
        assert_eq!((d.width, d.height), (784, 518));
        let ball = mean(&d, 0.25, 0.68, 0.35, 0.76);
        let floor_near = mean(&d, 0.6, 0.9, 0.9, 0.98);
        let floor_far = mean(&d, 0.6, 0.36, 0.9, 0.42);
        let sky = mean(&d, 0.1, 0.02, 0.9, 0.2);
        eprintln!(
            "ball {ball:.3}, near floor {floor_near:.3}, far floor {floor_far:.3}, sky {sky:.3}"
        );
        assert!(
            ball > floor_far && floor_near > floor_far,
            "nearer is larger"
        );
        assert!(floor_far > sky, "the sky is farthest");
    }
    // Real photos, timed (and written out when asked).
    let (Ok(photos), out) = (
        std::env::var("SHOEBOX_DEPTH_PHOTOS"),
        std::env::var("SHOEBOX_DEPTH_OUT").ok(),
    ) else {
        return;
    };
    for e in std::fs::read_dir(photos).unwrap().flatten() {
        let p = e.path();
        let Ok(img) = image::open(&p) else { continue };
        let img = img.to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let t = std::time::Instant::now();
        let d = model.estimate(img.as_raw(), w, h).unwrap();
        eprintln!(
            "{}: {w}×{h} → {}×{} in {:.2} s",
            p.display(),
            d.width,
            d.height,
            t.elapsed().as_secs_f32()
        );
        if let Some(out) = &out {
            let max = d.data.iter().cloned().fold(1e-6f32, f32::max);
            let bytes: Vec<u8> = d.data.iter().map(|v| (v / max * 255.0) as u8).collect();
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            image::GrayImage::from_raw(d.width as u32, d.height as u32, bytes)
                .unwrap()
                .save(std::path::Path::new(out).join(format!("{name}-depth.png")))
                .unwrap();
        }
    }
}
