#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tree;

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;

use vault::{Vault, VaultWatcher};

const AUTOSAVE_DELAY: f64 = 1.0;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([700.0, 450.0])
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
    vault: Option<VaultState>,
}

struct VaultState {
    vault: Vault,
    watcher: VaultWatcher,
    notes: Vec<PathBuf>,
    tree: Vec<tree::TreeNode>,
    open: Option<PathBuf>,
    editor_text: String,
    dirty: bool,
    last_edit: Option<f64>,
    new_note_name: String,
    show_preview: bool,
}

impl VaultState {
    fn open(vault: Vault, watcher: VaultWatcher) -> Self {
        let mut s = Self {
            vault,
            watcher,
            notes: Vec::new(),
            tree: Vec::new(),
            open: None,
            editor_text: String::new(),
            dirty: false,
            last_edit: None,
            new_note_name: String::new(),
            show_preview: true,
        };
        s.rescan();
        s
    }

    fn rescan(&mut self) {
        if let Ok(notes) = self.vault.list_notes() {
            self.notes = notes;
            self.tree = tree::build_tree(&self.notes);
        }
        // Если открытая заметка исчезла — закрываем её.
        if let Some(rel) = &self.open {
            if !self.notes.contains(rel) {
                self.open = None;
                self.editor_text.clear();
                self.dirty = false;
                self.last_edit = None;
            }
        }
    }

    fn load(&mut self, rel: PathBuf) {
        self.flush();
        match self.vault.read(&rel) {
            Ok(text) => {
                self.editor_text = text;
                self.dirty = false;
                self.last_edit = None;
                self.open = Some(rel);
            }
            Err(e) => eprintln!("failed to read {}: {e}", rel.display()),
        }
    }

    fn flush(&mut self) {
        if self.dirty {
            if let Some(rel) = &self.open {
                if let Err(e) = self.vault.write(rel, &self.editor_text) {
                    eprintln!("failed to save {}: {e}", rel.display());
                }
            }
            self.dirty = false;
        }
    }

    fn maybe_autosave(&mut self, now: f64, ctx: &egui::Context) {
        if !self.dirty {
            return;
        }
        match self.last_edit {
            None => self.flush(),
            Some(t) => {
                let elapsed = now - t;
                if elapsed >= AUTOSAVE_DELAY {
                    self.flush();
                } else {
                    ctx.request_repaint_after(Duration::from_secs_f64(AUTOSAVE_DELAY - elapsed));
                }
            }
        }
    }

    fn create_note(&mut self) {
        let name = self.new_note_name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let name = if name.ends_with(".md") {
            name
        } else {
            format!("{name}.md")
        };
        let rel = PathBuf::from(name);
        if let Err(e) = self.vault.create(&rel) {
            eprintln!("failed to create {rel:?}: {e}");
            return;
        }
        self.new_note_name.clear();
        self.rescan();
        self.load(rel);
    }
}

impl App {
    fn open_vault(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            match VaultWatcher::new(&path) {
                Ok(watcher) => {
                    self.vault = Some(VaultState::open(Vault::open(path), watcher));
                }
                Err(e) => eprintln!("watch error: {e}"),
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);

        if let Some(vs) = self.vault.as_mut() {
            if vs.watcher.has_changes() {
                vs.rescan();
            }
            vs.maybe_autosave(now, ui.ctx());
        }

        egui::Panel::top("menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("mdwriter");
                ui.separator();
                if ui.button("Open vault…").clicked() {
                    self.open_vault();
                }
                if let Some(vs) = self.vault.as_mut() {
                    ui.separator();
                    ui.toggle_value(&mut vs.show_preview, "Preview");
                    ui.separator();
                    ui.label(vs.vault.root().display().to_string());
                }
            });
        });

        match self.vault.as_mut() {
            None => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        if ui.button("Open vault…").clicked() {
                            self.open_vault();
                        }
                    });
                });
            }
            Some(vs) => {
                egui::Panel::left("tree")
                    .resizable(true)
                    .default_size(240.0)
                    .min_size(150.0)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let name_edit = egui::TextEdit::singleline(&mut vs.new_note_name)
                                .hint_text("note.md")
                                .desired_width(120.0);
                            let resp = ui.add(name_edit);
                            let enter = resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.button("+").on_hover_text("Новая заметка").clicked()
                                || enter
                            {
                                vs.create_note();
                            }
                        });
                        ui.separator();

                        let selected = vs.open.clone();
                        let clicked = egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| tree::show_tree(ui, &vs.tree, &selected))
                            .inner;
                        if let Some(rel) = clicked {
                            vs.load(rel);
                        }
                    });

                if vs.show_preview {
                    egui::Panel::right("preview")
                        .resizable(true)
                        .default_size(560.0)
                        .min_size(250.0)
                        .show(ui, |ui| {
                            ui.add_space(4.0);
                            ui.strong("Preview");
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.add_space(6.0);
                                    preview::markdown(ui, &vs.editor_text);
                                });
                        });
                }

                egui::CentralPanel::default().show(ui, |ui| {
                    if vs.open.is_some() {
                        ui.horizontal(|ui| {
                            ui.strong(
                                vs.open
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default(),
                            );
                            if vs.dirty {
                                ui.label("●");
                            }
                        });
                        ui.separator();

                        let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(
                            ui.ctx(),
                            ui.style(),
                        );
                        let mut layouter =
                            |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                                let mut job = egui_extras::syntax_highlighting::highlight(
                                    ui.ctx(),
                                    ui.style(),
                                    &theme,
                                    buf.as_str(),
                                    "md",
                                );
                                job.wrap.max_width = wrap_width;
                                ui.fonts_mut(|f| f.layout_job(job))
                            };

                        let output = egui::ScrollArea::both()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.horizontal_top(|ui| {
                                    // Нумерация строк: отдельный gutter слева от редактора.
                                    let n_lines = vs.editor_text.lines().count();
                                    let digits = n_lines.to_string().len().max(2);
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 0.0;
                                        for i in 1..=n_lines {
                                            ui.monospace(format!("{i:>digits$} "));
                                        }
                                    });
                                    egui::TextEdit::multiline(&mut vs.editor_text)
                                        .code_editor()
                                        .frame(egui::Frame::NONE)
                                        .margin(egui::Margin::symmetric(4, 0))
                                        .desired_width(f32::INFINITY)
                                        .layouter(&mut layouter)
                                        .show(ui)
                                })
                                .inner
                            })
                            .inner;

                        if output.response.changed() {
                            vs.dirty = true;
                            vs.last_edit = Some(now);
                        }
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("Выберите заметку слева или создайте новую.");
                        });
                    }
                });
            }
        }
    }
}
