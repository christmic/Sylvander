//! Status bar — top of the screen, one line tall.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::AppState;
use crate::component::Component;

pub struct StatusPanel;

impl Component for StatusPanel {
    fn height(&self) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let connected = if state.connected {
            Span::styled("Connected", Style::default().fg(Color::Green))
        } else {
            Span::styled("Disconnected", Style::default().fg(Color::Red))
        };
        let model = Span::styled("deepseek-v4-flash", Style::default().fg(Color::Cyan));
        let line = Line::from(vec![
            Span::raw("Sylvander · "),
            model,
            Span::raw(" · "),
            connected,
            Span::raw(" · "),
            Span::raw(state.status.as_str()),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}