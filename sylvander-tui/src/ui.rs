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
    layout::Layout,
    style::{Modifier, Stylize},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::AppState;
use crate::compat::Breakpoint;
use crate::component::Component;
use crate::panel::{ChatPanel, InputPanel, StatusPanel};

pub fn dispatch(frame: &mut Frame, state: &AppState) {
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
        return;
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
    let panels: [&dyn Component; 3] = [&chat, &input, &status];
    let constraints: Vec<ratatui::layout::Constraint> =
        panels.iter().map(|p| p.height(state, area.width)).collect();
    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (panel, chunk) in panels.iter().zip(chunks.iter()) {
        panel.render(frame, *chunk, state);
    }

    // 2. Modal layer — drawn on top, full-screen rect (modal centers itself).
    for modal in state.modals.iter() {
        if modal.active() {
            modal.render(frame, area, state);
        }
    }
}

// Local re-export so the imports above don't pull ratatui::text::Span into
// the public surface elsewhere.
use ratatui::text::Span;
