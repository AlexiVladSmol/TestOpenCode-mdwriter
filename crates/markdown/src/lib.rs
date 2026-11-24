//! Парсинг markdown-специфики Obsidian: frontmatter, wikilinks, теги.
//!
//! Никаких зависимостей: только `std`.

use std::ops::Range;

/// YAML-шапка заметки (`--- ... ---` в начале файла).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// Значение `title`, если задано.
    pub title: Option<String>,
    /// Теги из `tags: a, b, c`.
    pub tags: Vec<String>,
    /// Все пары ключ-значение (в порядке следования).
    pub pairs: Vec<(String, String)>,
}

/// Ссылка в стиле Obsidian: `[[target]]`, `[[target|alias]]`, `![[target]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wikilink {
    /// Цель ссылки (обычно без расширения `.md`).
    pub target: String,
    /// Текст для отображения (`[[target|alias]]`).
    pub alias: Option<String>,
    /// Это встраивание `![[...]]`?
    pub embed: bool,
    /// Байтовый диапазон в исходном тексте.
    pub range: Range<usize>,
}

/// Цель wikilink, разложенная на путь и якорь: `note#Заголовок` → (`note`, `Заголовок`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkTarget<'a> {
    /// Путь заметки без якоря (может быть пустым для `[[#Заголовок]]`).
    pub path: &'a str,
    /// Якорь после первого `#` (заголовок или блок `^id`).
    pub anchor: Option<&'a str>,
}

/// Делит цель wikilink по первому `#`.
pub fn parse_link_target(raw: &str) -> LinkTarget<'_> {
    match raw.split_once('#') {
        Some((path, anchor)) => LinkTarget {
            path: path.trim(),
            anchor: Some(anchor.trim()),
        },
        None => LinkTarget {
            path: raw.trim(),
            anchor: None,
        },
    }
}

/// Отделяет frontmatter от тела документа.
/// Возвращает `None`, если документ не начинается с закрытой YAML-шапки.
///
/// Поддерживается: UTF-8 BOM в начале файла, парные кавычки вокруг значений,
/// теги в виде `tags: a, b`, `tags: [a, b]`, многострочного списка
/// (`tags:` + строки `  - a`), а также одиночного ключа `tag`.
pub fn split_frontmatter(text: &str) -> Option<(Frontmatter, &str)> {
    // BOM от редакторов Windows не должен мешать распознать шапку.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut iter = text.split_inclusive('\n');
    let first = iter.next()?;
    if first.trim() != "---" {
        return None;
    }

    let mut fm = Frontmatter::default();
    let mut consumed = first.len();
    let mut closed = false;
    // Последний ключ верхнего уровня — к нему относятся строки-продолжения списка.
    let mut last_key: Option<String> = None;
    for line in iter {
        if line.trim() == "---" {
            consumed += line.len();
            closed = true;
            break;
        }
        consumed += line.len();
        let line = line.trim_end_matches(['\n', '\r']);

        // Строка-продолжение YAML-списка: `  - значение`.
        if let Some(item) = list_item(line) {
            let item = unquote(item);
            if !item.is_empty() {
                if let Some(key) = last_key.as_deref() {
                    if is_tags_key(key) {
                        push_tag(&mut fm.tags, &item);
                    }
                }
            }
            continue;
        }

        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            if k.is_empty() {
                continue;
            }
            let value = unquote(v);
            if k == "title" {
                fm.title = Some(value.clone());
            }
            if is_tags_key(k) {
                for tag in parse_tag_value(v) {
                    push_tag(&mut fm.tags, &tag);
                }
            }
            fm.pairs.push((k.to_string(), value));
            last_key = Some(k.to_string());
        }
    }

    if closed {
        Some((fm, &text[consumed..]))
    } else {
        None
    }
}

/// Снимает парные кавычки (`"…"` / `'…'`) вокруг значения.
fn unquote(value: &str) -> String {
    let v = value.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// `tag` и `tags` — оба задают теги заметки.
fn is_tags_key(key: &str) -> bool {
    key == "tags" || key == "tag"
}

/// Строка-продолжение списка: отступ + `- значение`.
fn list_item(line: &str) -> Option<&str> {
    let indented = line.starts_with(' ') || line.starts_with('\t');
    if !indented {
        return None;
    }
    let rest = line.trim_start().strip_prefix('-')?;
    Some(rest.trim())
}

/// Теги из значения: `a, b`, `[a, b]`, `#a` (с необязательным `#`).
fn parse_tag_value(value: &str) -> Vec<String> {
    let v = value.trim();
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(v);
    inner
        .split(',')
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Добавляет тег, отбрасывая `#` и дубликаты (порядок — первое появление).
fn push_tag(tags: &mut Vec<String>, raw: &str) {
    let tag = raw.trim().trim_start_matches('#');
    if tag.is_empty() {
        return;
    }
    if !tags.iter().any(|t| t == tag) {
        tags.push(tag.to_string());
    }
}

/// Находит все wikilinks вне кода (огороженные блоки и инлайн-код пропускаются).
pub fn extract_wikilinks(text: &str) -> Vec<Wikilink> {
    let mut out = Vec::new();
    for_each_text_run(text, |run, offset| {
        scan_wikilinks(run, offset, &mut out);
    });
    out
}

/// Находит все хэштеги `#tag` вне кода.
pub fn extract_tags(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for_each_text_run(text, |run, _offset| {
        let mut i = 0usize;
        while i < run.len() {
            let rest = &run[i..];
            if let Some(escaped) = rest.strip_prefix('\\') {
                let mut skip = 1;
                if let Some(c) = escaped.chars().next() {
                    skip += c.len_utf8();
                }
                i += skip;
                continue;
            }
            if rest.starts_with('#') {
                let prev_is_word = run[..i]
                    .chars()
                    .next_back()
                    .map(|c| c.is_alphanumeric())
                    .unwrap_or(false);
                let next_is_word = run[i + 1..]
                    .chars()
                    .next()
                    .map(|c| c.is_alphanumeric())
                    .unwrap_or(false);
                if !prev_is_word && next_is_word {
                    let mut j = i + 1;
                    while j < run.len() {
                        let c = run[j..].chars().next().unwrap();
                        if c.is_alphanumeric() || c == '-' || c == '_' || c == '/' {
                            j += c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    let tag = &run[i + 1..j];
                    if !tag.is_empty() {
                        out.push(tag.to_string());
                    }
                    i = j;
                    continue;
                }
            }
            i += run[i..].chars().next().unwrap().len_utf8();
        }
    });
    out
}

// ---------------------------------------------------------------------------
// Внутренние
// ---------------------------------------------------------------------------

/// Вызывает `f(run, offset)` для каждого непрерывного куска текста
/// **вне** огороженных блоков (```...```) и инлайн-кода (`...`).
/// `offset` — байтовое смещение куска от начала документа.
fn for_each_text_run(text: &str, mut f: impl FnMut(&str, usize)) {
    let mut offset = 0usize;
    let mut in_fence = false;

    for raw_line in text.split_inclusive('\n') {
        let line_len = raw_line.len();
        let line = raw_line.trim_end_matches(['\n', '\r']);
        let line_offset = offset;

        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            // Упрощение: любая строка-«ограда» переключает состояние.
            in_fence = !in_fence;
        } else if !in_fence {
            // Сегменты строки вне инлайн-кода.
            let mut run_start = 0usize;
            let mut in_code = false;
            let mut i = 0usize;
            while i < line.len() {
                let c = line[i..].chars().next().unwrap();
                if c == '\\' {
                    i += c.len_utf8();
                    if i < line.len() {
                        i += line[i..].chars().next().unwrap().len_utf8();
                    }
                    continue;
                }
                if c == '`' {
                    if !in_code && run_start < i {
                        f(&line[run_start..i], line_offset + run_start);
                    }
                    let mut j = i;
                    while j < line.len() && line.as_bytes()[j] == b'`' {
                        j += 1;
                    }
                    in_code = !in_code;
                    run_start = j;
                    i = j;
                    continue;
                }
                i += c.len_utf8();
            }
            if !in_code && run_start < line.len() {
                f(&line[run_start..], line_offset + run_start);
            }
        }
        offset += line_len;
    }
}

fn scan_wikilinks(run: &str, offset: usize, out: &mut Vec<Wikilink>) {
    let mut i = 0usize;
    while i < run.len() {
        let rest = &run[i..];
        if let Some(escaped) = rest.strip_prefix('\\') {
            // Экранированный символ: пропускаем бэкслеш и следующий символ.
            let mut skip = 1;
            if let Some(c) = escaped.chars().next() {
                skip += c.len_utf8();
            }
            i += skip;
            continue;
        }
        let (prefix, inner_start) = if rest.starts_with("![[") {
            (true, 3)
        } else if rest.starts_with("[[") {
            (false, 2)
        } else {
            i += run[i..].chars().next().unwrap().len_utf8();
            continue;
        };

        if let Some(close) = rest.find("]]") {
            let inner = &rest[inner_start..close];
            let (target, alias) = match inner.split_once('|') {
                Some((t, a)) => (t.trim().to_string(), Some(a.trim().to_string())),
                None => (inner.trim().to_string(), None),
            };
            if !target.is_empty() {
                let range = offset + i..offset + i + close + 2;
                out.push(Wikilink {
                    target,
                    alias,
                    embed: prefix,
                    range,
                });
            }
            i += close + 2;
        } else {
            // Незакрытая скобка — пропускаем.
            i += 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_basic() {
        let text = "---\ntitle: Заметка\ntags: a, #b, c\n---\nТело\n";
        let (fm, body) = split_frontmatter(text).unwrap();
        assert_eq!(fm.title.as_deref(), Some("Заметка"));
        assert_eq!(fm.tags, vec!["a", "b", "c"]);
        assert_eq!(fm.pairs.len(), 2);
        assert_eq!(body, "Тело\n");
    }

    #[test]
    fn frontmatter_crlf() {
        let text = "---\r\ntitle: X\r\n---\r\nBody\r\n";
        let (fm, body) = split_frontmatter(text).unwrap();
        assert_eq!(fm.title.as_deref(), Some("X"));
        assert_eq!(body, "Body\r\n");
    }

    #[test]
    fn frontmatter_absent_or_unclosed() {
        assert!(split_frontmatter("# Просто текст\n").is_none());
        assert!(split_frontmatter("---\ntitle: X\nнезакрыто\n").is_none());
    }

    #[test]
    fn frontmatter_bom_is_skipped() {
        let text = "\u{FEFF}---\ntitle: X\n---\nBody\n";
        let (fm, body) = split_frontmatter(text).unwrap();
        assert_eq!(fm.title.as_deref(), Some("X"));
        assert_eq!(body, "Body\n");
    }

    #[test]
    fn frontmatter_strips_paired_quotes() {
        let text = "---\ntitle: \"Заметка\"\nalias: 'одинарные'\nplain: без кавычек\n---\n";
        let (fm, _) = split_frontmatter(text).unwrap();
        assert_eq!(fm.title.as_deref(), Some("Заметка"));
        assert_eq!(fm.pairs[1], ("alias".to_string(), "одинарные".to_string()));
        assert_eq!(
            fm.pairs[2],
            ("plain".to_string(), "без кавычек".to_string())
        );
    }

    #[test]
    fn frontmatter_tags_square_brackets() {
        let text = "---\ntags: [a, \"b c\", #d]\n---\n";
        let (fm, _) = split_frontmatter(text).unwrap();
        assert_eq!(fm.tags, vec!["a", "b c", "d"]);
    }

    #[test]
    fn frontmatter_tags_multiline_list() {
        let text = "---\ntitle: T\ntags:\n  - a\n  - \"b c\"\n---\nBody";
        let (fm, body) = split_frontmatter(text).unwrap();
        assert_eq!(fm.tags, vec!["a", "b c"]);
        assert_eq!(body, "Body");
    }

    #[test]
    fn frontmatter_tag_singular_key() {
        let text = "---\ntag: одиночный\n---\n";
        let (fm, _) = split_frontmatter(text).unwrap();
        assert_eq!(fm.tags, vec!["одиночный"]);
    }

    #[test]
    fn wikilinks_basic() {
        let text = "См. [[Заметка]] и [[Другая|алиас]].";
        let links = extract_wikilinks(text);

        let r1 = text.find("[[").unwrap();
        let r2 = text[r1 + 2..].find("[[").unwrap() + r1 + 2;
        let end1 = r1 + text[r1..].find("]]").unwrap() + 2;
        let end2 = r2 + text[r2..].find("]]").unwrap() + 2;

        assert_eq!(
            links,
            vec![
                Wikilink {
                    target: "Заметка".into(),
                    alias: None,
                    embed: false,
                    range: r1..end1,
                },
                Wikilink {
                    target: "Другая".into(),
                    alias: Some("алиас".into()),
                    embed: false,
                    range: r2..end2,
                },
            ]
        );
    }

    #[test]
    fn wikilinks_embed() {
        let links = extract_wikilinks("Вставить: ![[Картинка]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Картинка");
        assert!(links[0].embed);
    }

    #[test]
    fn wikilinks_skip_code() {
        let text =
            "[[вне]]\n\n```\n[[внутри огряды]]\n```\n\n`[[в инлайн-коде]]` и [[снова вне]]\n";
        let links = extract_wikilinks(text);
        let targets: Vec<_> = links.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(targets, vec!["вне", "снова вне"]);
    }

    #[test]
    fn wikilinks_skip_escaped() {
        let links = extract_wikilinks("Экранировано: \\[[не ссылка]]");
        assert!(links.is_empty());
    }

    #[test]
    fn tags_basic() {
        let tags = extract_tags("Текст #тег и #второй-тег, а это#нет");
        assert_eq!(tags, vec!["тег", "второй-тег"]);
    }

    #[test]
    fn tags_skip_code() {
        let text = "#снаружи\n\n```\n#внутри\n```\n\n`#винлайн`\n";
        let tags = extract_tags(text);
        assert_eq!(tags, vec!["снаружи"]);
    }

    #[test]
    fn parse_link_target_with_anchor() {
        assert_eq!(
            parse_link_target("note#Заголовок"),
            LinkTarget {
                path: "note",
                anchor: Some("Заголовок"),
            }
        );
        assert_eq!(
            parse_link_target("note#^block"),
            LinkTarget {
                path: "note",
                anchor: Some("^block"),
            }
        );
    }

    #[test]
    fn parse_link_target_anchor_only() {
        assert_eq!(
            parse_link_target("#Заголовок"),
            LinkTarget {
                path: "",
                anchor: Some("Заголовок"),
            }
        );
    }

    #[test]
    fn parse_link_target_without_anchor() {
        assert_eq!(
            parse_link_target("note"),
            LinkTarget {
                path: "note",
                anchor: None,
            }
        );
    }
}
