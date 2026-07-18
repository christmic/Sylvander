//! UI dispatcher — the ONLY function that decides what gets drawn.
//!
//! Layout strategy:
//! 1. Split every available width vertically according to panel height.
//! 2. Render panels top-to-bottom in registration order.
//! 3. Render modals (top of stack last, so it overlays everything).
//!
//! No business logic lives here — just pure orchestration.

use ratatui::{
    Frame,
    layout::{Layout, Rect},
    widgets::Block,
};

use crate::app::AppState;
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

#[cfg(test)]
#[path = "../tests/unit/ui.rs"]
mod tests;
