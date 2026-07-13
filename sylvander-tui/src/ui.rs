//! UI dispatcher — the ONLY function that decides what gets drawn.
//!
//! Layout strategy:
//! 1. If the terminal is smaller than the minimum supported viewport
//!    (50 cols × 12 rows, per UX §13), render a clear resize-message
//!    block and return — do not corrupt terminal state with partial draws.
//! 2. Split the screen vertically according to each panel's height().
//! 3. Render panels top-to-bottom in registration order.
//! 4. Render modals (top of stack last, so it overlays everything).
//!
//! No business logic lives here — just pure orchestration.

use ratatui::{
    Frame,
    layout::{Layout, Rect},
    style::{Modifier, Stylize},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::AppState;
use crate::compat::Breakpoint;
use crate::component::Component;
use crate::modal::ModalPlacement;
use crate::panel::{ChatPanel, InputPanel, StatusPanel};

pub fn dispatch(frame: &mut Frame, state: &AppState) {
    dispatch_with_metrics(frame, state);
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameMetrics {
    pub transcript_scroll_limit: usize,
}

pub fn dispatch_with_metrics(frame: &mut Frame, state: &AppState) -> FrameMetrics {
    let area = frame.area();
    let breakpoint = Breakpoint::from_width(area.width);

    // TooSmall — render a single full-screen resize prompt only. Do not
    // attempt any panel splits because the layout would overflow vertically
    // and leave terminal state in an inconsistent state on exit.
    if breakpoint == Breakpoint::TooSmall {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Sylvander — please resize ")
            .title_style(crate::theme::warning());
        frame.render_widget(block, area);
        let msg = Line::from(vec![
            Span::styled("Terminal too small", crate::theme::danger().bold()),
            Span::raw(" — minimum supported viewport is "),
            Span::styled(
                "50 columns × 12 rows",
                crate::theme::text().add_modifier(Modifier::BOLD),
            ),
            Span::raw("."),
            Span::raw("\n\nResize the window to continue."),
        ]);
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let p = Paragraph::new(msg).wrap(Wrap { trim: false });
        frame.render_widget(p, inner);
        return FrameMetrics::default();
    }

    // 1. Panel layer.
    // Paint one warm-neutral canvas first. Individual widgets may leave
    // whitespace intentionally; that whitespace is still part of the UI.
    frame.render_widget(Block::default().style(crate::theme::text_on_canvas()), area);

    // Presentation owns its component graph. Domain/application state never
    // stores renderer instances or decides component order.
    let chat = ChatPanel;
    let input = InputPanel;
    let status = StatusPanel;
    let chunks = panel_chunks(area, state);
    let transcript_scroll_limit = chat.render_with_scroll_limit(frame, chunks[0], state);
    input.render(frame, chunks[1], state);

    // Temporary choices are structurally below the Composer. Long-form review
    // views remain overlays, but neither surface may displace the status line
    // from the bottom edge.
    for modal in state.modals.iter().filter(|modal| modal.active()) {
        if matches!(
            modal.placement(state, area.width),
            ModalPlacement::BelowComposer { .. }
        ) {
            modal.render(frame, chunks[2], state);
        }
    }
    status.render(frame, chunks[3], state);

    // 2. Long-form modal layer.
    for modal in state.modals.iter().filter(|modal| modal.active()) {
        if modal.placement(state, area.width) == ModalPlacement::Overlay {
            modal.render(frame, area, state);
        }
    }
    FrameMetrics {
        transcript_scroll_limit,
    }
}

pub fn transcript_scroll_limit(area: Rect, state: &AppState) -> usize {
    if Breakpoint::from_width(area.width) == Breakpoint::TooSmall {
        return 0;
    }
    ChatPanel.scroll_limit(panel_chunks(area, state)[0], state)
}

fn panel_chunks(area: Rect, state: &AppState) -> std::rc::Rc<[Rect]> {
    let chat_height = ChatPanel.height(state, area.width);
    let input_height = InputPanel.height(state, area.width);
    let status_height = StatusPanel.height(state, area.width);
    let requested_dock_rows = state
        .modals
        .iter()
        .filter(|modal| modal.active())
        .last()
        .and_then(|modal| match modal.placement(state, area.width) {
            ModalPlacement::BelowComposer { rows } => Some(rows),
            ModalPlacement::Overlay => None,
        })
        .unwrap_or(0);
    let fixed_rows = constraint_rows(input_height)
        .saturating_add(constraint_rows(status_height))
        .saturating_add(1);
    let dock_rows = requested_dock_rows.min(area.height.saturating_sub(fixed_rows));
    Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            chat_height,
            input_height,
            ratatui::layout::Constraint::Length(dock_rows),
            status_height,
        ])
        .split(area)
}

fn constraint_rows(constraint: ratatui::layout::Constraint) -> u16 {
    match constraint {
        ratatui::layout::Constraint::Length(rows) => rows,
        _ => 0,
    }
}

// Local re-export so the imports above don't pull ratatui::text::Span into
// the public surface elsewhere.
use ratatui::text::Span;

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn empty_focused_composer_exposes_a_hardware_cursor_after_prompt() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| dispatch(frame, &AppState::new()))
            .expect("draw");
        terminal.backend_mut().assert_cursor_position((2, 21));
    }

    #[test]
    fn chinese_composer_cursor_uses_display_cells_not_scalar_count() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new();
        for character in "你好".chars() {
            state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        terminal
            .draw(|frame| dispatch(frame, &state))
            .expect("draw");
        terminal.backend_mut().assert_cursor_position((6, 21));
    }

    #[test]
    fn transcript_scroll_uses_the_rendered_top_as_a_hard_limit() {
        let mut state = AppState::new();
        state.welcomed = false;
        for index in 0..40 {
            state.messages.push(crate::app::ChatMessage::Info(format!(
                "history row {index}"
            )));
        }
        let limit = transcript_scroll_limit(ratatui::layout::Rect::new(0, 0, 80, 24), &state);
        assert!(limit > 0);
        state.set_chat_scroll_limit(limit);
        state.scroll_transcript(isize::MAX);
        assert_eq!(state.chat_scroll, limit);
        state.scroll_transcript(-4);
        assert_eq!(state.chat_scroll, limit - 4);
    }
}
