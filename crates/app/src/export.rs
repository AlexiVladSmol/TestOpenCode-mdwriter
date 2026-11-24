//! Экспорт открытой заметки в HTML.

use crate::App;

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl App {
    /// Экспорт открытой заметки в HTML.
    pub(crate) fn export_html(&mut self) {
        let Some(vs) = &self.vault else { return };
        let Some(rel) = &vs.open else { return };
        let text = match vs.vault.read(rel) {
            Ok(text) => text,
            Err(e) => {
                self.notices.error(format!(
                    "Экспорт невозможен: не удалось прочитать {}: {e}",
                    rel.display()
                ));
                return;
            }
        };
        let default_name = format!(
            "{}.html",
            rel.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "note".to_string())
        );
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };

        let options = markdown::cmark_options();

        let mut html =
            String::from("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>");
        html.push_str(&escape_html(&default_name));
        html.push_str(
            "</title>\n<style>body{max-width:760px;margin:2em auto;padding:0 1em;font-family:\
             system-ui,sans-serif;line-height:1.6}pre{background:#f5f5f5;padding:1em;\
             overflow:auto}code{background:#f5f5f5;padding:.1em .3em}blockquote{border-left:\
             4px solid #ccc;margin-left:0;padding-left:1em;color:#555}</style>\n</head>\n<body>\n",
        );
        pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new_ext(&text, options));
        html.push_str("\n</body>\n</html>\n");

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
