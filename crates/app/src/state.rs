//! Состояние открытого vault: заметки, индекс, редактор, конфликты, отрисовка.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};

use graph::GraphView;
use index::Index;
use vault::{Vault, VaultError, VaultWatcher};

use crate::format::{lines_range, toggle_task_at};
use crate::notice::Notices;
use crate::sync::{self, Reconcile};
use crate::tree;

pub(crate) const AUTOSAVE_DELAY: f64 = 1.0;

/// Через сколько секунд после первого события наблюдателя пересканировать
/// vault: одно сохранение даёт серию событий (временный файл, rename…),
/// и все они схлопываются в одно пересканирование.
const RESCAN_DELAY: f64 = 0.3;

/// Планировщик пересканирования. `pending` — запланированное время,
/// `changed` — пришли ли новые события. Возвращает новое запланированное
/// время и пора ли пересканировать сейчас. Отсчёт идёт от первого события,
/// поэтому непрерывный поток изменений не откладывает пересканирование
/// бесконечно.
fn rescan_schedule(pending: Option<f64>, changed: bool, now: f64) -> (Option<f64>, bool) {
    let pending = match pending {
        Some(t) => Some(t),
        None if changed => Some(now + RESCAN_DELAY),
        None => None,
    };
    match pending {
        Some(t) if now >= t => (None, true),
        p => (p, false),
    }
}

/// Фиксированный id редактора — для доступа к состоянию TextEdit (курсор/выделение).
const EDITOR_ID_SRC: &str = "mdwriter_editor";

fn editor_id() -> egui::Id {
    egui::Id::unique(EDITOR_ID_SRC)
}

/// Режим отображения: только редактор / сплит / только предпросмотр.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub(crate) enum ViewMode {
    Edit,
    #[default]
    Split,
    Preview,
}

impl ViewMode {
    pub(crate) fn next(self) -> Self {
        match self {
            ViewMode::Edit => ViewMode::Split,
            ViewMode::Split => ViewMode::Preview,
            ViewMode::Preview => ViewMode::Edit,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            ViewMode::Edit => "Edit",
            ViewMode::Split => "Split",
            ViewMode::Preview => "Preview",
        }
    }
}

/// Состояние быстрого переключателя (Ctrl+O).
pub(crate) struct QuickOpen {
    pub(crate) query: String,
    pub(crate) selected: usize,
}

pub(crate) struct VaultState {
    pub(crate) vault: Vault,
    pub(crate) watcher: VaultWatcher,
    pub(crate) notes: Vec<PathBuf>,
    pub(crate) tree: Vec<tree::TreeNode>,
    pub(crate) index: Index,
    pub(crate) open: Option<PathBuf>,
    pub(crate) editor_text: String,
    pub(crate) dirty: bool,
    /// Последняя попытка сохранения открытой заметки завершилась ошибкой.
    pub(crate) save_failed: bool,
    /// Содержимое файла на момент последнего чтения или успешной записи:
    /// по нему видно, что файл изменили извне.
    pub(crate) disk_text: Option<String>,
    /// Файл изменён извне, а в редакторе есть несохранённые правки.
    /// Пока флаг стоит, автосохранение не пишет на диск.
    pub(crate) conflict: bool,
    /// Файл удалён/переименован извне, а в редакторе есть несохранённые правки.
    pub(crate) orphaned: bool,
    /// Когда пересканировать vault после событий наблюдателя (см. `poll_watcher`).
    pub(crate) pending_rescan_at: Option<f64>,
    pub(crate) last_edit: Option<f64>,
    pub(crate) new_note_name: String,
    pub(crate) view_mode: ViewMode,
    /// Доля ширины предпросмотра в режиме Split (0..1).
    pub(crate) preview_frac: f32,
    /// Ширина панели дерева, px.
    pub(crate) tree_w: f32,
    pub(crate) show_graph: bool,
    pub(crate) search_query: String,
    pub(crate) quick_open: Option<QuickOpen>,
    pub(crate) graph: GraphView,
    pub(crate) notices: Notices,
}

impl VaultState {
    pub(crate) fn open(vault: Vault, watcher: VaultWatcher, notices: Notices) -> Self {
        let mut s = Self {
            vault,
            watcher,
            notes: Vec::new(),
            tree: Vec::new(),
            index: Index::default(),
            open: None,
            editor_text: String::new(),
            dirty: false,
            save_failed: false,
            disk_text: None,
            conflict: false,
            orphaned: false,
            pending_rescan_at: None,
            last_edit: None,
            new_note_name: String::new(),
            view_mode: ViewMode::Split,
            preview_frac: 0.5,
            tree_w: 240.0,
            show_graph: false,
            search_query: String::new(),
            quick_open: None,
            graph: GraphView::default(),
            notices,
        };
        s.rescan();
        s
    }

    pub(crate) fn rescan(&mut self) {
        match self.vault.list_notes() {
            Ok(notes) => {
                self.notes = notes;
                self.tree = tree::build_tree(&self.notes);
            }
            Err(e) => self
                .notices
                .error(format!("Не удалось прочитать список заметок: {e}")),
        }
        self.index = match Index::build(&self.vault) {
            Ok(index) => index,
            Err(e) => {
                self.notices.error(format!(
                    "Индекс не построен (поиск, backlinks и граф недоступны): {e}"
                ));
                Index::default()
            }
        };
        self.graph.update(&self.index);
        self.check_open_on_disk();
    }

    /// Читает файл заметки с диска. `Ok(None)` — файла нет.
    fn read_disk(&self, rel: &Path) -> Result<Option<String>, String> {
        match self.vault.read(rel) {
            Ok(text) => Ok(Some(text)),
            Err(e) => match &e {
                VaultError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => Ok(None),
                #[allow(unreachable_patterns)] // на случай новых вариантов VaultError
                _ => Err(e.to_string()),
            },
        }
    }

    /// Сверяет открытую заметку с файлом на диске (после внешних изменений).
    fn check_open_on_disk(&mut self) {
        let Some(rel) = self.open.clone() else {
            return;
        };
        let disk = match self.read_disk(&rel) {
            Ok(disk) => disk,
            Err(e) => {
                self.notices.error_keyed(
                    &format!("check:{}", rel.display()),
                    format!("Не удалось перечитать {}: {e}", rel.display()),
                );
                return;
            }
        };
        self.notices.clear_key(&format!("check:{}", rel.display()));
        match sync::reconcile(
            disk.as_deref(),
            self.disk_text.as_deref(),
            &self.editor_text,
            self.dirty,
        ) {
            Reconcile::Unchanged => {
                // Файл вернулся к известному состоянию — писать снова безопасно.
                self.conflict = false;
                self.orphaned = false;
            }
            Reconcile::Synced => {
                self.clear_save_error();
                self.disk_text = disk;
                self.dirty = false;
                self.conflict = false;
                self.orphaned = false;
            }
            Reconcile::Reload => {
                self.clear_save_error();
                self.editor_text = disk.clone().unwrap_or_default();
                self.disk_text = disk;
                self.conflict = false;
                self.orphaned = false;
                self.notices
                    .info(format!("Заметка обновлена с диска: {}", rel.display()));
            }
            Reconcile::Conflict => {
                self.conflict = true;
                self.orphaned = false;
            }
            Reconcile::Close => {
                self.close_note();
                self.notices
                    .info(format!("Заметка удалена на диске: {}", rel.display()));
            }
            Reconcile::Orphaned => {
                self.orphaned = true;
                self.conflict = false;
            }
        }
    }

    /// Снимает висящую ошибку сохранения: правки больше не нужно сохранять
    /// (заметку перезагрузили с диска или закрыли).
    fn clear_save_error(&mut self) {
        if self.save_failed {
            if let Some(rel) = &self.open {
                self.notices.clear_key(&format!("save:{}", rel.display()));
            }
            self.save_failed = false;
        }
    }

    /// Закрывает заметку без сохранения.
    fn close_note(&mut self) {
        self.clear_save_error();
        self.open = None;
        self.editor_text.clear();
        self.disk_text = None;
        self.dirty = false;
        self.save_failed = false;
        self.conflict = false;
        self.orphaned = false;
        self.last_edit = None;
    }

    pub(crate) fn load(&mut self, rel: PathBuf) {
        // Не уходим с заметки, которую не удалось сохранить: иначе правки
        // будут потеряны. Ошибка уже показана в `flush` или полосой конфликта.
        if self.flush().is_err() {
            self.notices.error(format!(
                "Заметка {} не открыта: сначала сохраните текущую или разрешите конфликт",
                rel.display()
            ));
            return;
        }
        match self.vault.read(&rel) {
            Ok(text) => {
                self.disk_text = Some(text.clone());
                self.editor_text = text;
                self.dirty = false;
                self.save_failed = false;
                self.conflict = false;
                self.orphaned = false;
                self.last_edit = None;
                self.open = Some(rel);
            }
            Err(e) => self
                .notices
                .error(format!("Не удалось открыть {}: {e}", rel.display())),
        }
    }

    /// Записывает открытую заметку, если есть несохранённые правки.
    /// При ошибке правки остаются несохранёнными (`dirty`), ошибка
    /// показывается пользователю. Если файл изменили или удалили извне,
    /// ничего не пишет и ставит `conflict`/`orphaned` — решает пользователь.
    pub(crate) fn flush(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let Some(rel) = self.open.clone() else {
            self.dirty = false;
            return Ok(());
        };
        if self.conflict || self.orphaned {
            return Err(format!("{}: ждёт решения конфликта", rel.display()));
        }
        // Защита от гонки: watcher мог ещё не сообщить о внешнем изменении.
        if let Ok(disk) = self.read_disk(&rel) {
            match sync::reconcile(
                disk.as_deref(),
                self.disk_text.as_deref(),
                &self.editor_text,
                true,
            ) {
                Reconcile::Synced => {
                    self.clear_save_error();
                    self.disk_text = disk;
                    self.dirty = false;
                    return Ok(());
                }
                Reconcile::Conflict => {
                    self.conflict = true;
                    return Err(format!("{} изменён на диске", rel.display()));
                }
                Reconcile::Orphaned => {
                    self.orphaned = true;
                    return Err(format!("{} удалён на диске", rel.display()));
                }
                // При dirty = true остальные варианты — «можно писать».
                Reconcile::Unchanged | Reconcile::Reload | Reconcile::Close => {}
            }
        }
        // Если файл прочитать не удалось, всё равно пробуем записать:
        // ошибка записи будет показана ниже.
        self.write_now()
    }

    /// Безусловно записывает буфер в файл открытой заметки.
    fn write_now(&mut self) -> Result<(), String> {
        let Some(rel) = self.open.clone() else {
            return Ok(());
        };
        let key = format!("save:{}", rel.display());
        match self.vault.write(&rel, &self.editor_text) {
            Ok(()) => {
                if self.save_failed {
                    // Проблема разрешилась — убираем висящую ошибку.
                    self.notices.clear_key(&key);
                    self.notices.info(format!("Сохранено: {}", rel.display()));
                }
                self.disk_text = Some(self.editor_text.clone());
                self.dirty = false;
                self.save_failed = false;
                self.conflict = false;
                self.orphaned = false;
                Ok(())
            }
            Err(e) => {
                let msg = format!("Не удалось сохранить {}: {e}", rel.display());
                self.save_failed = true;
                self.notices.error_keyed(&key, msg.clone());
                Err(msg)
            }
        }
    }

    // ------------------- Разрешение конфликта (A3) -------------------

    /// «Загрузить с диска»: мои правки отбрасываются.
    fn resolve_take_disk(&mut self) {
        let Some(rel) = self.open.clone() else {
            return;
        };
        match self.read_disk(&rel) {
            Ok(Some(text)) => {
                self.clear_save_error();
                self.disk_text = Some(text.clone());
                self.editor_text = text;
                self.dirty = false;
                self.conflict = false;
                self.orphaned = false;
            }
            Ok(None) => self.close_note(),
            Err(e) => self
                .notices
                .error(format!("Не удалось прочитать {}: {e}", rel.display())),
        }
    }

    /// «Оставить мои» / «Сохранить снова»: перезаписать файл буфером.
    fn resolve_keep_mine(&mut self) {
        // Ошибка записи уже показана уведомлением.
        let _ = self.write_now();
    }

    /// Сохраняет мою версию копией рядом с заметкой. Возвращает путь копии.
    fn save_copy(&mut self) -> Result<PathBuf, String> {
        let Some(rel) = self.open.clone() else {
            return Err("нет открытой заметки".to_string());
        };
        let copy = sync::conflict_copy_path(&rel, |p| self.vault.abs_path(p).exists());
        match self.vault.write(&copy, &self.editor_text) {
            Ok(()) => {
                self.notices
                    .info(format!("Ваша версия сохранена как {}", copy.display()));
                Ok(copy)
            }
            Err(e) => {
                let msg = format!("Не удалось сохранить копию {}: {e}", copy.display());
                self.notices.error(msg.clone());
                Err(msg)
            }
        }
    }

    /// «Сохранить мои как копию»: копия рядом, в редакторе — версия с диска.
    fn resolve_save_copy(&mut self) {
        if self.save_copy().is_ok() {
            self.resolve_take_disk();
            self.rescan();
        }
    }

    /// Для выхода из приложения и смены vault: сохраняет правки, а если
    /// мешает конфликт или удалённый файл — пишет копию, чтобы ничего
    /// не потерять.
    pub(crate) fn flush_or_copy(&mut self) -> Result<(), String> {
        if self.dirty && (self.conflict || self.orphaned) {
            return self.save_copy().map(|_| ());
        }
        match self.flush() {
            Ok(()) => Ok(()),
            // Запись не удалась (или flush сам обнаружил конфликт) — копия.
            Err(e) if self.dirty => self.save_copy().map(|_| ()).map_err(|_| e),
            Err(e) => Err(e),
        }
    }

    /// Забирает события наблюдателя за файлами и пересканирует vault через
    /// `RESCAN_DELAY` после первого из них. Вызывать раз в кадр.
    pub(crate) fn poll_watcher(&mut self, now: f64, ctx: &egui::Context) {
        let changed = !self.watcher.take_changes().is_empty();
        let (pending, due) = rescan_schedule(self.pending_rescan_at, changed, now);
        self.pending_rescan_at = pending;
        if due {
            self.rescan();
        } else if let Some(t) = pending {
            // Кадр нужен к назначенному времени, даже если пользователь бездействует.
            ctx.request_repaint_after(Duration::from_secs_f64((t - now).max(0.0)));
        }
    }

    pub(crate) fn maybe_autosave(&mut self, now: f64, ctx: &egui::Context) {
        // При конфликте ждём решения пользователя (полоса над редактором).
        if !self.dirty || self.conflict || self.orphaned {
            return;
        }
        let due = match self.last_edit {
            None => true,
            Some(t) => {
                let elapsed = now - t;
                if elapsed < AUTOSAVE_DELAY {
                    ctx.request_repaint_after(Duration::from_secs_f64(AUTOSAVE_DELAY - elapsed));
                }
                elapsed >= AUTOSAVE_DELAY
            }
        };
        if due && self.flush().is_err() {
            // Повторим через AUTOSAVE_DELAY, а не в каждом кадре.
            self.last_edit = Some(now);
            ctx.request_repaint_after(Duration::from_secs_f64(AUTOSAVE_DELAY));
        }
    }

    pub(crate) fn create_note(&mut self) {
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
            self.notices
                .error(format!("Не удалось создать {}: {e}", rel.display()));
            return;
        }
        self.new_note_name.clear();
        self.rescan();
        self.load(rel);
    }

    /// Разрешает действие предпросмотра в путь заметки.
    /// Несуществующую цель создаёт (как Obsidian) рядом с текущей заметкой.
    pub(crate) fn open_wiki(&mut self, action: &preview::Action) -> Option<PathBuf> {
        match action {
            preview::Action::OpenWiki(target) => {
                let from = self.open.clone()?;
                if let Some(rel) = self.index.resolve_wikilink(&from, target) {
                    return Some(rel);
                }
                // Якорь (`#…`) в имя файла не попадает; `[[#Заголовок]]` ничего не создаёт.
                let rel = new_note_path(&from, target)?;
                match self.vault.create(&rel) {
                    Ok(()) => {
                        self.rescan();
                        Some(rel)
                    }
                    Err(e) => {
                        self.notices
                            .error(format!("Не удалось создать {}: {e}", rel.display()));
                        None
                    }
                }
            }
            preview::Action::ToggleTask { .. } => None,
        }
    }

    /// Переключает `[ ]` ↔ `[x]` у маркера задачи по байтовому смещению
    /// во всём тексте (его даёт предпросмотр).
    fn toggle_task(&mut self, offset: usize) {
        if toggle_task_at(&mut self.editor_text, offset) {
            self.dirty = true;
            self.last_edit = None; // автосохранение сработает сразу
        } else {
            self.notices
                .info("Задача не переключена: текст изменился, повторите клик");
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
            state
                .cursor
                .set_char_range(Some(egui::text_selection::CCursorRange::one(
                    egui::text::CCursor::new(char_idx),
                )));
            egui::TextEdit::store_state(ctx, editor_id(), state);
        }
    }

    /// Заворачивает выделение между `before` и `after` (снимает, если уже обёрнуто).
    pub(crate) fn wrap_selection(&mut self, ctx: &egui::Context, before: &str, after: &str) {
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
    pub(crate) fn fmt_link(&mut self, ctx: &egui::Context) {
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
        lines_range(&self.editor_text, start, end)
    }

    /// Применяет построчное преобразование к строкам выделения.
    pub(crate) fn fmt_lines(&mut self, ctx: &egui::Context, f: fn(&str) -> String) {
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

    /// Полоса над редактором, если файл изменён или удалён извне,
    /// а в редакторе есть несохранённые правки.
    fn ui_conflict_bar(&mut self, ui: &mut egui::Ui) {
        #[derive(Clone, Copy)]
        enum Choice {
            TakeDisk,
            KeepMine,
            SaveCopy,
            Close,
        }
        if !(self.conflict || self.orphaned) {
            return;
        }
        let mut choice = None;
        let color = ui.visuals().warn_fg_color;
        egui::Frame::group(ui.style())
            .stroke(egui::Stroke::new(1.0, color))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if self.conflict {
                    ui.label(
                        egui::RichText::new(
                            "Файл изменён на диске другой программой, а у вас есть \
                             несохранённые правки. Автосохранение приостановлено.",
                        )
                        .color(color),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button("Загрузить с диска")
                            .on_hover_text("Ваши несохранённые правки пропадут")
                            .clicked()
                        {
                            choice = Some(Choice::TakeDisk);
                        }
                        if ui
                            .button("Оставить мои")
                            .on_hover_text("Перезаписать файл вашей версией")
                            .clicked()
                        {
                            choice = Some(Choice::KeepMine);
                        }
                        if ui
                            .button("Сохранить мои как копию")
                            .on_hover_text(
                                "Ваша версия — в «имя (конфликт).md», в редакторе — версия с диска",
                            )
                            .clicked()
                        {
                            choice = Some(Choice::SaveCopy);
                        }
                    });
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Файл удалён или переименован на диске, а у вас есть \
                             несохранённые правки.",
                        )
                        .color(color),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Сохранить снова").clicked() {
                            choice = Some(Choice::KeepMine);
                        }
                        if ui
                            .button("Закрыть без сохранения")
                            .on_hover_text("Ваши несохранённые правки пропадут")
                            .clicked()
                        {
                            choice = Some(Choice::Close);
                        }
                    });
                }
            });
        match choice {
            Some(Choice::TakeDisk) => self.resolve_take_disk(),
            Some(Choice::KeepMine) => {
                self.resolve_keep_mine();
                // Заметка могла быть удалена — обновим дерево и индекс.
                if self.orphaned || !self.notes.iter().any(|n| Some(n) == self.open.as_ref()) {
                    self.rescan();
                }
            }
            Some(Choice::SaveCopy) => self.resolve_save_copy(),
            Some(Choice::Close) => self.close_note(),
            None => {}
        }
    }

    /// Отрисовка редактора.
    pub(crate) fn ui_editor(&mut self, ui: &mut egui::Ui, now: f64) {
        if self.open.is_some() {
            ui.horizontal(|ui| {
                ui.strong(
                    self.open
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                );
                // «•» вместо «●»: второго символа нет во встроенных шрифтах egui.
                if self.dirty {
                    if self.save_failed {
                        ui.label(egui::RichText::new("•").color(ui.visuals().error_fg_color))
                            .on_hover_text("Не сохранено: ошибка записи, повтор автоматически");
                    } else {
                        ui.label("•").on_hover_text("Есть несохранённые правки");
                    }
                }
            });
            ui.separator();
            self.ui_conflict_bar(ui);

            let theme =
                egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
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
                .id_salt("editor_scroll")
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
    pub(crate) fn ui_preview(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.strong("Preview");
        ui.separator();
        let actions = egui::ScrollArea::vertical()
            .id_salt("preview_scroll")
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
                preview::Action::ToggleTask { offset } => {
                    self.toggle_task(offset);
                }
            }
        }
    }
}

/// Путь новой заметки по цели wikilink: берётся только `path` (без якоря),
/// `None` — если путь пустой (`[[#Заголовок]]`), создавать нечего.
pub(crate) fn new_note_path(from: &Path, raw: &str) -> Option<PathBuf> {
    let markdown::LinkTarget { path, .. } = markdown::parse_link_target(raw);
    if path.is_empty() {
        return None;
    }
    let name = if path.to_lowercase().ends_with(".md") {
        path.to_string()
    } else {
        format!("{path}.md")
    };
    let dir = from.parent().unwrap_or(Path::new(""));
    Some(dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn new_note_path_uses_path_without_anchor() {
        let from = Path::new("sub/cur.md");
        assert_eq!(
            new_note_path(from, "Note#Заголовок"),
            Some(PathBuf::from("sub/Note.md"))
        );
        assert_eq!(
            new_note_path(from, "Note#^block"),
            Some(PathBuf::from("sub/Note.md"))
        );
        // Расширение не дублируется, регистр не важен.
        assert_eq!(new_note_path(from, "a.MD"), Some(PathBuf::from("sub/a.MD")));
        // Только якорь — создавать нечего.
        assert_eq!(new_note_path(from, "#Только якорь"), None);
    }

    // --------- A3: внешние изменения открытой заметки (на реальных файлах) ---------

    /// Vault во временной папке с заметкой `a.md` = "исходный", открытой в редакторе.
    fn open_a() -> (VaultState, PathBuf) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mdwriter-a3-{unique}-{}",
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "исходный").unwrap();
        let watcher = VaultWatcher::new(&dir, || {}).unwrap();
        let mut vs = VaultState::open(Vault::open(dir.clone()), watcher, Notices::default());
        vs.load(PathBuf::from("a.md"));
        assert_eq!(vs.editor_text, "исходный");
        (vs, dir)
    }

    fn edit(vs: &mut VaultState, text: &str) {
        vs.editor_text = text.to_string();
        vs.dirty = true;
    }

    fn disk(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("a.md")).unwrap()
    }

    #[test]
    fn a3_reload_when_changed_externally_without_edits() {
        let (mut vs, dir) = open_a();
        std::fs::write(dir.join("a.md"), "извне").unwrap();
        vs.rescan();
        assert_eq!(vs.editor_text, "извне");
        assert!(!vs.conflict && !vs.dirty);
    }

    #[test]
    fn a3_own_save_is_not_a_conflict() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        vs.flush().unwrap();
        vs.rescan(); // как после события watcher о нашей же записи
        assert!(!vs.conflict && !vs.dirty);
        assert_eq!(disk(&dir), "мои");
    }

    #[test]
    fn a3_conflict_on_rescan_keeps_edits_and_file() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        std::fs::write(dir.join("a.md"), "чужие").unwrap();
        vs.rescan();
        assert!(vs.conflict);
        assert_eq!(vs.editor_text, "мои");
        assert!(vs.flush().is_err(), "при конфликте автосохранение не пишет");
        assert_eq!(disk(&dir), "чужие");
    }

    #[test]
    fn a3_flush_detects_race_before_watcher() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        std::fs::write(dir.join("a.md"), "чужие").unwrap();
        // rescan ещё не было — flush сам замечает внешнее изменение.
        assert!(vs.flush().is_err());
        assert!(vs.conflict);
        assert_eq!(disk(&dir), "чужие");
    }

    #[test]
    fn a3_resolve_keep_mine_overwrites() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        std::fs::write(dir.join("a.md"), "чужие").unwrap();
        vs.rescan();
        vs.resolve_keep_mine();
        assert_eq!(disk(&dir), "мои");
        assert!(!vs.conflict && !vs.dirty);
    }

    #[test]
    fn a3_resolve_take_disk_drops_edits() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        std::fs::write(dir.join("a.md"), "чужие").unwrap();
        vs.rescan();
        vs.resolve_take_disk();
        assert_eq!(vs.editor_text, "чужие");
        assert!(!vs.conflict && !vs.dirty);
        assert_eq!(disk(&dir), "чужие");
    }

    #[test]
    fn a3_resolve_save_copy_keeps_both_versions() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        std::fs::write(dir.join("a.md"), "чужие").unwrap();
        vs.rescan();
        vs.resolve_save_copy();
        assert_eq!(
            std::fs::read_to_string(dir.join("a (конфликт).md")).unwrap(),
            "мои"
        );
        assert_eq!(vs.editor_text, "чужие");
        assert_eq!(disk(&dir), "чужие");
        assert!(!vs.conflict && !vs.dirty);
    }

    #[test]
    fn a3_orphaned_keeps_buffer_and_can_save_again() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        std::fs::remove_file(dir.join("a.md")).unwrap();
        vs.rescan();
        assert!(vs.orphaned);
        assert_eq!(vs.open.as_deref(), Some(Path::new("a.md")));
        assert_eq!(vs.editor_text, "мои");
        vs.resolve_keep_mine();
        assert_eq!(disk(&dir), "мои");
        assert!(!vs.orphaned && !vs.dirty);
    }

    #[test]
    fn a3_close_when_deleted_without_edits() {
        let (mut vs, dir) = open_a();
        std::fs::remove_file(dir.join("a.md")).unwrap();
        vs.rescan();
        assert!(vs.open.is_none());
        assert!(vs.editor_text.is_empty());
    }

    #[test]
    fn a3_flush_or_copy_never_loses_edits() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        std::fs::write(dir.join("a.md"), "чужие").unwrap();
        vs.rescan();
        vs.flush_or_copy().unwrap();
        assert_eq!(disk(&dir), "чужие");
        assert_eq!(
            std::fs::read_to_string(dir.join("a (конфликт).md")).unwrap(),
            "мои"
        );
    }

    #[test]
    fn g4_flush_or_copy_falls_back_to_copy_on_write_error() {
        let (mut vs, dir) = open_a();
        edit(&mut vs, "мои");
        // Вместо файла заметки — каталог: и чтение, и запись падают.
        std::fs::remove_file(dir.join("a.md")).unwrap();
        std::fs::create_dir(dir.join("a.md")).unwrap();
        vs.flush_or_copy().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("a (конфликт).md")).unwrap(),
            "мои"
        );
    }

    #[test]
    fn a3_load_other_note_blocked_during_conflict() {
        let (mut vs, dir) = open_a();
        std::fs::write(dir.join("b.md"), "b").unwrap();
        edit(&mut vs, "мои");
        std::fs::write(dir.join("a.md"), "чужие").unwrap();
        vs.rescan();
        vs.load(PathBuf::from("b.md"));
        assert_eq!(vs.open.as_deref(), Some(Path::new("a.md")));
        assert_eq!(vs.editor_text, "мои");
    }

    // ------------------- B7: наблюдатель и пересканирование -------------------

    #[test]
    fn b7_schedule_waits_from_first_event() {
        // Ничего не пришло — ничего не планируем.
        assert_eq!(rescan_schedule(None, false, 1.0), (None, false));
        // Первое событие — пересканирование через RESCAN_DELAY.
        let (p, due) = rescan_schedule(None, true, 1.0);
        assert_eq!((p, due), (Some(1.0 + RESCAN_DELAY), false));
        // Новые события не откладывают уже назначенное время.
        assert_eq!(rescan_schedule(p, true, 1.2), (p, false));
        // Время пришло — пересканировать и сбросить план.
        assert_eq!(rescan_schedule(p, false, 1.0 + RESCAN_DELAY), (None, true));
    }

    #[test]
    fn b7_external_change_reaches_editor_through_watcher() {
        use std::time::{Duration, Instant};

        let (mut vs, dir) = open_a();
        let ctx = egui::Context::default();
        std::fs::write(dir.join("a.md"), "извне").unwrap();

        // Ждём, пока событие дойдёт и пересканирование будет запланировано.
        let deadline = Instant::now() + Duration::from_secs(5);
        while vs.pending_rescan_at.is_none() && Instant::now() < deadline {
            vs.poll_watcher(0.0, &ctx);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(vs.pending_rescan_at, Some(RESCAN_DELAY));
        // До срока ничего не меняется.
        vs.poll_watcher(RESCAN_DELAY / 2.0, &ctx);
        assert_eq!(vs.editor_text, "исходный");
        // В срок — пересканирование, A3 перезагружает чистую заметку.
        vs.poll_watcher(RESCAN_DELAY, &ctx);
        assert_eq!(vs.pending_rescan_at, None);
        assert_eq!(vs.editor_text, "извне");
    }

    // ------------------- B2: переключение задач -------------------

    /// Документ, на котором старый построчный поиск переключал не ту задачу:
    /// frontmatter, задача в цитате (её не видел счётчик строк) и `[ ]` в
    /// тексте задачи.
    const B2_DOC: &str =
        "---\ntitle: Т\n---\n> - [ ] в цитате\n\n- [x] купить [ ] хлеб\n10. [ ] десятая\n";

    #[test]
    fn b2_click_toggles_exactly_that_task() {
        let (mut vs, _dir) = open_a();
        vs.editor_text = B2_DOC.to_string();
        let offsets = preview::document_task_offsets(&vs.editor_text);
        assert_eq!(offsets.len(), 3);

        // Клик по второму чекбоксу («купить»): меняется только её маркер.
        vs.toggle_task(offsets[1]);
        assert_eq!(
            vs.editor_text,
            "---\ntitle: Т\n---\n> - [ ] в цитате\n\n- [ ] купить [ ] хлеб\n10. [ ] десятая\n"
        );
        assert!(vs.dirty);

        // Третий («десятая», список с номером 10).
        vs.toggle_task(offsets[2]);
        assert!(vs.editor_text.ends_with("10. [x] десятая\n"));
        // Первый — в цитате.
        vs.toggle_task(offsets[0]);
        assert!(vs.editor_text.contains("> - [x] в цитате"));
    }

    #[test]
    fn b2_stale_offset_changes_nothing() {
        let (mut vs, _dir) = open_a();
        vs.editor_text = B2_DOC.to_string();
        let offsets = preview::document_task_offsets(&vs.editor_text);
        // Пользователь успел дописать текст в начало — смещение устарело.
        vs.editor_text.insert(0, 'x');
        vs.dirty = false;
        vs.toggle_task(offsets[1]);
        assert_eq!(vs.editor_text, format!("x{B2_DOC}"));
        assert!(!vs.dirty);
    }
}
