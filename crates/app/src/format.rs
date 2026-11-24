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
}
