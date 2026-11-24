//! Vault: локальное хранилище заметок (папка с `.md` файлами).

pub mod store;
pub mod watcher;

pub use store::{Result, Vault, VaultError};
pub use watcher::VaultWatcher;

/// Общие помощники для тестов крейта.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Уникальная временная папка: время + pid + счётчик.
    ///
    /// Одного `SystemTime` мало: на Windows системные часы тикают примерно раз
    /// в 15 мс, поэтому тесты, запущенные параллельно, получали одинаковые
    /// имена, работали в одном каталоге и падали случайным образом.
    pub(crate) fn unique_temp_dir(prefix: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "{prefix}{nanos}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
