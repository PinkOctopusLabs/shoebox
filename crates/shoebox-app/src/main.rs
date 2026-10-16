//! Shoebox: a fast, local-first photo library browser.

use eframe::egui;
use shoebox_app::{AppPaths, ShoeboxApp};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    tracing::info!("starting Shoebox {}", env!("CARGO_PKG_VERSION"));
    let paths = AppPaths::default_locations()?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Shoebox")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Shoebox",
        options,
        Box::new(|cc| Ok(Box::new(ShoeboxApp::new(cc, paths)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}
