#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tree;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::egui;
use serde::{Deserialize, Serialize};

use graph::GraphView;
use index::Index;
use vault::{Vault, VaultWatcher};

const AUTOSAVE_DELAY: f64 = 1.0;

/// Фиксированный id редактора — для доступа к состоянию TextEdit (курсор/выделение).
const EDITOR_ID_SRC: &str = "mdwriter_editor";

fn editor_id() -> egui::Id {
    egui::Id::unique(EDITOR_ID_SRC)
}

/// Режим отображения: только редактор / сплит / только предпросмотр.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[derive(Default)]
enum ViewMode {
    Edit,
    #[default]
    Split,
    Preview,
}


impl ViewMode {
    fn next(self) -> Self {
        match self {
            ViewMode::Edit => ViewMode::Split,
            ViewMode::Split => ViewMode::Preview,
            ViewMode::Preview => ViewMode::Edit,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ViewMode::Edit => "Edit",
            ViewMode::Split => "Split",
            ViewMode::Preview => "Preview",
        }
    }
}

/// Настройки приложения (persist в JSON).
#[derive(Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    dark: Option<bool>,
    #[serde(default)]
    view_mode: ViewMode,
    #[serde(default)]
    show_graph: bool,
    #[serde(default)]
    last_vault: Option<PathBuf>,
    #[serde(default)]
    window_size: Option<[f32; 2]>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: None,
            view_mode: ViewMode::Split,
            show_graph: false,
            last_vault: None,
            window_size: None,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("mdwriter").join("settings.json"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        base.map(|p| p.join("mdwriter").join("settings.json"))
    }
}

fn load_settings() -> Settings {
    let Some(path) = settings_path() else {
        return Settings::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            eprintln!("settings parse failed ({e}); используем значения по умолчанию");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Пытается найти жирный системный шрифт (Windows/Linux).
fn load_bold_font() -> Option<egui::FontData> {
    const CANDIDATES: &[&str] = &[
        // Windows
        "C:\\Windows\\Fonts\\segoeuib.ttf",
        "C:\\Windows\\Fonts\\arialbd.ttf",
        "C:\\Windows\\Fonts\\verdanab.ttf",
        "C:\\Windows\\Fonts\\tahomabd.ttf",
        // Linux
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    ];
    for p in CANDIDATES {
        if let Ok(bytes) = std::fs::read(p) {
            return Some(egui::FontData::from_owned(bytes));
        }
    }
    None
}

fn main() -> eframe::Result {
    let settings = load_settings();
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
    view_mode: ViewMode,
    /// Доля ширины предпросмотра в режиме Split (0..1).
    preview_frac: f32,
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
            view_mode: ViewMode::Split,
            preview_frac: 0.5,
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
            preview::Action::ToggleTask { .. } => None,
        }
    }

    /// Переключает `[ ]` ↔ `[x]` в исходнике по индексу строки тела документа.
    fn toggle_task(&mut self, line_in_body: usize) {
        // Смещение строки из-за frontmatter.
        let fm_lines = match markdown::split_frontmatter(&self.editor_text) {
            Some((_, body)) => {
                let consumed = self.editor_text.len() - body.len();
                self.editor_text[..consumed].bytes().filter(|&b| b == b'\n').count()
            }
            None => 0,
        };
        let line_idx = fm_lines + line_in_body;

        // Начало и конец целевой строки.
        let mut start = 0;
        for _ in 0..line_idx {
            let Some(rel) = self.editor_text[start..].find('\n') else {
                return;
            };
            start += rel + 1;
        }
        let line_end = self.editor_text[start..]
            .find('\n')
            .map(|r| start + r)
            .unwrap_or(self.editor_text.len());

        let line = &self.editor_text[start..line_end];
        for pat in ["[ ]", "[x]", "[X]"] {
            if let Some(pos) = line.find(pat) {
                let abs = start + pos;
                let replacement = if pat == "[ ]" { "[x]" } else { "[ ]" };
                self.editor_text.replace_range(abs..abs + 3, replacement);
                self.dirty = true;
                self.last_edit = None; // автосохранение сработает сразу
                return;
            }
        }
    }

    // ------------------------- Форматирование -------------------------

    /// Текущее выделение в редакторе (символьный диапазон), если доступно.
    fn selected_char_range(&self, ctx: &egui::Context) -> Option<std::ops::Range<usize>> {
        let state = egui::TextEdit::load_state(ctx, editor_id())?;
        let range = state.cursor.char_range()?.as_sorted_char_range();
        Some(range.start.0..range.end.0)
    }

    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.editor_text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.editor_text.len())
    }

    fn set_cursor(&mut self, ctx: &egui::Context, char_idx: usize) {
        if let Some(mut state) = egui::TextEdit::load_state(ctx, editor_id()) {
            state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::one(
                egui::text::CCursor::new(char_idx),
            )));
            egui::TextEdit::store_state(ctx, editor_id(), state);
        }
    }

    /// Заворачивает выделение между `before` и `after` (снимает, если уже обёрнуто).
    fn wrap_selection(&mut self, ctx: &egui::Context, before: &str, after: &str) {
        let Some(range) = self.selected_char_range(ctx) else {
            return;
        };
        let start = self.char_to_byte(range.start);
        let end = self.char_to_byte(range.end);
        let sel = self.editor_text[start..end].to_string();

        if !sel.is_empty() && sel.starts_with(before) && sel.ends_with(after) {
            // Снять форматирование.
            let inner = &sel[before.len()..sel.len() - after.len()];
            self.editor_text.replace_range(start..end, inner);
            let cursor = self.editor_text[..start].chars().count() + inner.chars().count();
            self.set_cursor(ctx, cursor);
        } else {
            let new = format!("{before}{sel}{after}");
            self.editor_text.replace_range(start..end, &new);
            let cursor = self.editor_text[..start].chars().count() + new.chars().count();
            self.set_cursor(ctx, cursor);
        }
        self.dirty = true;
        self.last_edit = None;
    }

    /// Вставляет ссылку: выделение → `[текст]()`, курсор внутри скобок.
    fn fmt_link(&mut self, ctx: &egui::Context) {
        let Some(range) = self.selected_char_range(ctx) else {
            return;
        };
        let start = self.char_to_byte(range.start);
        let end = self.char_to_byte(range.end);
        let sel = self.editor_text[start..end].to_string();
        if sel.starts_with('[') && sel.ends_with(')') {
            return; // уже похоже на ссылку — не трогаем
        }
        let new = format!("[{sel}]()");
        self.editor_text.replace_range(start..end, &new);
        let cursor = self.editor_text[..start].chars().count() + sel.chars().count() + 3;
        self.set_cursor(ctx, cursor);
        self.dirty = true;
        self.last_edit = None;
    }

    /// Диапазон строк, покрывающих выделение [start, end).
    fn lines_range(&self, start: usize, end: usize) -> std::ops::Range<usize> {
        let ls = self.editor_text[..start]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let le = if end > start {
            self.editor_text[end - 1..]
                .find('\n')
                .map(|p| end - 1 + p)
                .unwrap_or(self.editor_text.len())
        } else {
            self.editor_text[end..]
                .find('\n')
                .map(|p| end + p)
                .unwrap_or(self.editor_text.len())
        };
        ls..le
    }

    /// Применяет построчное преобразование к строкам выделения.
    fn fmt_lines(&mut self, ctx: &egui::Context, f: fn(&str) -> String) {
        let Some(range) = self.selected_char_range(ctx) else {
            return;
        };
        let start = self.char_to_byte(range.start);
        let end = self.char_to_byte(range.end);
        let lr = self.lines_range(start, end);
        let block = self.editor_text[lr.clone()].to_string();
        let new_block = block.split('\n').map(f).collect::<Vec<_>>().join("\n");
        self.editor_text.replace_range(lr, &new_block);
        self.dirty = true;
        self.last_edit = None;
    }

    // ------------------------- Центральная область -------------------------

    /// Отрисовка редактора.
    fn ui_editor(&mut self, ui: &mut egui::Ui, now: f64) {
        if self.open.is_some() {
            ui.horizontal(|ui| {
                ui.strong(
                    self.open
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                );
                if self.dirty {
                    ui.label("●");
                }
            });
            ui.separator();

            let theme =
                egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
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

            let n_lines = self.editor_text.lines().count();
            let digits = n_lines.to_string().len().max(2);
            let mut edit = egui::TextEdit::multiline(&mut self.editor_text)
                .code_editor()
                .id(editor_id())
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(4, 0))
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter);
            if self.quick_open.is_some() {
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
                self.dirty = true;
                self.last_edit = Some(now);
            }
        } else {
            ui.centered_and_justified(|ui| {
                ui.label("Выберите заметку слева или создайте новую.");
            });
        }
    }

    /// Отрисовка предпросмотра.
    fn ui_preview(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.strong("Preview");
        ui.separator();
        let actions = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                preview::markdown(ui, &self.editor_text)
            })
            .inner;
        for action in actions {
            match action {
                preview::Action::OpenWiki(_) => {
                    if let Some(rel) = self.open_wiki(&action) {
                        self.load(rel);
                    }
                }
                preview::Action::ToggleTask { line_in_body } => {
                    self.toggle_task(line_in_body);
                }
            }
        }
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Регистрируем жирный системный шрифт (если найден).
        if let Some(bold) = load_bold_font() {
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

        let settings = load_settings();
        if let Some(dark) = settings.dark {
            cc.egui_ctx.set_theme(if dark {
                egui::ThemePreference::Dark
            } else {
                egui::ThemePreference::Light
            });
        }

        let mut app = App {
            vault: None,
            settings,
            settings_dirty: false,
        };

        // Восстанавливаем последний vault.
        if let Some(path) = app.settings.last_vault.clone() {
            if path.is_dir() {
                if let Ok(watcher) = VaultWatcher::new(&path) {
                    let mut vs = VaultState::open(Vault::open(path), watcher);
                    vs.view_mode = app.settings.view_mode;
                    vs.show_graph = app.settings.show_graph;
                    app.vault = Some(vs);
                }
            }
        }

        app
    }

    fn open_vault(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            match VaultWatcher::new(&path) {
                Ok(watcher) => {
                    let mut vs = VaultState::open(Vault::open(path.clone()), watcher);
                    vs.view_mode = self.settings.view_mode;
                    vs.show_graph = self.settings.show_graph;
                    self.vault = Some(vs);
                    self.settings.last_vault = Some(path);
                    self.settings_dirty = true;
                }
                Err(e) => eprintln!("watch error: {e}"),
            }
        }
    }

    /// Синхронизирует UI-состояние в настройки и сохраняет при изменениях.
    fn sync_settings(&mut self, ctx: &egui::Context) {
        let dark = ctx.theme() == egui::Theme::Dark;
        if self.settings.dark != Some(dark) {
            self.settings.dark = Some(dark);
            self.settings_dirty = true;
        }

        if let Some(vs) = &self.vault {
            if vs.view_mode != self.settings.view_mode {
                self.settings.view_mode = vs.view_mode;
                self.settings_dirty = true;
            }
            if vs.show_graph != self.settings.show_graph {
                self.settings.show_graph = vs.show_graph;
                self.settings_dirty = true;
            }
        }

        // Размер окна сохраняем всегда (но без флага dirty — пишем при выходе).
        if let Some(size) = ctx.input(|i| i.viewport().inner_rect).map(|r| [r.width(), r.height()])
        {
            self.settings.window_size = Some(size);
        }

        if self.settings_dirty {
            self.save_settings();
        }
    }

    fn save_settings(&mut self) {
        let Some(path) = settings_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(&self.settings) {
            Ok(s) => {
                if let Err(e) = std::fs::write(&path, s) {
                    eprintln!("settings save failed: {e}");
                }
            }
            Err(e) => eprintln!("settings serialize failed: {e}"),
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
        options.insert(pulldown_cmark::Options::ENABLE_TABLES);
        options.insert(pulldown_cmark::Options::ENABLE_FOOTNOTES);
        options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
        options.insert(pulldown_cmark::Options::ENABLE_TASKLISTS);
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

// ---------- Построчные преобразования для панели форматирования ----------

/// Заголовок уровня `level` (переключается: повторное применение снимает).
fn fmt_heading(line: &str, level: usize) -> String {
    let body = line.trim_start_matches('#').trim_start_matches(' ');
    let prefix = format!("{} ", "#".repeat(level));
    if body.is_empty() {
        "#".repeat(level)
    } else if line.starts_with(&prefix) {
        body.to_string()
    } else {
        format!("{prefix}{body}")
    }
}

fn fmt_list(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("- ") {
        rest.to_string()
    } else {
        format!("- {line}")
    }
}

fn fmt_task(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("- [ ] ") {
        format!("- [x] {rest}")
    } else if let Some(rest) = line.strip_prefix("- [x] ") {
        format!("- [ ] {rest}")
    } else {
        format!("- [ ] {line}")
    }
}

fn fmt_quote(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("> ") {
        rest.to_string()
    } else {
        format!("> {line}")
    }
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

        // --- Верхняя панель (собственная раскладка, без egui::Panel — в 0.37
        // панели накапливают margin в хранимом размере, из-за чего «уезжают») ---
        let full = ui.available_rect_before_wrap();
        let top_h;
        {
            let mut top_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(full)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            top_ui.add_space(4.0);
            top_ui.horizontal(|ui| {
                ui.heading("mdwriter");
                ui.separator();
                if ui.button("Open vault…").clicked() {
                    self.open_vault();
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
                            vs.fmt_lines(ui.ctx(), |l| fmt_heading(l, 1));
                        }
                        if ui.small_button("H2").clicked() {
                            vs.fmt_lines(ui.ctx(), |l| fmt_heading(l, 2));
                        }
                        if ui.small_button("H3").clicked() {
                            vs.fmt_lines(ui.ctx(), |l| fmt_heading(l, 3));
                        }
                        ui.separator();
                        if ui.small_button("•").on_hover_text("Список").clicked() {
                            vs.fmt_lines(ui.ctx(), fmt_list);
                        }
                        if ui.small_button("☑").on_hover_text("Задача").clicked() {
                            vs.fmt_lines(ui.ctx(), fmt_task);
                        }
                        if ui.small_button("❝").on_hover_text("Цитата").clicked() {
                            vs.fmt_lines(ui.ctx(), fmt_quote);
                        }
                        ui.separator();
                        if ui.small_button("code").on_hover_text("Блок кода").clicked() {
                            vs.wrap_selection(ui.ctx(), "```\n", "\n```");
                        }
                        if ui.small_button("link").on_hover_text("Ссылка").clicked() {
                            vs.fmt_link(ui.ctx());
                        }
                        if ui.small_button("—").on_hover_text("Горизонтальная линия").clicked() {
                            vs.wrap_selection(ui.ctx(), "\n---\n", "");
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
        const TREE_W: f32 = 240.0;
        let tree_rect = egui::Rect::from_min_max(
            egui::pos2(full.min.x, top_h),
            egui::pos2(full.min.x + TREE_W, full.max.y),
        );
        let main_rect = egui::Rect::from_min_max(
            egui::pos2(full.min.x + TREE_W, top_h),
            full.max,
        );
        ui.painter().vline(tree_rect.max.x, tree_rect.y_range(), sep_stroke);

        match self.vault.as_mut() {
            None => {
                let mut main_ui = ui.new_child(egui::UiBuilder::new().max_rect(main_rect));
                main_ui.centered_and_justified(|ui| {
                    if ui.button("Open vault…").clicked() {
                        self.open_vault();
                    }
                });
            }
            Some(vs) => {
                // Дерево заметок.
                let mut tree_ui = ui.new_child(egui::UiBuilder::new().max_rect(tree_rect));
                let tree_clicked = {
                    let ui = &mut tree_ui;
                    let mut clicked: Option<PathBuf> = None;

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let name_edit = egui::TextEdit::singleline(&mut vs.new_note_name)
                            .hint_text("note.md")
                            .desired_width(110.0);
                        let resp = ui.add(name_edit);
                        let enter = resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("+").on_hover_text("Новая заметка").clicked() || enter {
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
                                        ui.label(egui::RichText::new(snippet).weak().small());
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
                let mut main_ui = ui.new_child(egui::UiBuilder::new().max_rect(main_rect));
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
                            let preview_w = (full.width() * vs.preview_frac)
                                .clamp(240.0, full.width() - 340.0);
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

                            let mut editor_ui = main_ui
                                .new_child(egui::UiBuilder::new().max_rect(editor_rect));
                            vs.ui_editor(&mut editor_ui, now);

                            let mut preview_ui = main_ui
                                .new_child(egui::UiBuilder::new().max_rect(preview_rect));
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

        self.sync_settings(ui.ctx());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}
