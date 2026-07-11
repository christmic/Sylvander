//! Help panel — last row of the screen.
//!
//! Per `18-composer-interactions.svg` (rule "Right-side hints are
//! contextual, maximum three. No permanent shortcut manual in the
//! footer"), this panel renders the same contextual hints that the
//! status panel's right-side text already shows, so the operator
//! gets the same context everywhere. We keep the panel + its layout
//! slot for now — it's a no-op when the status row carries the hints,
//! and the panel system still has the slot reserved for future
//! status-row expansion.

use ratatui::{
    layout::{Constraint, Rect},
    text::Line,
    widgets::Paragraph,
    Frame,
};

use crate::app::AppState;
use crate::component::Component;

pub struct HelpPanel;

impl Component for HelpPanel {
    fn height(&self) -> Constraint {
        // Reserved space — the status panel currently owns the
        // contextual hints. Keeping this slot at height 0 means the
        // chat panel gets an extra row rather than a dead bar.
        Constraint::Length(0)
    }

    fn render(&self, _frame: &mut Frame, _area: Rect, _state: &AppState) {
        // Intentionally empty — the status row below carries the
        // context per design. This panel exists only to keep the
        // modular layout slot for future use.
        let line = Line::from("");
        let _ = Paragraph::new(line);
    }
}
