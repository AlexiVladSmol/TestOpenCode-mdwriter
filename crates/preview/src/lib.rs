//! Живой предпросмотр Markdown: рендер pulldown-cmark AST в egui.
//!
//! Поддерживается: заголовки, абзацы, emphasis/strong/strikethrough,
//! inline-код, код-блоки (с подсветкой syntect), цитаты, списки (в т.ч.
//! упорядоченные и задачи), таблицы, горизонтальные линии, сноски,
//! frontmatter. Wikilinks `[[target]]` рендерятся кликабельными ссылками.

use std::iter::Peekable;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Margin, RichText, Stroke, TextStyle, Ui};
use pulldown_cmark::{BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};

/// Действие, которое пользователь запросил в предпросмотре.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Клик по wikilink с целью `target` (без расширения).
    OpenWiki(String),
    /// Клик по чекбоксу задачи. `offset` — байтовое смещение `[` маркера
    /// (`[ ]`/`[x]`) во всём тексте документа, включая frontmatter.
    ToggleTask { offset: usize },
}

/// Рисует markdown-текст в `ui`. Возвращает действия (например, клики по ссылкам).
pub fn markdown(ui: &mut Ui, text: &str) -> Vec<Action> {
    let mut actions = Vec::new();

    let (frontmatter, body) = match markdown::split_frontmatter(text) {
        Some((fm, body)) => (Some(fm), body),
        None => (None, text),
    };
    if let Some(fm) = &frontmatter {
        render_frontmatter(ui, fm);
    }

    let task_offsets = document_task_offsets(text);
    let body = wiki_to_md(body);
    let mut task_counter = 0usize;
    let mut block_counter = 0usize;
    render_body(
        ui,
        &body,
        &task_offsets,
        &mut task_counter,
        &mut block_counter,
        &mut actions,
    );
    actions
}

/// Заменяет `[[target|alias]]` на `[alias](<wiki:target>)` в тексте вне кода.
/// Сама сборка ссылки живёт в крейте `markdown` (общая с экспортом).
fn wiki_to_md(text: &str) -> String {
    markdown::wikilinks_to_md(text, |target| format!("wiki:{target}"))
}

/// Смещения маркеров задач во **всём** тексте документа (с frontmatter) —
/// ровно те, что предпросмотр кладёт в [`Action::ToggleTask`]: n-й
/// отрисованный чекбокс получает n-е смещение.
///
/// Считаются по исходному телу (до замены wikilinks, которая сдвигает байты);
/// замена не меняет блочную структуру, поэтому порядок маркеров совпадает.
pub fn document_task_offsets(text: &str) -> Vec<usize> {
    let body = markdown::split_frontmatter(text).map_or(text, |(_, body)| body);
    let fm_len = text.len() - body.len();
    task_marker_offsets(body)
        .into_iter()
        .map(|o| o + fm_len)
        .collect()
}

/// Байтовые смещения `[` у всех маркеров задач в `body`, в порядке документа.
///
/// Маркеры находит сам парсер (теми же опциями, что и рендер), поэтому
/// учитываются задачи в цитатах, в списках `10.` и `1)`, во вложенных
/// списках, и не учитываются строки, похожие на задачи, внутри кода.
pub fn task_marker_offsets(body: &str) -> Vec<usize> {
    Parser::new_ext(body, markdown::cmark_options())
        .into_offset_iter()
        .filter_map(|(ev, range)| match ev {
            // Диапазон события может включать окружение — ищем `[` внутри.
            Event::TaskListMarker(_) => body[range.clone()].find('[').map(|i| range.start + i),
            _ => None,
        })
        .collect()
}

fn render_frontmatter(ui: &mut Ui, fm: &markdown::Frontmatter) {
    egui::Frame::group(ui.style())
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            egui::CollapsingHeader::new("Metadata")
                .default_open(false)
                .show(ui, |ui| {
                    for (k, v) in &fm.pairs {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(k).color(ui.visuals().weak_text_color()));
                            ui.label(v);
                        });
                    }
                });
        });
}

// ---------------------------------------------------------------------------
// Блоки
// ---------------------------------------------------------------------------

fn render_body(
    ui: &mut Ui,
    md: &str,
    task_offsets: &[usize],
    task_counter: &mut usize,
    block_counter: &mut usize,
    actions: &mut Vec<Action>,
) {
    let parser = Parser::new_ext(md, markdown::cmark_options());
    render_blocks(
        ui,
        &mut parser.peekable(),
        task_offsets,
        task_counter,
        block_counter,
        actions,
    );
}

fn render_blocks(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    task_offsets: &[usize],
    task_counter: &mut usize,
    block_counter: &mut usize,
    actions: &mut Vec<Action>,
) {
    while let Some(ev) = events.peek().cloned() {
        // Блочные элементы рисует `render_block_once` — тот же код, что и для
        // вложенного содержимого списков и цитат (раньше здесь был дубль).
        if render_block_once(
            ui,
            events,
            task_offsets,
            task_counter,
            block_counter,
            actions,
        ) {
            continue;
        }
        match ev {
            Event::Start(Tag::FootnoteDefinition(_)) => {
                events.next();
                ui.indent("footnote", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        render_inline_with(
                            ui,
                            events,
                            InlineStop::FootnoteDef,
                            small_format(ui),
                            actions,
                        );
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
                render_inline(ui, events, InlineStop::None, actions);
            }
        }
    }
}

/// Рендер одного блока. `true` — событие было блоком и обработано,
/// `false` — событие не блочное (его разбирает вызывающий).
fn render_block_once(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    task_offsets: &[usize],
    task_counter: &mut usize,
    block_counter: &mut usize,
    actions: &mut Vec<Action>,
) -> bool {
    match events.peek().cloned() {
        Some(Event::Start(Tag::Paragraph)) => {
            events.next();
            render_inline(ui, events, InlineStop::Paragraph, actions);
            true
        }
        Some(Event::Start(Tag::Heading { level, .. })) => {
            events.next();
            render_heading(ui, events, level, actions);
            true
        }
        Some(Event::Start(Tag::List(start))) => {
            events.next();
            render_list(
                ui,
                events,
                start,
                task_offsets,
                task_counter,
                block_counter,
                actions,
            );
            true
        }
        Some(Event::Start(Tag::BlockQuote(kind))) => {
            events.next();
            render_blockquote(
                ui,
                events,
                kind,
                task_offsets,
                task_counter,
                block_counter,
                actions,
            );
            true
        }
        Some(Event::Start(Tag::CodeBlock(kind))) => {
            events.next();
            render_codeblock(ui, events, &kind, block_counter);
            true
        }
        Some(Event::Start(Tag::Table(_))) => {
            events.next();
            render_table(ui, events, block_counter);
            true
        }
        Some(Event::DisplayMath(m)) => {
            events.next();
            render_math_block(ui, &m);
            true
        }
        _ => false,
    }
}

fn render_heading(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    level: HeadingLevel,
    actions: &mut Vec<Action>,
) {
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
        font_id: bold_font_id(size),
        color: ui.visuals().strong_text_color(),
        ..Default::default()
    };
    render_inline_with(ui, events, InlineStop::Heading(level), fmt, actions);
    ui.add_space(size * 0.15);
}

fn render_list(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    start: Option<u64>,
    task_offsets: &[usize],
    task_counter: &mut usize,
    block_counter: &mut usize,
    actions: &mut Vec<Action>,
) {
    let ordered = start.is_some();
    let mut n = start.unwrap_or(1);
    loop {
        match events.peek() {
            Some(Event::Start(Tag::Item)) => {
                events.next();
                render_item(
                    ui,
                    events,
                    ordered,
                    n,
                    task_offsets,
                    task_counter,
                    block_counter,
                    actions,
                );
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

#[allow(clippy::too_many_arguments)]
fn render_item(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    ordered: bool,
    index: u64,
    task_offsets: &[usize],
    task_counter: &mut usize,
    block_counter: &mut usize,
    actions: &mut Vec<Action>,
) {
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
                let offset = task_offsets.get(*task_counter).copied();
                *task_counter += 1;
                if ui.checkbox(&mut c, "").clicked() {
                    if let Some(offset) = offset {
                        actions.push(Action::ToggleTask { offset });
                    }
                }
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
        render_inline(ui, events, InlineStop::Item, actions);
    });

    // Вложенные блоки (вложенные списки, код, цитаты…)
    loop {
        match events.peek().cloned() {
            Some(Event::End(TagEnd::Item)) => {
                events.next();
                break;
            }
            Some(Event::Start(_)) => {
                ui.indent("nested", |ui| {
                    render_block_once(
                        ui,
                        events,
                        task_offsets,
                        task_counter,
                        block_counter,
                        actions,
                    )
                });
            }
            Some(_) => {
                events.next();
            }
            None => break,
        }
    }
}

fn render_blockquote(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    kind: Option<BlockQuoteKind>,
    task_offsets: &[usize],
    task_counter: &mut usize,
    block_counter: &mut usize,
    actions: &mut Vec<Action>,
) {
    match kind {
        // GFM-alert / Obsidian-callout: `> [!NOTE]` и т.п.
        Some(k) => {
            let (name, color) = match k {
                BlockQuoteKind::Note => ("NOTE", Color32::from_rgb(59, 130, 246)),
                BlockQuoteKind::Tip => ("TIP", Color32::from_rgb(34, 197, 94)),
                BlockQuoteKind::Important => ("IMPORTANT", Color32::from_rgb(139, 92, 246)),
                BlockQuoteKind::Warning => ("WARNING", Color32::from_rgb(245, 158, 11)),
                BlockQuoteKind::Caution => ("CAUTION", Color32::from_rgb(239, 68, 68)),
            };
            let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24);
            egui::Frame::default()
                .fill(fill)
                .stroke(Stroke::new(2.0, color))
                .corner_radius(6.0)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(RichText::new(name).color(color).strong());
                    render_quote_content(
                        ui,
                        events,
                        task_offsets,
                        task_counter,
                        block_counter,
                        actions,
                    );
                });
        }
        None => {
            egui::Frame::default()
                .fill(ui.visuals().faint_bg_color)
                .stroke(Stroke::new(
                    1.0,
                    ui.visuals().widgets.noninteractive.bg_stroke.color,
                ))
                .corner_radius(4.0)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    render_quote_content(
                        ui,
                        events,
                        task_offsets,
                        task_counter,
                        block_counter,
                        actions,
                    )
                });
        }
    }
}

fn render_quote_content(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    task_offsets: &[usize],
    task_counter: &mut usize,
    block_counter: &mut usize,
    actions: &mut Vec<Action>,
) {
    loop {
        match events.peek().cloned() {
            Some(Event::End(TagEnd::BlockQuote(_))) => {
                events.next();
                break;
            }
            Some(_) => {
                let handled = render_block_once(
                    ui,
                    events,
                    task_offsets,
                    task_counter,
                    block_counter,
                    actions,
                );
                // Неизвестное событие пропускаем, иначе цикл встанет на нём.
                if !handled {
                    events.next();
                }
            }
            None => break,
        }
    }
}

fn render_math_block(ui: &mut Ui, formula: &str) {
    let color = ui.visuals().weak_text_color();
    egui::Frame::default()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(4.0)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.centered_and_justified(|ui| {
                ui.add(
                    egui::Label::new(RichText::new(formula).monospace().italics().color(color))
                        .selectable(false),
                );
            });
        });
}

fn render_codeblock(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    kind: &CodeBlockKind<'_>,
    block_counter: &mut usize,
) {
    let block_n = *block_counter;
    *block_counter += 1;

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
            egui::ScrollArea::horizontal()
                .id_salt(("code_block", block_n))
                .show(ui, |ui| {
                    ui.add(egui::Label::new(job).selectable(false));
                });
        });
}

fn render_table(ui: &mut Ui, events: &mut Peekable<Parser<'_>>, block_counter: &mut usize) {
    let block_n = *block_counter;
    *block_counter += 1;
    // Собираем строки: (заголовок?, ячейки)
    let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
    let mut in_head = false;
    let mut row: Vec<String> = Vec::new();
    let mut row_is_head = false;

    while let Some(ev) = events.peek().cloned() {
        match ev {
            Event::Start(Tag::TableHead) => {
                events.next();
                in_head = true;
            }
            Event::End(TagEnd::TableHead) => {
                events.next();
                in_head = false;
            }
            Event::Start(Tag::TableRow) => {
                events.next();
                row.clear();
                row_is_head = in_head;
            }
            Event::Start(Tag::TableCell) => {
                events.next();
                row.push(collect_plain_text(events, TagEnd::TableCell));
            }
            Event::End(TagEnd::TableRow) => {
                events.next();
                rows.push((row_is_head, row.clone()));
            }
            Event::End(TagEnd::Table) => {
                events.next();
                break;
            }
            _ => {
                events.next();
            }
        }
    }

    egui::Frame::group(ui.style())
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt(("md_table", block_n))
                .show(ui, |ui| {
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

/// Диапазон символов кликабельного wikilink внутри LayoutJob.
struct LinkRange {
    start: usize,
    end: usize,
    target: String,
}

fn is_block_start(ev: &Event<'_>) -> bool {
    matches!(
        ev,
        Event::Start(
            Tag::List(_)
                | Tag::BlockQuote(_)
                | Tag::CodeBlock(_)
                | Tag::Table(_)
                | Tag::Heading { .. }
                | Tag::FootnoteDefinition(_)
                | Tag::HtmlBlock
        )
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

/// Жирный шрифт: системный bold, зарегистрированный приложением под именем
/// `mdwriter-bold`. Если не найден, egui откатится на обычный.
fn bold_font_id(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("mdwriter-bold".into()))
}

fn render_inline(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    stop: InlineStop,
    actions: &mut Vec<Action>,
) {
    let fmt = base_format(ui);
    render_inline_with(ui, events, stop, fmt, actions);
}

fn render_inline_with(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    stop: InlineStop,
    base: TextFormat,
    actions: &mut Vec<Action>,
) {
    let (job, links) = collect_inline(ui, events, stop, base);
    if !job.text.is_empty() {
        render_job(ui, job, &links, actions);
    }
}

/// Wikilink, в диапазон которого попадает индекс символа.
/// Диапазон полуинтервальный: `[start, end)` — символ сразу после ссылки
/// кликабельным не считается.
fn link_at(links: &[LinkRange], idx: usize) -> Option<&LinkRange> {
    links.iter().find(|lr| lr.start <= idx && idx < lr.end)
}

/// Рисует LayoutJob и обрабатывает клики по wikilinks внутри него.
fn render_job(ui: &mut Ui, job: LayoutJob, links: &[LinkRange], actions: &mut Vec<Action>) {
    let response = ui.label(job.clone());
    if !links.is_empty() && response.clicked() {
        let galley = ui.fonts_mut(|f| f.layout_job(job));
        if let Some(pos) = response.interact_pointer_pos() {
            let local = pos - response.rect.min;
            let cursor = galley.cursor_from_pos(local);
            let idx = cursor.index.0;
            if let Some(lr) = link_at(links, idx) {
                actions.push(Action::OpenWiki(lr.target.clone()));
            }
        }
    }
}

/// Собирает инлайн-события в `LayoutJob` до условия остановки.
/// Возвращает также диапазоны wikilinks для hit-testing.
fn collect_inline(
    ui: &mut Ui,
    events: &mut Peekable<Parser<'_>>,
    stop: InlineStop,
    base: TextFormat,
) -> (LayoutJob, Vec<LinkRange>) {
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let mut stack: Vec<TextFormat> = vec![base];
    let mut links: Vec<LinkRange> = Vec::new();
    let mut open_wiki: Option<(usize, String)> = None;
    let mut char_cursor = 0usize;

    let push = |job: &mut LayoutJob, s: &str, f: TextFormat, cursor: &mut usize| {
        job.append(s, 0.0, f);
        *cursor += s.chars().count();
    };

    while let Some(ev) = events.peek().cloned() {
        if should_stop(&ev, stop) {
            break;
        }

        match ev {
            Event::Start(Tag::Strong) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.color = ui.visuals().strong_text_color();
                f.font_id = bold_font_id(f.font_id.size);
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
            Event::Start(Tag::Link { dest_url, .. }) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.color = ui.visuals().hyperlink_color;
                f.underline = Stroke::new(1.0, ui.visuals().hyperlink_color);
                stack.push(f);
                if let Some(target) = dest_url.strip_prefix("wiki:") {
                    open_wiki = Some((char_cursor, target.to_string()));
                }
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
                if matches!(ev, Event::End(TagEnd::Link)) {
                    if let Some((start, target)) = open_wiki.take() {
                        links.push(LinkRange {
                            start,
                            end: char_cursor,
                            target,
                        });
                    }
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
                push(
                    &mut job,
                    &t,
                    stack.last().unwrap().clone(),
                    &mut char_cursor,
                );
            }
            Event::Code(c) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.font_id = FontId::monospace(f.font_id.size * 0.92);
                f.background = ui.visuals().faint_bg_color;
                push(&mut job, &c, f, &mut char_cursor);
            }
            Event::SoftBreak => {
                events.next();
                push(
                    &mut job,
                    " ",
                    stack.last().unwrap().clone(),
                    &mut char_cursor,
                );
            }
            Event::HardBreak => {
                events.next();
                push(
                    &mut job,
                    "\n",
                    stack.last().unwrap().clone(),
                    &mut char_cursor,
                );
            }
            Event::InlineHtml(h) => {
                events.next();
                push(
                    &mut job,
                    &h,
                    stack.last().unwrap().clone(),
                    &mut char_cursor,
                );
            }
            Event::InlineMath(m) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.font_id = FontId::monospace(f.font_id.size * 0.95);
                f.italics = true;
                f.background = ui.visuals().faint_bg_color;
                push(&mut job, &m, f, &mut char_cursor);
            }
            Event::FootnoteReference(r) => {
                events.next();
                let mut f = stack.last().unwrap().clone();
                f.font_id = FontId::proportional(f.font_id.size * 0.75);
                f.color = ui.visuals().hyperlink_color;
                push(&mut job, &format!("[{r}]"), f, &mut char_cursor);
            }
            Event::Rule | Event::TaskListMarker(_) => {
                events.next();
            }
            Event::DisplayMath(_) => {
                // Блочная математика рисуется на уровне блоков — прерываем инлайн.
                break;
            }
            Event::Html(_) => {
                // Сырой HTML-блок в предпросмотре не интерпретируем.
                events.next();
            }
        }
    }

    (job, links)
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
    fn wiki_links_transformed() {
        let md = wiki_to_md("См. [[Заметка]] и [[Другая|алиас]].");
        assert_eq!(
            md,
            "См. [Заметка](<wiki:Заметка>) и [алиас](<wiki:Другая>)."
        );
    }

    /// Адреса ссылок, которые реально видит парсер markdown.
    fn link_dests(md: &str) -> Vec<String> {
        Parser::new_ext(md, markdown::cmark_options())
            .filter_map(|e| match e {
                Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn wiki_links_with_spaces_are_real_links() {
        let md = wiki_to_md("См. [[Моя заметка]].");
        assert_eq!(link_dests(&md), vec!["wiki:Моя заметка"]);
    }

    #[test]
    fn wiki_link_target_with_anchor() {
        let md = wiki_to_md("[[Моя заметка#Заголовок]]");
        assert_eq!(link_dests(&md), vec!["wiki:Моя заметка#Заголовок"]);
    }

    #[test]
    fn wiki_link_with_angle_brackets_is_kept_as_text() {
        let md = wiki_to_md("[[a<b]] и [[ok]]");
        assert!(md.starts_with("[[a<b]] и "));
        assert_eq!(link_dests(&md).len(), 1);
    }

    #[test]
    fn wiki_links_untouched_in_code() {
        let md = wiki_to_md("`[[не ссылка]]`");
        assert_eq!(md, "`[[не ссылка]]`");
    }

    #[test]
    fn ordered_task_list_marker() {
        let md = "1. [ ] один\n2. [x] два\n- [ ] три\n";
        let parser = Parser::new_ext(md, markdown::cmark_options());
        let markers: Vec<bool> = parser
            .filter_map(|e| match e {
                Event::TaskListMarker(b) => Some(b),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec![false, true, false]);
    }

    // ------------------- B2: маркеры задач по смещению -------------------

    /// Текст маркера по каждому найденному смещению.
    fn markers(body: &str) -> Vec<&str> {
        task_marker_offsets(body)
            .into_iter()
            .map(|o| &body[o..o + 3])
            .collect()
    }

    #[test]
    fn b2_offsets_point_at_markers() {
        let body =
            "- [ ] один\n1. [x] два\n  * [ ] вложенная\nпросто строка\n```\n- [ ] в коде\n```\n";
        assert_eq!(markers(body), vec!["[ ]", "[x]", "[ ]"]);
        assert_eq!(task_marker_offsets(body)[0], 2);
    }

    #[test]
    fn b2_tasks_in_blockquote_are_counted() {
        // Старый построчный поиск их не видел, а парсер рисует чекбокс —
        // нумерация сдвигалась, и клик переключал другую задачу.
        let body = "> - [ ] в цитате\n\n- [x] обычная\n";
        assert_eq!(markers(body), vec!["[ ]", "[x]"]);
        assert_eq!(task_marker_offsets(body)[1], body.find("[x]").unwrap());
    }

    #[test]
    fn b2_multi_digit_and_paren_ordered_lists() {
        let body = "10. [ ] десять\n1) [x] скобка\n";
        assert_eq!(markers(body), vec!["[ ]", "[x]"]);
    }

    #[test]
    fn b2_indented_code_is_not_a_task() {
        // 4 пробела после абзаца — блок кода, а не задача.
        let body = "абзац\n\n    - [ ] в коде\n\n- [x] задача\n";
        assert_eq!(markers(body), vec!["[x]"]);
    }

    #[test]
    fn b2_marker_not_text_with_brackets() {
        // В строке есть ещё «[ ]» в тексте — смещение указывает на маркер.
        let body = "- [x] купить [ ] что-то\n";
        assert_eq!(task_marker_offsets(body), vec![2]);
    }

    #[test]
    fn b2_wikilink_transform_keeps_task_count() {
        // Рендер идёт по тексту после замены wikilinks, а смещения — по
        // исходному; число и порядок маркеров обязаны совпадать.
        let body = "- [ ] позвонить [[Иван Петров|Ивану]]\n> - [x] см. [[a#b]]\n10. [ ] [[c]]\n";
        let original = task_marker_offsets(body).len();
        let transformed = task_marker_offsets(&wiki_to_md(body)).len();
        assert_eq!(original, 3);
        assert_eq!(transformed, original);
    }

    #[test]
    fn link_hit_test_is_half_open() {
        let links = vec![LinkRange {
            start: 3,
            end: 7,
            target: "A".to_string(),
        }];
        assert!(link_at(&links, 2).is_none());
        assert!(link_at(&links, 3).is_some());
        assert!(link_at(&links, 6).is_some());
        // Символ сразу после ссылки уже не кликабелен.
        assert!(link_at(&links, 7).is_none());
    }

    /// D3: блоки рендерятся одним кодом (`render_block_once`) и для верхнего
    /// уровня, и для вложенного содержимого. Прогоняем документ со всеми
    /// блоками, включая цитату со списком и кодом, — паника или зацикливание
    /// здесь означали бы регрессию рефакторинга.
    #[test]
    fn nested_blocks_render_without_panicking() {
        let text = "\
# Заголовок

Абзац с *курсивом* и [[ссылкой]].

> Цитата
> - пункт списка
> - второй пункт
>
> ```rust
> fn main() {}
> ```

| a | b |
|---|---|
| 1 | 2 |

- [ ] задача
- [x] сделано

---

Текст после правила.
";
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let actions = markdown(ui, text);
            assert!(actions.is_empty(), "в документе нет кликабельных действий");
        });
        // В тестах нет рендерера: дельты текстур нужно «принять».
        output.textures_delta.clear();
    }

    #[test]
    fn tables_parse() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let parser = Parser::new_ext(md, markdown::cmark_options());
        let tags: Vec<_> = parser
            .filter_map(|e| match e {
                Event::Start(t) => Some(t),
                _ => None,
            })
            .collect();
        assert!(tags.iter().any(|t| matches!(t, Tag::Table(_))));
    }
}
