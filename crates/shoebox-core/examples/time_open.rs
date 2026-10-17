//! Time opening a library from the cache, stage by stage:
//! `cargo run --release -p shoebox-core --example time_open -- <cache.sqlite> <root>`
//! (use a copy of the cache: opening it may migrate it).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use shoebox_core::cache::Cache;
use shoebox_core::catalog::Catalog;
use shoebox_core::scan::{Role, Root};

fn main() {
    let mut args = std::env::args().skip(1);
    let db = args.next().expect("cache.sqlite");
    let root = std::fs::canonicalize(args.next().expect("root")).unwrap();
    let t = Instant::now();
    let cache = Cache::open(std::path::Path::new(&db)).unwrap();
    eprintln!("open cache: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let id = cache.root_id(&root, false).unwrap();
    let t = Instant::now();
    let meta = cache.load_meta(id).unwrap();
    eprintln!(
        "load_meta ({} rows): {:.0} ms",
        meta.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    drop(meta);
    let t = Instant::now();
    let roots = vec![Root {
        path: root,
        role: Role::Scan,
    }];
    let c = Catalog::from_cache(&cache, roots).unwrap();
    eprintln!(
        "from_cache ({} photos, {} stacks): {:.0} ms",
        c.photos.len(),
        c.stacks.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let _ = shoebox_core::stacks::build(&c.photos, &c.links);
    eprintln!(
        "  of which stacks::build: {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let c2 = c.clone();
    eprintln!("clone: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    drop(c2);
}
