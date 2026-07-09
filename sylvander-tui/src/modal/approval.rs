//! Approval modal — shown when the server wants permission to run tools.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{AppMode, AppState, ToolInfo};
use crate::event::Action;
use crate::modal::{Consumed, Modal};

/// One batch of tools awaiting user approval.
pub struct ApprovalModal {
    pub batch_id: String,
    pub tools: Vec<ToolInfo>,
    pub current: usize,
    /// Per-tool decision: true = approve, false = reject.
    pub decisions: Vec<bool>,
}

impl ApprovalModal {
    pub fn new(batch_id: String, tools: Vec<ToolInfo>) -> Self {
        let decisions = vec![true; tools.len()];
        Self {
            batch_id,
            tools,
            current: 0,
            decisions,
        }
    }
}

impl Modal for ApprovalModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        "Tool Approval"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let popup_area = centered_rect(55, 10 + self.tools.len() as u16 * 2, parent);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Tool Approval ")
                .title_style(Style::default().fg(Color::Yellow)),
            popup_area,
        );

        let inner = Block::default().borders(Borders::ALL).inner(popup_area);
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from("Agent wants to run:".bold()));
        for (i, tool) in self.tools.iter().enumerate() {
            let marker = if i == self.current { " >> " } else { "    " };
            let marker_color = if i == self.current {
                Color::Yellow
            } else {
                Color::White
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "{marker}{}. {} ({})",
                    i + 1,
                    tool.tool_name,
                    tool.input
                ),
                Style::default().fg(marker_color),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "Tool {}/{}: y=approve  n=reject  Esc=cancel",
                self.current + 1,
                self.tools.len()
            ),
            Style::default().fg(Color::Yellow),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Char('y') => {
                self.decisions[self.current] = true;
                advance(self, state)
            }
            KeyCode::Char('n') => {
                self.decisions[self.current] = false;
                advance(self, state)
            }
            KeyCode::Esc => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn advance(modal: &mut ApprovalModal, state: &mut AppState) -> Consumed {
    if modal.current + 1 < modal.tools.len() {
        modal.current += 1;
        return Consumed::Yes { dismiss: false };
    }
    // All decided — drain into pending_actions, signal dismissal.
    let decisions = std::mem::take(&mut modal.decisions);
    let tools = std::mem::take(&mut modal.tools);
    state.mode = AppMode::Normal;
    for (tool, approved) in tools.iter().zip(decisions.iter()) {
        state.pending_actions.push(Action::SendApprove {
            call_id: tool.call_id.clone(),
            approved: *approved,
        });
    }
    Consumed::Yes { dismiss: true }
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