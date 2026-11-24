//! Построчные преобразования для панели форматирования.

/// Диапазон строк в `text`, покрывающих байтовый диапазон [start, end).
/// Безопасен для многобайтовых символов (UTF-8).
pub(crate) fn lines_range(text: &str, start: usize, end: usize) -> std::ops::Range<usize> {
    let ls = text[..start].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let le = if end > start {
        // Конец строки, содержащей последний символ выделения.
        // `end` указывает на байт ПОСЛЕ последнего символа — берём начало
        // последнего символа через char_indices, а не `end - 1` (который
        // может попасть внутрь многобайтового символа).
        let last_char = text[..end]
            .char_indices()
            .next_back()
            .map(|(b, _)| b)
            .unwrap_or(start);
        text[last_char..]
            .find('\n')
            .map(|p| last_char + p)
            .unwrap_or(text.len())
    } else {
        text[end..]
            .find('\n')
            .map(|p| end + p)
            .unwrap_or(text.len())
    };
    ls..le
}

/// Заголовок уровня `level` (переключается: повторное применение снимает).
pub(crate) fn fmt_heading(line: &str, level: usize) -> String {
    let body = line.trim_start_matches('#').trim_start_matches(' ');
    let prefix = format!("{} ", "#".repeat(level));
    if body.is_empty() {
        "#".repeat(level)
    } else if line.starts_with(&prefix) {
        body.to_string()
    } else {
        format!("{prefix}{body}")
    }
}

pub(crate) fn fmt_list(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("- ") {
        rest.to_string()
    } else {
        format!("- {line}")
    }
}

pub(crate) fn fmt_task(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("- [ ] ") {
        format!("- [x] {rest}")
    } else if let Some(rest) = line.strip_prefix("- [x] ") {
        format!("- [ ] {rest}")
    } else {
        format!("- [ ] {line}")
    }
}

pub(crate) fn fmt_quote(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("> ") {
        rest.to_string()
    } else {
        format!("> {line}")
    }
}

/// Переключает маркер задачи, начинающийся с байта `offset`:
/// `[ ]` → `[x]`, `[x]`/`[X]` → `[ ]`. Возвращает `false` и ничего не
/// меняет, если по смещению нет маркера (например, текст изменился после
/// отрисовки предпросмотра) — лучше пропустить клик, чем испортить текст.
pub(crate) fn toggle_task_at(text: &mut String, offset: usize) -> bool {
    let Some(marker) = text.get(offset..offset + 3) else {
        return false; // вне текста или не на границе символа
    };
    let replacement = match marker {
        "[ ]" => "[x]",
        "[x]" | "[X]" => "[ ]",
        _ => return false,
    };
    text.replace_range(offset..offset + 3, replacement);
    true
}

/// Оборачивает выделение маркерами (`before`/`after`).
///
/// Возвращает новый текст и позицию курсора в символах от начала замены:
/// - выделение уже обёрнуто → маркеры снимаются, курсор в конце текста;
/// - выделение пустое → курсор ставится **между** маркерами (`**|**`);
/// - иначе → курсор в конце обёрнутого текста.
pub(crate) fn wrap_selection_text(sel: &str, before: &str, after: &str) -> (String, usize) {
    if !sel.is_empty() && sel.starts_with(before) && sel.ends_with(after) {
        let inner = &sel[before.len()..sel.len() - after.len()];
        (inner.to_string(), inner.chars().count())
    } else if sel.is_empty() {
        let text = format!("{before}{after}");
        (text, before.chars().count())
    } else {
        let text = format!("{before}{sel}{after}");
        let cursor = text.chars().count();
        (text, cursor)
    }
}

/// Что вставляет кнопка «—». Пустая строка перед `---` обязательна: строка
/// `---` прямо под текстом по CommonMark делает этот текст заголовком H2
/// (setext), а не рисует линию.
const HORIZONTAL_RULE: &str = "\n\n---\n";

/// Вставляет горизонтальную линию после строки, в которой стоит байтовый
/// `end`, отделяя её пустой строкой. Выделенный текст не изменяется.
/// Возвращает новый текст и позицию курсора в байтах (сразу после линии).
pub(crate) fn insert_horizontal_rule(text: &str, end: usize) -> (String, usize) {
    let end = end.min(text.len());
    let line_end = text[end..]
        .find('\n')
        .map(|p| end + p)
        .unwrap_or(text.len());
    let mut out = String::with_capacity(text.len() + HORIZONTAL_RULE.len());
    out.push_str(&text[..line_end]);
    out.push_str(HORIZONTAL_RULE);
    out.push_str(&text[line_end..]);
    (out, line_end + HORIZONTAL_RULE.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b2_toggle_task_at_marker() {
        let mut t = String::from("- [ ] a\n- [X] b\n");
        assert!(toggle_task_at(&mut t, 2));
        assert_eq!(t, "- [x] a\n- [X] b\n");
        assert!(toggle_task_at(&mut t, 10));
        assert_eq!(t, "- [x] a\n- [ ] b\n");
        assert!(toggle_task_at(&mut t, 2));
        assert_eq!(t, "- [ ] a\n- [ ] b\n");
    }

    #[test]
    fn b2_toggle_task_at_rejects_wrong_offset() {
        let mut t = String::from("- [ ] задача");
        let before = t.clone();
        assert!(!toggle_task_at(&mut t, 0)); // не маркер
        assert!(!toggle_task_at(&mut t, 7)); // внутри многобайтового символа
        assert!(!toggle_task_at(&mut t, 100)); // за концом текста
        assert_eq!(t, before);
    }

    #[test]
    fn lines_range_does_not_panic_on_utf8() {
        let text = "первая строка\nвторая\nтретья\n";
        // Выделение ровно одного многобайтового символа «в» (2 байта) в начале
        // строки «вторая». Раньше `end - 1` попадал внутрь символа и паниковал.
        let start = text.find("вторая").unwrap();
        let end = start + 2; // после «в»
        let r = lines_range(text, start, end);
        // Трейлинговая \n последней строки в диапазон не входит.
        assert_eq!(&text[r], "вторая");
    }

    #[test]
    fn lines_range_multiline_selection() {
        let text = "a\nb\nc\n";
        let start = text.find('a').unwrap();
        let end = text.find("b\n").unwrap() + 2; // конец строки «b»
        let r = lines_range(text, start, end);
        assert_eq!(&text[r], "a\nb");
    }

    #[test]
    fn lines_range_empty_selection() {
        let text = "a\nb\nc\n";
        let pos = text.find('b').unwrap();
        let r = lines_range(text, pos, pos);
        assert_eq!(&text[r], "b");
    }

    // --------- B11: обёртка выделения и горизонтальная линия ---------

    #[test]
    fn wrap_selection_text_puts_cursor_between_markers_when_empty() {
        let (text, cursor) = wrap_selection_text("", "**", "**");
        assert_eq!(text, "****");
        assert_eq!(cursor, 2, "курсор должен стоять между маркерами");
    }

    #[test]
    fn wrap_selection_text_wraps_and_unwraps() {
        let (text, cursor) = wrap_selection_text("слово", "**", "**");
        assert_eq!(text, "**слово**");
        assert_eq!(cursor, text.chars().count());

        let (text, cursor) = wrap_selection_text("**слово**", "**", "**");
        assert_eq!(text, "слово");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn insert_horizontal_rule_after_the_last_line() {
        // Линия ставится после строки, в которой стоит курсор; текст не трогаем.
        let (text, cursor) = insert_horizontal_rule("abc", 1);
        assert_eq!(text, "abc\n\n---\n");
        assert_eq!(cursor, 9);

        let (text, cursor) = insert_horizontal_rule("abc\ndef", 1);
        assert_eq!(text, "abc\n\n---\n\ndef");
        assert_eq!(cursor, 9);

        // Выделение внутри последней строки — линия всё равно после неё.
        let (text, _) = insert_horizontal_rule("a\nbb\nc", 4);
        assert_eq!(text, "a\nbb\n\n---\n\nc");
    }

    /// Что из вставленного видит парсер: линию или заголовок.
    fn block_kinds(md: &str) -> Vec<&'static str> {
        use pulldown_cmark::{Event, Parser, Tag};
        Parser::new_ext(md, markdown::cmark_options())
            .filter_map(|e| match e {
                Event::Rule => Some("rule"),
                Event::Start(Tag::Heading { .. }) => Some("heading"),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn inserted_rule_is_a_rule_not_a_setext_heading() {
        // Регрессия: `abc\n---` — это заголовок H2 «abc», а не линия.
        assert_eq!(block_kinds("abc\n---\n"), vec!["heading"]);
        for (text, at) in [("abc", 3), ("abc\ndef", 1), ("a\nbb\nc", 4), ("", 0)] {
            let (out, _) = insert_horizontal_rule(text, at);
            assert_eq!(block_kinds(&out), vec!["rule"], "{out:?}");
        }
    }
}
