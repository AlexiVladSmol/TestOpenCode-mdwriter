//! Индекс vault: метаданные заметок, разрешение wikilinks, backlinks, поиск.
//!
//! Связи между заметками вычисляются один раз при построении индекса
//! ([`Index::build`]) и хранятся в картах, поэтому `resolve_wikilink`,
//! `backlinks` и `links` работают без перебора всех заметок — их можно
//! вызывать хоть в каждом кадре UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use vault::{Result, Vault};

/// Источник номеров поколений: уникален для всех экземпляров `Index`, чтобы
/// кеш, привязанный к поколению, не «узнал» новый индекс после пересборки.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

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

    // --- Производные данные: пересчитываются в `rebuild_derived`. ---
    /// Нормализованный путь (`sub/note.md`: нижний регистр, `/`) → путь заметки.
    by_lower_path: HashMap<String, PathBuf>,
    /// Имя файла без расширения в нижнем регистре → пути (отсортированы).
    by_lower_stem: HashMap<String, Vec<PathBuf>>,
    /// Разрешённые исходящие ссылки: без дублей и ссылок на себя, отсортированы.
    links: HashMap<PathBuf, Vec<PathBuf>>,
    /// Обратные ссылки (кто ссылается на заметку), отсортированы.
    backlinks: HashMap<PathBuf, Vec<PathBuf>>,
    /// Номер поколения: меняется при каждом изменении индекса.
    generation: u64,
    /// Проблемы последнего построения: пропущенные/нечитаемые файлы.
    warnings: Vec<String>,
}

impl Index {
    /// Читает все заметки vault и строит индекс.
    ///
    /// Файлы, которые не удаётся прочитать, и файлы не в UTF-8 не прерывают
    /// построение: они попадают в [`Index::warnings`], а текст читается с
    /// заменами (поиск и ссылки продолжают работать).
    pub fn build(vault: &Vault) -> Result<Index> {
        let mut index = Index::default();
        let (notes, mut warnings) = vault.list_notes()?;
        for rel in notes {
            match vault.read_lossy(&rel) {
                Ok((text, had_invalid_bytes)) => {
                    if had_invalid_bytes {
                        warnings.push(format!(
                            "{}: не UTF-8, текст прочитан с заменами",
                            rel.display()
                        ));
                    }
                    index.add_note(rel, text);
                }
                Err(e) => warnings.push(format!("{}: {e}", rel.display())),
            }
        }
        index.warnings = warnings;
        index.rebuild_derived();
        Ok(index)
    }

    /// Предупреждения последнего [`Index::build`]: пропущенные и битые файлы.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Добавляет (или заменяет) заметку без пересчёта производных данных.
    fn add_note(&mut self, rel: PathBuf, text: String) {
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

        self.notes.insert(
            rel.clone(),
            NoteMeta {
                title,
                tags,
                links_out,
            },
        );
        self.contents.insert(rel, text);
    }

    /// Пересчитывает карты путей, связи и обратные ссылки и меняет поколение.
    /// Стоимость — O(заметок + ссылок).
    fn rebuild_derived(&mut self) {
        let mut paths: Vec<&PathBuf> = self.notes.keys().collect();
        // Сортировка делает выбор детерминированным: при совпадении путей
        // без учёта регистра выигрывает первый по алфавиту.
        paths.sort();

        let mut by_lower_path = HashMap::with_capacity(paths.len());
        let mut by_lower_stem: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for p in &paths {
            by_lower_path
                .entry(normalize_path(&p.to_string_lossy()))
                .or_insert_with(|| (*p).clone());
            if let Some(stem) = p.file_stem() {
                by_lower_stem
                    .entry(stem.to_string_lossy().to_lowercase())
                    .or_default()
                    .push((*p).clone());
            }
        }
        self.by_lower_path = by_lower_path;
        self.by_lower_stem = by_lower_stem;

        // Разрешение ссылок пользуется картами выше.
        let mut links: HashMap<PathBuf, Vec<PathBuf>> = HashMap::with_capacity(self.notes.len());
        let mut backlinks: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for (from, meta) in &self.notes {
            let mut out: Vec<PathBuf> = meta
                .links_out
                .iter()
                .filter_map(|t| self.resolve_wikilink(from, t))
                .filter(|to| to != from)
                .collect();
            out.sort();
            out.dedup();
            for to in &out {
                backlinks.entry(to.clone()).or_default().push(from.clone());
            }
            links.insert(from.clone(), out);
        }
        for v in backlinks.values_mut() {
            v.sort();
        }
        self.links = links;
        self.backlinks = backlinks;
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    /// Номер поколения индекса: другой номер — другое содержимое. Удобно как
    /// ключ кеша (например, результатов поиска).
    pub fn generation(&self) -> u64 {
        self.generation
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

    /// Разрешает `[[target]]`, `[[target#якорь]]` относительно заметки `from`.
    /// Сначала ищет рядом с `from`, затем по всему vault.
    ///
    /// Регистр не важен, `\` и `/` считаются одним разделителем. Якорь
    /// (`#Заголовок`, `#^block`) отбрасывается: он не часть имени файла.
    pub fn resolve_wikilink(&self, from: &Path, raw: &str) -> Option<PathBuf> {
        let markdown::LinkTarget { path, .. } = markdown::parse_link_target(raw);
        // `[[#Заголовок]]` — ссылка на саму заметку.
        if path.is_empty() {
            return Some(from.to_path_buf());
        }

        let with_ext = if path.to_lowercase().ends_with(".md") {
            path.to_string()
        } else {
            format!("{path}.md")
        };

        // 1. Рядом с исходной заметкой.
        let from_dir = from.parent().unwrap_or(Path::new(""));
        let near = from_dir.join(&with_ext);
        if let Some(p) = self
            .by_lower_path
            .get(&normalize_path(&near.to_string_lossy()))
        {
            return Some(p.clone());
        }

        // 2. Точное совпадение относительного пути в любом месте.
        if let Some(p) = self.by_lower_path.get(&normalize_path(&with_ext)) {
            return Some(p.clone());
        }

        // 3. По имени файла (первый в алфавитном порядке).
        let stem = Path::new(&with_ext)
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())?;
        self.by_lower_stem
            .get(&stem)
            .and_then(|v| v.first())
            .cloned()
    }

    /// Обратные ссылки: все заметки, чьи wikilinks ведут на `rel`
    /// (кроме самой `rel`), по алфавиту.
    pub fn backlinks(&self, rel: &Path) -> Vec<PathBuf> {
        self.backlinks.get(rel).cloned().unwrap_or_default()
    }

    /// Разрешённые исходящие ссылки заметки `rel` (без дублей и ссылок на
    /// себя), по алфавиту. Неразрешённые ссылки сюда не попадают.
    pub fn links(&self, rel: &Path) -> &[PathBuf] {
        self.links.get(rel).map(Vec::as_slice).unwrap_or(&[])
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

/// Ключ для сравнения путей без учёта регистра: `\` → `/`, нижний регистр.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
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
    fn build_survives_file_with_invalid_utf8() {
        let (vault, dir) = test_vault();
        vault.write(Path::new("good.md"), "пони живут тут").unwrap();
        // Байты, не являющиеся UTF-8 (например, файл в CP1251).
        std::fs::write(dir.join("bad.md"), [0xC0u8, 0xC1]).unwrap();

        let index = Index::build(&vault).expect("один плохой файл не ломает индекс");
        assert_eq!(index.len(), 2);
        assert!(index.title(Path::new("bad.md")).is_some());
        // Поиск по обычной заметке работает.
        let hits = index.search("пони", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, PathBuf::from("good.md"));
        // О проблемном файле сообщается предупреждением.
        assert_eq!(index.warnings().len(), 1);
        assert!(index.warnings()[0].contains("bad.md"));
    }

    #[test]
    fn resolve_wikilink_ignores_anchor_and_case() {
        let (vault, _dir) = test_vault();
        vault.write(Path::new("Note.md"), "текст").unwrap();
        vault.write(Path::new("sub/deep.md"), "текст").unwrap();

        let index = Index::build(&vault).unwrap();

        // Якорь отбрасывается: ищем файл `Note.md`.
        assert_eq!(
            index.resolve_wikilink(Path::new("sub/deep.md"), "Note#Заголовок"),
            Some(PathBuf::from("Note.md"))
        );
        // Блочный якорь.
        assert_eq!(
            index.resolve_wikilink(Path::new("sub/deep.md"), "note#^block"),
            Some(PathBuf::from("Note.md"))
        );
        // Регистр пути и расширения не важен.
        assert_eq!(
            index.resolve_wikilink(Path::new("sub/deep.md"), "NOTE.MD"),
            Some(PathBuf::from("Note.md"))
        );
        // Только якорь — ссылка на саму заметку.
        assert_eq!(
            index.resolve_wikilink(Path::new("Note.md"), "#Заголовок"),
            Some(PathBuf::from("Note.md"))
        );
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

    // ------------------- C1: предвычисленные связи -------------------

    /// Индекс из текстов в памяти (без диска).
    fn mem_index(notes: &[(String, String)]) -> Index {
        let mut index = Index::default();
        for (p, t) in notes {
            index.add_note(PathBuf::from(p), t.clone());
        }
        index.rebuild_derived();
        index
    }

    fn mem(notes: &[(&str, &str)]) -> Index {
        let owned: Vec<(String, String)> = notes
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        mem_index(&owned)
    }

    /// Прежний алгоритм (до C1) — перебор всех заметок. Эталон для сравнения.
    fn naive_resolve(index: &Index, from: &Path, raw: &str) -> Option<PathBuf> {
        let markdown::LinkTarget { path, .. } = markdown::parse_link_target(raw);
        if path.is_empty() {
            return Some(from.to_path_buf());
        }
        let with_ext = if path.to_lowercase().ends_with(".md") {
            path.to_string()
        } else {
            format!("{path}.md")
        };
        let from_dir = from.parent().unwrap_or(Path::new(""));
        let near_key = normalize_path(&from_dir.join(&with_ext).to_string_lossy());
        if let Some(p) = index
            .notes
            .keys()
            .find(|p| normalize_path(&p.to_string_lossy()) == near_key)
        {
            return Some(p.clone());
        }
        let want = normalize_path(&with_ext);
        if let Some(p) = index
            .notes
            .keys()
            .find(|p| normalize_path(&p.to_string_lossy()) == want)
        {
            return Some(p.clone());
        }
        let stem = Path::new(&with_ext)
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())?;
        let mut m: Vec<PathBuf> = index
            .notes
            .keys()
            .filter(|p| {
                p.file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase() == stem)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        m.sort();
        m.into_iter().next()
    }

    fn naive_backlinks(index: &Index, rel: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = index
            .notes
            .iter()
            .filter(|(from, _)| from.as_path() != rel)
            .filter(|(from, meta)| {
                meta.links_out
                    .iter()
                    .any(|t| naive_resolve(index, from, t).as_deref() == Some(rel))
            })
            .map(|(from, _)| from.clone())
            .collect();
        out.sort();
        out
    }

    /// Детерминированный «vault» из `n` заметок в 10 папках по `per` ссылок:
    /// повторяющиеся имена в разных папках, разный регистр, якоря, `.md`,
    /// пути с папкой, ссылки в никуда и на себя.
    fn generated(n: usize, per: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| {
                let path = format!("d{}/n{}.md", i % 10, i / 3);
                let mut text = format!("# Заметка {i}\n");
                for k in 0..per {
                    let j = (i * 7 + k * 13 + 1) % n;
                    let target = match k % 6 {
                        0 => format!("n{}", j / 3),
                        1 => format!("N{}#Заголовок", j / 3),
                        2 => format!("d{}/n{}", j % 10, j / 3),
                        3 => format!("n{}.MD", j / 3),
                        4 => "#только якорь".to_string(),
                        _ => format!("нет{j}"),
                    };
                    text.push_str(&format!("см. [[{target}]]\n"));
                }
                (path, text)
            })
            .collect()
    }

    #[test]
    fn c1_matches_previous_algorithm() {
        let notes = generated(300, 8);
        let index = mem_index(&notes);
        // Эталонные backlinks собираем за один проход прежним resolve
        // (вызов naive_backlinks для каждой заметки — O(N²·L), слишком долго).
        let mut expected: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut checked = 0;
        for (from, meta) in &index.notes {
            for t in &meta.links_out {
                let naive = naive_resolve(&index, from, t);
                assert_eq!(
                    index.resolve_wikilink(from, t),
                    naive,
                    "resolve {from:?} -> {t}"
                );
                if let Some(to) = naive.filter(|to| to != from) {
                    expected.entry(to).or_default().push(from.clone());
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 300 * 8);
        for v in expected.values_mut() {
            v.sort();
            v.dedup();
        }
        for rel in index.notes.keys() {
            assert_eq!(
                index.backlinks(rel),
                expected.get(rel).cloned().unwrap_or_default(),
                "backlinks {rel:?}"
            );
        }
        // Выборочно — и через прежний backlinks целиком.
        for rel in index.notes.keys().take(5) {
            assert_eq!(index.backlinks(rel), naive_backlinks(&index, rel));
        }
    }

    #[test]
    fn c1_links_are_resolved_deduped_and_without_self() {
        let index = mem(&[
            (
                "a.md",
                "[[b]] [[B]] [[b#h]] [[#себя]] [[a]] [[нет]] [[sub/c]]",
            ),
            ("b.md", "[[a]]"),
            ("sub/c.md", "текст"),
        ]);
        assert_eq!(
            index.links(Path::new("a.md")),
            &[PathBuf::from("b.md"), PathBuf::from("sub/c.md")]
        );
        assert_eq!(
            index.backlinks(Path::new("b.md")),
            vec![PathBuf::from("a.md")]
        );
        assert_eq!(
            index.backlinks(Path::new("a.md")),
            vec![PathBuf::from("b.md")]
        );
        assert!(index.links(Path::new("нет.md")).is_empty());
        assert!(index.backlinks(Path::new("нет.md")).is_empty());
    }

    #[test]
    fn c1_case_only_duplicates_resolve_deterministically() {
        // На регистрозависимых ФС возможны «Up.md» и «up.md» одновременно:
        // выбирается первый по алфавиту, а не случайный из HashMap.
        let index = mem(&[("up.md", ""), ("Up.md", ""), ("x.md", "[[UP]]")]);
        assert_eq!(
            index.resolve_wikilink(Path::new("x.md"), "UP"),
            Some(PathBuf::from("Up.md"))
        );
    }

    #[test]
    fn c1_generation_changes_on_every_rebuild() {
        assert_eq!(Index::default().generation(), 0);
        let a = mem(&[("a.md", "")]);
        let b = mem(&[("a.md", "")]);
        assert_ne!(a.generation(), 0);
        assert_ne!(a.generation(), b.generation());
    }

    /// Замер «до/после» (не входит в обычный прогон):
    /// `cargo test -p index --release -- --ignored --nocapture c1_timing`
    #[test]
    #[ignore]
    fn c1_timing() {
        use std::time::Instant;
        let notes = generated(2000, 10);
        let t = Instant::now();
        let index = mem_index(&notes);
        let build = t.elapsed();

        // Прежний backlinks — секунды на вызов, поэтому выборка маленькая.
        let sample: Vec<PathBuf> = index.notes.keys().take(3).cloned().collect();
        let t = Instant::now();
        for p in &sample {
            std::hint::black_box(naive_backlinks(&index, p));
        }
        let naive = t.elapsed() / sample.len() as u32;
        let t = Instant::now();
        for p in &sample {
            std::hint::black_box(index.backlinks(p));
        }
        let fast = t.elapsed() / sample.len() as u32;
        println!(
            "2000 заметок × 10 ссылок: build {build:?}; backlinks() до {naive:?}, после {fast:?}"
        );
    }

    #[test]
    fn snippet_boundaries() {
        let text = "абвгд".repeat(30); // много кириллицы
        let pos = text.find("д").unwrap();
        let s = snippet(&text, pos, 2);
        assert!(s.contains('д'));
    }
}
