//! Слежение за изменениями файлов vault через `notify`.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// Фоновый наблюдатель за папкой vault. О любом изменении сигнализирует
/// через канал; приложение опрашивает его раз в кадр.
pub struct VaultWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<()>,
}

impl VaultWatcher {
    pub fn new(root: &Path) -> notify::Result<Self> {
        let (tx, rx) = channel::<()>();
        let mut watcher = notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| {
                if res.is_ok() {
                    let _ = tx.send(());
                }
            },
        )?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            rx,
        })
    }

    /// Было ли хоть одно изменение с прошлого вызова (неблокирующе).
    pub fn has_changes(&self) -> bool {
        self.rx.try_iter().next().is_some()
    }
}
