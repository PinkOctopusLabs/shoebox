//! Folder pickers that don't block the frame. A blocking dialog runs its own
//! event loop inside ours, and a repaint asked for by a background thread
//! while it's open aborts winit ("tried to handle event while another event
//! is currently being handled"). The async dialog is a sheet on the window:
//! it's started in one frame and polled in the next ones.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use eframe::egui;

/// What a picked folder is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderFor {
    /// A new library folder.
    AddRoot,
    /// Tasks → Import's source.
    ImportSource,
    /// Move the selection into it.
    MoveSelection,
}

type Pick = Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>;

#[derive(Default)]
pub struct Picker {
    open: Option<(FolderFor, Pick)>,
}

struct Repaint(egui::Context);

impl Wake for Repaint {
    fn wake(self: Arc<Self>) {
        self.0.request_repaint();
    }
}

impl Picker {
    /// Show a folder picker (unless one is open already).
    pub fn open(&mut self, purpose: FolderFor, title: &str, start: Option<&Path>) {
        if self.open.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
        if let Some(d) = start {
            dialog = dialog.set_directory(d);
        }
        self.open = Some((purpose, Box::pin(dialog.pick_folder())));
    }

    /// The folder picked, once the picker has closed (`None` while it's
    /// open, or when it was cancelled).
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<(FolderFor, PathBuf)> {
        let (purpose, pick) = self.open.as_mut()?;
        let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
        match pick.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Pending => None,
            Poll::Ready(picked) => {
                let purpose = *purpose;
                self.open = None;
                picked.map(|h| (purpose, h.path().to_path_buf()))
            }
        }
    }
}
