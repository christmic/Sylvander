//! Application controller.
//!
//! This is the boundary between terminal input, domain state, and outbound
//! service effects. It owns no renderer and performs no I/O.

use crossterm::event::KeyEvent;

use crate::app::AppState;
use crate::event::{Action, DomainEvent};

#[derive(Debug, PartialEq, Eq)]
pub enum UserIntent {
    Key(KeyEvent),
    Paste(String),
    ScrollTranscript { lines: isize },
    Redraw,
}

pub struct Application {
    pub state: AppState,
}

impl Application {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub fn handle(&mut self, intent: UserIntent) {
        match intent {
            UserIntent::Key(key) => {
                if let Some(action) = self.state.handle_key(&key) {
                    self.state.pending_actions.push(action);
                }
            }
            UserIntent::Paste(text) => self.state.handle_paste(&text),
            UserIntent::ScrollTranscript { lines } => self.state.scroll_transcript(lines),
            UserIntent::Redraw => self.state.dirty.mark(),
        }
        self.state.enforce_memory_budget();
    }

    pub fn apply(&mut self, event: DomainEvent) {
        if let Some(action) = self.state.apply(event) {
            self.state.pending_actions.push(action);
        }
    }

    pub fn take_effects(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.state.pending_actions)
    }
}

#[cfg(test)]
#[path = "../tests/unit/application.rs"]
mod tests;
