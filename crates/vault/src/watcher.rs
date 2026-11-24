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
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            rx,
        })
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
