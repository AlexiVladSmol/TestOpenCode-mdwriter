#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daily;
mod export;
mod fonts;
mod format;
mod notice;
mod settings;
mod state;
mod sync;
mod theme;
mod tree;
mod ui;

use std::path::PathBuf;

use eframe::egui;

use vault::{Vault, VaultWatcher};

use crate::notice::Notices;
use crate::settings::Settings;
use crate::state::{VaultState, ViewMode};

fn main() -> eframe::Result {
    let settings = settings::load_settings();
    let window_size = settings
        .window_size
        .map(|[w, h]| egui::vec2(w.max(600.0), h.max(400.0)))
        .unwrap_or_else(|| egui::vec2(1100.0, 720.0));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(window_size)
            .with_min_inner_size([700.0, 450.0])
            .with_title("mdwriter"),
        ..Default::default()
    };

    eframe::run_native(
        "mdwriter",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct App {
    vault: Option<VaultState>,
    settings: Settings,
    settings_dirty: bool,
    /// Уведомления пользователю (общая очередь, клон есть у `VaultState`).
    notices: Notices,
    /// Контекст egui: через него наблюдатель за файлами будит окно.
    ctx: egui::Context,
    /// Открытая модалка выбора папки (vault или экспорт).
    picker: Option<ui::folder_picker::Picker>,
}

/// Колбэк для наблюдателя: из фонового потока просит перерисовать окно,
/// чтобы внешние изменения были видны без движения мыши.
fn repaint_on_change(ctx: &egui::Context) -> impl Fn() + Send + 'static {
    let ctx = ctx.clone();
    move || ctx.request_repaint()
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Регистрируем жирный системный шрифт (если найден).
        if let Some(bold) = fonts::load_bold_font() {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("mdwriter-bold".to_string(), std::sync::Arc::new(bold));
            fonts.families.insert(
                egui::FontFamily::Name("mdwriter-bold".into()),
                vec!["mdwriter-bold".to_string()],
            );
            cc.egui_ctx.set_fonts(fonts);
        }

        let settings = settings::load_settings();
        theme::apply_theme(&cc.egui_ctx, settings.theme);

        let mut app = App {
            vault: None,
            settings,
            settings_dirty: false,
            notices: Notices::default(),
            ctx: cc.egui_ctx.clone(),
            picker: None,
        };

        // Восстанавливаем последний vault.
        if let Some(path) = app.settings.last_vault.clone() {
            if path.is_dir() {
                match VaultWatcher::new(&path, repaint_on_change(&app.ctx)) {
                    Ok(watcher) => {
                        let mut vs =
                            VaultState::open(Vault::open(path), watcher, app.notices.clone());
                        vs.view_mode = app.settings.view_mode;
                        vs.show_graph = app.settings.show_graph;
                        vs.tree_w = app.settings.tree_w;
                        app.vault = Some(vs);
                    }
                    Err(e) => app.notices.error(format!(
                        "Не удалось открыть последний vault {}: {e}",
                        path.display()
                    )),
                }
            } else {
                app.notices
                    .info(format!("Последний vault не найден: {}", path.display()));
            }
        }

        app
    }

    /// Открывает модалку выбора папки (вместо системного диалога).
    fn start_vault_picker(&mut self) {
        let start = self.start_dir();
        self.picker = Some(ui::folder_picker::Picker::open_vault(start));
    }

    /// Папка, с которой начинается просмотр: корень открытого vault, затем
    /// последний сохранённый vault, затем домашняя папка.
    fn start_dir(&self) -> PathBuf {
        if let Some(vs) = &self.vault {
            return vs.vault.root().to_path_buf();
        }
        if let Some(last) = &self.settings.last_vault {
            if last.is_dir() {
                return last.clone();
            }
        }
        ui::folder_picker::home_dir().unwrap_or_else(|| PathBuf::from("."))
    }

    fn open_vault(&mut self, path: PathBuf) {
        // Перед сменой vault дописываем несохранённую заметку старого vault.
        // Если сохранить не удалось ни в файл, ни копией — остаёмся,
        // иначе правки пропадут вместе со старым VaultState.
        if let Some(vs) = &mut self.vault {
            if vs.flush_or_copy().is_err() {
                self.notices
                    .error("Vault не сменён: не удалось сохранить открытую заметку".to_string());
                return;
            }
        }
        match VaultWatcher::new(&path, repaint_on_change(&self.ctx)) {
            Ok(watcher) => {
                // Недоступные подпапки (нет прав, кончились inotify watches) не
                // мешают открыть vault — просто предупреждаем один раз.
                let skipped = watcher.skipped().len();
                let first_skipped = watcher.skipped().first().cloned();
                let mut vs =
                    VaultState::open(Vault::open(path.clone()), watcher, self.notices.clone());
                vs.view_mode = self.settings.view_mode;
                vs.show_graph = self.settings.show_graph;
                vs.tree_w = self.settings.tree_w;
                self.vault = Some(vs);
                self.settings.last_vault = Some(path);
                self.settings_dirty = true;
                if skipped > 0 {
                    let example = first_skipped.unwrap_or_default();
                    self.notices.info(format!(
                        "Vault открыт, но слежение недоступно для {skipped} папок (например, \
                         {example}): изменения в них не подхватятся автоматически"
                    ));
                }
            }
            Err(e) => self
                .notices
                .error(format!("Не удалось открыть vault {}: {e}", path.display())),
        }
    }

    /// Синхронизирует UI-состояние в настройки и сохраняет при изменениях.
    fn sync_settings(&mut self, ctx: &egui::Context) {
        if let Some(vs) = &self.vault {
            if vs.view_mode != self.settings.view_mode {
                self.settings.view_mode = vs.view_mode;
                self.settings_dirty = true;
            }
            if vs.show_graph != self.settings.show_graph {
                self.settings.show_graph = vs.show_graph;
                self.settings_dirty = true;
            }
            if vs.tree_w != self.settings.tree_w {
                self.settings.tree_w = vs.tree_w;
                self.settings_dirty = true;
            }
        }

        // Размер окна сохраняем всегда (но без флага dirty — пишем при выходе).
        if let Some(size) = ctx
            .input(|i| i.viewport().inner_rect)
            .map(|r| [r.width(), r.height()])
        {
            self.settings.window_size = Some(size);
        }

        if settings::should_save_settings(self.settings_dirty, ctx.input(|i| i.pointer.any_down()))
        {
            self.save_settings();
        }
    }

    fn save_settings(&mut self) {
        if let Err(msg) = settings::save(&self.settings) {
            self.notices.error(msg);
        }
        self.settings_dirty = false;
    }
}

impl App {
    /// Создаёт/открывает ежедневную заметку `ГГГГ-ММ-ДД.md` в корне vault.
    fn open_today(&mut self) {
        let Some(vs) = self.vault.as_mut() else {
            return;
        };
        let rel = PathBuf::from(format!("{}.md", daily::today_str()));
        if let Err(e) = vs.vault.create(&rel) {
            self.notices.error(format!(
                "Не удалось создать ежедневную заметку {}: {e}",
                rel.display()
            ));
            return;
        }
        vs.rescan();
        vs.load(rel);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        // Пока открыта модалка выбора папки, глобальные шорткаты не работают:
        // Esc должен закрывать её, а не сбрасывать поиск.
        let picker_open = self.picker.is_some();

        // Глобальные шорткаты (читаем до того, как виджеты успеют поглотить события).
        let ctrl_o = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O));
        let ctrl_s = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
        let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
        // Фокус нужен до отрисовки виджетов: состояние — с прошлого кадра.
        let search_focused = ui.memory(|m| m.has_focus(state::search_id()));

        if let Some(vs) = self.vault.as_mut() {
            vs.poll_watcher(now, ui.ctx());
            vs.maybe_autosave(now, ui.ctx());
            if !picker_open {
                if ctrl_s {
                    // Ошибка уже показана уведомлением; правки остаются несохранёнными.
                    let _ = vs.flush();
                }
                if ctrl_o {
                    vs.quick_open = Some(state::QuickOpen {
                        query: String::new(),
                        selected: 0,
                    });
                }
                if vs.quick_open.is_none()
                    && state::should_clear_search(esc, search_focused, vs.search_query.is_empty())
                {
                    vs.search_query.clear();
                }
            }
        }

        // --- Верхняя панель (собственная раскладка, без egui::Panel — в 0.37
        // панели накапливают margin в хранимом размере, из-за чего «уезжают») ---
        let full = ui.available_rect_before_wrap();
        let top_h;
        {
            let mut top_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("top")
                    .max_rect(full)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            top_ui.add_space(4.0);
            top_ui.horizontal(|ui| {
                ui.heading("mdwriter");
                ui.separator();
                if ui.button("Open vault…").clicked() {
                    self.start_vault_picker();
                }
                let mut open_today = false;
                let mut export_html = false;
                if let Some(vs) = self.vault.as_mut() {
                    ui.separator();
                    let mode = vs.view_mode;
                    if ui
                        .button(format!("View: {}", mode.label()))
                        .on_hover_text("Режим: Edit → Split → Preview")
                        .clicked()
                    {
                        vs.view_mode = mode.next();
                    }
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
                let theme = self.settings.theme;
                if ui
                    .button(format!("Theme: {}", theme.label()))
                    .on_hover_text("Тема: System → Dark → Light → Nord")
                    .clicked()
                {
                    let next = theme.next();
                    theme::apply_theme(ui.ctx(), next);
                    self.settings.theme = next;
                    self.settings_dirty = true;
                }
                if open_today {
                    self.open_today();
                }
                if export_html {
                    self.start_export_picker();
                }
            });

            // Панель форматирования текста.
            if let Some(vs) = self.vault.as_mut() {
                top_ui.horizontal(|ui| {
                    ui.add_enabled_ui(vs.open.is_some(), |ui| {
                        if ui.small_button("B").on_hover_text("Жирный").clicked() {
                            vs.wrap_selection(ui.ctx(), "**", "**");
                        }
                        if ui.small_button("I").on_hover_text("Курсив").clicked() {
                            vs.wrap_selection(ui.ctx(), "*", "*");
                        }
                        if ui.small_button("S").on_hover_text("Зачёркнутый").clicked() {
                            vs.wrap_selection(ui.ctx(), "~~", "~~");
                        }
                        if ui.small_button("`").on_hover_text("Инлайн-код").clicked() {
                            vs.wrap_selection(ui.ctx(), "`", "`");
                        }
                        ui.separator();
                        if ui.small_button("H1").clicked() {
                            vs.fmt_lines(ui.ctx(), |l| format::fmt_heading(l, 1));
                        }
                        if ui.small_button("H2").clicked() {
                            vs.fmt_lines(ui.ctx(), |l| format::fmt_heading(l, 2));
                        }
                        if ui.small_button("H3").clicked() {
                            vs.fmt_lines(ui.ctx(), |l| format::fmt_heading(l, 3));
                        }
                        ui.separator();
                        if ui.small_button("•").on_hover_text("Список").clicked() {
                            vs.fmt_lines(ui.ctx(), format::fmt_list);
                        }
                        if ui.small_button("☑").on_hover_text("Задача").clicked() {
                            vs.fmt_lines(ui.ctx(), format::fmt_task);
                        }
                        if ui.small_button("❝").on_hover_text("Цитата").clicked() {
                            vs.fmt_lines(ui.ctx(), format::fmt_quote);
                        }
                        ui.separator();
                        if ui.small_button("code").on_hover_text("Блок кода").clicked() {
                            vs.wrap_selection(ui.ctx(), "```\n", "\n```");
                        }
                        if ui.small_button("link").on_hover_text("Ссылка").clicked() {
                            vs.fmt_link(ui.ctx());
                        }
                        if ui
                            .small_button("—")
                            .on_hover_text("Горизонтальная линия")
                            .clicked()
                        {
                            vs.insert_horizontal_rule(ui.ctx());
                        }
                    });
                });
            }
            top_ui.add_space(4.0);
            top_h = top_ui.min_rect().bottom().max(1.0);
        }

        // Разделители и области: верх / дерево / основная.
        let sep_stroke =
            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
        ui.painter().hline(full.x_range(), top_h, sep_stroke);
        let tree_w = self.vault.as_ref().map(|v| v.tree_w).unwrap_or(240.0);
        let tree_rect = egui::Rect::from_min_max(
            egui::pos2(full.min.x, top_h),
            egui::pos2(full.min.x + tree_w, full.max.y),
        );
        let main_rect = egui::Rect::from_min_max(egui::pos2(full.min.x + tree_w, top_h), full.max);
        ui.painter()
            .vline(tree_rect.max.x, tree_rect.y_range(), sep_stroke);

        match self.vault.as_mut() {
            None => {
                let mut main_ui =
                    ui.new_child(egui::UiBuilder::new().id_salt("main").max_rect(main_rect));
                main_ui.shrink_clip_rect(main_rect);
                main_ui.centered_and_justified(|ui| {
                    if ui.button("Open vault…").clicked() {
                        self.start_vault_picker();
                    }
                });
            }
            Some(vs) => {
                // Дерево заметок.
                let mut tree_ui =
                    ui.new_child(egui::UiBuilder::new().id_salt("tree").max_rect(tree_rect));
                tree_ui.shrink_clip_rect(tree_rect);
                let tree_clicked = {
                    let ui = &mut tree_ui;
                    let mut clicked: Option<PathBuf> = None;

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        // Стабильный id: на него ставим фокус, если «+» или Enter
                        // нажали с пустым именем (см. ниже).
                        let name_id = egui::Id::unique("mdwriter_new_note_name");
                        let name_edit = egui::TextEdit::singleline(&mut vs.new_note_name)
                            .id(name_id)
                            .hint_text("note.md")
                            .desired_width(110.0);
                        let resp = ui.add(name_edit);
                        let enter =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("+").on_hover_text("Новая заметка").clicked() || enter
                        {
                            if vs.new_note_name.trim().is_empty() {
                                // Имя пустое — это не ошибка: Enter часто жмут,
                                // чтобы открыть заметку. Просто ставим курсор
                                // в поле, чтобы имя можно было ввести.
                                ui.memory_mut(|m| m.request_focus(name_id));
                            } else {
                                vs.create_note();
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut vs.search_query)
                                .id(state::search_id())
                                .hint_text("Поиск…")
                                .desired_width(f32::INFINITY),
                        );
                        if ui.button("✕").clicked() {
                            vs.search_query.clear();
                        }
                    });
                    ui.separator();

                    if !vs.search_query.is_empty() {
                        // Кеш внутри: пересчёт только при смене запроса/индекса.
                        let results = vs.search_results(30);
                        egui::ScrollArea::vertical()
                            .id_salt("search_list")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if results.is_empty() {
                                    ui.weak("Ничего не найдено");
                                }
                                for (rel, snippet) in results {
                                    let title = vs.index.title(&rel).unwrap_or("").to_string();
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
                                        ui.label(egui::RichText::new(snippet).weak().small());
                                    });
                                }
                            });
                    } else {
                        let selected = vs.open.clone();
                        let tree_click = egui::ScrollArea::vertical()
                            .id_salt("note_tree")
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
                                let title = vs.index.title(&rel).unwrap_or("").to_string();
                                let label = format!("{title}  ·  {}", rel.display());
                                if ui.selectable_label(false, label).clicked() {
                                    clicked = Some(rel);
                                }
                            }
                        });

                    clicked
                };
                if let Some(rel) = tree_clicked {
                    vs.load(rel);
                }

                // Основная область: редактор / предпросмотр / сплит.
                let mut main_ui =
                    ui.new_child(egui::UiBuilder::new().id_salt("main").max_rect(main_rect));
                main_ui.shrink_clip_rect(main_rect);
                match vs.view_mode {
                    ViewMode::Edit => vs.ui_editor(&mut main_ui, now),
                    ViewMode::Preview => vs.ui_preview(&mut main_ui),
                    ViewMode::Split => {
                        let full = main_ui.available_rect_before_wrap();
                        if full.width() < 620.0 {
                            // Слишком узко для сплита — только редактор.
                            vs.ui_editor(&mut main_ui, now);
                        } else {
                            const DIVIDER_W: f32 = 6.0;
                            let preview_w =
                                (full.width() * vs.preview_frac).clamp(240.0, full.width() - 340.0);
                            let divider_x = full.max.x - preview_w;
                            let editor_rect = egui::Rect::from_min_max(
                                full.min,
                                egui::pos2(divider_x, full.max.y),
                            );
                            let divider_rect = egui::Rect::from_min_max(
                                egui::pos2(divider_x, full.min.y),
                                egui::pos2(divider_x + DIVIDER_W, full.max.y),
                            );
                            let preview_rect = egui::Rect::from_min_max(
                                egui::pos2(divider_x + DIVIDER_W, full.min.y),
                                full.max,
                            );

                            let mut editor_ui = main_ui.new_child(
                                egui::UiBuilder::new()
                                    .id_salt("editor")
                                    .max_rect(editor_rect),
                            );
                            editor_ui.shrink_clip_rect(editor_rect);
                            vs.ui_editor(&mut editor_ui, now);

                            let mut preview_ui = main_ui.new_child(
                                egui::UiBuilder::new()
                                    .id_salt("preview")
                                    .max_rect(preview_rect),
                            );
                            preview_ui.shrink_clip_rect(preview_rect);
                            vs.ui_preview(&mut preview_ui);

                            // Разделитель: ширина меняется на дельту перетаскивания,
                            // поэтому случайный клик сдвигает его на пару пикселей,
                            // а не «складывает» панели.
                            let resp = main_ui.interact(
                                divider_rect,
                                egui::Id::unique("split_divider"),
                                egui::Sense::click_and_drag(),
                            );
                            if resp.dragged_by(egui::PointerButton::Primary) {
                                let delta = resp.drag_delta().x;
                                vs.preview_frac =
                                    (vs.preview_frac - delta / full.width()).clamp(0.2, 0.85);
                            }
                            let color = if resp.dragged() || resp.hovered() {
                                ui.visuals().widgets.hovered.bg_stroke.color
                            } else {
                                ui.visuals().widgets.noninteractive.bg_stroke.color
                            };
                            main_ui.painter().rect_filled(
                                divider_rect.shrink2(egui::vec2(2.0, 0.0)),
                                1.0,
                                color,
                            );
                            if resp.hovered() {
                                main_ui.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                            }
                        }
                    }
                }

                // Разделитель дерева: перетаскивание меняет ширину на дельту
                // (случайный клик сдвинет его на пару пикселей).
                {
                    let strip = egui::Rect::from_min_max(
                        egui::pos2(tree_rect.max.x - 3.0, tree_rect.min.y),
                        egui::pos2(tree_rect.max.x + 3.0, tree_rect.max.y),
                    );
                    let resp = ui.interact(
                        strip,
                        egui::Id::unique("tree_divider"),
                        egui::Sense::click_and_drag(),
                    );
                    if resp.dragged_by(egui::PointerButton::Primary) {
                        let delta = resp.drag_delta().x;
                        vs.tree_w = (vs.tree_w + delta).clamp(160.0, 640.0);
                    }
                    if resp.hovered() || resp.dragged() {
                        ui.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                }

                // Граф связей (перестраивается лениво — только пока окно открыто).
                if vs.show_graph {
                    vs.refresh_graph();
                    if let Some(rel) = ui::graph_window::show(ui, vs) {
                        vs.load(rel);
                    }
                }

                // Быстрый переключатель (Ctrl+O).
                ui::quick_open::show(
                    ui,
                    vs,
                    ui::quick_open::Keys {
                        esc,
                        enter,
                        up,
                        down,
                    },
                );
            }
        }

        // Модалка выбора папки — последней, чтобы быть поверх остального UI.
        let picker_action = match self.picker.as_mut() {
            Some(picker) => ui::folder_picker::show(ui, picker),
            None => None,
        };
        match picker_action {
            Some(ui::folder_picker::Action::Opened(path)) => {
                self.picker = None;
                self.open_vault(path);
            }
            Some(ui::folder_picker::Action::Saved(path)) => {
                self.picker = None;
                self.export_html_to(path);
            }
            Some(ui::folder_picker::Action::Cancelled) => self.picker = None,
            None => {}
        }

        self.sync_settings(ui.ctx());
        self.notices.show(ui.ctx(), now);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Текст, набранный за последнюю секунду (AUTOSAVE_DELAY), иначе теряется.
        // При конфликте или ошибке записи правки уходят в копию рядом с заметкой.
        if let Some(vs) = &mut self.vault {
            let _ = vs.flush_or_copy();
        }
        self.save_settings();
    }
}
