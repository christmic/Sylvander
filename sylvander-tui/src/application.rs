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
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn mouse_scroll_intent_changes_transcript_not_composer_history() {
        let mut app = Application::new(AppState::new());
        app.state
            .composer
            .history
            .push_back("previous command".into());
        app.handle(UserIntent::ScrollTranscript { lines: 4 });
        assert_eq!(app.state.chat_scroll, 4);
        assert!(app.state.composer.is_empty());
    }

    #[test]
    fn keyboard_up_belongs_to_composer_history_not_transcript() {
        let mut app = Application::new(AppState::new());
        app.state
            .composer
            .history
            .push_back("previous command".into());
        app.handle(UserIntent::Key(KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.state.composer.row(0), "previous command");
        assert_eq!(app.state.chat_scroll, 0);
    }

    #[test]
    fn mouse_scroll_down_returns_to_live_and_clears_unread() {
        let mut app = Application::new(AppState::new());
        app.state.chat_scroll = 3;
        app.state.unread_events = 2;
        app.handle(UserIntent::ScrollTranscript { lines: -4 });
        assert_eq!(app.state.chat_scroll, 0);
        assert_eq!(app.state.unread_events, 0);
    }

    #[test]
    fn terminal_event_starts_exactly_one_locally_queued_prompt() {
        let mut app = Application::new(AppState::new());
        app.state.session_id = Some("session-1".into());
        app.state.turn_active = true;
        app.state.queued_prompts.push_back("follow up".into());
        app.state
            .messages
            .push(crate::app::ChatMessage::QueuedUser("follow up".into()));

        app.apply(DomainEvent::AgentDone {
            final_text: "done".into(),
        });

        assert!(app.state.turn_active);
        assert!(app.state.queued_prompts.is_empty());
        assert!(matches!(
            app.take_effects().as_slice(),
            [Action::SendChat { text, session_id: Some(session_id), .. }]
                if text == "follow up" && session_id == "session-1"
        ));
    }
}
