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

use crate::app::{AppState, ChatMessage, ToolStatus};
use crate::component::Component;
use crate::theme;

pub struct ChatPanel;

impl Component for ChatPanel {
    fn height(&self, _state: &AppState, _viewport_width: u16) -> Constraint {
        Constraint::Min(0)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Block::default().borders(Borders::NONE);
        let inner = readable_area(area);
        frame.render_widget(block, area);

        let width = inner.width as usize;

        // Welcome is the first block in a newly started conversation, not a
        // separate page. Once the user submits, turns append below it and the
        // lockup leaves the viewport only through ordinary transcript scroll.
        let show_welcome = state.welcomed
            || (state.messages.is_empty() && state.sessions.is_empty() && state.modals.is_empty());
        let mut lines: Vec<Line> = if show_welcome {
            build_welcome_lockup(width, state).unwrap_or_default()
        } else {
            Vec::new()
        };
        for msg in &state.messages {
            push_message_lines(msg, &mut lines, width, state.tool_details_expanded);
        }

        if !state.streaming_thinking.is_empty() {
            for chunk in char_chunks(&state.streaming_thinking, width) {
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
    }
}

fn push_message_lines<'a>(
    msg: &'a ChatMessage,
    lines: &mut Vec<Line<'a>>,
    width: usize,
    tool_details_expanded: bool,
) {
    match msg {
        ChatMessage::User(text) => {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            for (index, chunk) in char_chunks(text, width.saturating_sub(2))
                .into_iter()
                .enumerate()
            {
                lines.push(Line::from(vec![
                    Span::styled(if index == 0 { "› " } else { "  " }, theme::user_speaker()),
                    Span::styled(chunk.to_string(), theme::text()),
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
            let (icon, st) = theme::tool_status_glyph_and_style(*status);
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), st),
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
            let summary = truncate(output, width.saturating_sub(name.len() + 6));
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
            let (step_glyph, step_style) = if any_error {
                (
                    theme::tool_status_glyph(ToolStatus::Error),
                    theme::warning(),
                )
            } else if all_done {
                (
                    theme::tool_status_glyph(ToolStatus::Done),
                    theme::verified(),
                )
            } else {
                (
                    theme::tool_status_glyph(ToolStatus::Pending),
                    theme::active_bold(),
                )
            };
            let elapsed = format_elapsed(*started_at_secs);
            lines.push(Line::from(vec![
                Span::styled(format!("{step_glyph} "), step_style),
                Span::styled(name.clone(), step_style),
                Span::styled(format!("{elapsed:>8}"), theme::text_muted()),
            ]));
            for child in children {
                let (g, gstyle) = theme::tool_status_glyph_and_style(child.status);
                let target = crate::tool_presenter::compact_target(&child.name, &child.input);
                let meta = if tool_details_expanded {
                    String::new()
                } else {
                    child
                        .output
                        .as_deref()
                        .map(|o| summarize_output(o, width))
                        .unwrap_or_default()
                };
                lines.push(Line::from(vec![
                    Span::styled("│ ", theme::guide()),
                    Span::styled(format!("{g} "), gstyle),
                    Span::styled(target, theme::text_dim()),
                    Span::styled(
                        if meta.is_empty() {
                            String::new()
                        } else {
                            format!("  {meta}")
                        },
                        theme::text_muted(),
                    ),
                ]));
                if tool_details_expanded {
                    for detail in crate::tool_presenter::detail_rows(
                        &child.name,
                        &child.input,
                        child.output.as_deref(),
                        child.is_error.unwrap_or(false),
                        width.saturating_sub(4),
                    ) {
                        let style = match detail.kind {
                            crate::tool_presenter::DetailKind::Added => theme::verified(),
                            crate::tool_presenter::DetailKind::Removed
                            | crate::tool_presenter::DetailKind::Error => theme::danger(),
                            crate::tool_presenter::DetailKind::Label => theme::header(),
                            crate::tool_presenter::DetailKind::Meta => theme::text_muted(),
                            crate::tool_presenter::DetailKind::Normal => {
                                if child.is_error == Some(true) {
                                    theme::warning()
                                } else {
                                    theme::text_dim()
                                }
                            }
                        };
                        lines.push(Line::from(vec![
                            Span::styled("│   ", theme::guide()),
                            Span::styled(detail.text, style),
                        ]));
                    }
                }
            }
        }
        ChatMessage::Thinking(text) => {
            for chunk in char_chunks(text, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    theme::thinking_text(),
                )));
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

/// Render one meaningful Sylvander turn. The compact presence mark appears
/// once; it is not a miniature replacement for the welcome character.
fn push_agent_turn(text: &str, lines: &mut Vec<Line<'_>>, width: usize) {
    let body_width = width.saturating_sub(3).max(1);
    let body = crate::markdown::render(text, body_width);
    let mut marked = false;
    for mut row in body {
        if row.spans.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        let marker = if marked { "   " } else { "◆  " };
        marked = true;
        row.spans
            .insert(0, Span::styled(marker, theme::agent_speaker()));
        lines.push(row);
    }
}

fn wrap_words(text: &str, first_prefix: &str, continuation: String, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut current = first_prefix.to_string();
    let mut has_word = false;

    for word in text.split_whitespace() {
        let separator = usize::from(has_word);
        if has_word && current.chars().count() + separator + word.chars().count() > width {
            rows.push(current);
            current = continuation.clone();
            has_word = false;
        }
        if has_word {
            current.push(' ');
        }
        current.push_str(word);
        has_word = true;
    }

    if has_word || !first_prefix.is_empty() {
        rows.push(current);
    }
    rows
}

fn readable_area(area: Rect) -> Rect {
    // Anchor the transcript and welcome lockup to a stable left gutter.
    // Centering a capped reading column makes the whole interface drift
    // toward the middle as a terminal is widened or taken fullscreen.
    const LEFT_GUTTER: u16 = 2;
    const RIGHT_GUTTER: u16 = 2;
    const MAX_READING_WIDTH: u16 = 110;

    let available = area
        .width
        .saturating_sub(LEFT_GUTTER.saturating_add(RIGHT_GUTTER));
    Rect {
        x: area.x + LEFT_GUTTER.min(area.width),
        y: area.y,
        width: available.min(MAX_READING_WIDTH),
        height: area.height,
    }
}

fn char_chunks(s: &str, width: usize) -> Vec<&str> {
    if width == 0 {
        return vec![s];
    }
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in s.char_indices() {
        if i - start >= width {
            out.push(&s[start..i]);
            start = i;
        }
    }
    out.push(&s[start..]);
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn truncate_first(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
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
                .map(|k| k.chars().count())
                .max()
                .unwrap_or(0)
                .min(20);
            for (k, v) in map {
                let rendered = render_json_value(v);
                let label = format!("  {:<w$}  ", k, w = label_w);
                let val_str = truncate(&rendered, width.saturating_sub(label_w + 6));
                out.push(Line::from(vec![
                    Span::styled(label, theme::kv_label()),
                    Span::styled(val_str, theme::kv_value()),
                ]));
            }
        }
        _ => {
            let rendered = render_json_value(input);
            out.push(Line::from(Span::styled(
                format!("  → {}", truncate(&rendered, width.saturating_sub(6))),
                theme::text(),
            )));
        }
    }
    out
}

fn summarize_output(output: &str, width: usize) -> String {
    let line = output
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        return String::new();
    }
    truncate_first(line, width.saturating_sub(20).min(80))
}

fn format_elapsed(started_at_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(started_at_secs);
    let s = now.saturating_sub(started_at_secs);
    if s < 60 {
        format!("{:02}s", s)
    } else if s < 3600 {
        format!("{:02}:{:02}", s / 60, s % 60)
    } else {
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

fn build_welcome_lockup(width: usize, state: &AppState) -> Option<Vec<Line<'static>>> {
    if width < 24 {
        return None;
    }
    let workspace_label = theme::compact_workspace(&state.metadata.workspace, 34);
    let model = state.metadata.model.clone();
    let branch = state.metadata.branch.clone();
    let session = state
        .session_id
        .as_deref()
        .map(|id| id.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "new".into());

    let info = vec![
        vec![],
        vec![Span::styled("Sylvander", theme::brand_wordmark())],
        vec![Span::styled("agent workspace", theme::brand_tagline())],
        vec![],
        welcome_meta("model", model),
        welcome_meta("workspace", workspace_label),
        welcome_meta("branch", branch),
        welcome_meta("session", session),
        vec![],
        vec![Span::styled(
            "What should we work through?",
            theme::text_dim(),
        )],
        vec![],
    ];

    let mut lines = Vec::new();
    if width >= WELCOME_HORIZONTAL_MIN_WIDTH {
        for (row, right) in TERMINAL_LARGE_SEED_CRAB.into_iter().zip(info) {
            let mut spans = seed_crab_spans(row);
            let row_width = row.chars().count();
            spans.push(Span::raw(" ".repeat(
                SEED_CRAB_CELL_WIDTH.saturating_sub(row_width) + WELCOME_GAP,
            )));
            spans.extend(right);
            lines.push(Line::from(spans));
        }
    } else {
        for row in TERMINAL_LARGE_SEED_CRAB {
            lines.push(Line::from(seed_crab_spans(row)));
        }
        lines.push(Line::from(""));
        lines.extend(info.into_iter().map(Line::from));
    }
    Some(lines)
}

const WELCOME_HORIZONTAL_MIN_WIDTH: usize = 88;
const SEED_CRAB_CELL_WIDTH: usize = 44;
const WELCOME_GAP: usize = 4;
const SEED_CRAB_COLOR_SPLIT: usize = 22;

// One canonical terminal character. The complete silhouette includes the
// sprout, both claws, the full shell, both eyes, and the lower walking legs.
// Narrow viewports reflow this same asset; they never substitute another logo.
const TERMINAL_LARGE_SEED_CRAB: [&str; 11] = [
    "                     ⢀⣠⢠⡖",
    "                  ⢀⣠⣶⣿⡇⣿⣷⣤⣀",
    "                ⢀⣠⣤⠄⣿⣿⡇⣿⣿⡇⣤⣤⡀",
    "             ⢀⣴⣶⣿⠿⣋⣾⡿⠛⠁⠙⢿⣷⣜⢿⣿⣾⣦⠄",
    "      ⣠⣤    ⣠⣿⣿⠟⣡⡾⠛⠉     ⠉⠻⢿⣬⡻⣿⡌⣧⡀",
    "  ⢠⣴⣄⠸⠛⠉   ⢰⣿⣿⡃⣿⡟  ⣀⡀   ⢀⡀  ⢹⣷⠄⣾⣿⣿",
    "   ⠈⠁⠠⡀⣀   ⢰⣦⡙⠇⣿⠁ ⢸⣿⣿   ⣿⣿⡆  ⣿⡦⢏⣵⣶⡆",
    "   ⣠⣔⣚⠿⠏⣠⣄ ⢈⣙⢿⣦⡩⣷⣀ ⠉⠁   ⠈⠉ ⣀⣴⠌⣴⡿⢋⣤⠁⢀⠲⠿⢤⡀",
    " ⢀⣼⣿⣿⠟⠼⣷ ⠏⠘⢂⡡⢤⣌⠳⠮⣝⡻⢷⣤⣄ ⣠⣴⣾⠿⣛⣥⠞⣫⡤⢌⡑⠛⠉⣶⠸⣿⣿⣷",
    " ⠘⢿⡟⡄ ⠸⠃  ⢠⡛⠿⡆⠉ ⡐⠬⠙⠳⠌⣟ ⣿⡿⠽⠋⢥⡶⢂⠙⠱⠿⢟⡃ ⠙⡇⠈⢹⣿⡇",
    "   ⠙       ⠻⠁   ⠉            ⠋  ⠈⡿⠃     ⠟",
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
mod tests {
    use super::*;
    use crate::app::{AppState, ChatMessage, ToolStatus};
    use crate::component::Component;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn terminal(w: u16, h: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(w, h)).expect("terminal")
    }

    fn seeded() -> AppState {
        let mut s = AppState::new();
        s.apply(crate::event::DomainEvent::Connected);
        s.messages.push(ChatMessage::User("Hi".into()));
        s.apply(crate::event::DomainEvent::TextChunk {
            delta: "world".into(),
        });
        s.apply(crate::event::DomainEvent::AgentDone {
            final_text: "world".into(),
        });
        s
    }

    #[test]
    fn user_speaker_uses_dim_color() {
        let s = seeded();
        let mut t = terminal(60, 12);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 12), &s))
            .unwrap();
        let cell = t
            .backend()
            .buffer()
            .content()
            .iter()
            .find(|cell| cell.symbol() == "›")
            .expect("user speaker cell");
        assert_eq!(cell.fg, crate::theme::TEXT_DIM);
    }

    #[test]
    fn agent_body_uses_primary_text() {
        let s = seeded();
        let mut t = terminal(60, 12);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 12), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let mut found = false;
        for y in 0..12 {
            for x in 0..60 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.fg == crate::theme::TEXT {
                        found = true;
                        break;
                    }
                }
            }
            if found {
                break;
            }
        }
        assert!(found, "expected a primary-text cell");
    }

    #[test]
    fn welcome_lockup_renders_crab_at_first_launch() {
        let s = AppState::new();
        let mut t = terminal(120, 36);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 120, 36), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let mut found_warm = false;
        let mut found_violet = false;
        for y in 0..36 {
            for x in 0..120 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.fg == crate::theme::BRAND_WARM && c.symbol() != " " {
                        found_warm = true;
                    }
                    if c.fg == crate::theme::BRAND_VIOLET && c.symbol() != " " {
                        found_violet = true;
                    }
                }
            }
        }
        assert!(found_warm, "expected warm half of Terminal Large Seed-Crab");
        assert!(
            found_violet,
            "expected violet half of Terminal Large Seed-Crab"
        );
    }

    #[test]
    fn welcome_uses_complete_canonical_character_and_horizontal_info() {
        let state = AppState::new();
        assert_eq!(TERMINAL_LARGE_SEED_CRAB.len(), 11);
        assert!(
            TERMINAL_LARGE_SEED_CRAB[8..]
                .iter()
                .all(|row| !row.trim().is_empty()),
            "lower claws and walking legs must remain in the canonical asset"
        );
        assert!(
            TERMINAL_LARGE_SEED_CRAB
                .iter()
                .all(|row| row.chars().count() <= SEED_CRAB_CELL_WIDTH),
            "canonical character must stay inside its reserved column"
        );

        let wide = build_welcome_lockup(110, &state).expect("wide welcome");
        assert_eq!(wide.len(), TERMINAL_LARGE_SEED_CRAB.len());
        assert!(
            wide.iter()
                .any(|line| line.to_string().contains("Sylvander")),
            "brand information must render beside the character"
        );

        let narrow = build_welcome_lockup(70, &state).expect("narrow welcome");
        assert!(
            narrow.len() > TERMINAL_LARGE_SEED_CRAB.len(),
            "narrow welcome reflows information below the same character"
        );
        for (rendered, canonical) in narrow.iter().zip(TERMINAL_LARGE_SEED_CRAB.iter()) {
            assert_eq!(rendered.to_string(), *canonical);
        }
    }

    #[test]
    fn readable_column_stays_left_anchored_when_terminal_goes_fullscreen() {
        let normal = readable_area(Rect::new(0, 0, 120, 36));
        let fullscreen = readable_area(Rect::new(0, 0, 240, 60));

        assert_eq!(normal.x, 2);
        assert_eq!(fullscreen.x, normal.x);
        assert_eq!(normal.width, 110);
        assert_eq!(fullscreen.width, normal.width);
    }

    #[test]
    fn welcome_prelude_remains_when_first_turn_is_appended() {
        let mut s = AppState::new();
        s.welcomed = true;
        s.messages.push(ChatMessage::User("x".into()));
        let mut t = terminal(120, 36);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 120, 36), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let mut found_brand = false;
        let mut found_turn = false;
        for y in 0..36 {
            for x in 0..120 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.fg == crate::theme::BRAND_WARM && c.symbol() != " " {
                        found_brand = true;
                    }
                    if c.symbol() == "›" {
                        found_turn = true;
                    }
                }
            }
        }
        assert!(found_brand, "Welcome must remain as the transcript prelude");
        assert!(found_turn, "submitted turn must append below Welcome");
    }

    #[test]
    fn wrapped_user_turn_marks_only_the_first_visual_row() {
        let mut lines = Vec::new();
        let message = ChatMessage::User("a user message that wraps across rows".into());
        push_message_lines(&message, &mut lines, 14, false);
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(rendered.matches('›').count(), 1);
        assert!(rendered.lines().skip(1).all(|line| line.starts_with("  ")));
    }

    #[test]
    fn agent_turn_is_clean_word_wrapped_content_with_one_presence_mark() {
        let mut lines = Vec::new();
        push_agent_turn(
            "I have tools:1. **`ask_user`** — Ask for missing information.2. **`Read`** — Read a workspace file.",
            &mut lines,
            42,
        );
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(rendered.matches('◆').count(), 1);
        assert!(!rendered.contains("/\\"));
        assert!(!rendered.contains("(••)"));
        assert!(!rendered.contains("<__>"));
        assert!(!rendered.contains("**"));
        assert!(!rendered.contains('`'));
        assert!(rendered.contains("\n   1. ask_user"));
        assert!(rendered.contains("\n   2. Read"));
        assert!(lines.iter().all(|line| line.width() <= 42));
    }

    #[test]
    fn streaming_and_settled_agent_turn_keep_the_same_vertical_origin() {
        let mut state = AppState::new();
        state.welcomed = true;
        state.messages.push(ChatMessage::User("hello".into()));
        state.streaming = "A stable reply".into();

        let marker_y = |state: &AppState| {
            let mut terminal = terminal(120, 36);
            terminal
                .draw(|frame| ChatPanel.render(frame, Rect::new(0, 0, 120, 36), state))
                .expect("render chat");
            let buffer = terminal.backend().buffer();
            (0..36)
                .find(|&y| (0..120).any(|x| buffer.cell((x, y)).is_some_and(|c| c.symbol() == "◆")))
                .expect("agent presence mark")
        };

        let streaming_y = marker_y(&state);
        state.apply(crate::event::DomainEvent::AgentDone {
            final_text: "A stable reply".into(),
        });
        let settled_y = marker_y(&state);
        assert_eq!(streaming_y, settled_y);
    }

    #[test]
    fn input_kv_lines_skips_null_and_empty_object() {
        assert!(input_kv_lines(&serde_json::Value::Null, 80).is_empty());
        assert!(input_kv_lines(&serde_json::json!({}), 80).is_empty());
    }

    #[test]
    fn input_kv_lines_emits_pair_per_object_key() {
        // serde_json::Map defaults to BTreeMap, so keys come back in
        // alphabetical order — assert set membership instead of ordering.
        let lines = input_kv_lines(&serde_json::json!({"path": "/tmp", "mode": "r"}), 60);
        assert_eq!(lines.len(), 2);
        let labels: Vec<String> = lines
            .iter()
            .map(|l| l.spans[0].content.to_string())
            .collect();
        assert!(
            labels.iter().any(|l| l.contains("path")),
            "expected `path` label, got: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("mode")),
            "expected `mode` label, got: {labels:?}"
        );
    }

    #[test]
    fn plan_step_three_glyphs() {
        let (gd, _) = theme::plan_step_glyph_and_style(true, false);
        let (gc, _) = theme::plan_step_glyph_and_style(false, true);
        let (gp, _) = theme::plan_step_glyph_and_style(false, false);
        assert_eq!(gd, "✓");
        assert_eq!(gc, "●");
        assert_eq!(gp, "○");
    }

    #[test]
    fn tool_status_styles_three_distinct_fg() {
        let (_, sp) = theme::tool_status_glyph_and_style(ToolStatus::Pending);
        let (_, sd) = theme::tool_status_glyph_and_style(ToolStatus::Done);
        let (_, se) = theme::tool_status_glyph_and_style(ToolStatus::Error);
        assert_ne!(sp.fg, sd.fg);
        assert_ne!(sp.fg, se.fg);
        assert_ne!(sd.fg, se.fg);
    }

    #[test]
    fn render_order_user_then_agent_then_toolstep() {
        // Contract: messages render in insertion order. User at the
        // top, then the agent's reply, then a grouped tool step.
        let mut s = AppState::new();
        s.apply(crate::event::DomainEvent::Connected);
        s.messages.push(ChatMessage::User("Hi".into()));
        s.apply(crate::event::DomainEvent::TextChunk {
            delta: "Hello back".into(),
        });
        s.apply(crate::event::DomainEvent::AgentDone {
            final_text: "Hello back".into(),
        });
        s.apply(crate::event::DomainEvent::ToolStarted {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        });
        s.apply(crate::event::DomainEvent::ToolFinished {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            output: "a.rs".into(),
            is_error: false,
        });
        let mut t = terminal(60, 20);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 20), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        // Find the first row of each message kind. User turns use the
        // immersive `›` marker rather than a repeated "You:" heading.
        // The agent body is the row above any tool
        // step (which renders `● Run ...` or `✓ Run ...`).
        let mut you_y = None;
        for y in 0..20 {
            for x in 0..60 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.symbol() == "›" {
                        you_y = Some(y);
                        break;
                    }
                }
            }
            if you_y.is_some() {
                break;
            }
        }
        let you_y = you_y.expect("expected to find the user-turn marker");
        // Tool step row has `●` or `✓` glyph in the step header.
        let mut toolstep_y = None;
        for y in 0..20 {
            for x in 0..60 {
                if let Some(c) = buf.cell((x, y)) {
                    let sym = c.symbol();
                    if sym == "\u{25cf}" || sym == "\u{2713}" {
                        // Step glyphs are tool-step step-header characters.
                        toolstep_y = Some(y);
                        break;
                    }
                }
            }
            if toolstep_y.is_some() {
                break;
            }
        }
        let toolstep_y = toolstep_y.expect("expected to find tool step glyph");
        // Order: user (y=0 by convention but at least smallest) precedes
        // toolstep. They are guaranteed by the way push_message_lines
        // walks the messages vec.
        assert!(
            you_y < toolstep_y,
            "user row {you_y} must precede toolstep {toolstep_y}"
        );
    }
}
