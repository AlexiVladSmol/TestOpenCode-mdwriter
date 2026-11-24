//! Слежение за изменениями файлов vault через `notify`.
//!
//! Колбэк `notify` работает в фоновом потоке: он отбрасывает неинтересные
//! события (чтение файлов, скрытые папки вроде `.git`/`.obsidian`, временные
//! файлы атомарной записи), складывает пути изменённых файлов в канал и
//! будит UI через `on_change`. Приложение забирает накопленное
//! [`VaultWatcher::take_changes`].
//!
//! Режим слежения зависит от ОС ([`WatchMode`]):
//! - Windows и macOS — одно рекурсивное слежение за корнем: система сама
//!   следит за всем деревом, новые папки подхватываются автоматически, а
//!   недоступные подпапки ничего не ломают;
//! - Linux и прочие — слежение по папкам: рекурсивный режим `notify` на
//!   inotify обходит все подпапки и обрывается на первой недоступной (G7),
//!   поэтому папки ставятся на слежение по одной, а **новые папки** —
//!   по мере появления в [`VaultWatcher::take_changes`] (G7a).

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Как ставится слежение за деревом папок vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WatchMode {
    /// Одно `RecursiveMode::Recursive` на корень.
    Recursive,
    /// `NonRecursive` на каждую папку; новые папки добавляются по событиям.
    PerDirectory,
}

impl WatchMode {
    /// Режим для текущей ОС.
    fn for_platform() -> Self {
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            WatchMode::Recursive
        } else {
            WatchMode::PerDirectory
        }
    }
}

/// Фоновый наблюдатель за папкой vault.
pub struct VaultWatcher {
    watcher: RecommendedWatcher,
    rx: Receiver<Vec<PathBuf>>,
    root: PathBuf,
    mode: WatchMode,
    /// Папки, которые не удалось поставить на слежение (обычно нет прав).
    skipped: Vec<String>,
}

impl VaultWatcher {
    /// Начинает следить за `root`. `on_change` вызывается из фонового потока
    /// после каждого значимого события — например, чтобы перерисовать окно
    /// (`ctx.request_repaint()`), даже если пользователь ничего не делает.
    pub fn new(root: &Path, on_change: impl Fn() + Send + 'static) -> notify::Result<Self> {
        Self::with_mode(root, WatchMode::for_platform(), on_change)
    }

    pub(crate) fn with_mode(
        root: &Path,
        mode: WatchMode,
        on_change: impl Fn() + Send + 'static,
    ) -> notify::Result<Self> {
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
        let mut skipped = Vec::new();
        match mode {
            WatchMode::Recursive => watcher.watch(root, RecursiveMode::Recursive)?,
            WatchMode::PerDirectory => {
                // Рекурсивный режим на inotify обрывается на первой недоступной
                // подпапке (например, на симлинке внутрь `/tmp` из `~/.wine`),
                // и тогда vault не открывается вовсе. Поэтому корень
                // обязателен, остальные папки — по одной, недоступные
                // пропускаем и запоминаем.
                watcher.watch(root, RecursiveMode::NonRecursive)?;
                for dir in dirs_to_watch(root) {
                    watch_dir(&mut watcher, &dir, &mut skipped);
                }
            }
        }
        Ok(Self {
            watcher,
            rx,
            root: root.to_path_buf(),
            mode,
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
    ///
    /// В режиме слежения по папкам здесь же ставятся на слежение папки,
    /// появившиеся с прошлого вызова (созданные или перенесённые целиком), а
    /// всё, что уже лежит внутри них, добавляется в результат: файлы могли
    /// появиться раньше, чем слежение за папкой успело встать.
    pub fn take_changes(&mut self) -> Vec<PathBuf> {
        let mut all: Vec<PathBuf> = self.rx.try_iter().flatten().collect();
        if self.mode == WatchMode::PerDirectory {
            let new_dirs: Vec<PathBuf> = all
                .iter()
                .filter(|p| *p != &self.root && p.is_dir())
                .cloned()
                .collect();
            for dir in new_dirs {
                self.watch_new_tree(&dir, &mut all);
            }
        }
        all.sort();
        all.dedup();
        all
    }

    /// Ставит на слежение новую папку и её подпапки (кроме скрытых) и
    /// добавляет в `changes` всё, что внутри уже есть.
    fn watch_new_tree(&mut self, dir: &Path, changes: &mut Vec<PathBuf>) {
        if !is_relevant(&self.root, dir) {
            return;
        }
        // Повторная постановка той же папки безопасна: inotify вернёт тот же
        // дескриптор. Поэтому отдельный список «уже следим» не нужен — и не
        // устареет, если папку удалят и создадут заново.
        watch_dir(&mut self.watcher, dir, &mut self.skipped);
        for sub in dirs_to_watch(dir) {
            watch_dir(&mut self.watcher, &sub, &mut self.skipped);
        }
        let walker = walkdir::WalkDir::new(dir)
            .min_depth(1)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !crate::store::is_hidden(e));
        changes.extend(walker.flatten().map(|e| e.into_path()));
    }
}

/// Ставит одну папку на нерекурсивное слежение; ошибку запоминает в `skipped`.
fn watch_dir(watcher: &mut RecommendedWatcher, dir: &Path, skipped: &mut Vec<String>) {
    if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
        skipped.push(format!("{}: {e}", dir.display()));
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
            let dir = crate::test_support::unique_temp_dir(&format!("mdwriter-watch-{tag}-"));
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

        // Сценарий G7 — режим по папкам (на Linux он и так по умолчанию).
        let watcher = VaultWatcher::with_mode(dir.path(), WatchMode::PerDirectory, || {});
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

        let dir = crate::test_support::unique_temp_dir("mdwriter-watch-");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        // Канонический путь: так его возвращает и notify (на macOS /var → /private/var).
        let dir = dir.canonicalize().unwrap_or(dir);

        let woken = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&woken);
        let mut watcher = VaultWatcher::new(&dir, move || {
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

    // ------------- G7a: папки, появившиеся после открытия vault -------------

    /// Копит `take_changes`, пока `done` не выполнится (до 5 с).
    fn collect_until(w: &mut VaultWatcher, done: impl Fn(&[PathBuf]) -> bool) -> Vec<PathBuf> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(w.take_changes());
            if done(&seen) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        seen
    }

    fn has(seen: &[PathBuf], name: &str) -> bool {
        seen.iter().any(|p| p.ends_with(name))
    }

    /// Новая папка, файл в ней — до и после того, как приложение заметило
    /// папку, вложенное дерево, перенесённое целиком, и скрытая папка.
    fn new_folders_are_watched(mode: WatchMode) {
        let root = crate::test_support::unique_temp_dir("mdwriter-g7a-");
        let root = root.canonicalize().unwrap_or(root);
        std::fs::create_dir_all(root.join("old")).unwrap();
        let mut w = VaultWatcher::with_mode(&root, mode, || {}).unwrap();

        // 1. Папка создана, приложение её заметило, потом в ней появилась заметка.
        std::fs::create_dir(root.join("new")).unwrap();
        collect_until(&mut w, |s| has(s, "new"));
        std::fs::write(root.join("new").join("b.md"), "b").unwrap();
        let seen = collect_until(&mut w, |s| has(s, "b.md"));
        assert!(
            has(&seen, "b.md"),
            "{mode:?}: нет события о new/b.md: {seen:?}"
        );

        // 2. Папка и заметка появились сразу, до любого опроса наблюдателя.
        std::fs::create_dir(root.join("fast")).unwrap();
        std::fs::write(root.join("fast").join("c.md"), "c").unwrap();
        let seen = collect_until(&mut w, |s| has(s, "c.md"));
        assert!(
            has(&seen, "c.md"),
            "{mode:?}: нет события о fast/c.md: {seen:?}"
        );

        // 3. Дерево перенесено в vault целиком (так делают синхронизация и git).
        let outside = crate::test_support::unique_temp_dir("mdwriter-g7a-src-");
        std::fs::create_dir_all(outside.join("sub").join("inner")).unwrap();
        std::fs::write(outside.join("sub").join("inner").join("d.md"), "d").unwrap();
        std::fs::rename(&outside, root.join("moved")).unwrap();
        let seen = collect_until(&mut w, |s| has(s, "d.md") || has(s, "moved"));
        assert!(
            has(&seen, "d.md") || has(&seen, "moved"),
            "{mode:?}: перенос дерева не замечен: {seen:?}"
        );
        if mode == WatchMode::PerDirectory {
            // Здесь содержимое перенесённой папки сообщается явно.
            let seen = if has(&seen, "d.md") {
                seen
            } else {
                collect_until(&mut w, |s| has(s, "d.md"))
            };
            assert!(
                has(&seen, "d.md"),
                "содержимое переноса не сообщено: {seen:?}"
            );
        }
        // ...и за вложенными папками перенесённого дерева тоже следим.
        std::thread::sleep(Duration::from_millis(100));
        let _ = w.take_changes();
        let inner = root.join("moved").join("sub").join("inner");
        std::fs::write(inner.join("e.md"), "e").unwrap();
        let seen = collect_until(&mut w, |s| has(s, "e.md"));
        assert!(
            has(&seen, "e.md"),
            "{mode:?}: нет события о moved/sub/inner/e.md: {seen:?}"
        );

        // 4. Скрытая новая папка не сообщается и на слежение не ставится.
        std::fs::create_dir(root.join(".cache")).unwrap();
        std::fs::write(root.join(".cache").join("x.md"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let seen = w.take_changes();
        assert!(
            seen.iter().all(|p| is_relevant(&root, p)),
            "{mode:?}: {seen:?}"
        );
        assert!(w.skipped().is_empty(), "{mode:?}: {:?}", w.skipped());
    }

    /// Рекурсивный режим проверяем только там, где он включается
    /// (`WatchMode::for_platform`). На Linux `notify` эмулирует его поверх
    /// inotify и ставит слежение за новой папкой с запаздыванием: файл,
    /// записанный сразу после `mkdir`, теряется (сценарий 2 падает в CI на
    /// Ubuntu). Поэтому на Linux приложение использует режим по папкам — его
    /// тест ниже идёт на всех ОС.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn g7a_new_folders_recursive_mode() {
        new_folders_are_watched(WatchMode::Recursive);
    }

    #[test]
    fn g7a_new_folders_per_directory_mode() {
        new_folders_are_watched(WatchMode::PerDirectory);
    }

    #[test]
    fn g7a_platform_mode() {
        let expected = if cfg!(any(target_os = "windows", target_os = "macos")) {
            WatchMode::Recursive
        } else {
            WatchMode::PerDirectory
        };
        assert_eq!(WatchMode::for_platform(), expected);
    }
}
