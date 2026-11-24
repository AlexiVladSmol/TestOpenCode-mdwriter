//! Встроенный выбор папки для vault и папки экспорта.
//!
//! Раньше здесь использовался `rfd`, то есть на Linux — xdg-desktop-portal.
//! Вызов портала блокирует поток отрисовки на всё время работы диалога, а без
//! родительского окна портал открывает выбор папки «сиротой» (в журнале:
//! `Failed to associate portal window with parent window`): приложение
//! выглядело зависшим, а сам диалог мог оказаться позади окна. Собственный
//! браузер папок не зависит от D-Bus и порталов, одинаково работает на Linux
//! и Windows, а его логика покрыта тестами.

use std::path::{Path, PathBuf};

use eframe::egui;

/// Что выбираем: папку или папку + имя файла (экспорт).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Open,
    Save,
}

/// Результат работы модалки за текущий кадр.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Выбрана папка (режим [`Mode::Open`]).
    Opened(PathBuf),
    /// Выбран путь файла (режим [`Mode::Save`]).
    Saved(PathBuf),
    /// Пользователь закрыл модалку.
    Cancelled,
}

/// Одна подпапка в списке.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DirItem {
    name: String,
    path: PathBuf,
}

/// Состояние модального окна выбора папки.
pub(crate) struct Picker {
    mode: Mode,
    /// Текущая открытая папка (её и возвращаем в режиме [`Mode::Open`]).
    current: PathBuf,
    dirs: Vec<DirItem>,
    selected: Option<usize>,
    /// Поле ввода пути: можно вставить путь и нажать Enter.
    path_input: String,
    /// Имя файла в режиме [`Mode::Save`].
    name_input: String,
    show_hidden: bool,
    error: Option<String>,
    /// Путь, который уже существует и ждёт подтверждения замены.
    confirm: Option<PathBuf>,
}

impl Picker {
    /// Выбор папки для vault.
    pub(crate) fn open_vault(start: PathBuf) -> Self {
        Self::new(Mode::Open, start, String::new())
    }

    /// Выбор папки и имени файла для экспорта в HTML.
    pub(crate) fn export_html(start: PathBuf, file_name: String) -> Self {
        Self::new(Mode::Save, start, file_name)
    }

    fn new(mode: Mode, start: PathBuf, file_name: String) -> Self {
        let mut picker = Self {
            mode,
            current: PathBuf::new(),
            dirs: Vec::new(),
            selected: None,
            path_input: String::new(),
            name_input: file_name,
            show_hidden: false,
            error: None,
            confirm: None,
        };
        picker.goto(start);
        picker
    }

    /// Переходит в папку, если её можно прочитать; иначе оставляет текущую и
    /// запоминает сообщение об ошибке.
    fn goto(&mut self, dir: PathBuf) {
        match read_dirs(&dir, self.show_hidden) {
            Ok(dirs) => {
                self.path_input = dir.display().to_string();
                self.current = dir;
                self.dirs = dirs;
                self.selected = None;
                self.error = None;
                // Другая папка — вопрос о замене относится к прежнему пути.
                self.confirm = None;
            }
            Err(e) => {
                self.error = Some(format!("Не удалось открыть {}: {e}", dir.display()));
            }
        }
    }

    fn up(&mut self) {
        if let Some(parent) = self.current.parent() {
            let parent = parent.to_path_buf();
            self.goto(parent);
        }
    }

    /// Переход по тексту из поля пути (поддерживает `~` и относительные пути).
    fn apply_typed_path(&mut self) {
        let path = resolve_input(&self.path_input, &self.current);
        self.goto(path);
    }

    /// Итоговый путь файла в режиме [`Mode::Save`].
    ///
    /// Экспорт — только HTML: `.html`/`.htm`. Имя без расширения получает
    /// `.html`; любое другое расширение (`заметка.md`) — ошибка, иначе файл
    /// молча записался бы не в том формате.
    fn save_path(&self) -> Result<PathBuf, String> {
        let name = self.name_input.trim();
        if name.is_empty() {
            return Err("Укажите имя файла".to_string());
        }
        if name.contains('/') || name.contains('\\') {
            return Err("Имя файла не должно содержать слэши".to_string());
        }
        let name = with_html_extension(name);
        if !has_html_extension(&name) {
            return Err("Экспорт возможен только в файл .html или .htm".to_string());
        }
        Ok(self.current.join(name))
    }

    /// Что делать по кнопке «Сохранить сюда»: писать сразу, спросить о замене
    /// существующего файла или показать ошибку в модалке.
    fn save_step(&self) -> SaveStep {
        match self.save_path() {
            Ok(path) if path.exists() => SaveStep::Confirm(path),
            Ok(path) => SaveStep::Save(path),
            Err(message) => SaveStep::Reject(message),
        }
    }
}

/// Решение по кнопке сохранения в режиме [`Mode::Save`].
#[derive(Debug, PartialEq, Eq)]
enum SaveStep {
    /// Файла нет — можно писать.
    Save(PathBuf),
    /// Файл уже есть — нужен явный ответ «Заменить».
    Confirm(PathBuf),
    /// Имя или расширение не подходят.
    Reject(String),
}

/// Рисует модалку и возвращает результат, если пользователь что-то выбрал.
pub(crate) fn show(ui: &mut egui::Ui, picker: &mut Picker) -> Option<Action> {
    let mut action: Option<Action> = None;
    let mut goto: Option<PathBuf> = None;
    let mut hide_toggled = false;

    let modal = egui::Modal::new(egui::Id::unique("folder_picker"))
        .area(
            egui::Area::new(egui::Id::unique("folder_picker_area"))
                .kind(egui::UiKind::Modal)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .order(egui::Order::Foreground)
                .interactable(true)
                .sense(egui::Sense::hover()),
        )
        .show(ui.ctx(), |ui| {
            ui.set_min_width(640.0);
            let title = match picker.mode {
                Mode::Open => "Открыть папку с заметками",
                Mode::Save => "Экспорт заметки в HTML",
            };
            ui.strong(title);
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                if ui
                    .button("Вверх")
                    .on_hover_text("Родительская папка")
                    .clicked()
                {
                    picker.up();
                }
                if ui.button("Домой").clicked() {
                    if let Some(home) = home_dir() {
                        picker.goto(home);
                    }
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut picker.path_input)
                        .hint_text("Путь к папке (Enter — перейти)")
                        .desired_width(f32::INFINITY)
                        .id_salt("folder_picker_path"),
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    picker.apply_typed_path();
                }
            });

            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if picker.dirs.is_empty() {
                        ui.weak("Вложенных папок нет");
                    }
                    for (i, item) in picker.dirs.iter().enumerate() {
                        let selected = picker.selected == Some(i);
                        let response = ui.selectable_label(selected, item.name.as_str());
                        if response.clicked() {
                            picker.selected = Some(i);
                        }
                        if response.double_clicked() {
                            goto = Some(item.path.clone());
                        }
                    }
                });

            ui.add_space(4.0);
            ui.separator();

            if picker.mode == Mode::Save {
                ui.horizontal(|ui| {
                    ui.label("Имя файла:");
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut picker.name_input)
                            .desired_width(260.0)
                            .id_salt("folder_picker_name"),
                    );
                    // Другое имя — вопрос о замене уже не актуален.
                    if response.changed() {
                        picker.confirm = None;
                    }
                });
                if let Some(path) = picker.confirm.clone() {
                    let color = ui.visuals().warn_fg_color;
                    ui.colored_label(
                        color,
                        format!("Файл «{}» уже существует. Заменить?", path.display()),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Заменить").clicked() {
                            picker.confirm = None;
                            action = Some(Action::Saved(path.clone()));
                        }
                        if ui.button("Отмена").clicked() {
                            picker.confirm = None;
                        }
                    });
                }
            }

            ui.horizontal(|ui| {
                if ui
                    .checkbox(&mut picker.show_hidden, "Показывать скрытые")
                    .changed()
                {
                    hide_toggled = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let confirm = match picker.mode {
                        Mode::Open => "Открыть эту папку",
                        Mode::Save => "Сохранить сюда",
                    };
                    if ui.button("Отмена").clicked() {
                        action = Some(Action::Cancelled);
                    }
                    if ui.button(confirm).clicked() {
                        match picker.mode {
                            Mode::Open => action = Some(Action::Opened(picker.current.clone())),
                            Mode::Save => match picker.save_step() {
                                SaveStep::Save(path) => action = Some(Action::Saved(path)),
                                // Молча не перезаписываем: спрашиваем в модалке.
                                SaveStep::Confirm(path) => picker.confirm = Some(path),
                                // Ошибку ввода показываем в модалке, не закрывая её.
                                SaveStep::Reject(message) => picker.error = Some(message),
                            },
                        }
                    }
                });
            });

            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(picker.current.display().to_string())
                    .small()
                    .weak(),
            );
            if let Some(error) = &picker.error {
                let color = ui.visuals().error_fg_color;
                ui.colored_label(color, error);
            }
            ui.label(
                egui::RichText::new("Двойной клик по папке — войти в неё")
                    .small()
                    .weak(),
            );
        });

    // Отложенные изменения: нельзя менять `picker` во время обхода `dirs`.
    if let Some(path) = goto {
        picker.goto(path);
    }
    if hide_toggled {
        let current = picker.current.clone();
        picker.goto(current);
    }

    if modal.should_close() {
        return Some(Action::Cancelled);
    }
    action
}

/// Читает подпапки `dir` (без файлов), сортирует по имени без учёта регистра.
fn read_dirs(dir: &Path, show_hidden: bool) -> std::io::Result<Vec<DirItem>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Симлинк на папку тоже показываем: это обычный способ держать vault
        // в другом месте (например, `~/notes -> /mnt/notes`).
        let is_dir = file_type.is_dir() || (file_type.is_symlink() && entry.path().is_dir());
        if !is_dir {
            continue;
        }
        dirs.push(DirItem {
            name,
            path: entry.path(),
        });
    }
    dirs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(dirs)
}

/// Домашняя папка без внешних крейтов (`dirs`/`home`).
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Превращает введённый текст в путь: `~`, абсолютный или относительный.
fn resolve_input(input: &str, base: &Path) -> PathBuf {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix('~') {
        let rest = rest.trim_start_matches(['/', '\\']);
        return match home_dir() {
            Some(home) if rest.is_empty() => home,
            Some(home) => home.join(rest),
            None => base.to_path_buf(),
        };
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

/// Расширение `.html`/`.htm` (без учёта регистра).
fn has_html_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

/// Дописывает `.html`, если у имени нет расширения.
fn with_html_extension(name: &str) -> String {
    if Path::new(name).extension().is_some() {
        name.to_string()
    } else {
        format!("{name}.html")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    use super::*;

    /// Временная папка, удаляется в `Drop`.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            // Время + счётчик: на Windows часы тикают ~15 мс, и параллельные
            // тесты успевали получить одинаковые имена папок.
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mdwriter-picker-{tag}-{nanos}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("временная папка");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_dirs_returns_only_visible_directories_sorted() {
        let tmp = TempDir::new("list");
        std::fs::create_dir(tmp.path().join("beta")).unwrap();
        std::fs::create_dir(tmp.path().join("Alpha")).unwrap();
        std::fs::create_dir(tmp.path().join(".hidden")).unwrap();
        std::fs::write(tmp.path().join("note.md"), "текст").unwrap();

        let names: Vec<String> = read_dirs(tmp.path(), false)
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, vec!["Alpha".to_string(), "beta".to_string()]);

        let with_hidden: Vec<String> = read_dirs(tmp.path(), true)
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(with_hidden.len(), 3);
        assert!(with_hidden.contains(&".hidden".to_string()));
    }

    #[test]
    fn resolve_input_handles_absolute_relative_and_home() {
        let tmp = TempDir::new("resolve");
        let base = tmp.path().join("base");
        std::fs::create_dir_all(&base).unwrap();

        // Абсолютный путь (строится из реальной временной папки, чтобы тест
        // работал и на Windows) не должен склеиваться с базой.
        let absolute = tmp.path().to_path_buf();
        assert_eq!(
            resolve_input(&absolute.display().to_string(), &base),
            absolute
        );
        assert_eq!(resolve_input("notes", &base), base.join("notes"));
        assert_eq!(resolve_input("  notes  ", &base), base.join("notes"));

        if let Some(home) = home_dir() {
            assert_eq!(resolve_input("~", &base), home);
            assert_eq!(resolve_input("~/notes", &base), home.join("notes"));
        }
    }

    #[test]
    fn with_html_extension_adds_extension_only_when_missing() {
        assert_eq!(with_html_extension("note"), "note.html");
        assert_eq!(with_html_extension("note.html"), "note.html");
        assert_eq!(with_html_extension("note.HTML"), "note.HTML");
        assert_eq!(with_html_extension("my.note"), "my.note");
    }

    #[test]
    fn has_html_extension_accepts_only_html_and_htm() {
        assert!(has_html_extension("note.html"));
        assert!(has_html_extension("note.HTML"));
        assert!(has_html_extension("note.htm"));
        assert!(!has_html_extension("note.md"));
        assert!(!has_html_extension("note.htm.txt"));
        assert!(!has_html_extension("note"));
    }

    /// G6a: экспорт — только в `.html`/`.htm`, иначе файл записался бы
    /// в формате, которого пользователь не ждал (`заметка.md` с HTML внутри).
    #[test]
    fn save_path_accepts_only_html_extensions() {
        let tmp = TempDir::new("save-ext");
        let mut picker = Picker::export_html(tmp.path().to_path_buf(), String::new());

        picker.name_input = "заметка.md".to_string();
        let message = picker.save_path().unwrap_err();
        assert!(message.contains(".html"), "{message}");

        picker.name_input = "note.htm".to_string();
        assert_eq!(picker.save_path().unwrap(), tmp.path().join("note.htm"));

        picker.name_input = "note.HTML".to_string();
        assert_eq!(picker.save_path().unwrap(), tmp.path().join("note.HTML"));

        picker.name_input = "note".to_string();
        assert_eq!(picker.save_path().unwrap(), tmp.path().join("note.html"));
    }

    /// G6a: существующий файл не перезаписывается без явного подтверждения.
    #[test]
    fn save_step_asks_before_overwriting() {
        let tmp = TempDir::new("save-exists");
        std::fs::write(tmp.path().join("note.html"), "старый").unwrap();
        let mut picker = Picker::export_html(tmp.path().to_path_buf(), "note.html".to_string());

        let expected = tmp.path().join("note.html");
        assert_eq!(picker.save_step(), SaveStep::Confirm(expected.clone()));

        // Новое имя — писать можно без вопросов.
        picker.name_input = "fresh.html".to_string();
        assert_eq!(
            picker.save_step(),
            SaveStep::Save(tmp.path().join("fresh.html"))
        );

        // Ошибка ввода — не решение о записи.
        picker.name_input = "note.md".to_string();
        assert!(matches!(picker.save_step(), SaveStep::Reject(_)));
    }

    #[test]
    fn save_path_rejects_slashes_and_empty_names() {
        let tmp = TempDir::new("save");
        let mut picker = Picker::export_html(tmp.path().to_path_buf(), "note.html".to_string());
        assert_eq!(picker.save_path().unwrap(), tmp.path().join("note.html"));

        picker.name_input = "   ".to_string();
        assert!(picker.save_path().is_err());

        picker.name_input = "sub/note".to_string();
        assert!(picker.save_path().is_err());

        picker.name_input = "note".to_string();
        assert_eq!(picker.save_path().unwrap(), tmp.path().join("note.html"));
    }

    #[test]
    fn goto_sets_current_and_up_walks_to_parent() {
        let tmp = TempDir::new("goto");
        let child = tmp.path().join("child");
        std::fs::create_dir(&child).unwrap();

        let mut picker = Picker::open_vault(tmp.path().to_path_buf());
        assert_eq!(picker.current, tmp.path());
        assert_eq!(picker.dirs.len(), 1);

        picker.goto(child.clone());
        assert_eq!(picker.current, child);
        assert!(picker.dirs.is_empty());

        picker.up();
        assert_eq!(picker.current, tmp.path());
    }

    /// Прогоняет один кадр модалки в тестовом egui-контексте.
    fn run_frame(
        ctx: &egui::Context,
        picker: &mut Picker,
        events: Vec<egui::Event>,
    ) -> Option<Action> {
        let mut action = None;
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            action = show(ui, picker);
        });
        // В тестах нет рендерера: дельты текстур нужно «принять», иначе
        // epaint паникует при их удалении.
        output.textures_delta.clear();
        action
    }

    #[test]
    fn show_renders_without_panicking() {
        let ctx = egui::Context::default();
        let mut picker = Picker::open_vault(std::env::temp_dir());
        assert_eq!(run_frame(&ctx, &mut picker, Vec::new()), None);
    }

    #[test]
    fn escape_closes_the_modal() {
        let ctx = egui::Context::default();
        let mut picker = Picker::open_vault(std::env::temp_dir());
        // Первый кадр — модалка становится верхней, второй — обрабатывает Esc.
        assert_eq!(run_frame(&ctx, &mut picker, Vec::new()), None);
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        assert_eq!(
            run_frame(&ctx, &mut picker, vec![esc]),
            Some(Action::Cancelled)
        );
    }

    #[test]
    fn goto_keeps_previous_dir_on_error() {
        let tmp = TempDir::new("err");
        let mut picker = Picker::open_vault(tmp.path().to_path_buf());
        picker.goto(tmp.path().join("нет-такой-папки"));
        assert_eq!(picker.current, tmp.path());
        assert!(picker.error.is_some());
    }
}
