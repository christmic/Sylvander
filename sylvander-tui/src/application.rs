//! Application controller.
//!
//! This is the boundary between terminal input, domain state, and outbound
//! service effects. It owns no renderer and performs no I/O.

use crossterm::event::KeyEvent;

use crate::app::AppState;
use crate::event::{Action, DomainEvent};

#[derive(PartialEq, Eq)]
struct TranscriptInputSignature {
    message_count: usize,
    streaming_bytes: usize,
    thinking_bytes: usize,
    welcomed: bool,
    tool_details_expanded: bool,
    session_id: Option<String>,
    model: String,
    workspace: std::path::PathBuf,
    branch: String,
    theme: crate::theme::ThemeName,
}

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
        let before = transcript_input_signature(&self.state);
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
        if before != transcript_input_signature(&self.state) {
            self.state.touch_transcript();
        }
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

fn transcript_input_signature(state: &AppState) -> TranscriptInputSignature {
    TranscriptInputSignature {
        message_count: state.messages.len(),
        streaming_bytes: state.streaming.len(),
        thinking_bytes: state.streaming_thinking.len(),
        welcomed: state.welcomed,
        tool_details_expanded: state.tool_details_expanded,
        session_id: state.session_id.clone(),
        model: state.metadata.model.clone(),
        workspace: state.metadata.workspace.clone(),
        branch: state.metadata.branch.clone(),
        theme: crate::theme::active_name(),
    }
}

#[cfg(test)]
#[path = "../tests/unit/application.rs"]
mod tests;
