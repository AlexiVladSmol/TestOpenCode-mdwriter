#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([600.0, 400.0])
            .with_title("mdwriter"),
        ..Default::default()
    };

    eframe::run_native(
        "mdwriter",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

#[derive(Default)]
struct App {
    vault_path: Option<std::path::PathBuf>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("mdwriter");
                ui.separator();
                if ui.button("Open vault…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.vault_path = Some(path);
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            match &self.vault_path {
                Some(path) => {
                    ui.label(format!("Vault: {}", path.display()));
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label("Откройте vault (папку с .md файлами), чтобы начать.");
                    });
                }
            }
        });
    }
}
