//! Help bar — bottom 1 line, mode-dependent shortcuts.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::component::Component;

pub struct HelpPanel;

impl Component for HelpPanel {
    fn height(&self) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let text = match state.mode {
            AppMode::Normal => "Enter:send  Esc:quit  Ctrl+C:quit",
            AppMode::ApprovalPending => "y:approve  n:reject  Esc:cancel",
            AppMode::AskPending => "Enter:submit  Esc:cancel",
        };
        let line = Line::from(Span::styled(
            text,
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(line), area);
    }
}