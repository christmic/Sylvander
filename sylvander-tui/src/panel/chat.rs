//! Chat panel — the main conversation area. Uses virtual scroll: only
//! renders the lines that fit on screen, bottom-aligned.

use ratatui::{
    layout::{Constraint, Rect},
    prelude::Stylize,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
    Frame,
};

use crate::app::{AppMode, AppState, ChatMessage, ToolStatus};
use crate::component::Component;

pub struct ChatPanel;

impl Component for ChatPanel {
    fn height(&self) -> Constraint {
        Constraint::Min(0)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Block::default().borders(Borders::NONE);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Build all lines first (cheap, just string formatting).
        let width = inner.width as usize;
        let mut lines: Vec<Line> = Vec::new();

        for msg in &state.messages {
            push_message_lines(msg, &mut lines, width);
        }

        // Streaming thinking (italic, gray) — render above streaming text.
        if !state.streaming_thinking.is_empty() {
            for chunk in char_chunks(&state.streaming_thinking, width) {
                lines.push(Line::from(Span::styled(
                    format!("(thinking) {chunk}"),
                    Style::default().fg(Color::DarkGray).italic(),
                )));
            }
        }

        // Streaming text (white).
        if !state.streaming.is_empty() {
            for chunk in char_chunks(&state.streaming, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        }

        // Virtual scroll: show last N lines that fit, plus user scroll offset.
        let visible = inner.height as usize;
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

        // If we're in AskPending mode with an empty composer, render a hint.
        // Currently the AskUser popup overlays this region, so we skip.
        if matches!(state.mode, AppMode::AskPending) && state.composer.is_empty() {
            // placeholder for future inline hint
        }
    }
}

fn push_message_lines<'a>(msg: &'a ChatMessage, lines: &mut Vec<Line<'a>>, width: usize) {
    match msg {
        ChatMessage::User(text) => {
            for chunk in char_chunks(text, width.saturating_sub(5)) {
                lines.push(Line::from(Span::styled(
                    format!("You: {chunk}"),
                    Style::default().fg(Color::Cyan),
                )));
            }
        }
        ChatMessage::Agent(text) => {
            for chunk in char_chunks(text, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        }
        ChatMessage::ToolCall {
            name,
            status,
            input,
        } => {
            let (icon, color) = match status {
                ToolStatus::Pending => ("◐", Color::Yellow),
                ToolStatus::Done => ("✓", Color::Green),
                ToolStatus::Error => ("✗", Color::Red),
            };
            // Header: `◐ bash` (single line).
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color).bold()),
                Span::styled(name, Style::default().fg(color).bold()),
            ]));
            // Body: structured key/value rows for non-empty input.
            match input {
                serde_json::Value::Null => {
                    // No payload to show (e.g. wire-shape ToolCall without
                    // input echo). Already covered by the header.
                }
                serde_json::Value::Object(map) => {
                    if map.is_empty() {
                        // Same — keep the line clean: don't dump "{}".
                    } else {
                        let label_w = map
                            .keys()
                            .map(|k| k.chars().count())
                            .max()
                            .unwrap_or(0)
                            .min(20);
                        for (k, v) in map {
                            let rendered = render_json_value(v);
                            let label = format!("  {:<w$}  ", k, w = label_w);
                            // Truncate individual values to fit width.
                            let val_str =
                                truncate(&rendered, width.saturating_sub(label_w + 6));
                            lines.push(Line::from(vec![
                                Span::styled(
                                    label,
                                    Style::default().fg(Color::DarkGray),
                                ),
                                Span::styled(val_str, Style::default().fg(Color::White)),
                            ]));
                        }
                    }
                }
                _ => {
                    // Non-object inputs (rare for our tools) — show compact.
                    let rendered = render_json_value(input);
                    lines.push(Line::from(Span::styled(
                        format!("  → {trunc}", trunc = truncate(&rendered, width.saturating_sub(6))),
                        Style::default().fg(Color::White),
                    )));
                }
            }
        }
        ChatMessage::ToolResult {
            name,
            output,
            ok,
        } => {
            let icon = if *ok { "  +" } else { "  x" };
            let color = if *ok { Color::Green } else { Color::Red };
            let summary = truncate(output, width.saturating_sub(name.len() + 6));
            lines.push(Line::from(Span::styled(
                format!("{icon} {name}: {summary}"),
                Style::default().fg(color),
            )));
        }
        ChatMessage::Thinking(text) => {
            for chunk in char_chunks(text, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(Color::DarkGray).italic(),
                )));
            }
        }
        ChatMessage::Info(text) => {
            lines.push(Line::from(Span::styled(
                format!("  {text}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        ChatMessage::Plan {
            plan_id: _,
            steps,
            current,
        } => {
            // UX §9: plan renders as a numbered list with ✓ ● ○ markers.
            // Header says "Proposed plan" so the user can scan the
            // transcript and find it later.
            lines.push(Line::from(Span::styled(
                "Proposed plan",
                Style::default().fg(Color::Yellow).bold(),
            )));
            for (i, step) in steps.iter().enumerate() {
                let (marker, color) = if i < *current {
                    ("✓", Color::Green)
                } else if i == *current {
                    ("●", Color::Cyan)
                } else {
                    ("○", Color::DarkGray)
                };
                let label = format!("{marker} {}. {}", i + 1, step);
                lines.push(Line::from(Span::styled(
                    format!("  {label}"),
                    Style::default().fg(color),
                )));
            }
        }
        ChatMessage::TaskList { tasks } => {
            // Compact one-liner per UX §11: `▶ n/m done · <owner> <purpose>`.
            // Drops per-task lines (the agent loop verbose form) — too noisy
            // for the immersive transcript.
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
                Style::default().fg(Color::Magenta),
            )));
        }
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

/// Render a `serde_json::Value` as a compact single-line string.
/// Strings are shown unquoted (their content only), numbers / bools as
/// themselves, null as `-`, arrays collapsed to `[…]`, objects to
/// `{k=v, k2=v2}`.
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