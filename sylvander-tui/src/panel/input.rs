//! Composer panel — bottom of the screen.
//!
//! Wraps the multi-line composer in a chrome block per UX §5.3 + the
//! `18-composer-interactions.svg` states:
//!
//! - Hairline rule **above** (alongside the transcript's closing edge).
//! - 3-pixel coral left-edge bar when the composer owns focus
//!   (`focus_box()` border style). No bar when idle.
//! - A plain `>` prompt; no conversational placeholder copy.
//! - The composer rows themselves (multiline, hardware cursor).
//! - For large pastes (§12.4): side-by-side token chips with a
//!   removable `×` glyph. Each chip is a single-celled Box with a
//!   `▣` (paste) or `@` (file) prefix.
//! - Hairline rule **below** the composer (between composer and status
//!   row).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::AppState;
use crate::component::Component;
use crate::input::AttachmentKind;
use crate::theme;

pub struct InputPanel;

impl Component for InputPanel {
    fn height(&self, state: &AppState, viewport_width: u16) -> Constraint {
        let attachment_rows = if state.composer.attachment_count() == 0 {
            0
        } else {
            state
                .composer
                .attachment_count()
                .div_ceil(MAX_TOKENS_PER_ROW)
        };
        let inner_width = viewport_width.max(1) as usize;
        let draft_rows = visual_row_count(state, inner_width).clamp(1, 8);
        Constraint::Length((2 + attachment_rows + draft_rows) as u16)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let top_rule = Line::from("─".repeat(area.width as usize)).style(theme::rule());
        let top_rule_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(top_rule), top_rule_area);

        // The composer is borderless at rest. Focus is a short leading
        // accent only; the two horizontal rules provide stable anchoring.
        let chrome_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(2),
        };
        let block = Block::default().borders(Borders::NONE);
        frame.render_widget(block, chrome_area);
        // The prompt shares the exact left edge of the full-width rules.
        // Text after `> ` remains naturally indented by the prompt itself.
        let inner = chrome_area;

        // Layout inside chrome: [attachment-strip] [composer-rows].
        let attachment_strip_h: u16 = if state.composer.attachment_count() > 0 {
            (1 + (state.composer.attachment_count() - 1).div_ceil(MAX_TOKENS_PER_ROW)) as u16
        } else {
            0
        };
        let composer_rows = visual_row_count(state, inner.width as usize).clamp(1, 8) as u16;
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(attachment_strip_h),
                Constraint::Length(composer_rows),
            ])
            .split(inner);

        // (1) Attachment tokens (side-by-side chips).
        if attachment_strip_h > 0 {
            render_attachment_tokens(frame, state, layout[0]);
        }

        // (2) Composer rows — empty-state placeholder if buffer empty.
        render_composer_rows(frame, state, layout[1], inner);

        // Bottom hairline (mirrors top).
        let bot_rule = Line::from("─".repeat(area.width as usize)).style(theme::rule());
        let bot_rule_area = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(1),
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(bot_rule), bot_rule_area);
    }
}

/// Width of one attachment chip, including the `×` remove column.
const CHIP_W: usize = 24;
/// Max chips side-by-side per strip row. Excess wrap to next row.
const MAX_TOKENS_PER_ROW: usize = 6;

fn render_attachment_tokens(frame: &mut Frame, state: &AppState, area: Rect) {
    let composer = &state.composer;
    let lines = composer.attachments.chunks(MAX_TOKENS_PER_ROW).map(|attachments| {
        let mut line = String::with_capacity(area.width as usize);
        for (index, att) in attachments.iter().enumerate() {
            if index > 0 { line.push_str("  "); }
            let glyph = match att.kind {
                AttachmentKind::Paste => "▣",
                AttachmentKind::File => "@",
                AttachmentKind::Image => "◈",
                AttachmentKind::Selection => "≡",
                AttachmentKind::Diff => "±",
                AttachmentKind::TerminalOutput => "$",
            };
            let name = att.preview.replace(' ', "_");
            let chunk = format!("{glyph} {name} · {} lines  ×", att.line_count);
            line.push_str(&truncate(&chunk, CHIP_W));
        }
        Line::from(Span::styled(line, theme::text_dim()))
    }).collect::<Vec<_>>();
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

fn render_composer_rows(frame: &mut Frame, state: &AppState, area: Rect, inner: Rect) {
    let composer = &state.composer;
    let is_empty = composer.is_empty();
    let prompt = "> ";

    if is_empty {
        let placeholder = Line::from(Span::styled(">", theme::composer_placeholder()));
        let p = Paragraph::new(placeholder).wrap(Wrap { trim: false });
        frame.render_widget(p, area);
        return;
    }

    let n = composer.row_count();
    let mut visual_lines = Vec::new();
    for i in 0..n {
        let first_prefix = if i == 0 { prompt } else { "  " };
        visual_lines.extend(
            wrap_composer_row(composer.row(i), first_prefix, area.width.max(1) as usize)
                .map(Line::from),
        );
    }
    visual_lines.truncate(area.height as usize);
    frame.render_widget(Paragraph::new(visual_lines), area);

    // Hardware cursor at end of the cursor-row text.
    let cursor_row = composer.cursor_row();
    if cursor_row < n {
        let wrap_width = area.width.max(3) as usize;
        let content_width = wrap_width.saturating_sub(2).max(1);
        let rows_before: usize = (0..cursor_row)
            .map(|row| {
                composer
                    .row(row)
                    .chars()
                    .count()
                    .max(1)
                    .div_ceil(content_width)
            })
            .sum();
        let cursor_cells = composer.cursor_col_chars();
        let cursor_x = area.x + 2 + (cursor_cells % content_width) as u16;
        let cursor_y = area.y + rows_before as u16 + (cursor_cells / content_width) as u16;
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn visual_row_count(state: &AppState, width: usize) -> usize {
    let content_width = width.saturating_sub(2).max(1);
    (0..state.composer.row_count())
        .map(|index| {
            state
                .composer
                .row(index)
                .chars()
                .count()
                .max(1)
                .div_ceil(content_width)
        })
        .sum()
}

fn wrap_composer_row<'a>(
    text: &'a str,
    first_prefix: &'a str,
    width: usize,
) -> impl Iterator<Item = String> + 'a {
    let content_width = width.saturating_sub(2).max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut chunks: Vec<String> = if chars.is_empty() {
        vec![first_prefix.to_string()]
    } else {
        chars
            .chunks(content_width)
            .enumerate()
            .map(|(index, chunk)| {
                let prefix = if index == 0 { first_prefix } else { "  " };
                format!("{prefix}{}", chunk.iter().collect::<String>())
            })
            .collect()
    };
    if chunks.is_empty() {
        chunks.push(first_prefix.into());
    }
    chunks.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn composer_starts_one_row_and_grows_when_text_wraps() {
        let mut state = AppState::new();
        assert_eq!(visual_row_count(&state, 12), 1);
        for _ in 0..24 {
            state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        }
        assert_eq!(visual_row_count(&state, 12), 3);
    }
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
