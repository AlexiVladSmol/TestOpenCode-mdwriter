//! Слежение за изменениями файлов vault через `notify`.
//!
//! Колбэк `notify` работает в фоновом потоке: он отбрасывает неинтересные
//! события (чтение файлов, скрытые папки вроде `.git`/`.obsidian`, временные
//! файлы атомарной записи), складывает пути изменённых файлов в канал и
//! будит UI через `on_change`. Приложение забирает накопленное
//! [`VaultWatcher::take_changes`].

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Фоновый наблюдатель за папкой vault.
pub struct VaultWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<Vec<PathBuf>>,
    /// Папки, которые не удалось поставить на слежение (обычно нет прав).
    skipped: Vec<String>,
}

impl VaultWatcher {
    /// Начинает следить за `root`. `on_change` вызывается из фонового потока
    /// после каждого значимого события — например, чтобы перерисовать окно
    /// (`ctx.request_repaint()`), даже если пользователь ничего не делает.
    pub fn new(root: &Path, on_change: impl Fn() + Send + 'static) -> notify::Result<Self> {
        let (tx, rx) = channel::<Vec<PathBuf>>();
        let root_owned = root.to_path_buf();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let paths = match res {
                    Ok(event) => relevant_paths(&root_owned, &event),
                    // Ошибка наблюдателя (например, переполнение очереди ОС):
                    // что-то могло измениться — просим полное пересканирование.
                    Err(_) => Some(vec![root_owned.clone()]),
                };
                if let Some(paths) = paths {
                    if tx.send(paths).is_ok() {
                        on_change();
                    }
                }
            })?;
        // `RecursiveMode::Recursive` обрывается на первой недоступной
        // подпапке (например, на симлинке внутрь `/tmp` из `~/.wine`), и
        // тогда vault не открывается вовсе. Поэтому ставим слежение сами:
        // корень обязателен, остальные папки — по одной, недоступные
        // пропускаем и запоминаем.
        watcher.watch(root, RecursiveMode::NonRecursive)?;
        let mut skipped = Vec::new();
        for dir in dirs_to_watch(root) {
            if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                skipped.push(format!("{}: {e}", dir.display()));
            }
        }
        Ok(Self {
            _watcher: watcher,
            rx,
            skipped,
        })
    }

    /// Папки, за которыми слежение поставить не удалось (нет прав, кончились
    /// `inotify` watches). Изменения в них не придут автоматически.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Все изменённые пути с прошлого вызова (неблокирующе): без дублей,
    /// отсортированы. Пустой вектор — изменений не было. Путь, равный корню
    /// vault, означает «изменилось неизвестно что» — нужно полное
    /// пересканирование.
    pub fn take_changes(&self) -> Vec<PathBuf> {
        let mut all: Vec<PathBuf> = self.rx.try_iter().flatten().collect();
        all.sort();
        all.dedup();
        all
    }
}

/// Все папки vault, за которыми нужно следить, кроме самого корня.
///
/// Скрытые папки пропускаются — ровно так же, как их пропускает обход заметок
/// ([`crate::Vault::list_notes`]): в vault-е с домашней папкой это `.git`,
/// `.cache`, `.wine` и прочее, где слежение бессмысленно и часто невозможно.
/// Симлинк на папку тоже попадает в список, но внутрь него обход не идёт,
/// поэтому циклы невозможны.
pub(crate) fn dirs_to_watch(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !crate::store::is_hidden(e));
    for entry in walker.flatten() {
        if entry.depth() == 0 {
            continue;
        }
        let file_type = entry.file_type();
        let is_dir = file_type.is_dir() || (file_type.is_symlink() && entry.path().is_dir());
        if is_dir {
            dirs.push(entry.into_path());
        }
    }
    dirs
}

/// Пути события, которые стоит передать приложению; `None` — событие
/// неинтересно и его можно отбросить.
fn relevant_paths(root: &Path, event: &Event) -> Option<Vec<PathBuf>> {
    // Чтение/открытие файлов (в том числе нашим же индексатором) ничего не меняет.
    if matches!(event.kind, EventKind::Access(_)) {
        return None;
    }
    // ОС потеряла часть событий или не сообщила путь — пересканировать всё.
    if event.need_rescan() || event.paths.is_empty() {
        return Some(vec![root.to_path_buf()]);
    }
    let paths: Vec<PathBuf> = event
        .paths
        .iter()
        .filter(|p| is_relevant(root, p))
        .cloned()
        .collect();
    (!paths.is_empty()).then_some(paths)
}

/// Интересен ли путь приложению: всё внутри vault, кроме скрытого — любой
/// компонент пути относительно корня начинается с `.` (`.git/…`,
/// `.obsidian/…`, временные файлы атомарной записи `.имя.pid.n.tmp`).
/// Сам корень может быть скрытым (`~/.notes`) — он не учитывается.
/// Путь вне корня (не должен приходить) считается интересным.
pub fn is_relevant(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    !rel.components().any(|c| match c {
        Component::Normal(s) => s.to_string_lossy().starts_with('.'),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, Flag, ModifyKind, RemoveKind};
    use std::time::{Duration, Instant};

    fn root() -> PathBuf {
        PathBuf::from("vault")
    }

    #[test]
    fn hidden_and_temp_paths_are_ignored() {
        let r = root();
        assert!(!is_relevant(&r, &r.join(".git").join("index")));
        assert!(!is_relevant(
            &r,
            &r.join(".obsidian").join("workspace.json")
        ));
        assert!(!is_relevant(&r, &r.join(".a.md.1234.0.tmp")));
        assert!(!is_relevant(&r, &r.join("sub").join(".hidden.md")));
        assert!(is_relevant(&r, &r.join("sub").join("a.md")));
        assert!(is_relevant(&r, &r.join("a.md")));
    }

    #[test]
    fn hidden_root_itself_does_not_hide_notes() {
        let r = PathBuf::from("home").join(".notes");
        assert!(is_relevant(&r, &r.join("a.md")));
        assert!(!is_relevant(&r, &r.join(".git").join("HEAD")));
    }

    #[test]
    fn path_outside_root_is_relevant() {
        assert!(is_relevant(&root(), Path::new("elsewhere/a.md")));
    }

    #[test]
    fn access_events_are_dropped() {
        let r = root();
        let e = Event::new(EventKind::Access(AccessKind::Any)).add_path(r.join("a.md"));
        assert_eq!(relevant_paths(&r, &e), None);
    }

    #[test]
    fn only_relevant_paths_are_kept() {
        let r = root();
        let e = Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(r.join(".a.md.1.0.tmp"))
            .add_path(r.join("a.md"));
        assert_eq!(relevant_paths(&r, &e), Some(vec![r.join("a.md")]));

        let hidden_only =
            Event::new(EventKind::Create(CreateKind::Any)).add_path(r.join(".git").join("x"));
        assert_eq!(relevant_paths(&r, &hidden_only), None);
    }

    #[test]
    fn rescan_or_pathless_events_request_full_rescan() {
        let r = root();
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(relevant_paths(&r, &rescan), Some(vec![r.clone()]));
        let pathless = Event::new(EventKind::Remove(RemoveKind::Any));
        assert_eq!(relevant_paths(&r, &pathless), Some(vec![r.clone()]));
    }

    /// Временная папка для тестов слежения (удаляется в `Drop`).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("mdwriter-watch-{tag}-{unique}"));
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap_or(dir);
            Self(dir)
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
    fn dirs_to_watch_skips_hidden_directories_and_files() {
        let dir = TempDir::new("dirs");
        std::fs::create_dir_all(dir.path().join("sub").join("deep")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("note.md"), "текст").unwrap();

        let mut dirs: Vec<PathBuf> = dirs_to_watch(dir.path())
            .into_iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_path_buf())
            .collect();
        dirs.sort();
        assert_eq!(
            dirs,
            vec![PathBuf::from("sub"), PathBuf::from("sub").join("deep")],
            "корень не дублируется, скрытые папки и файлы пропущены"
        );
    }

    #[test]
    fn hidden_root_itself_is_watched() {
        // Vault может лежать в скрытой папке (`~/.notes`): корень не фильтруется.
        let dir = TempDir::new("hidden-root");
        let hidden = dir.path().join(".notes");
        std::fs::create_dir_all(&hidden).unwrap();
        let watcher = VaultWatcher::new(&hidden, || {}).unwrap();
        assert!(watcher.skipped().is_empty());
    }

    /// Регрессия G7: недоступная подпапка не должна мешать открыть vault.
    #[cfg(unix)]
    #[test]
    fn watcher_reports_inaccessible_subdirectory_instead_of_failing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("noaccess");
        let locked = dir.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::create_dir_all(dir.path().join("open")).unwrap();

        // Права 000: войти в папку от своего же пользователя больше нельзя.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_err();

        let watcher = VaultWatcher::new(dir.path(), || {});
        // Вернуть права до проверок, чтобы TempDir смог удалиться.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        let watcher = watcher.expect("недоступная подпапка не должна ломать открытие vault");
        if readable {
            assert!(
                watcher.skipped().iter().any(|s| s.contains("locked")),
                "ожидали запись о пропущенной папке: {:?}",
                watcher.skipped()
            );
        }
    }

    /// Настоящая ФС: изменение заметки доходит до `take_changes`, будит
    /// колбэк, а запись во временный скрытый файл и в `.git` — нет.
    #[test]
    fn real_fs_changes_are_reported_and_drained() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mdwriter-watch-{unique}"));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        // Канонический путь: так его возвращает и notify (на macOS /var → /private/var).
        let dir = dir.canonicalize().unwrap_or(dir);

        let woken = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&woken);
        let watcher = VaultWatcher::new(&dir, move || {
            w.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        std::fs::write(dir.join(".git").join("index"), "x").unwrap();
        std::fs::write(dir.join(".note.md.1.0.tmp"), "x").unwrap();
        std::fs::write(dir.join("note.md"), "текст").unwrap();

        // Ждём, пока событие о note.md дойдёт (backend может задержать его).
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen: Vec<PathBuf> = Vec::new();
        while Instant::now() < deadline {
            seen.extend(watcher.take_changes());
            if seen.iter().any(|p| p.ends_with("note.md")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            seen.iter().any(|p| p.ends_with("note.md")),
            "нет события о note.md: {seen:?}"
        );
        assert!(
            seen.iter().all(|p| is_relevant(&dir, p)),
            "просочились скрытые пути: {seen:?}"
        );
        assert!(woken.load(Ordering::SeqCst) > 0, "колбэк не вызван");

        // Всё забрано за один раз: следующий вызов пуст, пока ФС не трогают.
        std::thread::sleep(Duration::from_millis(300));
        let _ = watcher.take_changes();
        assert!(watcher.take_changes().is_empty());
    }
}
