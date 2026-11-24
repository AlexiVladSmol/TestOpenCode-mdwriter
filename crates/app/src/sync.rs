//! Согласование буфера редактора с файлом на диске.
//!
//! Заметку могут изменить или удалить другие программы (git, синхронизация,
//! другой редактор). Здесь — чистая логика принятия решения, без egui и
//! файловой системы, чтобы её можно было покрыть тестами.

use std::path::{Path, PathBuf};

/// Что делать с открытой заметкой после сверки с диском.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconcile {
    /// Файл не менялся с последнего чтения/записи.
    Unchanged,
    /// Файл на диске совпадает с буфером (например, ту же правку внесли
    /// извне) — достаточно запомнить его как известное состояние.
    Synced,
    /// Файл изменён извне, несохранённых правок нет — перезагрузить буфер.
    Reload,
    /// Файл изменён извне, а в буфере есть несохранённые правки.
    Conflict,
    /// Файл исчез, несохранённых правок нет — закрыть заметку.
    Close,
    /// Файл исчез, но в буфере есть несохранённые правки — их нельзя терять.
    Orphaned,
}

/// Решает, как поступить с открытой заметкой.
///
/// * `disk` — текущее содержимое файла (`None` — файла нет);
/// * `known` — содержимое на момент последнего чтения или успешной записи;
/// * `buffer` — текст в редакторе;
/// * `dirty` — есть ли несохранённые правки.
pub fn reconcile(disk: Option<&str>, known: Option<&str>, buffer: &str, dirty: bool) -> Reconcile {
    match disk {
        None if dirty => Reconcile::Orphaned,
        None => Reconcile::Close,
        Some(d) if Some(d) == known => Reconcile::Unchanged,
        Some(d) if d == buffer => Reconcile::Synced,
        Some(_) if dirty => Reconcile::Conflict,
        Some(_) => Reconcile::Reload,
    }
}

/// Путь для копии «моей» версии при конфликте: `имя (конфликт).md`,
/// при занятом имени — `имя (конфликт 2).md` и т.д. Копия кладётся рядом
/// с исходной заметкой.
pub fn conflict_copy_path(rel: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let stem = rel
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "note".to_string());
    let dir = rel.parent().unwrap_or(Path::new(""));
    let mut n = 1u32;
    loop {
        let name = if n == 1 {
            format!("{stem} (конфликт).md")
        } else {
            format!("{stem} (конфликт {n}).md")
        };
        let candidate = dir.join(name);
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Пять строк таблицы из FIX_PLAN (A3) + совпадение с буфером.

    #[test]
    fn unchanged_when_disk_matches_known() {
        assert_eq!(
            reconcile(Some("a"), Some("a"), "a", false),
            Reconcile::Unchanged
        );
        assert_eq!(
            reconcile(Some("a"), Some("a"), "a+", true),
            Reconcile::Unchanged
        );
    }

    #[test]
    fn reload_when_changed_and_clean() {
        assert_eq!(
            reconcile(Some("b"), Some("a"), "a", false),
            Reconcile::Reload
        );
    }

    #[test]
    fn conflict_when_changed_and_dirty() {
        assert_eq!(
            reconcile(Some("b"), Some("a"), "a+", true),
            Reconcile::Conflict
        );
    }

    #[test]
    fn close_when_missing_and_clean() {
        assert_eq!(reconcile(None, Some("a"), "a", false), Reconcile::Close);
    }

    #[test]
    fn orphaned_when_missing_and_dirty() {
        assert_eq!(reconcile(None, Some("a"), "a+", true), Reconcile::Orphaned);
    }

    #[test]
    fn synced_when_disk_equals_buffer() {
        // Извне записали ровно то же, что у нас в буфере, — конфликта нет.
        assert_eq!(
            reconcile(Some("a+"), Some("a"), "a+", true),
            Reconcile::Synced
        );
    }

    #[test]
    fn unknown_previous_state_is_treated_as_change() {
        assert_eq!(reconcile(Some("b"), None, "a", false), Reconcile::Reload);
        assert_eq!(reconcile(Some("b"), None, "a", true), Reconcile::Conflict);
    }

    #[test]
    fn conflict_copy_names() {
        let rel = Path::new("sub").join("Заметка.md");
        let first = conflict_copy_path(&rel, |_| false);
        assert_eq!(first, Path::new("sub").join("Заметка (конфликт).md"));

        let taken = first.clone();
        let second = conflict_copy_path(&rel, |p| p == taken);
        assert_eq!(second, Path::new("sub").join("Заметка (конфликт 2).md"));

        let root = conflict_copy_path(Path::new("a.md"), |_| false);
        assert_eq!(root, PathBuf::from("a (конфликт).md"));
    }
}
