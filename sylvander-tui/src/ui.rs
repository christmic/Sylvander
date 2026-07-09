//! UI rendering functions.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{AppMode, AppState, ChatMessage, ToolStatus};

// ===========================================================================
// Layout
// ===========================================================================

pub fn layout(area: Rect) -> [Rect; 4] {
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // status bar
            Constraint::Min(0),     // chat
            Constraint::Length(3),  // input
            Constraint::Length(1),  // help
        ])
        .split(area);
    [main[0], main[1], main[2], main[3]]
}

// ===========================================================================
// Main render
// ===========================================================================

pub fn ui(frame: &mut Frame, state: &AppState) {
    let area = frame.area();
    let [status_area, chat_area, input_area, help_area] = layout(area);

    render_status(frame, status_area, state);
    render_chat(frame, chat_area, state);
    render_input(frame, input_area, state);
    render_help(frame, help_area, state);

    // Popups
    match &state.mode {
        AppMode::Approval { tools, current, .. } => {
            render_approval_popup(frame, area, tools, *current);
        }
        AppMode::AskUser { question, options, answer, .. } => {
            render_ask_popup(frame, area, question, options, answer);
        }
        _ => {}
    }
}

// ===========================================================================
// Status bar
// ===========================================================================

fn render_status(frame: &mut Frame, area: Rect, state: &AppState) {
    let connected = if state.connected {
        Span::styled("Connected", Color::Green)
    } else {
        Span::styled("Disconnected", Color::Red)
    };
    let model = Span::styled("deepseek-v4-flash", Color::Cyan);
    let line = Line::from(vec![
        Span::raw("Sylvander · "),
        model,
        Span::raw(" · "),
        connected,
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

// ===========================================================================
// Chat (virtual scroll)
// ===========================================================================

fn render_chat(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::NONE);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build lines from messages + streaming buffer
    let mut lines: Vec<Line> = Vec::new();

    for msg in &state.messages {
        push_message_lines(msg, &mut lines, inner.width as usize);
    }

    // Streaming text (if any)
    if !state.streaming.is_empty() {
        let spans = vec![Span::styled(&state.streaming, Style::default().fg(Color::White))];
        for chunk in spans[0].content.chars().collect::<Vec<_>>().chunks(inner.width as usize) {
            lines.push(Line::from(Span::styled(
                chunk.iter().collect::<String>(),
                Style::default().fg(Color::White),
            )));
        }
    }

    // Virtual scroll: show last N lines that fit
    let visible = inner.height as usize;
    let start = if lines.len() > visible {
        lines.len() - visible
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
}

fn push_message_lines(msg: &ChatMessage, lines: &mut Vec<Line>, width: usize) {
    match msg {
        ChatMessage::User(text) => {
            for chunk in text.chars().collect::<Vec<_>>().chunks(width) {
                lines.push(Line::from(Span::styled(
                    format!("You: {}", chunk.iter().collect::<String>()),
                    Style::default().fg(Color::Cyan),
                )));
            }
        }
        ChatMessage::Agent(text) => {
            for chunk in text.chars().collect::<Vec<_>>().chunks(width) {
                lines.push(Line::from(Span::styled(
                    chunk.iter().collect::<String>(),
                    Style::default().fg(Color::White),
                )));
            }
        }
        ChatMessage::ToolCall { name, status } => {
            let (icon, color) = match status {
                ToolStatus::Pending => ("[ ]", Color::Yellow),
                ToolStatus::Done => ("[✓]", Color::Green),
                ToolStatus::Error => ("[✗]", Color::Red),
            };
            lines.push(Line::from(Span::styled(
                format!("{icon} {name}"),
                Style::default().fg(color),
            )));
        }
        ChatMessage::ToolResult { name, output, ok } => {
            let icon = if *ok { "  ✓" } else { "  ✗" };
            let color = if *ok { Color::Green } else { Color::Red };
            let summary: String = output.chars().take(width.saturating_sub(6)).collect();
            lines.push(Line::from(Span::styled(
                format!("{icon} {name}: {summary}"),
                Style::default().fg(color),
            )));
        }
        ChatMessage::Thinking(text) => {
            for chunk in text.chars().collect::<Vec<_>>().chunks(width) {
                lines.push(Line::from(Span::styled(
                    chunk.iter().collect::<String>(),
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

// ===========================================================================
// Input
// ===========================================================================

fn render_input(frame: &mut Frame, area: Rect, state: &AppState) {
    let prompt = match &state.mode {
        AppMode::Normal => "> ",
        AppMode::AskUser { .. } => "? ",
        AppMode::Approval { .. } => "[y/n] ",
    };
    let text = format!("{prompt}{}", state.input.buffer);
    let input = Paragraph::new(text).block(Block::default().borders(Borders::TOP));
    frame.render_widget(input, area);
}

// ===========================================================================
// Help bar
// ===========================================================================

fn render_help(frame: &mut Frame, area: Rect, _state: &AppState) {
    let help = Span::styled(
        "Enter:send  ↑↓:scroll  y/n:approve  Esc:cancel  Ctrl+C:quit",
        Style::default().fg(Color::DarkGray),
    );
    frame.render_widget(Paragraph::new(help), area);
}

// ===========================================================================
// Approval popup
// ===========================================================================

fn render_approval_popup(frame: &mut Frame, parent: Rect, tools: &[crate::app::ToolInfo], current: usize) {
    let popup_area = centered_rect(55, 10, parent);
    frame.render_widget(Clear, popup_area);
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(" Tool Approval ").style(Style::default().fg(Color::Yellow)),
        popup_area,
    );

    let inner = Block::default().borders(Borders::ALL).inner(popup_area);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from("Agent wants to run:".bold()));
    for (i, tool) in tools.iter().enumerate() {
        let marker = if i == current { " → " } else { "   " };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}. {} ({})", i + 1, tool.tool_name, tool.input),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Tool {}/{}: y=approve  n=reject  Esc=cancel", current + 1, tools.len()),
        Style::default().fg(Color::Yellow),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

// ===========================================================================
// AskUser popup
// ===========================================================================

fn render_ask_popup(frame: &mut Frame, parent: Rect, question: &str, options: &[String], answer: &str) {
    let popup_area = centered_rect(55, 10, parent);
    frame.render_widget(Clear, popup_area);
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(" Agent asks ").style(Style::default().fg(Color::Magenta)),
        popup_area,
    );

    let inner = Block::default().borders(Borders::ALL).inner(popup_area);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(question, Style::default().fg(Color::White).bold())));
    if !options.is_empty() {
        for (i, opt) in options.iter().enumerate() {
            lines.push(Line::from(Span::styled(
                format!("  [{}] {opt}", i + 1),
                Style::default().fg(Color::Cyan),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("> {answer}"),
        Style::default().fg(Color::Green),
    )));
    lines.push(Line::from(Span::styled(
        "Enter: submit  Esc: cancel",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

// ===========================================================================
// Helpers
// ===========================================================================

fn centered_rect(percent_x: u16, percent_y: u16, parent: Rect) -> Rect {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(parent);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(layout[1])[1]
}
