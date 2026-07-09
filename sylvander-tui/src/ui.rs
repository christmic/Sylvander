//! UI dispatcher — the ONLY function that decides what gets drawn.
//!
//! Layout strategy:
//! 1. Split the screen vertically according to each panel's height().
//! 2. Render panels top-to-bottom in registration order.
//! 3. Render modals (top of stack last, so it overlays everything).
//!
//! No business logic lives here — just pure orchestration.

use ratatui::{layout::Layout, Frame};

use crate::app::AppState;

pub fn dispatch(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // 1. Panel layer.
    let constraints: Vec<ratatui::layout::Constraint> =
        state.panels.iter().map(|p| p.height()).collect();
    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (panel, chunk) in state.panels.iter().zip(chunks.iter()) {
        panel.render(frame, *chunk, state);
    }

    // 2. Modal layer — drawn on top, full-screen rect (modal centers itself).
    for modal in state.modals.iter() {
        if modal.active() {
            modal.render(frame, area, state);
        }
    }
}