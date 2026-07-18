//! Chat panel — main conversation area. Uses virtual scroll to render
//! only the lines that fit on screen, bottom-aligned. Every color and
//! state-derived glyph routes through `crate::theme` so the design
//! palette + state glyphs stay in a single module.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, ChatMessage, ToolStatus};
use crate::component::Component;
use crate::theme;

pub struct ChatPanel;

impl Component for ChatPanel {
    fn height(&self, _state: &AppState, _viewport_width: u16) -> Constraint {
        Constraint::Min(0)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        self.render_with_scroll_limit(frame, area, state);
    }
}

impl ChatPanel {
    pub fn render_with_scroll_limit(
        &self,
        frame: &mut Frame,
        area: Rect,
        state: &AppState,
    ) -> usize {
        let block = Block::default().borders(Borders::NONE);
        let inner = readable_area(area);
        frame.render_widget(block, area);

        let width = inner.width as usize;

        let lines = transcript_lines(state, width);

        let has_unread = state.chat_scroll > 0 && state.unread_events > 0;
        let visible = inner.height.saturating_sub(u16::from(has_unread)) as usize;
        let total = lines.len();
        let start = if total > visible + state.chat_scroll {
            total - visible - state.chat_scroll
        } else {
            0
        };

        for (row, line) in lines.iter().skip(start).enumerate() {
            if row >= visible {
                break;
            }
            let line_area = Rect {
                x: inner.x,
                y: inner.y + row as u16,
                width: inner.width,
                height: 1,
            };
            frame.render_widget(line.clone(), line_area);
        }

        if has_unread {
            let unread_area = Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(1),
                width: inner.width,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("↓ {} new events · PgDn return to live", state.unread_events),
                    theme::active(),
                ))),
                unread_area,
            );
        }
        total.saturating_sub(visible)
    }
}

impl ChatPanel {
    pub fn scroll_limit(&self, area: Rect, state: &AppState) -> usize {
        let inner = readable_area(area);
        let has_unread = state.chat_scroll > 0 && state.unread_events > 0;
        let visible = inner.height.saturating_sub(u16::from(has_unread)) as usize;
        transcript_lines(state, inner.width as usize)
            .len()
            .saturating_sub(visible)
    }
}

fn transcript_lines(state: &AppState, width: usize) -> Vec<Line<'_>> {
    // Welcome is the first block in a newly started conversation, not a
    // separate page. Once the user submits, turns append below it and the
    // lockup leaves the viewport only through ordinary transcript scroll.
    // Temporary UI state must never decide whether transcript content exists.
    // Every session owns the Welcome prelude. Connection diagnostics, cached
    // session state, and temporary surfaces may append to or cover the
    // transcript, but must never remove its first block.
    let mut lines = build_welcome_lockup(width, state);
    for message in &state.messages {
        push_message_lines(
            message,
            &mut lines,
            width,
            state.tool_details_expanded,
            &state.platform.tool_presentations,
        );
    }
    if !state.streaming_thinking.is_empty() {
        for chunk in display_chunks(&state.streaming_thinking, width) {
            lines.push(Line::from(Span::styled(
                format!("(thinking) {chunk}"),
                theme::thinking_text(),
            )));
        }
    }
    if !state.streaming.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        push_agent_turn(&state.streaming, &mut lines, width);
    }
    lines
}

fn push_message_lines<'a>(
    msg: &'a ChatMessage,
    lines: &mut Vec<Line<'a>>,
    width: usize,
    tool_details_expanded: bool,
    tool_presentations: &[sylvander_protocol::ToolPresentationDescriptor],
) {
    match msg {
        ChatMessage::User(text) => {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            for (index, chunk) in display_chunks(text, width.saturating_sub(2))
                .into_iter()
                .enumerate()
            {
                lines.push(Line::from(vec![
                    Span::styled(if index == 0 { "❯ " } else { "  " }, theme::user_speaker()),
                    Span::styled(chunk, theme::text()),
                ]));
            }
        }
        ChatMessage::QueuedUser(text) => {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            for (index, row) in wrap_words(text, "", "  ".into(), width.saturating_sub(3))
                .into_iter()
                .enumerate()
            {
                let marker = if index == 0 { "↳  " } else { "   " };
                lines.push(Line::from(vec![
                    Span::styled(marker, theme::text_muted()),
                    Span::styled(row, theme::text_dim()),
                ]));
            }
            lines.push(Line::from(Span::styled("   queued", theme::text_muted())));
        }
        ChatMessage::Agent(text) => {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            push_agent_turn(text, lines, width);
        }
        ChatMessage::ToolCall {
            name,
            status,
            input,
        } => {
            let st = theme::tool_status_style(*status);
            lines.push(Line::from(vec![
                Span::styled("⏺ ", st),
                Span::styled(name, st),
            ]));
            for line in input_kv_lines(input, width) {
                lines.push(line);
            }
        }
        ChatMessage::ToolResult { name, output, ok } => {
            let icon = if *ok { "  ✓" } else { "  ✗" };
            let st = if *ok {
                theme::verified()
            } else {
                theme::warning()
            };
            let summary = truncate_display(
                output,
                width.saturating_sub(UnicodeWidthStr::width(name.as_str()) + 6),
            );
            lines.push(Line::from(vec![
                Span::styled(icon, st),
                Span::styled(format!(" {name}: "), theme::text_dim()),
                Span::styled(summary, st),
            ]));
        }
        ChatMessage::ToolStep {
            name,
            started_at_secs,
            children,
        } => {
            let any_error = children.iter().any(|c| c.status == ToolStatus::Error);
            let all_done = children.iter().all(|c| c.status != ToolStatus::Pending);
            let step_style = if any_error {
                theme::warning()
            } else if all_done {
                theme::verified()
            } else {
                theme::active_bold()
            };
            let elapsed = format_elapsed(*started_at_secs);
            lines.push(Line::from(vec![
                Span::styled("⏺ ", step_style),
                Span::styled(name.clone(), step_style),
                Span::styled(format!("{elapsed:>8}"), theme::text_muted()),
            ]));
            for child in children {
                let child_style = theme::tool_status_style(child.status);
                let target = crate::tool_presenter::compact_target_with_presentation(
                    &child.name,
                    &child.input,
                    tool_presentations,
                );
                let inline_diff = if tool_details_expanded || child.status == ToolStatus::Pending {
                    Vec::new()
                } else {
                    crate::tool_presenter::inline_mutation_rows(
                        &child.name,
                        &child.input,
                        width.saturating_sub(5),
                        9,
                    )
                };
                let meta = if tool_details_expanded || !inline_diff.is_empty() {
                    String::new()
                } else {
                    crate::tool_presenter::compact_output_summary(
                        &child.name,
                        child.output.as_deref(),
                        child.status == ToolStatus::Pending,
                        width,
                    )
                };
                lines.push(Line::from(vec![
                    Span::styled("  ⎿  ", theme::guide()),
                    Span::styled(target, child_style),
                    Span::styled(
                        if meta.is_empty() {
                            String::new()
                        } else {
                            format!("  {meta}")
                        },
                        theme::text_muted(),
                    ),
                ]));
                for detail in inline_diff {
                    let style = detail_style(detail.kind, false);
                    lines.push(Line::from(vec![
                        Span::styled("     ", theme::guide()),
                        Span::styled(detail.text, style),
                    ]));
                }
                if tool_details_expanded {
                    for detail in crate::tool_presenter::detail_rows(
                        &child.name,
                        &child.input,
                        child.output.as_deref(),
                        child.is_error.unwrap_or(false),
                        width.saturating_sub(4),
                    ) {
                        let style = detail_style(detail.kind, child.is_error == Some(true));
                        lines.push(Line::from(vec![
                            Span::styled("     ", theme::guide()),
                            Span::styled(detail.text, style),
                        ]));
                    }
                }
            }
        }
        ChatMessage::Thinking(text) => {
            for chunk in display_chunks(text, width) {
                lines.push(Line::from(Span::styled(chunk, theme::thinking_text())));
            }
        }
        ChatMessage::Info(text) => {
            lines.push(Line::from(Span::styled(
                format!("  {text}"),
                theme::text_muted(),
            )));
        }
        ChatMessage::Plan {
            plan_id: _,
            steps,
            current,
        } => {
            lines.push(Line::from(Span::styled("Proposed plan", theme::header())));
            for (i, step) in steps.iter().enumerate() {
                let completed = i < *current;
                let current_step = i == *current;
                let (marker, st) = theme::plan_step_glyph_and_style(completed, current_step);
                lines.push(Line::from(Span::styled(
                    format!("  {marker} {}. {step}", i + 1),
                    st,
                )));
            }
        }
        ChatMessage::TaskList { tasks } => {
            let total = tasks.len();
            let done = tasks
                .iter()
                .filter(|t| t.state == crate::app::TaskState::Done)
                .count();
            let running = tasks
                .iter()
                .filter(|t| t.state == crate::app::TaskState::Running)
                .count();
            lines.push(Line::from(Span::styled(
                format!("▶ tasks {done}/{total} done · {running} running"),
                theme::task_summary_line(),
            )));
            if tool_details_expanded {
                for task in tasks {
                    let (glyph, style) = match task.state {
                        crate::app::TaskState::Running => ("●", theme::warning()),
                        crate::app::TaskState::Done => ("✓", theme::verified()),
                        crate::app::TaskState::Failed => ("✗", theme::danger()),
                        crate::app::TaskState::Cancelled => ("○", theme::text_muted()),
                    };
                    let short_id = task.task_id.chars().take(8).collect::<String>();
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {glyph} "), style),
                        Span::styled(&task.purpose, theme::text()),
                        Span::styled(format!(" · {short_id}"), theme::text_muted()),
                    ]));
                    if !task.detail.is_empty() {
                        for row in
                            wrap_words(&task.detail, "    ", "    ".into(), width.saturating_sub(4))
                        {
                            lines.push(Line::from(Span::styled(row, theme::text_dim())));
                        }
                    }
                }
            }
        }
    }
}

fn detail_style(kind: crate::tool_presenter::DetailKind, is_error: bool) -> ratatui::style::Style {
    match kind {
        crate::tool_presenter::DetailKind::Added => theme::verified(),
        crate::tool_presenter::DetailKind::Removed | crate::tool_presenter::DetailKind::Error => {
            theme::danger()
        }
        crate::tool_presenter::DetailKind::Label => theme::header(),
        crate::tool_presenter::DetailKind::Meta => theme::text_muted(),
        crate::tool_presenter::DetailKind::Normal => {
            if is_error {
                theme::warning()
            } else {
                theme::text_dim()
            }
        }
    }
}

/// Render one meaningful Sylvander turn. The compact presence mark appears
/// once; it is not a miniature replacement for the welcome character.
fn push_agent_turn(text: &str, lines: &mut Vec<Line<'_>>, width: usize) {
    let body_width = width.saturating_sub(3).max(1);
    let body = crate::markdown::render(text, body_width);
    let mut has_marker = false;
    for mut row in body {
        if row.spans.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        let marker = if has_marker { "  " } else { "⏺ " };
        has_marker = true;
        row.spans
            .insert(0, Span::styled(marker, theme::agent_speaker()));
        lines.push(row);
    }
}

fn wrap_words(text: &str, first_prefix: &str, continuation: String, width: usize) -> Vec<String> {
    let width = width.max(1);
    let first_prefix = wrapping_prefix(first_prefix, width);
    let continuation = wrapping_prefix(&continuation, width);
    let mut rows = Vec::new();
    let mut current = first_prefix.clone();
    let mut current_width = UnicodeWidthStr::width(first_prefix.as_str());
    let mut has_word = false;

    for word in text.split_whitespace() {
        let word_width = UnicodeWidthStr::width(word);
        let separator = usize::from(has_word);
        if has_word && current_width.saturating_add(separator + word_width) > width {
            rows.push(std::mem::take(&mut current));
            current.push_str(&continuation);
            current_width = UnicodeWidthStr::width(continuation.as_str());
            has_word = false;
        }

        if !has_word && current_width.saturating_add(word_width) > width {
            for grapheme in word.graphemes(true) {
                let grapheme_width = UnicodeWidthStr::width(grapheme);
                if has_word && current_width.saturating_add(grapheme_width) > width {
                    rows.push(std::mem::take(&mut current));
                    current.push_str(&continuation);
                    current_width = UnicodeWidthStr::width(continuation.as_str());
                }
                current.push_str(grapheme);
                current_width = current_width.saturating_add(grapheme_width);
                has_word = true;
                // A grapheme wider than the available row is emitted intact
                // on its own row. This is the only permitted overflow: a
                // terminal grapheme cluster must never be split.
                if current_width >= width {
                    rows.push(std::mem::take(&mut current));
                    current.push_str(&continuation);
                    current_width = UnicodeWidthStr::width(continuation.as_str());
                    has_word = false;
                }
            }
            continue;
        }

        if has_word {
            current.push(' ');
            current_width = current_width.saturating_add(1);
        }
        current.push_str(word);
        current_width = current_width.saturating_add(word_width);
        has_word = true;
    }

    if has_word || (rows.is_empty() && !first_prefix.is_empty()) {
        rows.push(current);
    }
    rows
}

fn wrapping_prefix(prefix: &str, width: usize) -> String {
    let budget = width.saturating_sub(1);
    let mut output = String::new();
    let mut used = 0usize;
    for grapheme in prefix.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used.saturating_add(grapheme_width) > budget {
            break;
        }
        output.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    output
}

fn readable_area(area: Rect) -> Rect {
    // Anchor the transcript and welcome lockup to a stable left gutter.
    // Centering a capped reading column makes the whole interface drift
    // toward the middle as a terminal is widened or taken fullscreen.
    const LEFT_GUTTER: u16 = 0;
    const RIGHT_GUTTER: u16 = 2;
    const MAX_READING_WIDTH: u16 = 110;

    let available = area
        .width
        .saturating_sub(LEFT_GUTTER.saturating_add(RIGHT_GUTTER));
    Rect {
        x: area.x + LEFT_GUTTER,
        y: area.y,
        width: available.min(MAX_READING_WIDTH),
        height: area.height,
    }
}

/// Split visible transcript text by terminal display cells.
///
/// Grapheme clusters are atomic: combining sequences, emoji modifiers, and
/// ZWJ emoji never split across rows. A cluster wider than a very narrow
/// viewport is emitted intact on one row, which gives deterministic progress
/// without corrupting the text or risking an infinite loop.
fn display_chunks(text: &str, width: usize) -> Vec<&str> {
    let width = width.max(1);
    let mut out = Vec::new();
    for physical_line in text.split('\n') {
        let physical_line = physical_line.strip_suffix('\r').unwrap_or(physical_line);
        out.extend(display_line_chunks(physical_line, width));
    }
    out
}

fn display_line_chunks(text: &str, width: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut current_width = 0usize;

    for (index, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if index > start && current_width.saturating_add(grapheme_width) > width {
            out.push(&text[start..index]);
            start = index;
            current_width = 0;
        }
        current_width = current_width.saturating_add(grapheme_width);
        if current_width >= width {
            let end = index.saturating_add(grapheme.len());
            out.push(&text[start..end]);
            start = end;
            current_width = 0;
        }
    }

    if start < text.len() {
        out.push(&text[start..]);
    } else if out.is_empty() {
        out.push(&text[0..0]);
    }
    out
}

fn truncate_display(text: &str, max_cells: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_cells {
        return text.to_string();
    }
    if max_cells == 0 {
        return String::new();
    }

    let content_cells = max_cells.saturating_sub(UnicodeWidthStr::width("…"));
    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        if used.saturating_add(width) > content_cells {
            break;
        }
        out.push_str(grapheme);
        used = used.saturating_add(width);
    }
    out.push('…');
    out
}

fn render_json_value(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Null => "-".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let inner: Vec<String> = a.iter().take(4).map(render_json_value).collect();
            if a.len() > 4 {
                format!("[{}, …(+{})]", inner.join(", "), a.len() - 4)
            } else {
                format!("[{}]", inner.join(", "))
            }
        }
        Value::Object(o) => {
            let inner: Vec<String> = o
                .iter()
                .take(4)
                .map(|(k, v)| format!("{k}={}", render_json_value(v)))
                .collect();
            if o.len() > 4 {
                format!("{{{}, …(+{})}}", inner.join(", "), o.len() - 4)
            } else {
                format!("{{{}}}", inner.join(", "))
            }
        }
    }
}

fn input_kv_lines(input: &serde_json::Value, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    match input {
        serde_json::Value::Null => {}
        serde_json::Value::Object(map) if map.is_empty() => {}
        serde_json::Value::Object(map) => {
            let label_w = map
                .keys()
                .map(|key| UnicodeWidthStr::width(key.as_str()))
                .max()
                .unwrap_or(0)
                .min(20);
            for (k, v) in map {
                let rendered = render_json_value(v);
                let label_padding = label_w.saturating_sub(UnicodeWidthStr::width(k.as_str()));
                let label = format!("  {k}{}  ", " ".repeat(label_padding));
                let val_str = truncate_display(&rendered, width.saturating_sub(label_w + 6));
                out.push(Line::from(vec![
                    Span::styled(label, theme::kv_label()),
                    Span::styled(val_str, theme::kv_value()),
                ]));
            }
        }
        _ => {
            let rendered = render_json_value(input);
            out.push(Line::from(Span::styled(
                format!(
                    "  → {}",
                    truncate_display(&rendered, width.saturating_sub(6))
                ),
                theme::text(),
            )));
        }
    }
    out
}

fn format_elapsed(started_at_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(started_at_secs, |d| d.as_secs());
    let s = now.saturating_sub(started_at_secs);
    if s < 60 {
        format!("{s:02}s")
    } else if s < 3600 {
        format!("{:02}:{:02}", s / 60, s % 60)
    } else {
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

fn build_welcome_lockup(width: usize, state: &AppState) -> Vec<Line<'static>> {
    let workspace_label = theme::compact_workspace(&state.metadata.workspace, 34);
    let model = state.metadata.model_label();
    let branch = state.metadata.branch.clone();
    let session = state.session_id.as_deref().map_or_else(
        || "new".into(),
        |id| id.chars().take(12).collect::<String>(),
    );

    let info = vec![
        vec![Span::styled("Sylvander", theme::brand_wordmark())],
        vec![Span::styled("agent workspace", theme::brand_tagline())],
        welcome_meta("model", model),
        welcome_meta("workspace", workspace_label),
        vec![
            Span::styled(format!("{:<10}", "branch"), theme::text_muted()),
            Span::styled(branch, theme::text()),
            Span::styled(" · session ", theme::text_muted()),
            Span::styled(session, theme::text()),
        ],
        vec![],
        vec![Span::styled(
            "What should we work through?",
            theme::text_dim(),
        )],
        vec![],
    ];

    let mut lines = Vec::new();
    if width >= WELCOME_HORIZONTAL_MIN_WIDTH {
        for (row, right) in TERMINAL_SEED_CRAB.into_iter().zip(info) {
            let mut spans = seed_crab_spans(row);
            let row_width = UnicodeWidthStr::width(row);
            spans.push(Span::raw(" ".repeat(
                SEED_CRAB_CELL_WIDTH.saturating_sub(row_width) + WELCOME_GAP,
            )));
            spans.extend(right);
            lines.push(Line::from(spans));
        }
    } else {
        for row in TERMINAL_SEED_CRAB {
            lines.push(Line::from(seed_crab_spans(row)));
        }
        lines.push(Line::from(""));
        lines.extend(info.into_iter().map(Line::from));
    }
    lines.push(Line::from(""));
    lines
}

const WELCOME_HORIZONTAL_MIN_WIDTH: usize = 77;
const SEED_CRAB_CELL_WIDTH: usize = 31;
const WELCOME_GAP: usize = 2;
const SEED_CRAB_COLOR_SPLIT: usize = 16;

// One canonical terminal character. The complete silhouette includes the
// sprout, both claws, the full shell, both eyes, and the lower walking legs.
// Narrow viewports reflow this same asset; they never substitute another logo.
const TERMINAL_SEED_CRAB: [&str; 8] = [
    "              ⢀⣤⣴⡆",
    "            ⢀⣚⣿⣿⣿⣿⢓⣀",
    "         ⢀⣴⣾⢿⣳⡿⠛⠙⢿⣾⣿⣷⣦",
    "  ⢀ ⣴⠆  ⣰⣿⣿⣵⠟⠉    ⠙⠻⣿⠿⣾⣦",
    " ⠈⠛⠃⡁⡀  ⣿⡻⢾⡇ ⣶⣶  ⣶⣆ ⢻⣥⣿⣿⡄",
    " ⢀⣤⣶⣿⡣⣤⢀⢽⠿⣮⢷⣄⣉⠁  ⠉⣁⣤⡎⣵⠟⣭⣁⡔⢿⣄⡀",
    " ⣾⣿⠛⢹⡇⠈⢨⣶⡶⠎⢛⡿⣿⣳⡆⣶⣿⡿⣟⣋⢷⣶⣮⡅⢸⡜⢿⣿⡄",
    " ⠈⠻⠁   ⠘⠞⠁ ⠒⠁ ⠉⠁⠉⠉ ⠉⠑ ⠈⣿⠇ ⠁⠈⠿⠁",
];

fn seed_crab_spans(row: &str) -> Vec<Span<'static>> {
    let (warm, violet) = split_chars(row, SEED_CRAB_COLOR_SPLIT);
    vec![
        Span::styled(warm, theme::brand_warm()),
        Span::styled(violet, theme::brand_violet()),
    ]
}

fn welcome_meta(label: &'static str, value: String) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label:<10}"), theme::text_muted()),
        Span::styled(value, theme::text()),
    ]
}

fn split_chars(text: &str, index: usize) -> (String, String) {
    let mut left = String::new();
    let mut right = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i < index {
            left.push(ch);
        } else {
            right.push(ch);
        }
    }
    (left, right)
}

#[cfg(test)]
#[path = "../../tests/unit/panel_chat.rs"]
mod tests;
