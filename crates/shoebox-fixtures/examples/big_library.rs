//! A large synthetic library for performance checks, shaped like a camera
//! library: `YYYY/YYYY-MM-DD/` day folders over 2015–2026, shoots of bursts,
//! most frames as NEF + JPG pairs, GPS on most shoot days, and a rating
//! sidecar on about one frame in ten. Every image is a distinct small scene.
//!
//! `cargo run --release -p shoebox-fixtures --example big_library -- <empty dir> [files]`
//! (default 200,000 files; about 1.5 GB).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use shoebox_fixtures::{Shot, jpeg, nef, scene, with_exif_gps};

/// xorshift, as in the fixtures crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// (year, month, day) from days since 1970-01-01 (Howard Hinnant).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// One frame to write.
struct Frame {
    dir: PathBuf,
    name: String,
    datetime: String,
    subsec: String,
    gps: Option<(f64, f64)>,
    raw: bool,
    rating: Option<u8>,
    seed: u64,
}

fn plan(root: &Path, files: usize) -> Vec<Frame> {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    // Places shoots happen (synthetic, round numbers).
    let places = [
        (48.85, 2.35),
        (35.68, 139.69),
        (-33.87, 151.21),
        (51.50, -0.12),
        (40.71, -74.00),
        (41.90, 12.49),
        (-22.90, -43.20),
        (64.14, -21.94),
    ];
    let first = 16_436; // 2015-01-01
    let last = 20_726; // 2026-10-01
    let mut frames = Vec::new();
    let mut count = 0;
    let mut number = 0u32;
    let mut day = first;
    while count < files {
        day += 1 + rng.below(3) as i64;
        if day > last {
            day = first + rng.below(30) as i64;
        }
        let (y, m, d) = civil(day);
        let dir = root.join(format!("{y}")).join(format!("{y}-{m:02}-{d:02}"));
        let place = (rng.below(10) < 7).then(|| places[rng.below(places.len() as u64) as usize]);
        let mut secs = 8 * 3600 + rng.below(8 * 3600) as u32;
        let frames_today = 10 + rng.below(70);
        for _ in 0..frames_today {
            if count >= files {
                break;
            }
            number = (number + 1) % 10_000;
            // Bursts: mostly a second or less apart, sometimes a gap.
            secs += if rng.below(5) == 0 {
                30 + rng.below(1200) as u32
            } else {
                1
            };
            let secs = secs.min(86_399);
            let raw = rng.below(10) < 7 && count + 2 <= files;
            let gps = place.map(|(la, lo)| {
                let j = |r: &mut Rng| (r.below(2000) as f64 - 1000.0) / 20_000.0;
                (la + j(&mut rng), lo + j(&mut rng))
            });
            frames.push(Frame {
                dir: dir.clone(),
                name: format!("DSC_{number:04}"),
                datetime: format!(
                    "{y}:{m:02}:{d:02} {:02}:{:02}:{:02}",
                    secs / 3600,
                    secs / 60 % 60,
                    secs % 60
                ),
                subsec: format!("{:02}", rng.below(100)),
                gps,
                raw,
                rating: (rng.below(10) == 0).then(|| 1 + rng.below(5) as u8),
                seed: count as u64 + 1,
            });
            count += if raw { 2 } else { 1 };
        }
    }
    frames
}

fn sidecar(rating: u8) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/>
 </rdf:RDF>
</x:xmpmeta>
"#
    )
}

fn write(f: &Frame) {
    let img = scene(f.seed, 240, 160);
    let shot = Shot {
        datetime: f.datetime.clone().leak(),
        subsec: f.subsec.clone().leak(),
        orientation: 1,
        serial: "1234567",
    };
    // A camera JPG and (slightly different) embedded preview, as a Z f writes.
    let jpg = with_exif_gps(&jpeg(&img, 80).unwrap(), &shot, f.gps);
    let jpg_name = format!("{}.JPG", f.name);
    let path = unique(&f.dir, &jpg_name);
    std::fs::write(&path, jpg).unwrap();
    let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
    if f.raw {
        let preview = jpeg(&img, 92).unwrap();
        let raw = nef(&preview, (240, 160), &preview, &shot);
        std::fs::write(f.dir.join(format!("{stem}.NEF")), raw).unwrap();
    }
    if let Some(r) = f.rating {
        let main = if f.raw { "NEF" } else { "JPG" };
        std::fs::write(f.dir.join(format!("{stem}.{main}.xmp")), sidecar(r)).unwrap();
    }
}

/// `DSC_0042.JPG`, or `DSC_0042-1.JPG` when the counter wrapped within a day.
fn unique(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap();
    (1..)
        .map(|i| dir.join(format!("{stem}-{i}.{ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: big_library <empty dir> [files]"));
    let files: usize = args.next().map_or(200_000, |s| s.parse().unwrap());
    std::fs::create_dir_all(&root).unwrap();
    assert!(
        std::fs::read_dir(&root).unwrap().next().is_none(),
        "{} isn't empty",
        root.display()
    );
    let start = Instant::now();
    let frames = plan(&root, files);
    let dirs: std::collections::BTreeSet<&PathBuf> = frames.iter().map(|f| &f.dir).collect();
    for d in &dirs {
        std::fs::create_dir_all(d).unwrap();
    }
    // Each folder is written by one thread, so `unique` never races.
    let threads = std::thread::available_parallelism().map_or(8, |n| n.get());
    let mut grouped: std::collections::BTreeMap<&PathBuf, Vec<&Frame>> = Default::default();
    for f in &frames {
        grouped.entry(&f.dir).or_default().push(f);
    }
    let by_dir: Vec<Vec<&Frame>> = grouped.into_values().collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(group) = by_dir.get(i) else { break };
                    for f in group {
                        write(f);
                    }
                }
            });
        }
    });
    let raws = frames.iter().filter(|f| f.raw).count();
    eprintln!(
        "wrote {} frames ({} NEF + JPG, {} JPG only; {} files) in {} folders, {:.1} s",
        frames.len(),
        raws,
        frames.len() - raws,
        frames.len() + raws,
        dirs.len(),
        start.elapsed().as_secs_f64()
    );
}
