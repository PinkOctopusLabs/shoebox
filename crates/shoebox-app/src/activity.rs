//! What's running in the background, for Tasks → Activity. A thread puts
//! its work on the shared [`Board`] while it runs; the [`Task`] it gets back
//! takes it off when dropped, however the work ends. The view adds what the
//! app already knows (imports, exports, indexing) and reads this each frame.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A kind of work on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The library worker's current job.
    Library,
    /// A file being downloaded from a cloud drive.
    Download,
}

/// Something under way.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: u64,
    pub kind: Kind,
    /// What it is ("Moving 12 photos to _rejected").
    pub what: String,
    /// The file it's on, if one.
    pub file: Option<PathBuf>,
    pub started: Instant,
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    next: u64,
}

/// The shared list of work under way (cheap to clone).
#[derive(Clone, Default)]
pub struct Board(Arc<Mutex<State>>);

impl Board {
    /// Put work on the board until the returned task is dropped.
    pub fn start(&self, kind: Kind, what: impl Into<String>, file: Option<PathBuf>) -> Task {
        let id = match self.0.lock() {
            Ok(mut s) => {
                s.next += 1;
                let id = s.next;
                s.items.push(Item {
                    id,
                    kind,
                    what: what.into(),
                    file,
                    started: Instant::now(),
                });
                id
            }
            Err(_) => 0,
        };
        Task {
            board: self.clone(),
            id,
        }
    }

    /// [`Board::start`], as if begun at `started` (tests).
    #[doc(hidden)]
    pub fn start_since(
        &self,
        kind: Kind,
        what: impl Into<String>,
        file: Option<PathBuf>,
        started: Instant,
    ) -> Task {
        let task = self.start(kind, what, file);
        if let Ok(mut s) = self.0.lock()
            && let Some(i) = s.items.iter_mut().find(|i| i.id == task.id)
        {
            i.started = started;
        }
        task
    }

    /// Everything under way, oldest first.
    pub fn items(&self) -> Vec<Item> {
        self.0.lock().map(|s| s.items.clone()).unwrap_or_default()
    }
}

/// Work on the board; dropping it takes it off.
pub struct Task {
    board: Board,
    id: u64,
}

impl Task {
    /// Say what it's doing now.
    pub fn set(&self, what: impl Into<String>) {
        if let Ok(mut s) = self.board.0.lock()
            && let Some(i) = s.items.iter_mut().find(|i| i.id == self.id)
        {
            i.what = what.into();
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if let Ok(mut s) = self.board.0.lock() {
            s.items.retain(|i| i.id != self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_leaves_the_board_however_it_ends() {
        let board = Board::default();
        let a = board.start(Kind::Library, "Undoing", None);
        let b = board.start(Kind::Download, "Downloading", Some("x.jpg".into()));
        a.set("Undoing batch 3");
        let items = board.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].what, "Undoing batch 3");
        drop(a);
        assert_eq!(board.items().len(), 1);
        let r = std::panic::catch_unwind(move || {
            let _b = b;
            panic!("the work failed");
        });
        assert!(r.is_err());
        assert!(board.items().is_empty());
    }
}
