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

        // If we're in AskPending mode with an empty input, render a hint.
        if matches!(state.mode, AppMode::AskPending) && state.input.buffer.is_empty() {
            // The popup will overlay this, so nothing extra needed here.
        }
    }
}

fn push_message_lines(msg: &ChatMessage, lines: &mut Vec<Line>, width: usize) {
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
                ToolStatus::Pending => ("[ ]", Color::Yellow),
                ToolStatus::Done => ("[+]", Color::Green),
                ToolStatus::Error => ("[x]", Color::Red),
            };
            lines.push(Line::from(Span::styled(
                format!("{icon} {name}({input})"),
                Style::default().fg(color),
            )));
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