#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tree;

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;

use index::Index;
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

/// Состояние быстрого переключателя (Ctrl+O).
struct QuickOpen {
    query: String,
    selected: usize,
}

struct VaultState {
    vault: Vault,
    watcher: VaultWatcher,
    notes: Vec<PathBuf>,
    tree: Vec<tree::TreeNode>,
    index: Index,
    open: Option<PathBuf>,
    editor_text: String,
    dirty: bool,
    last_edit: Option<f64>,
    new_note_name: String,
    show_preview: bool,
    search_query: String,
    quick_open: Option<QuickOpen>,
}

impl VaultState {
    fn open(vault: Vault, watcher: VaultWatcher) -> Self {
        let mut s = Self {
            vault,
            watcher,
            notes: Vec::new(),
            tree: Vec::new(),
            index: Index::default(),
            open: None,
            editor_text: String::new(),
            dirty: false,
            last_edit: None,
            new_note_name: String::new(),
            show_preview: true,
            search_query: String::new(),
            quick_open: None,
        };
        s.rescan();
        s
    }

    fn rescan(&mut self) {
        if let Ok(notes) = self.vault.list_notes() {
            self.notes = notes;
            self.tree = tree::build_tree(&self.notes);
        }
        self.index = Index::build(&self.vault).unwrap_or_default();
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

    /// Разрешает действие предпросмотра в путь заметки.
    fn open_wiki(&self, action: &preview::Action) -> Option<PathBuf> {
        match action {
            preview::Action::OpenWiki(target) => {
                let from = self.open.as_deref()?;
                self.index.resolve_wikilink(from, target)
            }
        }
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

        // Глобальные шорткаты (читаем до того, как виджеты успеют поглотить события).
        let ctrl_o = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O));
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
        let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));

        if let Some(vs) = self.vault.as_mut() {
            if vs.watcher.has_changes() {
                vs.rescan();
            }
            vs.maybe_autosave(now, ui.ctx());
            if ctrl_o {
                vs.quick_open = Some(QuickOpen {
                    query: String::new(),
                    selected: 0,
                });
            }
            if vs.quick_open.is_none() && esc && !vs.search_query.is_empty() {
                vs.search_query.clear();
            }
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
                let tree_clicked = egui::Panel::left("tree")
                    .resizable(true)
                    .default_size(240.0)
                    .min_size(150.0)
                    .show(ui, |ui| {
                        let mut clicked: Option<PathBuf> = None;

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let name_edit =
                                egui::TextEdit::singleline(&mut vs.new_note_name)
                                    .hint_text("note.md")
                                    .desired_width(110.0);
                            let resp = ui.add(name_edit);
                            let enter = resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.button("+").on_hover_text("Новая заметка").clicked()
                                || enter
                            {
                                vs.create_note();
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut vs.search_query)
                                    .hint_text("Поиск…")
                                    .desired_width(f32::INFINITY),
                            );
                            if ui.button("✕").clicked() {
                                vs.search_query.clear();
                            }
                        });
                        ui.separator();

                        if !vs.search_query.is_empty() {
                            let results = vs.index.search(&vs.search_query, 30);
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if results.is_empty() {
                                        ui.weak("Ничего не найдено");
                                    }
                                    for (rel, snippet) in results {
                                        let title =
                                            vs.index.title(&rel).unwrap_or("").to_string();
                                        ui.vertical(|ui| {
                                            if ui
                                                .selectable_label(
                                                    false,
                                                    egui::RichText::new(&title).strong(),
                                                )
                                                .clicked()
                                            {
                                                clicked = Some(rel.clone());
                                            }
                                            ui.label(
                                                egui::RichText::new(snippet)
                                                    .weak()
                                                    .small(),
                                            );
                                        });
                                    }
                                });
                        } else {
                            let selected = vs.open.clone();
                            let tree_click = egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| tree::show_tree(ui, &vs.tree, &selected))
                                .inner;
                            if let Some(rel) = tree_click {
                                clicked = Some(rel);
                            }
                        }

                        ui.separator();
                        egui::CollapsingHeader::new("Backlinks")
                            .default_open(true)
                            .show(ui, |ui| {
                                let Some(open) = vs.open.clone() else {
                                    ui.weak("Откройте заметку");
                                    return;
                                };
                                let backlinks = vs.index.backlinks(&open);
                                if backlinks.is_empty() {
                                    ui.weak("Нет обратных ссылок");
                                }
                                for rel in backlinks {
                                    let title =
                                        vs.index.title(&rel).unwrap_or("").to_string();
                                    let label = format!("{title}  ·  {}", rel.display());
                                    if ui.selectable_label(false, label).clicked() {
                                        clicked = Some(rel);
                                    }
                                }
                            });

                        clicked
                    })
                    .inner;
                if let Some(rel) = tree_clicked {
                    vs.load(rel);
                }

                if vs.show_preview {
                    let actions = egui::Panel::right("preview")
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
                                    preview::markdown(ui, &vs.editor_text)
                                })
                                .inner
                        })
                        .inner;
                    for action in actions {
                        if let Some(rel) = vs.open_wiki(&action) {
                            vs.load(rel);
                        }
                    }
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

                        let theme =
                            egui_extras::syntax_highlighting::CodeTheme::from_memory(
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

                        let n_lines = vs.editor_text.lines().count();
                        let digits = n_lines.to_string().len().max(2);
                        let mut edit = egui::TextEdit::multiline(&mut vs.editor_text)
                            .code_editor()
                            .frame(egui::Frame::NONE)
                            .margin(egui::Margin::symmetric(4, 0))
                            .desired_width(f32::INFINITY)
                            .layouter(&mut layouter);
                        if vs.quick_open.is_some() {
                            edit = edit.interactive(false);
                        }

                        let output = egui::ScrollArea::both()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.horizontal_top(|ui| {
                                    // Нумерация строк: отдельный gutter слева от редактора.
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 0.0;
                                        for i in 1..=n_lines {
                                            ui.monospace(format!("{i:>digits$} "));
                                        }
                                    });
                                    edit.show(ui)
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

                // Быстрый переключатель (Ctrl+O).
                if vs.quick_open.is_some() {
                    let modal = egui::Modal::new(egui::Id::unique("quick_open"))
                        .area(
                            egui::Area::new(egui::Id::unique("quick_open_area"))
                                .kind(egui::UiKind::Modal)
                                .anchor(egui::Align2::CENTER_TOP, egui::Vec2::ZERO)
                                .order(egui::Order::Foreground)
                                .interactable(true)
                                .sense(egui::Sense::hover()),
                        )
                        .show(ui.ctx(), |ui| {
                            let qo = vs.quick_open.as_mut().unwrap();
                            ui.add_space(60.0);
                            ui.set_min_width(420.0);
                            ui.strong("Быстрый переход");
                            let text_out = ui.add(
                                egui::TextEdit::singleline(&mut qo.query)
                                    .hint_text("Название заметки…")
                                    .desired_width(f32::INFINITY)
                                    .id_salt("quick_input"),
                            );
                            text_out.request_focus();
                            if text_out.changed() {
                                qo.selected = 0;
                            }

                            let entries: Vec<(PathBuf, String)> = {
                                let q = qo.query.trim().to_lowercase();
                                vs.index
                                    .switcher_entries()
                                    .into_iter()
                                    .filter(|(p, t)| {
                                        q.is_empty()
                                            || t.to_lowercase().contains(&q)
                                            || p.to_string_lossy().to_lowercase().contains(&q)
                                    })
                                    .take(20)
                                    .collect()
                            };

                            let mut chosen: Option<PathBuf> = None;
                            let mut close = false;
                            if down {
                                qo.selected = (qo.selected + 1)
                                    .min(entries.len().saturating_sub(1));
                            }
                            if up {
                                qo.selected = qo.selected.saturating_sub(1);
                            }
                            if enter {
                                if let Some((p, _)) = entries.get(qo.selected) {
                                    chosen = Some(p.clone());
                                    close = true;
                                }
                            }
                            if esc {
                                close = true;
                            }

                            ui.separator();
                            if entries.is_empty() {
                                ui.weak("Ничего не найдено");
                            }
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                for (i, (p, t)) in entries.iter().enumerate() {
                                    let label = format!("{t}  ·  {}", p.display());
                                    if ui.selectable_label(i == qo.selected, label).clicked() {
                                        chosen = Some(p.clone());
                                        close = true;
                                    }
                                }
                            });
                            ui.separator();
                            ui.weak("Enter — открыть · Esc — закрыть");

                            (chosen, close)
                        });

                    if let Some(p) = modal.inner.0 {
                        vs.load(p);
                    }
                    let closed = modal.inner.1 || modal.backdrop_response.clicked();
                    if closed {
                        vs.quick_open = None;
                    }
                }
            }
        }
    }
}
