//! Shoebox: an empty three-panel window for M0.

use eframe::egui;

struct ShoeboxApp {
    left_open: bool,
    right_open: bool,
    bars_open: bool,
}

impl Default for ShoeboxApp {
    fn default() -> Self {
        Self {
            left_open: true,
            right_open: true,
            bars_open: true,
        }
    }
}

impl ShoeboxApp {
    /// Tab folds the side panels away; Shift+Tab hides every panel, as in Lightroom.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (tab, shift) = ctx.input_mut(|i| {
            let shift = i.modifiers.shift;
            let tab = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
            (tab, shift)
        });
        if !tab {
            return;
        }
        let any_side = self.left_open || self.right_open;
        if shift {
            let show = !(any_side || self.bars_open);
            self.left_open = show;
            self.right_open = show;
            self.bars_open = show;
        } else {
            self.left_open = !any_side;
            self.right_open = !any_side;
        }
    }
}

impl eframe::App for ShoeboxApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_keys(ui.ctx());

        let mut bars_open = self.bars_open;
        egui::Panel::top("toolbar").show_collapsible(ui, &mut bars_open, |ui| {
            ui.horizontal(|ui| {
                for view in ["Grid", "Masonry", "Loupe", "Compare", "Survey", "Timeline"] {
                    let _ = ui.selectable_label(view == "Grid", view);
                }
            });
        });
        egui::Panel::bottom("status").show_collapsible(ui, &mut bars_open, |ui| {
            ui.label("0 selected / 0 filtered / 0 total");
        });
        self.bars_open = bars_open;

        egui::Panel::left("library")
            .resizable(true)
            .default_size(260.0)
            .show_collapsible(ui, &mut self.left_open, |ui| {
                ui.heading("Library");
                ui.separator();
                ui.label("Events");
                ui.label("Collections");
                ui.label("Smart filters");
                ui.separator();
                ui.label("Tasks");
            });
        egui::Panel::right("info")
            .resizable(true)
            .default_size(280.0)
            .show_collapsible(ui, &mut self.right_open, |ui| {
                ui.heading("Info");
                ui.separator();
                ui.heading("Organise");
            });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.centered_and_justified(|ui| {
                ui.weak("No library roots yet");
            });
        });
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    tracing::info!("starting Shoebox {}", env!("CARGO_PKG_VERSION"));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Shoebox")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Shoebox",
        options,
        Box::new(|_cc| Ok(Box::new(ShoeboxApp::default()))),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}
