//! Tasks → Activity: what's running now (SPEC, "Activity"). The background
//! threads' own work comes from the [`crate::activity::Board`]; the rest
//! from what the app already shows in the status bar.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::activity::Kind;
use crate::app::ShoeboxApp;

/// How long a download waits before it's said to be waiting for its drive.
const WAITING: Duration = Duration::from_secs(10);

/// What a row's Stop does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Import,
    Icloud,
    Merge,
}

/// Something under way.
pub(crate) struct Row {
    pub what: String,
    /// The file, or more about it.
    detail: Option<String>,
    progress: Option<(usize, usize)>,
    since: Option<Instant>,
    stop: Option<Stop>,
}

impl Row {
    fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            detail: None,
            progress: None,
            since: None,
            stop: None,
        }
    }

    fn progress(mut self, done: usize, total: usize) -> Self {
        self.progress = Some((done, total));
        self
    }
}

/// Everything under way, most important first (the status bar shows the
/// first).
pub(crate) fn rows(app: &ShoeboxApp) -> Vec<Row> {
    let mut out = Vec::new();
    if let Some((done, total)) = crate::views::dates::progress(app) {
        let what = match crate::views::dates::stage(app) {
            Some(s) => format!("Finding dates · {s}"),
            None => "Finding dates".to_owned(),
        };
        out.push(Row::new(what).progress(done, total));
    }
    if let Some((p, _)) = &app.icloud.running {
        out.push(Row {
            stop: Some(Stop::Icloud),
            ..Row::new("Copying from iCloud Photos").progress(p.done, p.total)
        });
    }
    if let Some(c) = &app.import.copying {
        out.push(Row {
            stop: Some(Stop::Import),
            ..Row::new("Importing").progress(c.done, c.files)
        });
    }
    let now = Instant::now();
    let board = app.activity.items();
    for item in board.iter().filter(|i| i.kind == Kind::Download) {
        let waited = now.duration_since(item.started);
        let what = match item.file.as_deref().and_then(shoebox_core::cloud::provider) {
            Some(drive) if waited >= WAITING => format!("Waiting for {drive}"),
            _ => item.what.clone(),
        };
        out.push(Row {
            detail: item.file.as_ref().map(|f| f.display().to_string()),
            since: Some(item.started),
            ..Row::new(what)
        });
    }
    let queued = app.loader.queued_downloads();
    if queued > 0 {
        out.push(Row::new(format!(
            "{} waiting to download",
            super::duplicates::plural(queued, "photo")
        )));
    }
    for item in board.iter().filter(|i| i.kind == Kind::Library) {
        out.push(Row {
            since: Some(item.started),
            ..Row::new(item.what.clone())
        });
    }
    if let Some((done, total)) = app.scan_progress {
        out.push(Row::new("Reading photos").progress(done, total));
    } else if app.scanning {
        out.push(Row::new("Scanning"));
    }
    let busy = app.loader.busy();
    if busy > 0 {
        out.push(Row::new(format!("Loading {busy} thumbnails")));
    }
    if let Some((done, total)) = app.export.progress {
        out.push(Row::new("Exporting").progress(done, total));
    }
    if let Some(f) = app.merge.progress {
        out.push(Row {
            stop: Some(Stop::Merge),
            ..Row::new(format!("{} {:.0}%", super::merge::status(app), f * 100.0))
        });
    }
    if let Some(task) = crate::panels::semantic_task(&app.sem.status) {
        out.push(Row::new(task));
    }
    if let Some(task) = crate::panels::people_task(&app.ppl.status) {
        out.push(Row::new(task));
    }
    out
}

/// "12 s", "3 min", "1 h 5 min".
pub(crate) fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

pub fn show(app: &mut ShoeboxApp, ui: &mut egui::Ui) {
    ui.heading("Activity");
    ui.weak(
        "What's running now. A file in the cloud only arrives when its drive sends it: \
         nothing Shoebox does makes that faster.",
    );
    ui.separator();
    let rows = rows(app);
    if rows.is_empty() {
        ui.label("Nothing running.");
        return;
    }
    // Times move on while anything runs.
    ui.ctx().request_repaint_after(Duration::from_secs(1));
    let now = Instant::now();
    let mut stop = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in &rows {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.add(egui::Label::new(&row.what).extend());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(s) = row.stop
                            && ui.button("Stop").clicked()
                        {
                            stop = Some(s);
                        }
                        if let Some(t) = row.since {
                            ui.weak(elapsed(now.duration_since(t)));
                        }
                        if let Some((done, total)) = row.progress.filter(|p| p.1 > 0) {
                            ui.add(
                                egui::ProgressBar::new(done as f32 / total as f32)
                                    .desired_width(180.0)
                                    .text(format!("{done} of {total}")),
                            );
                        }
                    });
                });
                if let Some(d) = &row.detail {
                    // The file's name first: a long folder is cut short.
                    let path = std::path::Path::new(d);
                    let text = match (path.file_name(), path.parent()) {
                        (Some(name), Some(dir)) => format!(
                            "{}  ·  {}",
                            name.to_string_lossy(),
                            crate::panels::short_path(dir)
                        ),
                        _ => d.clone(),
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.add(egui::Label::new(RichText::new(text).weak().small()).truncate())
                            .on_hover_text(d);
                    });
                }
                ui.add_space(6.0);
                ui.separator();
            }
        });
    match stop {
        Some(Stop::Import) => app.cancel_import(),
        Some(Stop::Icloud) => super::icloud::cancel(app),
        Some(Stop::Merge) => super::merge::stop(app),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_reads_naturally() {
        assert_eq!(elapsed(Duration::from_secs(12)), "12 s");
        assert_eq!(elapsed(Duration::from_secs(190)), "3 min");
        assert_eq!(elapsed(Duration::from_secs(3900)), "1 h 5 min");
    }
}
