//! Индекс vault: метаданные заметок, разрешение wikilinks, backlinks, поиск.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vault::{Result, Vault};

/// Метаданные одной заметки.
#[derive(Debug, Clone, Default)]
pub struct NoteMeta {
    pub title: String,
    pub tags: Vec<String>,
    /// Цели wikilinks (в том виде, как написаны в тексте).
    pub links_out: Vec<String>,
}

/// In-memory индекс всех заметок vault.
#[derive(Debug, Default)]
pub struct Index {
    notes: HashMap<PathBuf, NoteMeta>,
    contents: HashMap<PathBuf, String>,
}

impl Index {
    /// Читает все заметки vault и строит индекс.
    pub fn build(vault: &Vault) -> Result<Index> {
        let mut index = Index::default();
        for rel in vault.list_notes()? {
            let text = vault.read(&rel)?;
            let fm = markdown::split_frontmatter(&text).map(|(fm, _)| fm);

            let title = fm
                .as_ref()
                .and_then(|f| f.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| default_title(&rel));
            let tags = fm.as_ref().map(|f| f.tags.clone()).unwrap_or_default();
            let links_out: Vec<String> = markdown::extract_wikilinks(&text)
                .into_iter()
                .map(|w| w.target)
                .collect();

            index.notes.insert(
                rel.clone(),
                NoteMeta {
                    title,
                    tags,
                    links_out,
                },
            );
            index.contents.insert(rel, text);
        }
        Ok(index)
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn meta(&self, rel: &Path) -> Option<&NoteMeta> {
        self.notes.get(rel)
    }

    pub fn title(&self, rel: &Path) -> Option<&str> {
        self.notes.get(rel).map(|m| m.title.as_str())
    }

    /// Все заметки с заголовками: (относительный путь, заголовок).
    pub fn switcher_entries(&self) -> Vec<(PathBuf, String)> {
        let mut v: Vec<(PathBuf, String)> = self
            .notes
            .iter()
            .map(|(p, m)| (p.clone(), m.title.clone()))
            .collect();
        v.sort_by_key(|a| a.1.to_lowercase());
        v
    }

    /// Разрешает `[[target]]` относительно заметки `from`.
    /// Сначала ищет рядом с `from`, затем по всему vault.
    pub fn resolve_wikilink(&self, from: &Path, target: &str) -> Option<PathBuf> {
        let with_ext = if target.ends_with(".md") {
            target.to_string()
        } else {
            format!("{target}.md")
        };

        // 1. Рядом с исходной заметкой.
        let from_dir = from.parent().unwrap_or(Path::new(""));
        let near = from_dir.join(&with_ext);
        if self.notes.contains_key(&near) {
            return Some(near);
        }

        // 2. Точное совпадение относительного пути в любом месте.
        let as_path = Path::new(&with_ext);
        if let Some(p) = self.notes.keys().find(|p| p.as_path() == as_path) {
            return Some(p.clone());
        }

        // 3. По имени файла (первый в алфавитном порядке).
        let stem = Path::new(&with_ext)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())?;
        let mut matches: Vec<PathBuf> = self
            .notes
            .keys()
            .filter(|p| {
                p.file_stem()
                    .map(|s| s.to_string_lossy() == stem)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        matches.sort();
        matches.into_iter().next()
    }

    /// Обратные ссылки: все заметки, чьи wikilinks ведут на `rel`.
    pub fn backlinks(&self, rel: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for (from, meta) in &self.notes {
            if from == rel {
                continue;
            }
            if meta
                .links_out
                .iter()
                .any(|t| self.resolve_wikilink(from, t).as_deref() == Some(rel))
            {
                out.push(from.clone());
            }
        }
        out.sort();
        out
    }

    /// Полнотекстовый поиск: (путь, сниппет с контекстом).
    pub fn search(&self, query: &str, limit: usize) -> Vec<(PathBuf, String)> {
        let q = query.to_lowercase();
        let mut out = Vec::new();
        for (rel, text) in &self.contents {
            let title_hit = self
                .notes
                .get(rel)
                .map(|m| m.title.to_lowercase().contains(&q))
                .unwrap_or(false);
            if let Some(pos) = text
                .to_lowercase()
                .find(&q)
                .or_else(|| title_hit.then_some(0))
            {
                out.push((rel.clone(), snippet(text, pos, q.len())));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.truncate(limit);
        out
    }
}

fn default_title(rel: &Path) -> String {
    rel.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rel.display().to_string())
}

/// Сниппет вокруг совпадения (с корректными границами UTF-8).
fn snippet(text: &str, byte_pos: usize, match_len: usize) -> String {
    let start = text.floor_char_boundary(byte_pos.saturating_sub(40));
    let end = text.ceil_char_boundary((byte_pos + match_len + 60).min(text.len()));
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.push_str(&text[start..end]);
    if end < text.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_vault() -> (Vault, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mdwriter-index-test-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        (Vault::open(dir.clone()), dir)
    }

    #[test]
    fn build_and_backlinks() {
        let (vault, _dir) = test_vault();
        vault
            .write(Path::new("a.md"), "Ссылка на [[b]] и [[c]]")
            .unwrap();
        vault.write(Path::new("b.md"), "Ссылка на [[c]]").unwrap();
        vault
            .write(Path::new("sub/c.md"), "---\ntitle: Си\n---\nОдинокая")
            .unwrap();

        let index = Index::build(&vault).unwrap();
        assert_eq!(index.len(), 3);

        let c = Path::new("sub/c.md");
        let mut backlinks = index.backlinks(c);
        assert_eq!(backlinks.len(), 2);
        backlinks.sort();
        assert_eq!(
            backlinks,
            vec![PathBuf::from("a.md"), PathBuf::from("b.md")]
        );
        assert_eq!(index.title(c), Some("Си"));
        assert!(index.backlinks(Path::new("a.md")).is_empty());
    }

    #[test]
    fn resolve_wikilink() {
        let (vault, _dir) = test_vault();
        vault.write(Path::new("root.md"), "[[sub/deep]]").unwrap();
        vault.write(Path::new("sub/deep.md"), "[[root]]").unwrap();
        vault
            .write(Path::new("sub/other.md"), "[[root]] и [[deep]]")
            .unwrap();

        let index = Index::build(&vault).unwrap();

        // Рядом с исходной заметкой.
        assert_eq!(
            index.resolve_wikilink(Path::new("sub/other.md"), "deep"),
            Some(PathBuf::from("sub/deep.md"))
        );
        // Из корня по точному пути.
        assert_eq!(
            index.resolve_wikilink(Path::new("root.md"), "sub/deep"),
            Some(PathBuf::from("sub/deep.md"))
        );
        // По имени файла (неоднозначно → первый).
        assert_eq!(
            index.resolve_wikilink(Path::new("sub/other.md"), "root"),
            Some(PathBuf::from("root.md"))
        );
        // С расширением.
        assert_eq!(
            index.resolve_wikilink(Path::new("root.md"), "deep.md"),
            Some(PathBuf::from("sub/deep.md"))
        );
        assert!(index
            .resolve_wikilink(Path::new("root.md"), "нет")
            .is_none());
    }

    #[test]
    fn search_finds_and_snippets() {
        let (vault, _dir) = test_vault();
        vault
            .write(Path::new("one.md"), "Просто текст про пони")
            .unwrap();
        vault.write(Path::new("two.md"), "Совсем другое").unwrap();

        let index = Index::build(&vault).unwrap();
        let hits = index.search("пони", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, PathBuf::from("one.md"));
        assert!(hits[0].1.contains("пони"));
        assert!(index.search("несуществующее", 10).is_empty());
    }

    #[test]
    fn snippet_boundaries() {
        let text = "абвгд".repeat(30); // много кириллицы
        let pos = text.find("д").unwrap();
        let s = snippet(&text, pos, 2);
        assert!(s.contains('д'));
    }
}
