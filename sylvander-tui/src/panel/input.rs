//! Input panel — bottom N lines. Contains the mode prompt + multiline composer.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::component::Component;

pub struct InputPanel;

impl InputPanel {
    fn prompt(mode: &AppMode) -> (&'static str, u16) {
        match mode {
            AppMode::Normal => ("> ", 2),
            AppMode::AskPending => ("? ", 2),
            AppMode::ApprovalPending => ("[y/n] ", 6),
        }
    }
}

impl Component for InputPanel {
    fn height(&self) -> Constraint {
        // We can't know the composer row count here (static API), so we
        // request a generous fixed budget and let ratatui truncate. The
        // InputPanel render() re-computes the actual visible row count.
        Constraint::Length(8)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (prompt, prompt_w) = Self::prompt(&state.mode);
        let composer = &state.composer;
        let n = composer.row_count();

        // Build visible lines, clipped to `inner.height`.
        let lines: Vec<Line<'_>> = (0..n.min(inner.height as usize))
            .map(|i| {
                let mut s = String::with_capacity(prompt.len() + 64);
                if i == 0 {
                    s.push_str(prompt);
                } else {
                    for _ in 0..prompt_w {
                        s.push(' ');
                    }
                }
                s.push_str(composer.row(i));
                Line::from(s)
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), inner);

        // Hardware cursor on the row matching `composer.cursor_row()`.
        let cursor_row = composer.cursor_row();
        let cursor_x = inner.x + prompt_w + composer.cursor_col_chars() as u16;
        let cursor_y = inner.y + cursor_row as u16;
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}
