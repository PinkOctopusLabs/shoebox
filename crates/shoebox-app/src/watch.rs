//! File watcher: FSEvents on macOS, inotify on Linux (through `notify`).
//! Raw events are debounced into batches of changed paths, so a tool that
//! rewrites a sidecar in several steps (DxO, digiKam) causes one refresh.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use notify::{RecursiveMode, Watcher as _};

/// Quiet time that ends a batch.
const QUIET: Duration = Duration::from_millis(250);
/// A busy folder still gets a refresh this often.
const MAX_WAIT: Duration = Duration::from_secs(2);

/// Watches roots while alive; batches arrive on the sender given to [`watch`].
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
}

/// Watch `roots` recursively. Each batch of changed paths is sent to `out`.
pub fn watch(roots: &[PathBuf], out: Sender<Vec<PathBuf>>) -> Option<Watcher> {
    let (tx, rx) = crossbeam_channel::unbounded::<Vec<PathBuf>>();
    let mut inner =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(ev) if !ev.kind.is_access() => {
                let _ = tx.send(ev.paths);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("file watcher: {e}"),
        })
        .map_err(|e| tracing::warn!("file watcher unavailable: {e}"))
        .ok()?;
    for r in roots {
        if let Err(e) = inner.watch(r, RecursiveMode::Recursive) {
            tracing::warn!("not watching {}: {e}", r.display());
        }
    }
    let spawned = std::thread::Builder::new()
        .name("watch-debounce".into())
        .spawn(move || debounce(&rx, &out));
    if let Err(e) = spawned {
        tracing::warn!("file watcher: {e}");
    }
    Some(Watcher { _inner: inner })
}

/// Collect events until things go quiet; ends when the watcher is dropped.
fn debounce(rx: &Receiver<Vec<PathBuf>>, out: &Sender<Vec<PathBuf>>) {
    while let Ok(first) = rx.recv() {
        let start = Instant::now();
        let mut batch = first;
        loop {
            match rx.recv_timeout(QUIET) {
                Ok(more) => batch.extend(more),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if start.elapsed() > MAX_WAIT {
                break;
            }
        }
        batch.sort();
        batch.dedup();
        if out.send(batch).is_err() {
            return;
        }
    }
}
