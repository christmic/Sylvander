//! AskUser modal — model asks the user a question, with optional choices.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::event::Action;
use crate::modal::{Consumed, Modal};

pub struct AskUserModal {
    pub call_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub answer: String,
}

impl AskUserModal {
    pub fn new(call_id: String, question: String, options: Vec<String>) -> Self {
        Self {
            call_id,
            question,
            options,
            answer: String::new(),
        }
    }
}

impl Modal for AskUserModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        "Agent asks"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let popup_area = centered_rect(55, 12, parent);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Agent asks ")
                .title_style(Style::default().fg(Color::Magenta)),
            popup_area,
        );

        let inner = Block::default().borders(Borders::ALL).inner(popup_area);
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            self.question.as_str(),
            Style::default().fg(Color::White).bold(),
        )));
        if !self.options.is_empty() {
            lines.push(Line::from(""));
            for (i, opt) in self.options.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!("  [{}] {opt}", i + 1),
                    Style::default().fg(Color::Cyan),
                )));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("> {}", self.answer),
            Style::default().fg(Color::Green),
        )));
        lines.push(Line::from(Span::styled(
            "Enter: submit  Esc: cancel",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Enter => {
                let call_id = std::mem::take(&mut self.call_id);
                let answer = if self.answer.is_empty() {
                    self.options.first().cloned().unwrap_or_default()
                } else {
                    std::mem::take(&mut self.answer)
                };
                state.mode = AppMode::Normal;
                state.pending_actions.push(Action::SendAnswer { call_id, answer });
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Esc => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char(c) => {
                // Numeric option shortcut.
                if let Some(d) = c.to_digit(10) {
                    if let Some(opt) = self.options.get((d as usize).saturating_sub(1)) {
                        self.answer = opt.clone();
                        return Consumed::Yes { dismiss: false };
                    }
                }
                // Free-text input.
                self.answer.push(c);
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                self.answer.pop();
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, parent: Rect) -> Rect {
    let y_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y.min(95)) / 2),
            Constraint::Percentage(percent_y.min(95)),
            Constraint::Percentage((100 - percent_y.min(95)) / 2),
        ])
        .split(parent);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x.min(95)) / 2),
            Constraint::Percentage(percent_x.min(95)),
            Constraint::Percentage((100 - percent_x.min(95)) / 2),
        ])
        .split(y_layout[1])[1]
}