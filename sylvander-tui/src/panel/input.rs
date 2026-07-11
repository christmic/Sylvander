//! Input panel — bottom N lines. Contains attachment tokens above + the
//! multiline composer below. The hardware cursor lives on the composer's
//! (row, col) — attachment rows offset the cursor Y down accordingly.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
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

    /// How many attachment-token rows to actually render. More than this
    /// collapses into a `… (+N more)` indicator, so layout stays bounded.
    const MAX_ATTACHMENT_ROWS: usize = 4;
}

impl Component for InputPanel {
    fn height(&self) -> Constraint {
        // Generous fixed budget; the render path re-computes the actual
        // visible row count and ratatui clips excess.
        Constraint::Length(12)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (prompt, prompt_w) = Self::prompt(&state.mode);
        let composer = &state.composer;

        // ----------------------------------------------------------------
        // 1. Attachment tokens (rendered above the composer rows).
        // ----------------------------------------------------------------
        let att_count = composer.attachment_count();
        let visible_att = att_count.min(Self::MAX_ATTACHMENT_ROWS);
        let hidden_att = att_count.saturating_sub(visible_att);
        let att_lines: Vec<Line<'_>> = composer
            .attachments
            .iter()
            .take(visible_att)
            .map(|a| {
                Line::from(format!("  ⎘ {}", a.label()))
                    .style(Style::default().add_modifier(Modifier::DIM))
            })
            .chain(if hidden_att > 0 {
                Some(Line::from(format!(
                    "  ⎘ … (+{hidden_att} more attachment{plural})",
                    plural = if hidden_att == 1 { "" } else { "s" }
                ))
                .style(Style::default().add_modifier(Modifier::DIM)))
            } else {
                None
            })
            .collect();

        // ----------------------------------------------------------------
        // 2. Composer rows.
        // ----------------------------------------------------------------
        let n_composer = composer.row_count();
        let att_rows = att_lines.len();

        // Boundary: do not let composer rows overflow into the rest of the
        // screen — if the inner area has e.g. 10 rows and we have 6
        // attachments visible, only 4 composer rows render.
        let composer_budget = (inner.height as usize).saturating_sub(att_rows);
        let composer_take = n_composer.min(composer_budget);

        let composer_lines: Vec<Line<'_>> = (0..composer_take)
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

        let mut all_lines = att_lines;
        all_lines.extend(composer_lines);

        // Pad with blank rows if everything is short — keeps the bottom
        // border stable across renders.
        while all_lines.len() < inner.height as usize {
            all_lines.push(Line::from(""));
        }

        frame.render_widget(Paragraph::new(all_lines), inner);

        // ----------------------------------------------------------------
        // 3. Hardware cursor — placed on the composer row, not on the
        //    attachment tokens.
        // ----------------------------------------------------------------
        let cursor_composer_row = composer.cursor_row();
        if cursor_composer_row < composer_take {
            let cursor_x = inner.x + prompt_w + composer.cursor_col_chars() as u16;
            let cursor_y = inner.y + att_rows as u16 + cursor_composer_row as u16;
            if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }
}
