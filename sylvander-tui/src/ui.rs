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
    layout::Layout,
    style::{Color, Modifier, Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::AppState;
use crate::compat::Breakpoint;

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
            .title_style(Style::default().fg(Color::Yellow));
        frame.render_widget(block, area);
        let msg = Line::from(vec![
            Span::styled("Terminal too small", Style::default().fg(Color::Red).bold()),
            Span::raw(" — minimum supported viewport is "),
            Span::styled("50 columns × 12 rows", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("."),
            Span::raw("\n\nResize the window to continue."),
        ]);
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let p = Paragraph::new(msg).wrap(Wrap { trim: false });
        frame.render_widget(p, inner);
        return;
    }

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

// Local re-export so the imports above don't pull ratatui::text::Span into
// the public surface elsewhere.
use ratatui::text::Span;