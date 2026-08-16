//! Small Markdown-to-ratatui renderer for assistant transcript messages.

use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::Event;
use pulldown_cmark::HeadingLevel;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::frontend::theme::Role;
use crate::frontend::theme::current;

pub(super) fn render(source: &str, base: Style) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    let mut writer = Writer::new(base);
    for event in Parser::new_ext(source, options) {
        writer.event(event);
    }
    writer.finish()
}

struct Item {
    first_prefix: String,
    continuation: String,
    first_line: bool,
}

#[derive(Default)]
struct Table {
    rows: Vec<(Vec<Vec<Span<'static>>>, bool)>,
    row: Vec<Vec<Span<'static>>>,
    cell: Vec<Span<'static>>,
    in_head: bool,
}

struct Writer {
    base: Style,
    lines: Vec<Line<'static>>,
    current: Option<Line<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    items: Vec<Item>,
    blockquote_depth: usize,
    in_code_block: bool,
    needs_blank: bool,
    link: Option<String>,
    table: Option<Table>,
}

impl Writer {
    fn new(base: Style) -> Self {
        Self {
            base,
            lines: Vec::new(),
            current: None,
            styles: vec![Style::default()],
            lists: Vec::new(),
            items: Vec::new(),
            blockquote_depth: 0,
            in_code_block: false,
            needs_blank: false,
            link: None,
            table: None,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        self.lines
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.push_styled(&code, current().style(Role::Code)),
            Event::SoftBreak | Event::HardBreak => self.flush(),
            Event::Rule => {
                self.start_block();
                self.push_styled("———", current().style(Role::Border));
                self.flush();
                self.needs_blank = true;
            }
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.start_block(),
            Tag::Heading { level, .. } => {
                self.start_block();
                self.push_style(heading_style(level));
            }
            Tag::BlockQuote => {
                self.start_block();
                self.blockquote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.start_block();
                self.in_code_block = true;
                if matches!(kind, CodeBlockKind::Indented) {
                    self.push("    ");
                }
            }
            Tag::List(start) => {
                self.start_block();
                self.lists.push(start);
            }
            Tag::Item => self.start_item(),
            Tag::Emphasis => {
                self.push_style(Style::default().add_modifier(Modifier::ITALIC));
            }
            Tag::Strong => {
                self.push_style(Style::default().add_modifier(Modifier::BOLD));
            }
            Tag::Strikethrough => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.into_string());
                self.push_style(
                    current()
                        .style(Role::Info)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Table(_) => {
                self.start_block();
                self.table = Some(Table::default());
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.in_head = true;
                }
            }
            Tag::TableRow => {}
            Tag::TableCell => {}
            Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::Image { .. }
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.needs_blank = true;
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush();
                self.needs_blank = true;
            }
            TagEnd::BlockQuote => {
                self.flush();
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.needs_blank = true;
            }
            TagEnd::CodeBlock => {
                self.flush();
                self.in_code_block = false;
                self.needs_blank = true;
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.needs_blank = true;
            }
            TagEnd::Item => {
                self.flush();
                self.items.pop();
                self.needs_blank = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some(destination) = self.link.take() {
                    self.push(" (");
                    self.push_styled(
                        &destination,
                        current()
                            .style(Role::Info)
                            .add_modifier(Modifier::UNDERLINED),
                    );
                    self.push(")");
                }
            }
            TagEnd::Table => self.end_table(),
            TagEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push((row, true));
                    table.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push((row, table.in_head));
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.row.push(std::mem::take(&mut table.cell));
                }
            }
            TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::Image
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn start_block(&mut self) {
        self.flush();
        if self.needs_blank && self.items.is_empty() && !self.lines.is_empty() {
            self.push_blank();
        }
        self.needs_blank = false;
    }

    fn start_item(&mut self) {
        self.flush();
        let depth = self.lists.len().max(1);
        let indent = " ".repeat((depth - 1) * 4);
        let marker = match self.lists.last_mut() {
            Some(Some(index)) => {
                let marker = format!("{index}. ");
                *index += 1;
                marker
            }
            Some(None) | None => "- ".into(),
        };
        self.items.push(Item {
            continuation: " ".repeat(indent.len() + marker.len()),
            first_prefix: indent + &marker,
            first_line: true,
        });
        self.needs_blank = false;
    }

    fn text(&mut self, text: &str) {
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.flush();
            }
            if !part.is_empty() {
                self.push(part);
            }
        }
    }

    fn push(&mut self, text: &str) {
        self.push_styled(text, Style::default());
    }

    fn push_styled(&mut self, text: &str, style: Style) {
        let style = self.styles.last().copied().unwrap_or_default().patch(style);
        if let Some(table) = self.table.as_mut() {
            table.cell.push(Span::styled(text.to_string(), style));
            return;
        }
        self.ensure_line();
        if let Some(line) = self.current.as_mut() {
            line.push_span(Span::styled(text.to_string(), style));
        }
    }

    fn ensure_line(&mut self) {
        if self.current.is_some() {
            return;
        }
        let style = if self.blockquote_depth > 0 {
            self.base.patch(current().style(Role::Muted))
        } else {
            self.base
        };
        let mut line = Line::default().style(style);
        for _ in 0..self.blockquote_depth {
            line.push_span(Span::styled("> ", current().style(Role::Neutral)));
        }
        if let Some(item) = self.items.last_mut() {
            let prefix = if std::mem::take(&mut item.first_line) {
                &item.first_prefix
            } else {
                &item.continuation
            };
            line.push_span(Span::raw(prefix.clone()));
        }
        self.current = Some(line);
    }

    fn flush(&mut self) {
        if let Some(line) = self.current.take()
            && !line.spans.is_empty()
        {
            self.lines.push(line);
        }
    }

    fn push_blank(&mut self) {
        if self.lines.last().is_none_or(|line| !line.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn push_style(&mut self, style: Style) {
        let current = self.styles.last().copied().unwrap_or_default();
        self.styles.push(current.patch(style));
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    fn end_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        for (row, header) in table.rows {
            self.ensure_line();
            for (column, mut cell) in row.into_iter().enumerate() {
                if header {
                    for span in &mut cell {
                        span.style = span
                            .style
                            .patch(Style::default().add_modifier(Modifier::BOLD));
                    }
                }
                if let Some(line) = self.current.as_mut() {
                    if column > 0 {
                        line.push_span(Span::styled(" │ ", current().style(Role::Border)));
                    }
                    line.spans.append(&mut cell);
                }
            }
            self.flush();
        }
        self.needs_blank = true;
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    match level {
        HeadingLevel::H1 => Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED),
        HeadingLevel::H2 => Style::default().add_modifier(Modifier::BOLD),
        HeadingLevel::H3 => Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::ITALIC),
        HeadingLevel::H4 | HeadingLevel::H5 | HeadingLevel::H6 => {
            Style::default().add_modifier(Modifier::ITALIC)
        }
    }
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
