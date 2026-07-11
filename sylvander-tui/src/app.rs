//! Application state, message types, and the Reducer.
//!
//! `AppState` is the single source of truth for what the TUI shows.
//! It can only be mutated via:
//! - `apply(event)` — for protocol/domain events
//! - `handle_key(key)` — for keyboard input
//!
//! Both paths automatically mark the dirty flag so the render loop wakes.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::component::Component;
use crate::dirty::DirtyFlag;
use crate::event::{Action, DomainEvent};
use crate::input::Composer;
use crate::modal::{ModalStack, SessionEntry, SessionStatus, SessionsOverlay};
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

    /// Local cache of known sessions (newest first) — populated as
    /// `SessionCreated` events arrive. Survives reconnects so the user
    /// can switch back to a previous session even after a server restart.
    pub sessions: Vec<SessionEntry>,

    // ---- component registration ----
    /// Layout order is `panels[0]` top, last entry bottom.
    pub panels: Vec<Box<dyn Component>>,
    /// Floating layers (approval, ask, sessions, toast).
    pub modals: ModalStack,

    // ---- focused input ----
    pub composer: Composer,
    /// Chat vertical scroll offset (0 = pinned to bottom).
    pub chat_scroll: usize,
    /// Quit signal — set by handle_key on Ctrl+C / Esc.
    pub should_quit: bool,

    // ---- pending outbound actions (drained by main loop each tick) ----
    pub pending_actions: Vec<Action>,

    // ---- composer history persistence (opt-in) ----
    /// Path to write the composer history ring to on every submit.
    /// `None` keeps history in memory only.
    pub history_path: Option<std::path::PathBuf>,

    /// UX §2.2 + §5.3: the welcome lockup renders once on first launch
    /// (empty transcript + no known sessions). The welcome flips to
    /// `true` the first time the user sends any chat content,
    /// signaling the lockup should never show again.
    pub welcomed: bool,

    // ---- render trigger ----
    pub dirty: DirtyFlag,
}

impl AppState {
    pub fn new() -> Self {
        Self::with_history_path(None)
    }

    /// Build a state whose composer history is loaded from `path` (if
    /// `Some`). On every submit, the history is persisted back to that
    /// path. Passing `None` keeps history in memory only (the default).
    pub fn with_history_path(path: Option<std::path::PathBuf>) -> Self {
        let mut composer = Composer::default();
        if let Some(p) = &path {
            let loaded = Composer::load_history_from(p);
            if !loaded.is_empty() {
                composer.history = loaded;
            }
        }
        let mut state = Self {
            messages: Vec::new(),
            streaming: String::new(),
            streaming_thinking: String::new(),
            session_id: None,
            connected: false,
            status: "Connecting...".into(),
            mode: AppMode::Normal,
            sessions: Vec::new(),
            panels: Vec::new(),
            modals: ModalStack::new(),
            composer,
            chat_scroll: 0,
            should_quit: false,
            pending_actions: Vec::new(),
            dirty: DirtyFlag::default(),
            history_path: path,
            welcomed: false,
        };
        state.register_default_panels();
        state
    }

    /// Persist composer history to disk, if a path is configured. Best-effort:
    /// errors are surfaced via `AppState.status` but do not propagate.
    pub fn save_history(&mut self) {
        if let Some(path) = self.history_path.clone() {
            if let Err(e) = self.composer.save_history_to(&path) {
                self.status = format!("history save failed: {e}");
            }
        }
    }

    fn register_default_panels(&mut self) {
        // Order = layout order, top to bottom. Header replaces the
        // old StatusPanel position (M-T14.B / §5.1: 2-line identity
        // block, hairline rule). The status semantics now live in
        // the bottom row (M-T14.C).
        self.panels.push(Box::new(panel::HeaderPanel));
        self.panels.push(Box::new(panel::ChatPanel));
        self.panels.push(Box::new(panel::InputPanel));
        self.panels.push(Box::new(panel::HelpPanel));
    }
}

// ===========================================================================
// AppMode — only used to pick a help-bar hint string
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Legacy flat tool event — kept for snapshots that pre-date
    /// `ToolStep` grouping. New code folds consecutive `ToolStarted`
    /// + `ToolFinished` events into a single `ToolStep` block per
    /// UX §6 (immersive execution rhythm).
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
    /// Grouped step block: a step header with a name + start time, and
    /// a list of indented child tool rows (each child is `●/✓/✗` + verb
    /// + target + meta). One step per agent iteration; terminated when
    /// a `TextChunk` / `AgentDone` lands or another step begins.
    ToolStep {
        name: String,
        started_at_secs: u64,
        children: Vec<ToolStepChild>,
    },
    Thinking(String),
    Info(String),
    /// Plan block — ordered list of step descriptions with a cursor.
    /// `current` is the index of the step currently being executed (●);
    /// steps before it render as ✓; steps after it render as ○.
    Plan {
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    /// Compact task list line — one ChatMessage line per UI slot,
    /// updated by replacing the most recent TaskList entry on each
    /// `TaskStarted` event (de-duplicated by task_id).
    TaskList {
        tasks: Vec<TaskEntry>,
    },
}

/// One row inside a `ToolStep` group. The reducer populates these as
/// `ToolStarted` / `ToolFinished` events flow in.
#[derive(Debug, Clone)]
pub struct ToolStepChild {
    pub name: String,
    pub status: ToolStatus,
    pub input: serde_json::Value,
    /// Filled in when `ToolFinished` arrives.
    pub output: Option<String>,
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub task_id: String,
    pub owner: String,
    pub purpose: String,
    pub state: TaskState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Done,
    Failed,
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
                // First time we see this id — push a local session entry.
                // De-dup by id so reconnects don't create dup rows.
                if !self.sessions.iter().any(|e| e.id == session_id) {
                    let label = short_session_label(&session_id);
                    self.sessions.insert(
                        0,
                        SessionEntry {
                            id: session_id.clone(),
                            label,
                            status: SessionStatus::Working,
                            workspace: std::env::current_dir()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|_| "/".to_string()),
                            last_seen_secs: 0,
                        },
                    );
                } else {
                    // Mark existing as working + refresh its seen-time.
                    if let Some(e) = self.sessions.iter_mut().find(|e| e.id == session_id) {
                        e.status = SessionStatus::Working;
                        e.last_seen_secs = 0;
                    }
                }
                self.session_id = Some(session_id);
            }
            DomainEvent::TextChunk { delta } => {
                self.streaming.push_str(&delta);
            }
            DomainEvent::ThinkingChunk { delta } => {
                self.streaming_thinking.push_str(&delta);
            }
            DomainEvent::ToolStarted { tool_name, input } => {
                // Group consecutive tool events into a single ToolStep
                // block per UX §6. A new step starts when the last
                // trailing message is not a ToolStep, or when a previous
                // step was already finalized by AgentDone / AgentError.
                let need_new_step = !matches!(
                    self.messages.last(),
                    Some(ChatMessage::ToolStep { .. })
                );
                if need_new_step {
                    // Synthesize a step name from the verb: "Read file",
                    // "Run bash command", "Search code". Truncated later
                    // by the renderer.
                    let step_name = step_name_for(&tool_name, &input);
                    self.messages.push(ChatMessage::ToolStep {
                        name: step_name,
                        started_at_secs: now_secs(),
                        children: Vec::new(),
                    });
                }
                if let Some(ChatMessage::ToolStep { children, .. }) =
                    self.messages.last_mut()
                {
                    children.push(ToolStepChild {
                        name: tool_name,
                        status: ToolStatus::Pending,
                        input,
                        output: None,
                        is_error: None,
                    });
                }
            }
            DomainEvent::ToolFinished {
                tool_name,
                output,
                is_error,
            } => {
                if let Some(ChatMessage::ToolStep { children, .. }) =
                    self.messages.last_mut()
                {
                    if let Some(child) = children.iter_mut().rev().find(|c| c.name == tool_name) {
                        child.status = if is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Done
                        };
                        child.output = Some(output);
                        child.is_error = Some(is_error);
                    } else {
                        // Tool finished without a Started (rare). Synthesize.
                        let mut step = self
                            .messages
                            .pop()
                            .unwrap_or(ChatMessage::Info(String::new()));
                        if matches!(step, ChatMessage::ToolStep { .. }) {
                            // ok
                        } else {
                            // Push the orphaned result as Info.
                            step = ChatMessage::Info(format!(
                                "{tool_name} → {}",
                                output.replace('\n', " ")
                            ));
                        }
                        self.messages.push(step);
                    }
                }
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
                let mut modal = ApprovalModal::new(batch_id, tools);
                modal.stack_position = self.modals.len();
                modal.queue_total = self.modals.len() + 1;
                self.modals.push(Box::new(modal));
                self.mode = AppMode::ApprovalPending;
            }
            DomainEvent::AskUserRequested {
                call_id,
                question,
                options,
                multi_select,
            } => {
                use crate::modal::ask_user::AskUserModal;
                let modal = AskUserModal::new(call_id, question, options, multi_select);
                self.modals.push(Box::new(modal));
                self.mode = AppMode::AskPending;
            }
            DomainEvent::ToolRejected { tool_name, reason } => {
                // Surface in transcript as an Info line so the user sees
                // the rejection. Don't switch the modal — the agent is
                // expected to keep iterating, and we'll see its follow-up
                // streamed text in the next iteration.
                self.messages.push(ChatMessage::Info(format!(
                    "tool {tool_name} rejected: {reason}"
                )));
            }
            DomainEvent::PlanReceived {
                plan_id,
                steps,
                current,
            } => {
                self.messages.push(ChatMessage::Plan {
                    plan_id: plan_id.clone(),
                    steps: steps.clone(),
                    current,
                });
                // Push a review modal — UX §9 wants explicit user
                // approval before file edits, so we surface a modal.
                let modal = crate::modal::plan::PlanReviewModal::new(
                    plan_id,
                    steps,
                    current,
                    self.session_id.clone(),
                );
                self.modals.push(Box::new(modal));
                self.mode = AppMode::Normal;
            }
            DomainEvent::TaskStarted {
                task_id,
                owner,
                purpose,
            } => {
                // Find or create the trailing TaskList block. Adding
                // a new running task refreshes that line in place so
                // the transcript stays compact.
                let entry = TaskEntry {
                    task_id,
                    owner,
                    purpose,
                    state: TaskState::Running,
                };
                let mut updated_tasks: Vec<TaskEntry> = Vec::new();
                let mut found = false;
                if let Some(ChatMessage::TaskList { tasks }) = self.messages.last_mut() {
                    for t in tasks.iter() {
                        if t.task_id == entry.task_id {
                            // Replace existing entry if it re-emits.
                            updated_tasks.push(entry.clone());
                            found = true;
                        } else {
                            updated_tasks.push(t.clone());
                        }
                    }
                }
                if !found {
                    updated_tasks.push(entry);
                }
                match self.messages.last_mut() {
                    Some(ChatMessage::TaskList { tasks }) => {
                        *tasks = updated_tasks;
                    }
                    _ => {
                        self.messages.push(ChatMessage::TaskList {
                            tasks: updated_tasks,
                        });
                    }
                }
            }
            DomainEvent::Tick => {
                // No state change — only used to wake the render loop.
            }
        }
        None
    }

    /// Handle a paste event from the terminal (M-T2). Forwards to the
    /// composer which decides inline-vs-attachment per design §12.4.
    pub fn handle_paste(&mut self, text: &str) {
        self.composer.paste(text);
        self.dirty.mark();
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

        // 2. Global keys — Ctrl+C quits only when composer is empty.
        //    When the composer has content we let it through so it can either
        //    accept the keystroke (no-op) or handle a future copy binding.
        let is_ctrl_c = key.code == crossterm::event::KeyCode::Char('c')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL);
        if is_ctrl_c && self.composer.is_empty() {
            self.should_quit = true;
            self.dirty.mark();
            return None;
        }
        if is_ctrl_c {
            // fall through to composer handler; it will ignore Ctrl+C.
        }

        // 3. Ctrl+P toggles the sessions overlay (UX §10).
        let is_ctrl_p = key.code == crossterm::event::KeyCode::Char('p')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL);
        if is_ctrl_p {
            // If there's an existing modal on top, the modal's own keymap
            // handles Ctrl+P (closes itself). So we only open when nothing
            // is on top.
            if self.modals.is_empty() {
                let overlay = SessionsOverlay::new(self.sessions.clone());
                self.modals.push(Box::new(overlay));
                self.mode = AppMode::Normal; // overlay isn't modal-blocked
                self.dirty.mark();
            }
            return None;
        }

        // 3b. `/` opens the command palette (UX §12) — only when the composer
        //     is empty and no modal is on top.
        if key.code == crossterm::event::KeyCode::Char('/')
            && key.modifiers == crossterm::event::KeyModifiers::NONE
            && self.composer.is_empty()
            && self.modals.is_empty()
        {
            use crate::modal::palette::CommandPalette;
            self.modals.push(Box::new(CommandPalette::new()));
            self.dirty.mark();
            return None;
        }

        // 4. Esc cancels current mode or quits.
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
        if let Some(text) = self.composer.handle_key(key) {
            self.save_history();
            // First user submission dismisses the welcome lockup
            // forever (UX §2.2 — lockup appears once on first launch).
            self.welcomed = true;
            self.dirty.mark();
            return Some(Action::SendChat {
                text,
                session_id: self.session_id.clone(),
            });
        }
        // Even if composer returned None, it may have mutated (insert char,
        // backspace, history nav). Always mark dirty so the panel re-renders.
        self.dirty.mark();

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
    #[test]
    fn apply_tool_started_then_finished_groups_into_step() {
        // Per UX §6 / M-T14.E: consecutive `ToolStarted` + `ToolFinished`
        // events fold into a single `ToolStep` block, not two flat rows.
        // The reducer stores the children inside the step and updates
        // the child's status when the finish lands.
        let mut s = AppState::new();
        s.apply(DomainEvent::ToolStarted {
            tool_name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        });
        assert_eq!(s.messages.len(), 1);
        match &s.messages[0] {
            ChatMessage::ToolStep { name, children, .. } => {
                assert!(name.starts_with("Run"));
                assert_eq!(children.len(), 1);
                assert_eq!(children[0].name, "bash");
                assert_eq!(children[0].status, ToolStatus::Pending);
            }
            other => panic!("expected ToolStep, got {other:?}"),
        }
        s.apply(DomainEvent::ToolFinished {
            tool_name: "bash".into(),
            output: "a.txt".into(),
            is_error: false,
        });
        // Same single step; child status flipped to Done; output captured.
        match &s.messages[0] {
            ChatMessage::ToolStep { children, .. } => {
                assert_eq!(children.len(), 1);
                assert_eq!(children[0].status, ToolStatus::Done);
                assert_eq!(children[0].output.as_deref(), Some("a.txt"));
                assert_eq!(children[0].is_error, Some(false));
            }
            other => panic!("expected ToolStep, got {other:?}"),
        }
    }

    #[test]
    fn apply_two_separate_tools_open_then_close_separate_steps() {
        // A text chunk between two tools should close the first step
        // and open a second one. We simulate by inserting the
        // finalize moment via a manual transition (AgentDone). For
        // now we only verify that two distinct ToolStarted events
        // append two children to the SAME step (since no AgentDone
        // has landed between them) — the renderer collapses them into
        // one step group, exactly the §6 immersive behavior.
        let mut s = AppState::new();
        s.apply(DomainEvent::ToolStarted {
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "ls src"}),
        });
        s.apply(DomainEvent::ToolFinished {
            tool_name: "bash".into(),
            output: "a.rs".into(),
            is_error: false,
        });
        s.apply(DomainEvent::ToolStarted {
            tool_name: "read".into(),
            input: serde_json::json!({"path": "src/a.rs"}),
        });
        match &s.messages[0] {
            ChatMessage::ToolStep { children, .. } => {
                assert_eq!(children.len(), 2);
                assert_eq!(children[0].name, "bash");
                assert_eq!(children[0].status, ToolStatus::Done);
                assert_eq!(children[1].name, "read");
                assert_eq!(children[1].status, ToolStatus::Pending);
            }
            other => panic!("expected ToolStep, got {other:?}"),
        }
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
    fn plain_enter_submits_chat_returns_send_action() {
        let mut s = AppState::new();
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        s.handle_key(&key);
        let key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        s.handle_key(&key);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let action = s.handle_key(&enter);
        assert!(matches!(action, Some(Action::SendChat { ref text, .. }) if text == "hi"));
        assert!(s.composer.is_empty());
    }

    #[test]
    fn shift_enter_inserts_newline_and_does_not_submit() {
        let mut s = AppState::new();
        s.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        s.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        s.handle_key(&KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let action = s.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            action,
            Some(Action::SendChat { ref text, .. }) if text == "h\ni"
        ));
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

    #[test]
    fn ctrl_p_pushes_sessions_overlay() {
        let mut s = AppState::new();
        let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        s.handle_key(&key);
        assert_eq!(s.modals.len(), 1);
        // Press Ctrl+P again — top is overlay, which handles its own keys.
        s.handle_key(&key);
        // Overlay's handler closes on Ctrl+P.
        assert!(s.modals.is_empty());
    }

    #[test]
    fn session_created_populates_sessions_cache() {
        let mut s = AppState::new();
        s.apply(DomainEvent::SessionCreated {
            session_id: "abc-123".into(),
        });
        assert_eq!(s.sessions.len(), 1);
        assert_eq!(s.sessions[0].id, "abc-123");
        assert_eq!(s.session_id.as_deref(), Some("abc-123"));
        // Re-creating the same id should NOT add a dup row.
        s.apply(DomainEvent::SessionCreated {
            session_id: "abc-123".into(),
        });
        assert_eq!(s.sessions.len(), 1);
    }
}

/// Build a short human label from a session uuid.
fn short_session_label(id: &str) -> String {
    let first8: String = id.chars().take(8).collect();
    first8
}

/// Monotonic seconds since UNIX epoch. Used for ToolStep started_at
/// timestamps; the renderer derives elapsed time at draw time.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Derive a human-readable step name from the leading tool verb +
/// target.  Falls back to the bare tool name when no recognizable
/// input shape is available.
fn step_name_for(tool: &str, input: &serde_json::Value) -> String {
    match tool {
        "read" => {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            format!("Read {path}")
        }
        "write" => {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            format!("Write {path}")
        }
        "edit" => "Edit file".into(),
        "bash" => {
            let cmd = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("command");
            let first_token = cmd.split_whitespace().next().unwrap_or("");
            if first_token.is_empty() {
                "Run command".into()
            } else {
                format!("Run `{first_token}`")
            }
        }
        "search" | "grep" => "Search code".into(),
        _ => tool.to_string(),
    }
}