//! Modal trait + stack — floating layers that overlay the panels and
//! capture keyboard input.

use ratatui::{Frame, layout::Rect};

use crate::app::AppState;

/// A floating layer drawn on top of the panels. Has its own state and
/// handles keys. When `handle_key` returns `Consumed::Yes(dismissed)`,
/// the dispatcher will pop the modal if `dismissed == true`.
pub trait Modal {
    /// Whether the modal should still be drawn this frame.
    /// For most modals this is `true`; for Toasts it's
    /// `Instant::now() < expires_at`.
    fn active(&self) -> bool;

    /// Title shown in the popup border.
    fn title(&self) -> &str;

    /// Draw into the full-screen `area`. Implementations should call
    /// `centered_rect` internally to position themselves.
    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState);

    /// Process a key. Return whether the key was consumed and whether
    /// the modal wants to be dismissed.
    fn handle_key(&mut self, key: &crossterm::event::KeyEvent, state: &mut AppState) -> Consumed;
}

/// Result of `Modal::handle_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumed {
    /// Modal didn't care about this key — let the dispatcher try
    /// other handlers (or fall through).
    Ignored,
    /// Modal handled the key. If `dismiss` is true, the modal will be
    /// popped from the stack.
    Yes { dismiss: bool },
}

// ===========================================================================
// ModalStack
// ===========================================================================

/// Simple stack of modals. The top of the stack receives keys and is
/// drawn last (on top of everything else).
pub struct ModalStack {
    stack: Vec<Box<dyn Modal>>,
}

impl ModalStack {
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub fn push(&mut self, modal: Box<dyn Modal>) {
        self.stack.push(modal);
    }

    pub fn pop(&mut self) -> Option<Box<dyn Modal>> {
        self.stack.pop()
    }

    pub fn top(&self) -> Option<&dyn Modal> {
        self.stack.last().map(|b| b.as_ref())
    }

    pub fn top_mut(&mut self) -> Option<&mut Box<dyn Modal>> {
        self.stack.last_mut()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// Iterate all modals (used by dispatcher to render Toasts that can
    /// stack alongside a main modal).
    pub fn iter(&self) -> impl Iterator<Item = &dyn Modal> {
        self.stack.iter().map(|b| b.as_ref())
    }

    /// Remove any modals whose `active()` returns false.
    pub fn reap(&mut self) {
        self.stack.retain(|m| m.active());
    }
}

impl Default for ModalStack {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Concrete modal implementations
// ===========================================================================

pub mod approval;
pub mod ask_user;
pub mod file_mention;
pub mod help;
pub mod palette;
pub mod plan;
pub mod sessions;
pub mod tool_inspector;

pub use approval::ApprovalModal;
pub use ask_user::AskUserModal;
pub use file_mention::FileMentionModal;
pub use help::HelpModal;
pub use palette::{COMMANDS, Command, CommandPalette};
pub use plan::PlanReviewModal;
pub use sessions::{SessionEntry, SessionStatus, SessionsOverlay};
pub use tool_inspector::ToolInspector;
