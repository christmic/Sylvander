//! Help bar — bottom 1 line, mode-dependent shortcuts.
//!
//! At Narrow + below (UX §13) the help text collapses to the bare minimum
//! so a 40-column terminal can still show "Enter:send  Esc:quit".

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::compat::{compact_help_for, Breakpoint};
use crate::component::Component;

pub struct HelpPanel;

impl Component for HelpPanel {
    fn height(&self) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let mode = match state.mode {
            AppMode::Normal => "Normal",
            AppMode::ApprovalPending => "ApprovalPending",
            AppMode::AskPending => "AskPending",
        };
        let breakpoint = Breakpoint::from_width(area.width);
        let text = compact_help_for(breakpoint, mode);
        let line = Line::from(Span::styled(
            text,
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(line), area);
    }
}
