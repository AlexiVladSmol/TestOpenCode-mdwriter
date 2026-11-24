//! Работа с файлами vault: обход, чтение, атомарная запись, создание.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;

/// Счётчик для уникальных имён временных файлов в пределах процесса.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Сколько раз повторять `rename` при `PermissionDenied` (Windows).
const RENAME_RETRIES: usize = 3;

/// Пауза между повторами `rename`.
const RENAME_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Атомарная запись файла: данные пишутся во временный файл рядом с целевым,
/// сбрасываются на диск (`sync_all`) и только потом заменяют цель через `rename`.
///
/// Так читатель видит либо старое, либо новое содержимое целиком, а сбой питания
/// не оставляет пустой файл. Временный файл начинается с точки, поэтому не
/// попадает в [`Vault::list_notes`] и в наблюдатель vault.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_path(path);
    match write_synced(&tmp, bytes).and_then(|()| rename_with_retry(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Не оставляем мусор после неудачной записи.
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn write_synced(tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::File::create(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    Ok(())
}

/// `fs::rename` с повторами: на Windows файл может быть кратковременно занят
/// антивирусом или синхронизатором.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if cfg!(windows)
                    && e.kind() == std::io::ErrorKind::PermissionDenied
                    && attempt < RENAME_RETRIES =>
            {
                attempt += 1;
                std::thread::sleep(RENAME_RETRY_DELAY);
            }
            Err(e) => return Err(e),
        }
    }
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, VaultError>;

/// Корневая папка vault. Все пути заметок хранятся как относительные к ней.
#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Относительный путь от корня vault (для абсолютных путей).
    pub fn rel_path(&self, abs: &Path) -> PathBuf {
        abs.strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| abs.to_path_buf())
    }

    pub fn abs_path(&self, rel: &Path) -> PathBuf {
        self.root.join(rel)
    }

    /// Рекурсивный список всех `.md` файлов (относительные пути, отсортированы).
    /// Скрытые файлы и папки (начинающиеся с `.`) пропускаются, кроме самого корня:
    /// vault может лежать в скрытой папке (`~/.notes`). Расширение сравнивается
    /// без учёта регистра (`.md`, `.MD`).
    pub fn list_notes(&self) -> Result<Vec<PathBuf>> {
        let mut notes = Vec::new();
        let walker = walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_hidden(e));

        for entry in walker {
            let entry = entry.map_err(std::io::Error::other)?;
            if entry.file_type().is_file() {
                let path = entry.path();
                let is_md = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
                if is_md {
                    notes.push(self.rel_path(path));
                }
            }
        }
        notes.sort();
        Ok(notes)
    }

    pub fn read(&self, rel: &Path) -> Result<String> {
        Ok(fs::read_to_string(self.abs_path(rel))?)
    }

    /// Атомарная запись: пишем во временный файл в той же папке, затем rename.
    pub fn write(&self, rel: &Path, content: &str) -> Result<()> {
        let abs = self.abs_path(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&abs, content.as_bytes())?;
        Ok(())
    }

    /// Создать новый (пустой) файл заметки, создавая родительские папки.
    /// Если файл уже существует — ничего не делает (содержимое не трогается).
    pub fn create(&self, rel: &Path) -> Result<()> {
        let abs = self.abs_path(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        // `create_new` — атомарная проверка «файла ещё нет» без гонки с `exists()`.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&abs)
        {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

/// Имя временного файла: `.{имя}.{pid}.{счётчик}.tmp` в той же папке.
/// Точка в начале скрывает файл от обхода vault и наблюдателя.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}{unique}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_vault() -> (Vault, PathBuf) {
        let dir = temp_dir("mdwriter-test-");
        (Vault::open(dir.clone()), dir)
    }

    #[test]
    fn create_and_list_notes() {
        let (vault, dir) = temp_vault();
        vault.create(Path::new("a.md")).unwrap();
        vault.create(Path::new("sub/b.md")).unwrap();
        vault.create(Path::new("sub/c.md")).unwrap();
        // не-md файлы не попадают
        fs::write(dir.join("notes.txt"), "hi").unwrap();
        // скрытая папка пропускается
        vault.create(Path::new(".hidden/d.md")).unwrap();

        let notes = vault.list_notes().unwrap();
        assert_eq!(
            notes,
            vec![
                PathBuf::from("a.md"),
                PathBuf::from("sub/b.md"),
                PathBuf::from("sub/c.md"),
            ]
        );
    }

    #[test]
    fn atomic_write_and_read() {
        let (vault, dir) = temp_vault();
        vault.create(Path::new("note.md")).unwrap();
        vault.write(Path::new("note.md"), "# Hello\n").unwrap();
        assert_eq!(vault.read(Path::new("note.md")).unwrap(), "# Hello\n");

        // не остаётся временного файла
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[test]
    fn rel_path() {
        let (vault, dir) = temp_vault();
        let abs = dir.join("x").join("y.md");
        assert_eq!(vault.rel_path(&abs), PathBuf::from("x").join("y.md"));
    }

    #[test]
    fn hidden_root_vault_is_not_empty() {
        // Vault в скрытой папке (`~/.notes`) не должен выглядеть пустым.
        let dir = temp_dir(".mdwriter-hidden-");
        let vault = Vault::open(dir.clone());
        vault.create(Path::new("note.md")).unwrap();
        vault.create(Path::new(".secret/inner.md")).unwrap();

        assert_eq!(vault.list_notes().unwrap(), vec![PathBuf::from("note.md")]);
    }

    #[test]
    fn uppercase_md_extension_is_listed() {
        let (vault, _dir) = temp_vault();
        vault.create(Path::new("A.MD")).unwrap();
        vault.create(Path::new("b.md")).unwrap();

        assert_eq!(
            vault.list_notes().unwrap(),
            vec![PathBuf::from("A.MD"), PathBuf::from("b.md")]
        );
    }

    /// Все файлы `*.tmp` в папке (рекурсивно).
    fn temp_leftovers(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(dir) {
            let entry = entry.unwrap();
            if entry.file_type().is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".tmp")
            {
                out.push(entry.path().to_path_buf());
            }
        }
        out
    }

    #[test]
    fn double_write_keeps_last_content_and_leaves_no_tmp() {
        let (vault, dir) = temp_vault();
        vault.write(Path::new("note.md"), "первая версия").unwrap();
        vault.write(Path::new("note.md"), "вторая версия").unwrap();

        assert_eq!(vault.read(Path::new("note.md")).unwrap(), "вторая версия");
        assert!(temp_leftovers(&dir).is_empty());
    }

    #[test]
    fn create_does_not_overwrite_existing_note() {
        let (vault, _dir) = temp_vault();
        vault.create(Path::new("note.md")).unwrap();
        vault.write(Path::new("note.md"), "важное").unwrap();
        // Повторное создание не должно затирать содержимое.
        vault.create(Path::new("note.md")).unwrap();

        assert_eq!(vault.read(Path::new("note.md")).unwrap(), "важное");
    }
}
