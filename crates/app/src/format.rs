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

/// Вставляет горизонтальную линию `\n---\n` после строки, в которой стоит
/// байтовый `end`. Выделенный текст не изменяется.
/// Возвращает новый текст и позицию курсора в байтах.
pub(crate) fn insert_horizontal_rule(text: &str, end: usize) -> (String, usize) {
    let end = end.min(text.len());
    let line_end = text[end..]
        .find('\n')
        .map(|p| end + p)
        .unwrap_or(text.len());
    let mut out = String::with_capacity(text.len() + 5);
    out.push_str(&text[..line_end]);
    out.push_str("\n---\n");
    out.push_str(&text[line_end..]);
    (out, line_end + 5)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(text, "abc\n---\n");
        assert_eq!(cursor, 8);

        let (text, cursor) = insert_horizontal_rule("abc\ndef", 1);
        assert_eq!(text, "abc\n---\n\ndef");
        assert_eq!(cursor, 8);

        // Выделение внутри последней строки — линия всё равно после неё.
        let (text, _) = insert_horizontal_rule("a\nbb\nc", 4);
        assert_eq!(text, "a\nbb\n---\n\nc");
    }
}
