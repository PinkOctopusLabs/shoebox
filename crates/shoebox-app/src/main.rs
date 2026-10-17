//! Shoebox: a fast, local-first photo library browser.

use std::io::IsTerminal;

use eframe::egui;
use shoebox_app::{AppPaths, ShoeboxApp};

/// The app icon (also used for the bundle, see `scripts/bundle-macos.sh`).
const ICON: &[u8] = include_bytes!("../../../assets/icon-1024.png");

/// Where logs go when there's no terminal (launched from Finder or the
/// Dock): `~/Library/Logs/Shoebox/shoebox.log` on macOS,
/// `$XDG_STATE_HOME/shoebox/shoebox.log` (default `~/.local/state`) elsewhere.
fn log_file() -> Option<std::path::PathBuf> {
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    #[cfg(target_os = "macos")]
    let dir = home.join("Library/Logs/Shoebox");
    #[cfg(not(target_os = "macos"))]
    let dir = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".local/state"), std::path::PathBuf::from)
        .join("shoebox");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("shoebox.log"))
}

fn init_logging() {
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };
    let file = (!std::io::stderr().is_terminal())
        .then(log_file)
        .flatten()
        .and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });
    match file {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter()).init(),
    }
    // A crash should leave a trace in the log, not just vanish.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default(info);
    }));
}

fn icon() -> Option<egui::IconData> {
    let img = image::load_from_memory_with_format(ICON, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("Shoebox {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    init_logging();
    tracing::info!("starting Shoebox {}", env!("CARGO_PKG_VERSION"));
    let paths = AppPaths::default_locations()?;

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Shoebox")
        .with_app_id("com.crankyunicorn.shoebox")
        .with_inner_size([1400.0, 900.0])
        .with_min_inner_size([800.0, 500.0]);
    if let Some(i) = icon() {
        viewport = viewport.with_icon(i);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Shoebox",
        options,
        Box::new(|cc| {
            let app = ShoeboxApp::new(cc, paths);
            shoebox_app::menu::install(&cc.egui_ctx);
            shoebox_app::updater::start(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}
