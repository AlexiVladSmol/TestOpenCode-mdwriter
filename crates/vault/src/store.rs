//! Работа с файлами vault: обход, чтение, атомарная запись, создание.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

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
        let tmp = temp_path(&abs);
        fs::write(&tmp, content)?;
        fs::rename(&tmp, &abs)?;
        Ok(())
    }

    /// Создать новый (пустой) файл заметки, создавая родительские папки.
    /// Если файл уже существует — ничего не делает.
    pub fn create(&self, rel: &Path) -> Result<()> {
        let abs = self.abs_path(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        if !abs.exists() {
            fs::write(&abs, "")?;
        }
        Ok(())
    }
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
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
}
