//! Input panel — bottom 3 lines, contains the prompt and cursor.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::component::Component;

pub struct InputPanel;

impl Component for InputPanel {
    fn height(&self) -> Constraint {
        Constraint::Length(3)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let prompt = match state.mode {
            AppMode::Normal => "> ",
            AppMode::AskPending => "? ",
            AppMode::ApprovalPending => "[y/n] ",
        };

        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Render prompt + buffer.
        let text = format!("{prompt}{}", state.input.buffer);
        frame.render_widget(Paragraph::new(text), inner);

        // Render hardware cursor.
        let cursor_x = inner.x + prompt.chars().count() as u16 + state.input.cursor as u16;
        let cursor_y = inner.y;
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}