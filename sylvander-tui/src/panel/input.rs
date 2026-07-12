//! Composer panel — bottom of the screen.
//!
//! Wraps the multi-line composer in a chrome block per UX §5.3 + the
//! `18-composer-interactions.svg` states:
//!
//! - Hairline rule **above** (alongside the transcript's closing edge).
//! - 3-pixel coral left-edge bar when the composer owns focus
//!   (`focus_box()` border style). No bar when idle.
//! - Placeholder text "Ask Sylvander…" in `TEXT_DIM` when the buffer
//!   is empty.
//! - The composer rows themselves (multiline, hardware cursor).
//! - For large pastes (§12.4): side-by-side token chips with a
//!   removable `×` glyph. Each chip is a single-celled Box with a
//!   `▣` (paste) or `@` (file) prefix.
//! - Helper line below the rows: "Type while I work — steer, queue,
//!   or interrupt." in `TEXT_MUTED`.
//! - Hairline rule **below** the composer (between composer and status
//!   row).

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::component::Component;
use crate::input::AttachmentKind;
use crate::theme;

pub struct InputPanel;

impl Component for InputPanel {
    fn height(&self) -> Constraint {
        // Reserve 1 line for the top hairline + 1 line for the bottom
        // hairline + a generous budget for content. Final layout
        // overlays contents; ratatui clips excess gracefully.
        Constraint::Length(12)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        // Top hairline as a separate one-row band above the chrome.
        let top_rule = Line::from("─".repeat(area.width as usize)).style(theme::rule());
        let top_rule_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(top_rule), top_rule_area);

        // Chrome block — open on top (top hairline already drawn
        // above), bottom + sides bordered. Left edge bar in coral
        // when focused.
        let chrome_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(2),
        };
        let mut block = Block::default()
            .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
            .border_style(theme::text_muted());
        // 3-state focus border per UX §18 IDLE / FOCUSED states.
        if state.composer.has_focus_interaction()
            && state.modals.is_empty()
            && matches!(state.mode, AppMode::Normal)
        {
            // FOCUSED — coral focus_box border.
            block = block.border_style(theme::focus_box());
        } else if state.composer.has_focus_interaction() {
            // INACTIVE — composer has been used but a modal is now
            // eating its input. Muted border so the chrome still
            // renders but no focus stroke is shown.
            block = block.border_style(theme::composer_idle_border());
        } else {
            // IDLE — user has not yet typed. Muted border, no coral.
            block = block.border_style(theme::composer_idle_border());
        }
        frame.render_widget(block, chrome_area);
        let inner = chrome_area.inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 0,
        });

        // Layout inside chrome: [attachment-strip] [composer-rows] [helper]
        let attachment_strip_h: u16 = if state.composer.attachment_count() > 0 {
            (1 + (state.composer.attachment_count() - 1).div_ceil(MAX_TOKENS_PER_ROW)) as u16
        } else {
            0
        };
        let composer_rows = state.composer.row_count().max(1) as u16;
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(attachment_strip_h),
                Constraint::Length(composer_rows),
                Constraint::Length(2), // 1 helper line + 1 spacer
            ])
            .split(inner);

        // (1) Attachment tokens (side-by-side chips).
        if attachment_strip_h > 0 {
            render_attachment_tokens(frame, state, layout[0]);
        }

        // (2) Composer rows — empty-state placeholder if buffer empty.
        render_composer_rows(frame, state, layout[1], inner);

        // (3) Helper line.
        let helper = match state.mode {
            AppMode::Normal => {
                "Type while I work — steer, queue, or interrupt."
            }
            AppMode::ApprovalPending => "Approving tools clears this draft.",
            AppMode::AskPending => "Answering this question clears this draft.",
        };
        let helper_area = Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(2),
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(helper, theme::composer_helper())),
            helper_area,
        );

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
    let visible = composer.attachment_count().min(MAX_TOKENS_PER_ROW);
    let mut line = String::with_capacity(area.width as usize);
    let hidden = composer
        .attachment_count()
        .saturating_sub(MAX_TOKENS_PER_ROW);
    for (i, att) in composer.attachments.iter().take(visible).enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        let glyph = match att.kind {
            AttachmentKind::Paste => "▣",
            AttachmentKind::File => "@",
        };
        // "▣ error.log · 84 lines  ×"
        let name = att.preview.replace(' ', "_");
        let chunk = format!("{glyph} {name} · {} lines  ×", att.line_count);
        let truncated = truncate(&chunk, CHIP_W);
        line.push_str(&truncated);
    }
    if hidden > 0 {
        line.push_str(&format!("  +{hidden} more"));
    }
    if line.is_empty() {
        line = "(no attachments)".into();
    }
    let paragraph = Paragraph::new(Line::from(Span::styled(line, theme::text_dim())));
    frame.render_widget(paragraph, area);
}

fn render_composer_rows(frame: &mut Frame, state: &AppState, area: Rect, inner: Rect) {
    let composer = &state.composer;
    let is_empty = composer.is_empty();
    let prompt = match state.mode {
        AppMode::Normal => "› ", // arrow prompt per §5.3 IDLE
        AppMode::AskPending => "? ",
        AppMode::ApprovalPending => "» ",
    };

    if is_empty {
        // Show centered placeholder.
        let placeholder = Line::from(Span::styled(
            "Ask Sylvander…",
            theme::composer_placeholder(),
        ));
        let p = Paragraph::new(placeholder)
            .wrap(Wrap { trim: false });
        frame.render_widget(p, area);
        return;
    }

    let n = composer.row_count();
    let prompt_w = prompt.chars().count() as u16;
    let lines: Vec<Line<'_>> = (0..n.min(area.height as usize))
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
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        area,
    );

    // Hardware cursor at end of the cursor-row text.
    let cursor_row = composer.cursor_row();
    if cursor_row < n.min(area.height as usize) {
        let cursor_x = inner.x + prompt_w + composer.cursor_col_chars() as u16;
        let cursor_y = area.y + cursor_row as u16;
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
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
