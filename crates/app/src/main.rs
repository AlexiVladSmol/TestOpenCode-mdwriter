#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tree;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;

use graph::GraphView;
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
    show_graph: bool,
    search_query: String,
    quick_open: Option<QuickOpen>,
    graph: GraphView,
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
            show_graph: false,
            search_query: String::new(),
            quick_open: None,
            graph: GraphView::default(),
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
        self.graph.update(&self.index);
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
    /// Несуществующую цель создаёт (как Obsidian) рядом с текущей заметкой.
    fn open_wiki(&mut self, action: &preview::Action) -> Option<PathBuf> {
        match action {
            preview::Action::OpenWiki(target) => {
                let from = self.open.clone()?;
                if let Some(rel) = self.index.resolve_wikilink(&from, target) {
                    return Some(rel);
                }
                let name = if target.ends_with(".md") {
                    target.to_string()
                } else {
                    format!("{target}.md")
                };
                let dir = from.parent().unwrap_or(Path::new(""));
                let rel = dir.join(name);
                match self.vault.create(&rel) {
                    Ok(()) => {
                        self.rescan();
                        Some(rel)
                    }
                    Err(e) => {
                        eprintln!("failed to create {rel:?}: {e}");
                        None
                    }
                }
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

    /// Создаёт/открывает ежедневную заметку `ГГГГ-ММ-ДД.md` в корне vault.
    fn open_today(&mut self) {
        let Some(vs) = self.vault.as_mut() else {
            return;
        };
        let rel = PathBuf::from(format!("{}.md", today_str()));
        if let Err(e) = vs.vault.create(&rel) {
            eprintln!("failed to create daily note: {e}");
            return;
        }
        vs.rescan();
        vs.load(rel);
    }

    /// Экспорт открытой заметки в HTML.
    fn export_html(&mut self) {
        let Some(vs) = &self.vault else { return };
        let Some(rel) = &vs.open else { return };
        let Ok(text) = vs.vault.read(rel) else {
            return;
        };
        let default_name = format!(
            "{}.html",
            rel.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "note".to_string())
        );
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };

        let mut options = pulldown_cmark::Options::empty();
        options.insert(pulldown_cmark::Options::ENABLE_GFM);
        options.insert(pulldown_cmark::Options::ENABLE_MATH);

        let mut html = String::from(
            "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>",
        );
        html.push_str(&escape_html(&default_name));
        html.push_str(
            "</title>\n<style>body{max-width:760px;margin:2em auto;padding:0 1em;font-family:\
             system-ui,sans-serif;line-height:1.6}pre{background:#f5f5f5;padding:1em;\
             overflow:auto}code{background:#f5f5f5;padding:.1em .3em}blockquote{border-left:\
             4px solid #ccc;margin-left:0;padding-left:1em;color:#555}</style>\n</head>\n<body>\n",
        );
        pulldown_cmark::html::push_html(
            &mut html,
            pulldown_cmark::Parser::new_ext(&text, options),
        );
        html.push_str("\n</body>\n</html>\n");

        if let Err(e) = std::fs::write(&path, html) {
            eprintln!("export failed: {e}");
        }
    }
}

/// Текущая дата (UTC) в формате `ГГГГ-ММ-ДД`.
fn today_str() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    // civil_from_days (алгоритм Говарда Хиннанта)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);

        // Глобальные шорткаты (читаем до того, как виджеты успеют поглотить события).
        let ctrl_o = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O));
        let ctrl_s = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
        let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));

        if let Some(vs) = self.vault.as_mut() {
            if vs.watcher.has_changes() {
                vs.rescan();
            }
            vs.maybe_autosave(now, ui.ctx());
            if ctrl_s {
                vs.flush();
            }
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
                let mut open_today = false;
                let mut export_html = false;
                if let Some(vs) = self.vault.as_mut() {
                    ui.separator();
                    ui.toggle_value(&mut vs.show_preview, "Preview");
                    ui.toggle_value(&mut vs.show_graph, "Graph");
                    ui.separator();
                    if ui
                        .button("Today")
                        .on_hover_text("Ежедневная заметка ГГГГ-ММ-ДД")
                        .clicked()
                    {
                        open_today = true;
                    }
                    if ui.button("Export HTML").clicked() {
                        export_html = true;
                    }
                    ui.separator();
                    ui.label(vs.vault.root().display().to_string());
                }
                ui.separator();
                let mut dark = ui.ctx().theme() == egui::Theme::Dark;
                if ui.toggle_value(&mut dark, "Dark").changed() {
                    ui.ctx().set_theme(if dark {
                        egui::ThemePreference::Dark
                    } else {
                        egui::ThemePreference::Light
                    });
                }
                if open_today {
                    self.open_today();
                }
                if export_html {
                    self.export_html();
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

                // Граф связей.
                if vs.show_graph {
                    let mut open = true;
                    let mut clicked: Option<PathBuf> = None;
                    egui::Window::new("Граф связей")
                        .open(&mut open)
                        .default_size([520.0, 420.0])
                        .resizable(true)
                        .show(ui.ctx(), |ui| {
                            for _ in 0..3 {
                                vs.graph.step();
                            }
                            let (response, painter) =
                                ui.allocate_painter(ui.available_size(), egui::Sense::click());
                            let rect = response.rect;
                            if rect.width() < 2.0 || rect.height() < 2.0 {
                                return;
                            }
                            painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

                            let n = vs.graph.node_count();
                            if n == 0 {
                                painter.text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "Пока нет заметок",
                                    egui::FontId::proportional(14.0),
                                    ui.visuals().weak_text_color(),
                                );
                                return;
                            }

                            // Границы позиций и масштаб.
                            let mut min = egui::Vec2::splat(f32::INFINITY);
                            let mut max = egui::Vec2::splat(f32::NEG_INFINITY);
                            for i in 0..n {
                                if let Some((_, _, p)) = vs.graph.node(i) {
                                    let p = p.to_vec2();
                                    min = min.min(p);
                                    max = max.max(p);
                                }
                            }
                            let size = (max - min).max(egui::Vec2::splat(1.0));
                            let scale = ((rect.width() - 60.0) / size.x)
                                .min((rect.height() - 60.0) / size.y)
                                .min(4.0);
                            let offset = rect.center() - (min + max) * 0.5 * scale;
                            let map = |p: egui::Pos2| {
                                egui::Pos2::new(offset.x + p.x * scale, offset.y + p.y * scale)
                            };

                            // Рёбра.
                            let stroke = egui::Stroke::new(
                                1.0,
                                ui.visuals().widgets.noninteractive.bg_stroke.color,
                            );
                            for (a, b) in vs.graph.edge_pairs() {
                                let (Some((_, _, pa)), Some((_, _, pb))) =
                                    (vs.graph.node(a), vs.graph.node(b))
                                else {
                                    continue;
                                };
                                painter.line_segment([map(pa), map(pb)], stroke);
                            }

                            // Узлы.
                            let click_pos = response
                                .clicked()
                                .then(|| response.interact_pointer_pos())
                                .flatten()
                                .unwrap_or(rect.center());
                            for i in 0..n {
                                let Some((title, path, p)) = vs.graph.node(i) else {
                                    continue;
                                };
                                let center = map(p);
                                let is_open = vs.open.as_deref() == Some(path);
                                let color = if is_open {
                                    ui.visuals().selection.bg_fill
                                } else {
                                    ui.visuals().widgets.inactive.fg_stroke.color
                                };
                                painter.circle_filled(
                                    center,
                                    if is_open { 7.0 } else { 5.0 },
                                    color,
                                );
                                painter.text(
                                    center + egui::vec2(0.0, 10.0),
                                    egui::Align2::CENTER_TOP,
                                    title,
                                    egui::FontId::proportional(11.0),
                                    ui.visuals().text_color(),
                                );
                                if response.clicked() && (click_pos - center).length() < 14.0 {
                                    clicked = Some(path.to_path_buf());
                                }
                            }
                        });

                    vs.show_graph = open;
                    if let Some(rel) = clicked {
                        vs.load(rel);
                    }
                }

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
