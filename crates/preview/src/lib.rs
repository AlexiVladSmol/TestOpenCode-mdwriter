//! Живой предпросмотр Markdown: рендер pulldown-cmark AST в egui.
//!
//! Поддерживается: заголовки, абзацы, emphasis/strong/strikethrough,
//! inline-код, код-блоки (с подсветкой syntect), цитаты, списки (в т.ч.
//! упорядоченные и задачи), таблицы, горизонтальные линии, сноски,
//! frontmatter (YAML-шапка).

use std::iter::Peekable;

use egui::text::{LayoutJob, TextFormat};
use egui::{FontId, Margin, RichText, Stroke, TextStyle, Ui};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Рисует markdown-текст в `ui`.
pub fn markdown(ui: &mut Ui, text: &str) {
    let (frontmatter, body) = split_frontmatter(text);
    if let Some(fm) = frontmatter {
        render_frontmatter(ui, &fm);
    }
    render_body(ui, body);
}

// ---------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------

/// Отделяет YAML-шапку `--- ... ---` в начале документа.
fn split_frontmatter(text: &str) -> (Option<Vec<(String, String)>>, &str) {
    let mut iter = text.split_inclusive('\n');
    let Some(first) = iter.next() else {
        return (None, text);
    };
    if first.trim() != "---" {
        return (None, text);
    }

    let mut kv = Vec::new();
    let mut consumed = first.len();
    let mut closed = false;
    for line in iter {
        if line.trim() == "---" {
            consumed += line.len();
            closed = true;
            break;
        }
        consumed += line.len();
        let line = line.trim_end_matches(['\n', '\r']);
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() {
                kv.push((k.to_string(), v.to_string()));
            }
        }
    }
    if closed {
        (Some(kv), &text[consumed..])
    } else {
        (None, text)
    }
}

fn render_frontmatter(ui: &mut Ui, fm: &[(String, String)]) {
    egui::Frame::group(ui.style())
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            egui::CollapsingHeader::new("Metadata")
                .default_open(false)
                .show(ui, |ui| {
                    for (k, v) in fm {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(k).color(ui.visuals().weak_text_color()),
                            );
                            ui.label(v);
                        });
                    }
                });
        });
}

// ---------------------------------------------------------------------------
// Блоки
// ---------------------------------------------------------------------------

fn render_body(ui: &mut Ui, md: &str) {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(md, options);
    render_blocks(ui, &mut parser.peekable());
}

fn render_blocks(ui: &mut Ui, events: &mut Peekable<Parser<'_>>) {
    while let Some(ev) = events.peek().cloned() {
        match ev {
            Event::Start(Tag::Paragraph) => {
                events.next();
                render_inline(ui, events, InlineStop::Paragraph);
            }
            Event::Start(Tag::Heading { level, .. }) => {
                events.next();
                render_heading(ui, events, level);
            }
            Event::Start(Tag::List(start)) => {
                events.next();
                render_list(ui, events, start);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                events.next();
                render_blockquote(ui, events);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                events.next();
                render_codeblock(ui, events, &kind);
            }
            Event::Start(Tag::Table(_)) => {
                events.next();
                render_table(ui, events);
            }
            Event::Start(Tag::FootnoteDefinition(_)) => {
                events.next();
                ui.indent("footnote", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        render_inline_with(ui, events, InlineStop::FootnoteDef, small_format(ui));
                    });
                });
            }
            Event::Rule => {
                events.next();
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
            }
            Event::End(_) | Event::Start(_) => {
                events.next();
            }
            _ => {
                // «Рыхлый» инлайн без абзаца — на всякий случай.
                render_inline(ui, events, InlineStop::None);
            }
        }
    }
}

/// Рендер одного блока (для вложенного контента: внутри пунктов списков и цитат).
fn render_block_once(ui: &mut Ui, events: &mut Peekable<Parser<'_>>) {
    match events.peek().cloned() {
        Some(Event::Start(Tag::Paragraph)) => {
            events.next();
            render_inline(ui, events, InlineStop::Paragraph);
        }
        Some(Event::Start(Tag::Heading { level, .. })) => {
            events.next();
            render_heading(ui, events, level);
        }
        Some(Event::Start(Tag::List(start))) => {
            events.next();
            render_list(ui, events, start);
        }
        Some(Event::Start(Tag::BlockQuote(_))) => {
            events.next();
            render_blockquote(ui, events);
        }
        Some(Event::Start(Tag::CodeBlock(kind))) => {
            events.next();
            render_codeblock(ui, events, &kind);
        }
        Some(Event::Start(Tag::Table(_))) => {
            events.next();
            render_table(ui, events);
        }
        _ => {}
    }
}

fn render_heading(ui: &mut Ui, events: &mut Peekable<Parser<'_>>, level: HeadingLevel) {
    let base = body_size(ui);
    let size = match level {
        HeadingLevel::H1 => base * 1.9,
        HeadingLevel::H2 => base * 1.5,
        HeadingLevel::H3 => base * 1.25,
        HeadingLevel::H4 => base * 1.1,
        HeadingLevel::H5 => base * 1.0,
        HeadingLevel::H6 => base * 0.9,
    };
    ui.add_space(size * 0.4);
    let fmt = TextFormat {
        font_id: FontId::proportional(size),
        color: ui.visuals().strong_text_color(),
        ..Default::default()
    };
    render_inline_with(ui, events, InlineStop::Heading(level), fmt);
    ui.add_space(size * 0.15);
}

fn render_list(ui: &mut Ui, events: &mut Peekable<Parser<'_>>, start: Option<u64>) {
    let ordered = start.is_some();
    let mut n = start.unwrap_or(1);
    loop {
        match events.peek() {
            Some(Event::Start(Tag::Item)) => {
                events.next();
                render_item(ui, events, ordered, n);
                n += 1;
            }
            Some(Event::End(TagEnd::List(_))) => {
                events.next();
                break;
            }
            Some(_) => {
                events.next();
            }
            None => break,
        }
    }
}

fn render_item(ui: &mut Ui, events: &mut Peekable<Parser<'_>>, ordered: bool, index: u64) {
    // Задачи: `- [ ] ...` / `- [x] ...`. Маркер может идти сразу или после
    // Start(Paragraph) («рыхлые» списки).
    if matches!(events.peek(), Some(Event::Start(Tag::Paragraph))) {
        events.next();
    }
    let task = if let Some(Event::TaskListMarker(b)) = events.peek() {
        let checked = *b;
        events.next();
        Some(checked)
    } else {
        None
    };

    ui.horizontal_top(|ui| {
        match task {
            Some(checked) => {
                ui.add_space(4.0);
                let mut c = checked;
                ui.checkbox(&mut c, "");
            }
            None => {
                let marker = if ordered {
                    format!("{index}.")
                } else {
                    "•".to_string()
                };
                ui.label(RichText::new(marker).monospace());
            }
        }
        render_inline(ui, events, InlineStop::Item);
    });

    // Вложенные блоки (вложенные списки, код, цитаты…)
    loop {
        match events.peek().cloned() {
            Some(Event::End(TagEnd::Item)) => {
                events.next();
                break;
            }
            Some(Event::Start(_)) => {
                ui.indent("nested", |ui| render_block_once(ui, events));
            }
            Some(_) => {
                events.next();
            }
            None => break,
        }
    }
}

fn render_blockquote(ui: &mut Ui, events: &mut Peekable<Parser<'_>>) {
    egui::Frame::default()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(4.0)
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            loop {
                match events.peek() {
                    Some(Event::End(TagEnd::BlockQuote(_))) => {
                        events.next();
                        break;
                    }
                    Some(Event::Start(_)) => render_block_once(ui, events),
                    Some(_) => {
                        events.next();
                    }
                    None => break,
                }
            }
        });
}

fn render_codeblock(ui: &mut Ui, events: &mut Peekable<Parser<'_>>, kind: &CodeBlockKind<'_>) {
    let mut code = String::new();
    while let Some(ev) = events.peek().cloned() {
        match ev {
            Event::Text(t) => {
                events.next();
                code.push_str(&t);
            }
            Event::End(TagEnd::CodeBlock) => {
                events.next();
                break;
            }
            _ => {
                events.next();
            }
        }
    }

    let lang = match kind {
        CodeBlockKind::Fenced(l) => l.to_string(),
        CodeBlockKind::Indented => String::new(),
    };

    let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
    let mut job =
        egui_extras::syntax_highlighting::highlight(ui.ctx(), ui.style(), &theme, &code, &lang);
    job.wrap.max_width = f32::INFINITY;

    egui::Frame::default()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(4.0)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.add(egui::Label::new(job).selectable(false));
            });
        });
}

fn render_table(ui: &mut Ui, events: &mut Peekable<Parser<'_>>) {
    // Собираем строки: (заголовок?, ячейки)
    let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
    let mut in_head = false;
    let mut row: Vec<String> = Vec::new();
    let mut row_is_head = false;

    loop {
        match events.peek().cloned() {
            Some(Event::Start(Tag::TableHead)) => {
                events.next();
                in_head = true;
            }
            Some(Event::End(TagEnd::TableHead)) => {
                events.next();
                in_head = false;
            }
            Some(Event::Start(Tag::TableRow)) => {
                events.next();
                row.clear();
                row_is_head = in_head;
            }
            Some(Event::Start(Tag::TableCell)) => {
                events.next();
                row.push(collect_plain_text(events, TagEnd::TableCell));
            }
            Some(Event::End(TagEnd::TableRow)) => {
                events.next();
                rows.push((row_is_head, row.clone()));
            }
            Some(Event::End(TagEnd::Table)) => {
                events.next();
                break;
            }
            Some(_) => {
                events.next();
            }
            None => break,
        }
    }

    egui::Frame::group(ui.style())
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                egui::Grid::new("md_table")
                    .striped(true)
                    .min_col_width(60.0)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for (is_head, cells) in &rows {
                            for cell in cells {
                                if *is_head {
                                    ui.strong(cell);
                                } else {
                                    ui.label(cell);
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
        });
}

// ---------------------------------------------------------------------------
// Инлайн
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum InlineStop {
    /// До конца абзаца.
    Paragraph,
    /// До конца заголовка уровня `level`.
    Heading(HeadingLevel),
    /// До конца определения сноски.
    FootnoteDef,
    /// До конца пункта списка (или начала вложенного блока).
    Item,
    /// До первого блока.
    None,
}

fn is_block_start(ev: &Event<'_>) -> bool {
    matches!(
        ev,
        Event::Start(Tag::List(_)
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::Table(_)
            | Tag::Heading { .. }
            | Tag::FootnoteDefinition(_)
            | Tag::HtmlBlock)
    )
}

fn should_stop(ev: &Event<'_>, stop: InlineStop) -> bool {
    match stop {
        InlineStop::Paragraph => matches!(ev, Event::End(TagEnd::Paragraph)),
        InlineStop::Heading(l) => {
            matches!(ev, Event::End(TagEnd::Heading(h)) if h == &l)
        }
        InlineStop::FootnoteDef => matches!(ev, Event::End(TagEnd::FootnoteDefinition)),
        InlineStop::Item => matches!(ev, Event::End(TagEnd::Item)) || is_block_start(ev),
        InlineStop::None => is_block_start(ev),
    }
}

fn base_format(ui: &Ui) -> TextFormat {
    TextFormat {
        font_id: FontId::proportional(body_size(ui)),
        color: ui.visuals().text_color(),
        ..Default::default()
    }
}

fn small_format(ui: &Ui) -> TextFormat {
    TextFormat {
        font_id: FontId::proportional(body_size(ui) * 0.85),
        color: ui.visuals().weak_text_color(),
        ..Default::default()
    }
}

fn body_size(ui: &Ui) -> f32 {
    ui.style()
        .text_styles
        .get(&TextStyle::Body)
        .map(|f| f.size)
        .unwrap_or(14.0)
}

fn render_inline(ui: &mut Ui, events: &mut Peekable<Parser<'_>>, stop: InlineStop) {
    let fmt = base_format(ui);
    render_inline_with(ui, events, stop, fmt);
}

fn render_inline_with(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    stop: InlineStop,
    base: TextFormat,
) {
    let job = collect_inline(ui, events, stop, base);
    if !job.text.is_empty() {
        ui.label(job);
    }
}

/// Собирает инлайн-события в `LayoutJob` до условия остановки.
fn collect_inline(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    stop: InlineStop,
    base: TextFormat,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let mut stack: Vec<TextFormat> = vec![base];

    while let Some(ev) = events.peek().cloned() {
        if should_stop(&ev, stop) {
            break;
        }

        match ev {
            Event::Start(Tag::Strong) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.color = ui.visuals().strong_text_color();
                stack.push(f);
            }
            Event::Start(Tag::Emphasis) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.italics = true;
                stack.push(f);
            }
            Event::Start(Tag::Strikethrough) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.strikethrough = Stroke::new(1.0, f.color);
                stack.push(f);
            }
            Event::Start(Tag::Link { .. }) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.color = ui.visuals().hyperlink_color;
                f.underline = Stroke::new(1.0, ui.visuals().hyperlink_color);
                stack.push(f);
            }
            Event::Start(Tag::Image { .. }) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.italics = true;
                f.color = ui.visuals().weak_text_color();
                stack.push(f);
            }
            Event::End(
                TagEnd::Strong
                | TagEnd::Emphasis
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image,
            ) => {
                events.next();
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            Event::End(TagEnd::Paragraph) => {
                // Прозрачно для «рыхлых» списков.
                events.next();
            }
            Event::End(_) => {
                events.next();
                break;
            }
            Event::Start(_) => {
                events.next();
                break;
            }
            Event::Text(t) => {
                events.next();
                job.append(&t, 0.0, stack.last().unwrap().clone());
            }
            Event::Code(c) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.font_id = FontId::monospace(f.font_id.size * 0.92);
                f.background = ui.visuals().faint_bg_color;
                job.append(&c, 0.0, f);
            }
            Event::SoftBreak => {
                events.next();
                job.append(" ", 0.0, stack.last().unwrap().clone());
            }
            Event::HardBreak => {
                events.next();
                job.append("\n", 0.0, stack.last().unwrap().clone());
            }
            Event::InlineHtml(h) => {
                events.next();
                job.append(&h, 0.0, stack.last().unwrap().clone());
            }
            Event::InlineMath(m) => {
                events.next();
                job.append(&m, 0.0, stack.last().unwrap().clone());
            }
            Event::FootnoteReference(r) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.font_id = FontId::proportional(f.font_id.size * 0.75);
                f.color = ui.visuals().hyperlink_color;
                job.append(&format!("[{r}]"), 0.0, f);
            }
            Event::Rule | Event::DisplayMath(_) | Event::TaskListMarker(_) => {
                events.next();
            }
            Event::Html(_) => {
                // Сырой HTML-блок в предпросмотре не интерпретируем.
                events.next();
            }
        }
    }

    job
}

/// Собирает только текст (для ячеек таблиц).
fn collect_plain_text(events: &mut Peekable<Parser<'_>>, end: TagEnd) -> String {
    let mut s = String::new();
    while let Some(ev) = events.peek().cloned() {
        match ev {
            Event::Text(t) => {
                events.next();
                s.push_str(&t);
            }
            Event::Code(c) => {
                events.next();
                s.push_str(&c);
            }
            Event::SoftBreak => {
                events.next();
                s.push(' ');
            }
            Event::HardBreak => {
                events.next();
                s.push('\n');
            }
            Event::End(e) if e == end => {
                events.next();
                break;
            }
            _ => {
                events.next();
            }
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_split() {
        let text = "---\ntitle: Hi\ntags: a, b\n---\nBody\n";
        let (fm, body) = split_frontmatter(text);
        assert_eq!(
            fm,
            Some(vec![
                ("title".to_string(), "Hi".to_string()),
                ("tags".to_string(), "a, b".to_string()),
            ])
        );
        assert_eq!(body, "Body\n");
    }

    #[test]
    fn frontmatter_split_crlf() {
        let text = "---\r\ntitle: Hi\r\n---\r\nBody\r\n";
        let (fm, body) = split_frontmatter(text);
        assert_eq!(fm, Some(vec![("title".to_string(), "Hi".to_string())]));
        assert_eq!(body, "Body\r\n");
    }

    #[test]
    fn no_frontmatter() {
        let text = "# Just a heading\n";
        let (fm, body) = split_frontmatter(text);
        assert!(fm.is_none());
        assert_eq!(body, text);
    }

    #[test]
    fn unterminated_frontmatter_is_kept() {
        let text = "---\ntitle: Hi\nbody text\n";
        let (fm, body) = split_frontmatter(text);
        assert!(fm.is_none());
        assert_eq!(body, text);
    }

    #[test]
    fn tables_parse() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        let parser = Parser::new_ext(md, options);
        let tags: Vec<_> = parser
            .filter_map(|e| match e {
                Event::Start(t) => Some(t),
                _ => None,
            })
            .collect();
        assert!(tags.iter().any(|t| matches!(t, Tag::Table(_))));
    }
}
