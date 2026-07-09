//! Application state, message types, and the Reducer.
//!
//! `AppState` is the single source of truth for what the TUI shows.
//! It can only be mutated via:
//! - `apply(event)` — for protocol/domain events
//! - `handle_key(key)` — for keyboard input
//!
//! Both paths automatically mark the dirty flag so the render loop wakes.

use crate::component::Component;
use crate::dirty::DirtyFlag;
use crate::event::{Action, DomainEvent};
use crate::input::InputState;
use crate::modal::ModalStack;
use crate::panel;

// ===========================================================================
// Top-level state
// ===========================================================================

pub struct AppState {
    // ---- business data (read-only for renderers) ----
    pub messages: Vec<ChatMessage>,
    pub streaming: String,
    pub streaming_thinking: String,
    pub session_id: Option<String>,
    pub connected: bool,
    pub status: String,
    pub mode: AppMode,

    // ---- component registration ----
    /// Layout order is `panels[0]` top, last entry bottom.
    pub panels: Vec<Box<dyn Component>>,
    /// Floating layers (approval, ask, toast).
    pub modals: ModalStack,

    // ---- focused input ----
    pub input: InputState,
    /// Chat vertical scroll offset (0 = pinned to bottom).
    pub chat_scroll: usize,
    /// Quit signal — set by handle_key on Ctrl+C / Esc.
    pub should_quit: bool,

    // ---- pending outbound actions (drained by main loop each tick) ----
    pub pending_actions: Vec<Action>,

    // ---- render trigger ----
    pub dirty: DirtyFlag,
}

impl AppState {
    pub fn new() -> Self {
        let mut state = Self {
            messages: Vec::new(),
            streaming: String::new(),
            streaming_thinking: String::new(),
            session_id: None,
            connected: false,
            status: "Connecting...".into(),
            mode: AppMode::Normal,
            panels: Vec::new(),
            modals: ModalStack::new(),
            input: InputState::default(),
            chat_scroll: 0,
            should_quit: false,
            pending_actions: Vec::new(),
            dirty: DirtyFlag::default(),
        };
        state.register_default_panels();
        state
    }

    fn register_default_panels(&mut self) {
        // Order = layout order, top to bottom.
        self.panels.push(Box::new(panel::StatusPanel));
        self.panels.push(Box::new(panel::ChatPanel));
        self.panels.push(Box::new(panel::InputPanel));
        self.panels.push(Box::new(panel::HelpPanel));
    }
}

// ===========================================================================
// AppMode — only used to pick a help-bar hint string
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMode {
    Normal,
    ApprovalPending,
    AskPending,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// ChatMessage — semantic message types, rendered by ChatPanel
// ===========================================================================

/// A message in the chat history. This type is rendering-layer data
/// (it describes how a message looks), but lives in AppState because
/// the Reducer pushes new entries here.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    Agent(String),
    ToolCall {
        name: String,
        status: ToolStatus,
        input: serde_json::Value,
    },
    ToolResult {
        name: String,
        output: String,
        ok: bool,
    },
    Thinking(String),
    Info(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Pending,
    Done,
    Error,
}

/// Tool info for approval requests.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

// ===========================================================================
// Reducer — the only way to mutate AppState from the event pipeline
// ===========================================================================

impl AppState {
    /// Apply a domain event. Always marks the dirty flag (whether the
    /// state changed or not — simpler, and the render is cheap).
    pub fn apply(&mut self, event: DomainEvent) -> Option<Action> {
        let action = self.apply_inner(event);
        self.dirty.mark();
        action
    }

    fn apply_inner(&mut self, event: DomainEvent) -> Option<Action> {
        match event {
            DomainEvent::Connected => {
                self.connected = true;
                self.status = "Connected".into();
            }
            DomainEvent::Disconnected { reason } => {
                self.connected = false;
                self.status = format!("Disconnected: {reason}");
                self.messages.push(ChatMessage::Info(format!("Disconnected: {reason}")));
            }
            DomainEvent::SessionCreated { session_id } => {
                self.session_id = Some(session_id);
            }
            DomainEvent::TextChunk { delta } => {
                self.streaming.push_str(&delta);
            }
            DomainEvent::ThinkingChunk { delta } => {
                self.streaming_thinking.push_str(&delta);
            }
            DomainEvent::ToolStarted { tool_name, input } => {
                self.messages.push(ChatMessage::ToolCall {
                    name: tool_name,
                    status: ToolStatus::Pending,
                    input,
                });
            }
            DomainEvent::ToolFinished {
                tool_name,
                output,
                is_error,
            } => {
                // Find the matching pending ToolCall and flip its status.
                for m in self.messages.iter_mut().rev() {
                    if let ChatMessage::ToolCall {
                        name,
                        status: s @ ToolStatus::Pending,
                        ..
                    } = m
                    {
                        if name == &tool_name {
                            *s = if is_error {
                                ToolStatus::Error
                            } else {
                                ToolStatus::Done
                            };
                            break;
                        }
                    }
                }
                self.messages.push(ChatMessage::ToolResult {
                    name: tool_name,
                    output,
                    ok: !is_error,
                });
            }
            DomainEvent::AgentDone { final_text } => {
                if !self.streaming.is_empty() {
                    self.messages.push(ChatMessage::Agent(self.streaming.clone()));
                    self.streaming.clear();
                } else if !final_text.is_empty() {
                    self.messages.push(ChatMessage::Agent(final_text));
                }
                self.streaming_thinking.clear();
            }
            DomainEvent::AgentError { message } => {
                self.messages.push(ChatMessage::Info(format!("Error: {message}")));
                self.streaming.clear();
                self.streaming_thinking.clear();
            }
            DomainEvent::ApprovalRequested { batch_id, tools } => {
                use crate::modal::approval::ApprovalModal;
                self.modals.push(Box::new(ApprovalModal::new(batch_id, tools)));
                self.mode = AppMode::ApprovalPending;
            }
            DomainEvent::Tick => {
                // No state change — only used to wake the render loop.
            }
        }
        None
    }

    /// Handle a keyboard event. Returns an Action if a side effect is
    /// required (e.g. user pressed Enter and we need to send a chat).
    pub fn handle_key(&mut self, key: &crossterm::event::KeyEvent) -> Option<Action> {
        // 1. Modal layer has priority.
        if self.modals.top().is_some() {
            // Pop the modal off the stack so we can &mut its inner state
            // without conflicting with &mut self. Push back if not dismissed.
            let mut modal = self.modals.pop().expect("checked above");
            let result = modal.handle_key(key, self);
            match result {
                crate::modal::Consumed::Ignored => {
                    self.modals.push(modal);
                }
                crate::modal::Consumed::Yes { dismiss } => {
                    if !dismiss {
                        self.modals.push(modal);
                    }
                    self.dirty.mark();
                    return None;
                }
            }
        }

        // 2. Global keys (Ctrl+C always quits).
        if key.code == crossterm::event::KeyCode::Char('c')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            self.should_quit = true;
            self.dirty.mark();
            return None;
        }

        // 3. Esc cancels current mode or quits.
        if key.code == crossterm::event::KeyCode::Esc {
            if !self.modals.is_empty() {
                self.modals.pop();
                self.mode = AppMode::Normal;
                self.dirty.mark();
                return None;
            }
            self.should_quit = true;
            self.dirty.mark();
            return None;
        }

        // 4. Otherwise, the focused panel owns the key.
        // Currently we only have InputPanel as a focusable panel.
        if let Some(text) = self.input.handle_key(&key.code) {
            self.dirty.mark();
            return Some(Action::SendChat {
                text,
                session_id: self.session_id.clone(),
            });
        }

        // Any non-input key in normal mode still marks dirty (e.g. Arrow keys
        // could be wired up later for chat scroll).
        if key.code != crossterm::event::KeyCode::Backspace {
            // Backspace already mutates buffer via input.handle_key above,
            // but didn't return Some. We still want to redraw.
            self.dirty.mark();
        }
        None
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::DomainEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn apply_text_chunks_accumulate_into_streaming() {
        let mut s = AppState::new();
        s.apply(DomainEvent::TextChunk { delta: "hel".into() });
        s.apply(DomainEvent::TextChunk { delta: "lo!".into() });
        assert_eq!(s.streaming, "hello!");
        assert!(s.messages.is_empty());
    }

    #[test]
    fn apply_agent_done_promotes_streaming_to_messages() {
        let mut s = AppState::new();
        s.apply(DomainEvent::TextChunk { delta: "hi".into() });
        s.apply(DomainEvent::AgentDone { final_text: "hi".into() });
        assert_eq!(s.streaming, "");
        assert_eq!(s.messages.len(), 1);
        assert!(matches!(s.messages[0], ChatMessage::Agent(ref t) if t == "hi"));
    }

    #[test]
    fn apply_agent_done_with_empty_streaming_uses_final_text() {
        let mut s = AppState::new();
        s.apply(DomainEvent::AgentDone { final_text: "bye".into() });
        assert_eq!(s.messages.len(), 1);
    }

    #[test]
    fn apply_tool_started_then_finished() {
        let mut s = AppState::new();
        s.apply(DomainEvent::ToolStarted {
            tool_name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        });
        assert_eq!(s.messages.len(), 1);
        assert!(matches!(
            s.messages[0],
            ChatMessage::ToolCall { status: ToolStatus::Pending, .. }
        ));
        s.apply(DomainEvent::ToolFinished {
            tool_name: "bash".into(),
            output: "a.txt".into(),
            is_error: false,
        });
        // Pending flipped to Done AND a ToolResult appended.
        assert!(matches!(
            s.messages[0],
            ChatMessage::ToolCall { status: ToolStatus::Done, .. }
        ));
        assert!(matches!(s.messages[1], ChatMessage::ToolResult { ok: true, .. }));
    }

    #[test]
    fn apply_approval_request_pushes_modal() {
        let mut s = AppState::new();
        s.apply(DomainEvent::ApprovalRequested {
            batch_id: "b1".into(),
            tools: vec![ToolInfo {
                call_id: "c1".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({}),
            }],
        });
        assert_eq!(s.modals.len(), 1);
        assert_eq!(s.mode, AppMode::ApprovalPending);
    }

    #[test]
    fn apply_connected_then_disconnected() {
        let mut s = AppState::new();
        s.apply(DomainEvent::Connected);
        assert!(s.connected);
        s.apply(DomainEvent::Disconnected {
            reason: "lost".into(),
        });
        assert!(!s.connected);
    }

    #[test]
    fn apply_marks_dirty() {
        let mut s = AppState::new();
        s.dirty.take(); // clear
        s.apply(DomainEvent::Connected);
        assert!(s.dirty.is_set());
    }

    #[test]
    fn enter_submits_chat_returns_send_action() {
        let mut s = AppState::new();
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        s.handle_key(&key);
        let key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        s.handle_key(&key);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = s.handle_key(&enter);
        assert!(matches!(action, Some(Action::SendChat { ref text, .. }) if text == "hi"));
        assert_eq!(s.input.buffer, "");
    }

    #[test]
    fn esc_quits_when_no_modal() {
        let mut s = AppState::new();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        s.handle_key(&esc);
        assert!(s.should_quit);
    }

    #[test]
    fn esc_dismisses_modal_first() {
        let mut s = AppState::new();
        s.apply(DomainEvent::ApprovalRequested {
            batch_id: "b".into(),
            tools: vec![ToolInfo {
                call_id: "c".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({}),
            }],
        });
        assert!(!s.modals.is_empty());
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        s.handle_key(&esc);
        assert!(s.modals.is_empty());
        assert!(!s.should_quit);
    }

    #[test]
    fn approval_y_sends_approve_action() {
        let mut s = AppState::new();
        s.apply(DomainEvent::ApprovalRequested {
            batch_id: "b".into(),
            tools: vec![ToolInfo {
                call_id: "c1".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({}),
            }],
        });
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        s.handle_key(&y);
        assert!(s.modals.is_empty());
        assert_eq!(s.pending_actions.len(), 1);
        assert!(matches!(
            s.pending_actions[0],
            Action::SendApprove { ref call_id, approved: true } if call_id == "c1"
        ));
    }
}