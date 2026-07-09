//! Component trait — anything that occupies a fixed area in the layout.
//!
//! Panels are *render-only*: they get a `Rect` from the dispatcher and
//! draw into it. They do NOT receive keys (modals do). To accept user
//! input, build a Modal or wire into the focused input panel.

use ratatui::{layout::Constraint, layout::Rect, Frame};

use crate::app::AppState;

/// A render-only region of the TUI.
pub trait Component {
    /// Vertical height hint used by the dispatcher when splitting the
    /// screen. Use `Constraint::Min(0)` for the area that should grow.
    fn height(&self) -> Constraint;

    /// Draw into the given area. Called by the dispatcher once per dirty
    /// frame. Implementations must NOT mutate `state` or call into the
    /// socket client.
    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState);
}