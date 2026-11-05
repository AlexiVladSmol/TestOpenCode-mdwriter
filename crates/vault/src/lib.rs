//! Vault: локальное хранилище заметок (папка с `.md` файлами).

pub mod store;
pub mod watcher;

pub use store::{Result, Vault, VaultError};
pub use watcher::VaultWatcher;
