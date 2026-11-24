//! Экспорт открытой заметки в HTML.

use crate::App;

/// Экранирует спецсимволы HTML (включая кавычки — они нужны в атрибутах).
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Относительная ссылка на соседний экспорт: `Note#Раздел` → `Note.html#Раздел`.
/// `[[#Раздел]]` ведёт на якорь этой же страницы.
fn wiki_href(target: &str) -> String {
    let markdown::LinkTarget { path, anchor } = markdown::parse_link_target(target);
    let anchor = anchor.map(|a| format!("#{a}")).unwrap_or_default();
    if path.is_empty() {
        return anchor;
    }
    let stem = path
        .strip_suffix(".md")
        .or_else(|| path.strip_suffix(".MD"))
        .unwrap_or(path);
    format!("{stem}.html{anchor}")
}

/// Собирает автономную HTML-страницу из markdown: frontmatter не выводится,
/// wikilinks становятся ссылками на соседние экспортированные файлы.
pub(crate) fn render_html(title: &str, md: &str) -> String {
    let body = markdown::split_frontmatter(md)
        .map(|(_, body)| body)
        .unwrap_or(md);
    let body = markdown::wikilinks_to_md(body, wiki_href);

    let mut html =
        String::from("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>");
    html.push_str(&escape_html(title));
    html.push_str(
        "</title>\n<style>body{max-width:760px;margin:2em auto;padding:0 1em;font-family:\
         system-ui,sans-serif;line-height:1.6}pre{background:#f5f5f5;padding:1em;\
         overflow:auto}code{background:#f5f5f5;padding:.1em .3em}blockquote{border-left:\
         4px solid #ccc;margin-left:0;padding-left:1em;color:#555}</style>\n</head>\n<body>\n",
    );
    pulldown_cmark::html::push_html(
        &mut html,
        pulldown_cmark::Parser::new_ext(&body, markdown::cmark_options()),
    );
    html.push_str("\n</body>\n</html>\n");
    html
}

impl App {
    /// Экспорт открытой заметки в HTML.
    ///
    /// Берётся текст из буфера редактора, а не с диска: последние правки
    /// (ещё не автосохранённые) тоже попадают в файл.
    pub(crate) fn export_html(&mut self) {
        let (default_name, text) = match &self.vault {
            Some(vs) => match &vs.open {
                Some(rel) => (
                    format!(
                        "{}.html",
                        rel.file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "note".to_string())
                    ),
                    vs.editor_text.clone(),
                ),
                None => return,
            },
            None => return,
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };

        let html = render_html(&default_name, &text);
        match std::fs::write(&path, html) {
            Ok(()) => self
                .notices
                .info(format!("Экспортировано: {}", path.display())),
            Err(e) => self
                .notices
                .error(format!("Не удалось записать {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_href_points_to_neighbour_export() {
        assert_eq!(wiki_href("Note"), "Note.html");
        assert_eq!(wiki_href("Note.md"), "Note.html");
        assert_eq!(wiki_href("sub/Note"), "sub/Note.html");
        assert_eq!(wiki_href("Note#Part"), "Note.html#Part");
        // Только якорь — ссылка внутри той же страницы.
        assert_eq!(wiki_href("#Part"), "#Part");
        // Кириллица в пути не теряется (кодирование делает pulldown-cmark).
        assert_eq!(wiki_href("Заметка#Раздел"), "Заметка.html#Раздел");
    }

    #[test]
    fn render_html_turns_wikilinks_into_relative_links() {
        let html = render_html("Заметка", "См. [[Note]] и [[Другая|алиас]].");
        assert!(html.contains("<a href=\"Note.html\">Note</a>"), "{html}");
        // Кириллица в адресе кодируется процентами — это допустимо в HTML.
        assert!(html.contains(">алиас</a>"), "{html}");
        assert!(
            !html.contains("[["),
            "wikilinks не должны оставаться текстом"
        );
    }

    #[test]
    fn render_html_keeps_anchor_in_wikilink() {
        let html = render_html("Заметка", "[[Note#Part]]");
        assert!(html.contains("href=\"Note.html#Part\""), "{html}");
    }

    #[test]
    fn render_html_skips_frontmatter() {
        let html = render_html("Заметка", "---\ntitle: Секретное\n---\nТело заметки\n");
        assert!(
            !html.contains("Секретное"),
            "frontmatter не выводится: {html}"
        );
        assert!(html.contains("Тело заметки"), "{html}");
    }

    #[test]
    fn render_html_escapes_title_and_quotes() {
        let html = render_html("<b>&\"", "");
        assert!(
            html.contains("<title>&lt;b&gt;&amp;&quot;</title>"),
            "{html}"
        );
    }
}
