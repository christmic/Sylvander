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
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
    let lines = composer
        .attachments
        .chunks(MAX_TOKENS_PER_ROW)
        .map(|attachments| {
            let mut line = String::with_capacity(area.width as usize);
            for (index, att) in attachments.iter().enumerate() {
                if index > 0 {
                    line.push_str("  ");
                }
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
        })
        .collect::<Vec<_>>();
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

fn render_composer_rows(frame: &mut Frame, state: &AppState, area: Rect, inner: Rect) {
    let composer = &state.composer;
    let is_empty = composer.is_empty();
    let prompt = "> ";

    if is_empty {
        let placeholder = Line::from(Span::styled(prompt, theme::composer_placeholder()));
        let p = Paragraph::new(placeholder).wrap(Wrap { trim: false });
        frame.render_widget(p, area);
        if state.modals.is_empty() && area.width > 2 {
            frame.set_cursor_position((area.x + 2, area.y));
        }
        return;
    }

    let content_width = area.width.saturating_sub(2).max(1) as usize;
    let mut visual_lines = Vec::new();
    let mut cursor = None;
    for row in 0..composer.row_count() {
        let first_prefix = if row == 0 { prompt } else { "  " };
        let mut wrapped = wrap_composer_row(composer.row(row), first_prefix, content_width);
        if row == composer.cursor_row() {
            let (line, cell) = cursor_position(
                composer.row(row),
                composer.cursor_col_cells(),
                content_width,
            );
            if line == wrapped.len() {
                wrapped.push("  ".into());
            }
            cursor = Some((visual_lines.len() + line, cell));
        }
        visual_lines.extend(wrapped.into_iter().map(Line::from));
    }

    // Keep the hardware cursor visible when a long draft exceeds the eight-row
    // composer cap. Earlier content scrolls inside the composer instead of
    // causing the cursor to disappear below the clipped viewport.
    let visible_rows = area.height as usize;
    let start = cursor
        .map(|(line, _)| line.saturating_add(1).saturating_sub(visible_rows))
        .unwrap_or(0);
    let visible = visual_lines
        .into_iter()
        .skip(start)
        .take(visible_rows)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), area);

    if let Some((line, cell)) = cursor.filter(|_| state.modals.is_empty()) {
        let cursor_x = area.x.saturating_add(2).saturating_add(cell as u16);
        let cursor_y = area.y.saturating_add(line.saturating_sub(start) as u16);
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn visual_row_count(state: &AppState, width: usize) -> usize {
    let content_width = width.saturating_sub(2).max(1);
    (0..state.composer.row_count())
        .map(|index| {
            let lines = wrapped_line_count(state.composer.row(index), content_width);
            if index == state.composer.cursor_row() {
                let (cursor_line, _) = cursor_position(
                    state.composer.row(index),
                    state.composer.cursor_col_cells(),
                    content_width,
                );
                lines.max(cursor_line.saturating_add(1))
            } else {
                lines
            }
        })
        .sum()
}

fn wrap_composer_row(text: &str, first_prefix: &str, content_width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut cells = 0usize;
    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        if cells > 0 && cells.saturating_add(width) > content_width {
            chunks.push(std::mem::take(&mut chunk));
            cells = 0;
        }
        chunk.push_str(grapheme);
        cells = cells.saturating_add(width);
        if cells >= content_width {
            chunks.push(std::mem::take(&mut chunk));
            cells = 0;
        }
    }
    if !chunk.is_empty() || chunks.is_empty() {
        chunks.push(chunk);
    }
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let prefix = if index == 0 { first_prefix } else { "  " };
            format!("{prefix}{chunk}")
        })
        .collect()
}

fn wrapped_line_count(text: &str, content_width: usize) -> usize {
    wrap_composer_row(text, "", content_width).len()
}

fn cursor_position(text: &str, cursor_cells: usize, content_width: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut cells = 0usize;
    let mut consumed = 0usize;
    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        if consumed.saturating_add(width) > cursor_cells {
            break;
        }
        if cells > 0 && cells.saturating_add(width) > content_width {
            line = line.saturating_add(1);
            cells = 0;
        }
        cells = cells.saturating_add(width);
        consumed = consumed.saturating_add(width);
        if cells >= content_width {
            line = line.saturating_add(1);
            cells = 0;
        }
    }
    (line, cells)
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

    #[test]
    fn chinese_text_wraps_and_positions_cursor_in_terminal_cells() {
        let mut state = AppState::new();
        for character in "你好世界中".chars() {
            state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert_eq!(state.composer.cursor_col_chars(), 5);
        assert_eq!(state.composer.cursor_col_cells(), 10);
        assert_eq!(
            wrap_composer_row("你好世界中", "> ", 8),
            ["> 你好世界", "  中"]
        );
        assert_eq!(cursor_position("你好世界中", 10, 8), (1, 2));
        assert_eq!(visual_row_count(&state, 10), 2);
    }

    #[test]
    fn exact_width_draft_allocates_a_cursor_continuation_row() {
        let mut state = AppState::new();
        for character in "你好世界".chars() {
            state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert_eq!(cursor_position("你好世界", 8, 8), (1, 0));
        assert_eq!(visual_row_count(&state, 10), 2);
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
