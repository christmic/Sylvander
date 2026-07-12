//! Semantic Markdown rendering with terminal-safe, width-correct output.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme;

pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let normalized = stabilize_incomplete_inline_markers(text);
    let clean = sanitize_terminal_text(&break_inline_numbered_lists(&normalized));
    let parser = Parser::new_ext(
        &clean,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
    );
    let mut out = RichWriter::new(width.max(1));
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut in_item = false;
    let mut quote_depth = 0usize;
    let mut strong = 0usize;
    let mut emphasis = 0usize;
    let mut strike = 0usize;
    let mut heading = false;
    let mut link: Option<String> = None;
    let mut image: Option<String> = None;
    let mut code_block = false;
    let mut table: Option<TableBuilder> = None;

    for event in parser {
        if let Some(builder) = table.as_mut() {
            if builder.consume(&event) {
                continue;
            }
            let finished = builder.clone();
            out.finish_block(false);
            render_table(&mut out, finished);
            table = None;
            continue;
        }

        match event {
            Event::Start(Tag::Paragraph) => {
                if !in_item {
                    let prefix = quote_prefix(quote_depth);
                    out.start_block(prefix.clone(), prefix);
                }
            }
            Event::End(TagEnd::Paragraph) => out.finish_block(!in_item),
            Event::Start(Tag::Heading { .. }) => {
                heading = true;
                let prefix = quote_prefix(quote_depth);
                out.start_block(prefix.clone(), prefix);
            }
            Event::End(TagEnd::Heading(_)) => {
                heading = false;
                out.finish_block(true);
            }
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::End(TagEnd::BlockQuote(_)) => quote_depth = quote_depth.saturating_sub(1),
            Event::Start(Tag::List(start)) => lists.push(start),
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    out.blank();
                }
            }
            Event::Start(Tag::Item) => {
                in_item = true;
                let indent = "  ".repeat(lists.len().saturating_sub(1));
                let marker = match lists.last_mut() {
                    Some(Some(next)) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    _ => "• ".into(),
                };
                let first = format!("{}{}{}", quote_prefix(quote_depth), indent, marker);
                let continuation = " ".repeat(UnicodeWidthStr::width(first.as_str()));
                out.start_block(first, continuation);
            }
            Event::End(TagEnd::Item) => {
                out.finish_block(false);
                in_item = false;
            }
            Event::TaskListMarker(done) => {
                out.push_text(if done { "[✓] " } else { "[ ] " }, theme::verified());
            }
            Event::Start(Tag::Strong) => strong += 1,
            Event::End(TagEnd::Strong) => strong = strong.saturating_sub(1),
            Event::Start(Tag::Emphasis) => emphasis += 1,
            Event::End(TagEnd::Emphasis) => emphasis = emphasis.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => strike += 1,
            Event::End(TagEnd::Strikethrough) => strike = strike.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => link = Some(dest_url.into_string()),
            Event::End(TagEnd::Link) => {
                if let Some(url) = link.take() {
                    out.push_text(&format!(" ({url})"), link_style());
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                image = Some(dest_url.into_string());
                out.push_text("[image: ", theme::text_muted());
            }
            Event::End(TagEnd::Image) => {
                if let Some(url) = image.take() {
                    out.push_text(&format!("] ({url})"), link_style());
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                code_block = true;
                out.finish_block(false);
                let language = match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        format!("code · {language}")
                    }
                    _ => "code".into(),
                };
                out.raw_line(&language, theme::text_muted(), "┌ ");
            }
            Event::End(TagEnd::CodeBlock) => {
                code_block = false;
                out.raw_line("", theme::text_muted(), "└ ");
                out.blank();
            }
            Event::Start(Tag::Table(_)) => table = Some(TableBuilder::default()),
            Event::Text(value) => {
                let style = inline_style(heading, strong, emphasis, strike, link.is_some());
                if code_block {
                    for line in value.lines() {
                        out.raw_line(line, code_style(), "│ ");
                    }
                } else {
                    out.push_text(&value, style);
                }
            }
            Event::Code(value) => out.push_text(&value, code_style()),
            Event::SoftBreak => out.push_text(" ", theme::text()),
            Event::HardBreak => out.new_line(),
            Event::Rule => {
                out.finish_block(false);
                out.raw_line(&"─".repeat(out.width.min(72)), theme::text_muted(), "");
                out.blank();
            }
            Event::Html(value) | Event::InlineHtml(value) => {
                out.push_text(&value, theme::text_dim())
            }
            Event::FootnoteReference(value) => out.push_text(&format!("[{value}]"), link_style()),
            _ => {}
        }
    }
    if let Some(table) = table {
        out.finish_block(false);
        render_table(&mut out, table);
    }
    out.finish()
}

fn stabilize_incomplete_inline_markers(text: &str) -> String {
    let mut value = text.to_string();
    for marker in ["**", "__"] {
        if value.matches(marker).count() % 2 == 1 {
            if let Some(index) = value.rfind(marker) {
                value.replace_range(index..index + marker.len(), "");
            }
        }
    }
    if !value.contains("```") && value.matches('`').count() % 2 == 1 {
        if let Some(index) = value.rfind('`') {
            value.remove(index);
        }
    }
    value
}

fn break_inline_numbered_lists(text: &str) -> String {
    let chars = text.replace('\r', "").chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(chars.len());
    let mut index = 0;
    let mut in_inline_code = false;
    while index < chars.len() {
        if chars[index] == '`' {
            in_inline_code = !in_inline_code;
        }
        if !in_inline_code && chars[index].is_ascii_digit() {
            let mut end = index;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            let marker =
                end + 1 < chars.len() && chars[end] == '.' && chars[end + 1].is_whitespace();
            let line_has_content = output
                .rsplit_once('\n')
                .map_or(!output.trim().is_empty(), |(_, line)| {
                    !line.trim().is_empty()
                });
            if marker && line_has_content {
                while output.ends_with(' ') {
                    output.pop();
                }
                output.push('\n');
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn inline_style(heading: bool, strong: usize, emphasis: usize, strike: usize, link: bool) -> Style {
    let mut style = if heading {
        theme::header()
    } else {
        theme::text()
    };
    if strong > 0 {
        style = style.add_modifier(Modifier::BOLD);
    }
    if emphasis > 0 {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if strike > 0 {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if link {
        style = link_style();
    }
    style
}

fn code_style() -> Style {
    theme::active()
}

fn link_style() -> Style {
    theme::brand_violet().add_modifier(Modifier::UNDERLINED)
}

fn quote_prefix(depth: usize) -> String {
    "│ ".repeat(depth)
}

#[derive(Clone, Default)]
struct TableBuilder {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
}

impl TableBuilder {
    fn consume(&mut self, event: &Event<'_>) -> bool {
        if matches!(event, Event::End(TagEnd::Table)) {
            return false;
        }
        match event {
            Event::Start(Tag::TableCell) => self.cell.clear(),
            Event::Text(value) | Event::Code(value) => self.cell.push_str(value),
            Event::End(TagEnd::TableCell) => self.row.push(self.cell.trim().to_string()),
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                if !self.row.is_empty() {
                    self.rows.push(std::mem::take(&mut self.row));
                }
            }
            _ => {}
        }
        true
    }
}

fn render_table(out: &mut RichWriter, table: TableBuilder) {
    let Some(header) = table.rows.first() else {
        return;
    };
    if out.width < 48 {
        for (row_index, row) in table.rows.iter().skip(1).enumerate() {
            if row_index > 0 {
                out.blank();
            }
            for (index, value) in row.iter().enumerate() {
                let label = header.get(index).map_or("column", String::as_str);
                out.start_block(format!("{label}: "), "  ".into());
                out.push_text(value, theme::text());
                out.finish_block(false);
            }
        }
        out.blank();
        return;
    }
    let columns = table.rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let cell_width = out.width.saturating_sub(columns + 1) / columns;
    for (row_index, row) in table.rows.iter().enumerate() {
        let mut line = String::from("│");
        for index in 0..columns {
            let value = row.get(index).map_or("", String::as_str);
            let value = truncate_width(value, cell_width.saturating_sub(2));
            let padding = cell_width.saturating_sub(1 + UnicodeWidthStr::width(value.as_str()));
            line.push(' ');
            line.push_str(&value);
            line.push_str(&" ".repeat(padding));
            line.push('│');
        }
        out.raw_line(
            &line,
            if row_index == 0 {
                theme::header()
            } else {
                theme::text()
            },
            "",
        );
    }
    out.blank();
}

struct RichWriter {
    width: usize,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    line_width: usize,
    first_prefix: String,
    continuation: String,
    first_line: bool,
    pending_space: bool,
}

impl RichWriter {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            spans: Vec::new(),
            line_width: 0,
            first_prefix: String::new(),
            continuation: String::new(),
            first_line: true,
            pending_space: false,
        }
    }

    fn start_block(&mut self, first: String, continuation: String) {
        self.flush();
        self.first_prefix = first;
        self.continuation = continuation;
        self.first_line = true;
        self.pending_space = false;
    }

    fn ensure_line(&mut self) {
        if self.spans.is_empty() {
            let prefix = if self.first_line {
                &self.first_prefix
            } else {
                &self.continuation
            };
            if !prefix.is_empty() {
                self.line_width = UnicodeWidthStr::width(prefix.as_str());
                self.spans
                    .push(Span::styled(prefix.clone(), theme::text_muted()));
            }
            self.first_line = false;
        }
    }

    fn push_text(&mut self, text: &str, style: Style) {
        let text = sanitize_terminal_text(text);
        let mut word = String::new();
        for ch in text.chars() {
            if ch == '\n' {
                self.push_word(&word, style);
                word.clear();
                self.new_line();
            } else if ch.is_whitespace() {
                self.push_word(&word, style);
                word.clear();
                self.pending_space = true;
            } else {
                word.push(ch);
            }
        }
        self.push_word(&word, style);
    }

    fn push_word(&mut self, word: &str, style: Style) {
        if word.is_empty() {
            return;
        }
        self.ensure_line();
        let space = usize::from(self.pending_space && self.line_width > 0);
        let word_width = UnicodeWidthStr::width(word);
        if self.line_width + space + word_width > self.width && self.line_width > 0 {
            self.flush();
            self.ensure_line();
        } else if space > 0 {
            self.spans.push(Span::styled(" ", style));
            self.line_width += 1;
        }
        self.pending_space = false;
        let mut chunk = String::new();
        for grapheme in word.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if self.line_width + UnicodeWidthStr::width(chunk.as_str()) + grapheme_width
                > self.width
                && !chunk.is_empty()
            {
                self.spans
                    .push(Span::styled(std::mem::take(&mut chunk), style));
                self.flush();
                self.ensure_line();
            }
            chunk.push_str(grapheme);
        }
        if !chunk.is_empty() {
            self.line_width += UnicodeWidthStr::width(chunk.as_str());
            self.spans.push(Span::styled(chunk, style));
        }
    }

    fn new_line(&mut self) {
        self.flush();
        self.pending_space = false;
    }

    fn raw_line(&mut self, text: &str, style: Style, prefix: &str) {
        self.flush();
        let available = self
            .width
            .saturating_sub(UnicodeWidthStr::width(prefix))
            .max(1);
        let mut rest = sanitize_terminal_text(text);
        loop {
            let (part, tail) = take_width(&rest, available);
            self.lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), theme::text_muted()),
                Span::styled(part, style),
            ]));
            if tail.is_empty() {
                break;
            }
            rest = tail;
        }
    }

    fn finish_block(&mut self, blank: bool) {
        self.flush();
        if blank {
            self.blank();
        }
    }

    fn flush(&mut self) {
        if !self.spans.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
            self.line_width = 0;
        }
    }

    fn blank(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
            self.lines.push(Line::from(""));
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        self.lines
    }
}

pub fn sanitize_terminal_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            } else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '\n' || ch == '\t' || !ch.is_control() {
            out.push(ch);
        }
    }
    out
}

fn take_width(text: &str, max: usize) -> (String, String) {
    let mut width = 0;
    let mut split = text.len();
    for (index, grapheme) in text.grapheme_indices(true) {
        let next = width + UnicodeWidthStr::width(grapheme);
        if next > max {
            split = index;
            break;
        }
        width = next;
    }
    (text[..split].to_string(), text[split..].to_string())
}

fn truncate_width(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_string();
    }
    let (mut value, _) = take_width(text, max.saturating_sub(1));
    value.push('…');
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn keeps_markdown_structure_and_width_correct_cjk() {
        let lines = render("## 标题\n\n- **重要**：你好世界你好世界", 12);
        let rows = plain(&lines);
        assert_eq!(rows[0], "标题");
        assert!(rows.iter().any(|row| row.starts_with("• 重要")));
        assert!(
            rows.iter()
                .all(|row| UnicodeWidthStr::width(row.as_str()) <= 12)
        );
    }

    #[test]
    fn renders_code_links_quotes_and_narrow_table_semantically() {
        let source = "> See [docs](https://example.com)\n\n```rs\nlet x = 1;\n```\n\n| A | B |\n|---|---|\n| x | y |";
        let rows = plain(&render(source, 30));
        assert!(rows.iter().any(|row| row.starts_with("│ See docs")));
        assert!(rows.iter().any(|row| row == "│ let x = 1;"));
        assert!(rows.iter().any(|row| row == "A: x"));
    }

    #[test]
    fn strips_terminal_control_sequences_without_losing_text() {
        assert_eq!(
            sanitize_terminal_text("ok\u{1b}[31mRED\u{1b}[0m\u{7}"),
            "okRED"
        );
    }

    #[test]
    fn incomplete_inline_markers_keep_streaming_geometry_stable() {
        let partial = plain(&render("Status: **working", 12));
        let settled = plain(&render("Status: **working**", 12));
        assert_eq!(partial, settled);
    }

    #[test]
    fn wrapping_never_splits_emoji_or_combining_graphemes() {
        let rows = plain(&render(
            "👩‍💻 e\u{301}cole https://example.com/very/long/path",
            12,
        ));
        assert!(
            rows.iter()
                .all(|row| UnicodeWidthStr::width(row.as_str()) <= 12)
        );
        assert!(rows.iter().any(|row| row.contains("👩‍💻")));
        assert!(rows.iter().any(|row| row.contains("e\u{301}")));
        assert!(!rows.iter().any(|row| row.starts_with('\u{301}')));
    }
}
