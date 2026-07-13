//! Application state, message types, and the Reducer.
//!
//! `AppState` is the single source of truth for what the TUI shows.
//! It can only be mutated via:
//! - `apply(event)` — for protocol/domain events
//! - `handle_key(key)` — for keyboard input
//!
//! Both paths automatically mark the dirty flag so the render loop wakes.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dirty::DirtyFlag;
use crate::event::{Action, DomainEvent};
use crate::input::Composer;
use crate::keymap::{KeyAction, KeyMap};
use crate::modal::{
    ModalStack, SessionEntry, SessionStatus, SessionsOverlay, ToolInspector, WorkspaceRollbackModal,
};
pub use crate::model::{
    AppMode, ChatMessage, HistoryRole, RuntimeMetadata, TaskEntry, TaskState, ToolInfo, ToolStatus,
    ToolStepChild,
};

// ===========================================================================
// Top-level state
// ===========================================================================

pub struct AppState {
    pub metadata: RuntimeMetadata,
    pub keymap: KeyMap,
    // ---- business data (read-only for renderers) ----
    pub messages: Vec<ChatMessage>,
    pub streaming: String,
    pub streaming_thinking: String,
    pub session_id: Option<String>,
    pub connected: bool,
    pub protocol_version: Option<u16>,
    pub protocol_capabilities: Vec<String>,
    pub platform: sylvander_protocol::PlatformSnapshot,
    pub status: String,
    pub mode: AppMode,
    pub iteration: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_nano_usd: Option<u64>,
    /// Source session for one safe conversation-only branch undo.
    pub last_branch_source_session_id: Option<String>,
    /// True from local submit until a terminal Done/Error/Interrupted event.
    pub turn_active: bool,
    /// Handles the small window before a new session receives its server id.
    pub interrupt_requested: bool,
    /// Prompts accepted while a turn is active. They are sent one at a time
    /// only after the previous turn reaches a terminal event.
    pub queued_prompts: VecDeque<String>,
    pub queued_prompt_attachments: VecDeque<Vec<sylvander_protocol::MessageAttachment>>,
    /// Whether tool inputs and multi-line results are expanded in transcript.
    pub tool_details_expanded: bool,
    /// Successfully invoked slash commands, newest first. This is deliberately
    /// session-local: command ranking must not become another persistence or
    /// privacy surface.
    pub recent_commands: VecDeque<crate::command::CommandId>,

    /// Local cache of known sessions (newest first) — populated as
    /// `SessionCreated` events arrive. Survives reconnects so the user
    /// can switch back to a previous session even after a server restart.
    pub sessions: Vec<SessionEntry>,
    /// Most recently archived session, retained until one undo or replacement.
    pub last_archived_session: Option<SessionEntry>,

    /// Floating layers (approval, ask, sessions, toast).
    pub modals: ModalStack,

    // ---- focused input ----
    pub composer: Composer,
    /// Chat vertical scroll offset (0 = pinned to bottom).
    pub chat_scroll: usize,
    /// Events received while the viewport is detached from live output.
    pub unread_events: usize,
    /// Quit signal — set by handle_key on Ctrl+C / Esc.
    pub should_quit: bool,

    // ---- pending outbound actions (drained by main loop each tick) ----
    pub pending_actions: Vec<Action>,

    // ---- composer history persistence (opt-in) ----
    /// Path to write the composer history ring to on every submit.
    /// `None` keeps history in memory only.
    pub history_path: Option<std::path::PathBuf>,
    pub draft_path: Option<std::path::PathBuf>,

    /// The session has crossed from an empty entry state into a conversation.
    /// The Welcome remains the transcript prelude after this flips to `true`;
    /// subsequent turns append below it and ordinary scrolling moves it away.
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
        Self::with_metadata(path, RuntimeMetadata::default())
    }

    pub fn with_metadata(path: Option<std::path::PathBuf>, metadata: RuntimeMetadata) -> Self {
        let mut composer = Composer::default();
        if let Some(p) = &path {
            let loaded = Composer::load_history_from(p);
            if !loaded.is_empty() {
                composer.history = loaded;
            }
        }
        let draft_path = path.as_ref().map(|path| path.with_file_name("draft.json"));
        if let Some(path) = &draft_path {
            let _ = composer.restore_draft_from(path);
        }
        let state = Self {
            metadata,
            keymap: KeyMap::default(),
            messages: Vec::new(),
            streaming: String::new(),
            streaming_thinking: String::new(),
            session_id: None,
            connected: false,
            protocol_version: None,
            protocol_capabilities: Vec::new(),
            platform: sylvander_protocol::PlatformSnapshot::default(),
            status: "Connecting...".into(),
            mode: AppMode::Normal,
            iteration: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_nano_usd: None,
            last_branch_source_session_id: None,
            turn_active: false,
            interrupt_requested: false,
            queued_prompts: VecDeque::new(),
            queued_prompt_attachments: VecDeque::new(),
            tool_details_expanded: false,
            recent_commands: VecDeque::new(),
            sessions: Vec::new(),
            last_archived_session: None,
            modals: ModalStack::new(),
            composer,
            chat_scroll: 0,
            unread_events: 0,
            should_quit: false,
            pending_actions: Vec::new(),
            dirty: DirtyFlag::default(),
            history_path: path,
            draft_path,
            welcomed: false,
        };
        state.dirty.mark();
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

    pub fn save_draft(&mut self) {
        if let Some(path) = self.draft_path.clone() {
            if let Err(error) = self.composer.save_draft_to(&path) {
                self.status = format!("draft save failed: {error}");
            }
        }
    }

    /// Move the transcript viewport without touching composer history.
    /// Positive values review older content; negative values move toward live.
    pub fn scroll_transcript(&mut self, lines: isize) {
        if lines >= 0 {
            self.chat_scroll = self.chat_scroll.saturating_add(lines as usize);
        } else {
            self.chat_scroll = self.chat_scroll.saturating_sub(lines.unsigned_abs());
        }
        if self.chat_scroll == 0 {
            self.unread_events = 0;
        }
        self.dirty.mark();
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Reducer — the only way to mutate AppState from the event pipeline
// ===========================================================================

impl AppState {
    /// Apply a domain event. Always marks the dirty flag (whether the
    /// state changed or not — simpler, and the render is cheap).
    pub fn apply(&mut self, event: DomainEvent) -> Option<Action> {
        if matches!(event, DomainEvent::Tick) {
            // A still interface must remain still. Ticks only repaint while
            // elapsed time or an active state can visibly change.
            if self.has_live_activity() {
                self.dirty.mark();
            }
            return None;
        }
        if self.chat_scroll > 0 && event_adds_transcript_content(&event) {
            self.unread_events = self.unread_events.saturating_add(1);
        }
        let action = self.apply_inner(event);
        self.dirty.mark();
        action
    }

    fn has_live_activity(&self) -> bool {
        self.turn_active
            || !self.streaming.is_empty()
            || !self.streaming_thinking.is_empty()
            || self.messages.iter().any(|message| match message {
                ChatMessage::ToolStep { children, .. } => children
                    .iter()
                    .any(|child| child.status == ToolStatus::Pending),
                _ => false,
            })
    }

    fn start_next_queued_prompt(&mut self) -> Option<Action> {
        let text = self.queued_prompts.pop_front()?;
        let attachments = self
            .queued_prompt_attachments
            .pop_front()
            .unwrap_or_default();
        if let Some(message) = self
            .messages
            .iter_mut()
            .find(|message| matches!(message, ChatMessage::QueuedUser(queued) if queued == &text))
        {
            *message = ChatMessage::User(text.clone());
        }
        self.turn_active = true;
        self.interrupt_requested = false;
        self.status = if self.queued_prompts.is_empty() {
            "Working".into()
        } else {
            format!("Working · {} queued", self.queued_prompts.len())
        };
        Some(Action::SendChat {
            text,
            attachments,
            session_id: self.session_id.clone(),
            workspace: self.metadata.workspace.display().to_string(),
        })
    }

    fn apply_inner(&mut self, event: DomainEvent) -> Option<Action> {
        match event {
            DomainEvent::Connected => {
                self.connected = true;
                self.status = "Connected".into();
                return Some(Action::RequestRuntimeInfo);
            }
            DomainEvent::ProtocolNegotiated {
                version,
                server_name,
                capabilities,
            } => {
                self.connected = true;
                self.protocol_version = Some(version);
                self.protocol_capabilities = capabilities;
                self.status = format!("Connected to {server_name} · protocol v{version}");
                if let Some(session_id) = self.session_id.clone() {
                    self.pending_actions.push(Action::RequestRuntimeInfo);
                    self.status = format!("Reattaching session · protocol v{version}");
                    return Some(Action::ReconcileSession { session_id });
                }
                return Some(Action::RequestRuntimeInfo);
            }
            DomainEvent::ProtocolDiagnostic { message } => {
                self.status = "Protocol diagnostic".into();
                self.messages.push(ChatMessage::Info(format!(
                    "protocol · {}",
                    compact_runtime_reason(&message)
                )));
            }
            DomainEvent::RuntimeInfo {
                model,
                reasoning_effort,
                models,
                permissions,
                capabilities,
                approval_enabled,
                max_attachment_bytes,
                platform,
            } => {
                let first_runtime_info = self.metadata.model == "—";
                let changed = self.metadata.model != "—"
                    && (self.metadata.model != model
                        || self.metadata.reasoning_effort != reasoning_effort);
                let permissions_changed = self.metadata.permissions != permissions;
                self.metadata.model = model;
                self.metadata.reasoning_effort = reasoning_effort;
                self.metadata.models = models;
                self.metadata.permissions = permissions;
                self.metadata.capabilities = capabilities;
                self.metadata.approval_enabled = approval_enabled;
                self.metadata.max_attachment_bytes = max_attachment_bytes;
                self.platform = platform;
                let migration = self
                    .metadata
                    .models
                    .iter()
                    .find(|entry| entry.id == self.metadata.model)
                    .and_then(model_migration_label);
                if (first_runtime_info || changed) && migration.is_some() {
                    let migration = migration.expect("checked above");
                    self.status = migration.clone();
                    self.messages.push(ChatMessage::Info(migration));
                } else if changed {
                    self.status = format!(
                        "Model selected · {} · {} · next turn",
                        self.metadata.model,
                        reasoning_label(self.metadata.reasoning_effort)
                    );
                } else if permissions_changed {
                    self.status = "Permissions updated · next turn".into();
                }
            }
            DomainEvent::ContextReported { report } => {
                let percent = if report.context_window == 0 {
                    0
                } else {
                    u64::from(report.used_tokens) * 100 / u64::from(report.context_window)
                };
                let sources = if report.sources.is_empty() {
                    "none yet".to_string()
                } else {
                    report
                        .sources
                        .iter()
                        .map(|source| format!("{} ({})", source.label, source.items))
                        .collect::<Vec<_>>()
                        .join(" · ")
                };
                self.messages.push(ChatMessage::Info(format!(
                    "context · {} · {} / {} tokens ({}%) · {} remaining\ncache · {} read · {} written\nsources · {}",
                    report.model,
                    report.used_tokens,
                    report.context_window,
                    percent,
                    report.remaining_tokens,
                    report.cache_read_tokens,
                    report.cache_write_tokens,
                    sources
                )));
                self.status = "Context report updated".into();
            }
            DomainEvent::CompactionStarted { automatic } => {
                self.status = if automatic {
                    "Auto-compacting context…".into()
                } else {
                    "Compacting context…".into()
                };
                self.messages.push(ChatMessage::Info(if automatic {
                    "context · automatic compaction started".into()
                } else {
                    "context · compaction started".into()
                }));
            }
            DomainEvent::CompactionCompleted { report } => {
                let summary = report
                    .summary
                    .as_deref()
                    .map(compact_runtime_reason)
                    .unwrap_or_else(|| "structural cleanup only".into());
                self.messages.push(ChatMessage::Info(format!(
                    "context · {}compacted · {} messages removed · {} blocks condensed · ~{} tokens freed\nsummary · {}",
                    if report.automatic { "auto-" } else { "" },
                    report.removed_messages,
                    report.condensed_blocks,
                    report.freed_tokens,
                    summary
                )));
                self.status = "Context compacted".into();
            }
            DomainEvent::CompactionFailed { automatic, reason } => {
                self.messages.push(ChatMessage::Info(format!(
                    "context · {}compaction failed · {}",
                    if automatic { "automatic " } else { "" },
                    compact_runtime_reason(&reason)
                )));
                self.status = "Context compaction failed".into();
            }
            DomainEvent::Disconnected { reason } => {
                self.connected = false;
                self.protocol_version = None;
                self.protocol_capabilities.clear();
                self.turn_active = false;
                self.interrupt_requested = false;
                while self.modals.top().is_some_and(|modal| {
                    matches!(
                        modal.title(),
                        "Tool Approval" | "Agent asks" | "Plan review" | "Plan · Edit step"
                    )
                }) {
                    self.modals.pop();
                }
                self.status = format!("Disconnected: {reason}");
                self.messages
                    .push(ChatMessage::Info(format!("Disconnected: {reason}")));
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
                            workspace: self.metadata.workspace.display().to_string(),
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
                if self.interrupt_requested {
                    if let Some(session_id) = self.session_id.clone() {
                        self.pending_actions
                            .push(Action::InterruptTurn { session_id });
                    }
                }
            }
            DomainEvent::SessionsLoaded { sessions } => {
                for session in sessions {
                    let entry = SessionEntry {
                        id: session.id,
                        label: session.label,
                        status: SessionStatus::Complete,
                        workspace: session.workspace,
                        last_seen_secs: session.last_seen_secs,
                    };
                    if let Some(existing) =
                        self.sessions.iter_mut().find(|item| item.id == entry.id)
                    {
                        *existing = entry;
                    } else {
                        self.sessions.push(entry);
                    }
                }
                self.sessions.sort_by_key(|entry| entry.last_seen_secs);
                if self
                    .modals
                    .top()
                    .is_some_and(|modal| modal.title() == "Sessions")
                {
                    self.modals.pop();
                    self.modals
                        .push(Box::new(SessionsOverlay::new(self.sessions.clone())));
                }
            }
            DomainEvent::SessionHistoryLoaded {
                session,
                messages,
                iterations,
                input_tokens,
                output_tokens,
                cost_nano_usd,
                notice,
                source_session_id,
                recovery,
                replay_truncated,
            } => {
                self.session_id = Some(session.id.clone());
                self.metadata.workspace = session.workspace.clone().into();
                self.messages = messages
                    .into_iter()
                    .map(|message| match message.role {
                        HistoryRole::User => ChatMessage::User(message.text),
                        HistoryRole::Assistant => ChatMessage::Agent(message.text),
                        HistoryRole::Tool => ChatMessage::Info(message.text),
                    })
                    .collect();
                self.streaming.clear();
                self.streaming_thinking.clear();
                self.turn_active = false;
                self.interrupt_requested = false;
                if recovery {
                    self.messages.extend(
                        self.queued_prompts
                            .iter()
                            .cloned()
                            .map(ChatMessage::QueuedUser),
                    );
                } else {
                    self.queued_prompts.clear();
                    self.queued_prompt_attachments.clear();
                }
                self.chat_scroll = 0;
                self.unread_events = 0;
                self.iteration = iterations;
                self.input_tokens = input_tokens;
                self.output_tokens = output_tokens;
                self.cost_nano_usd = cost_nano_usd;
                self.last_branch_source_session_id = source_session_id;
                if let Some(notice) = notice {
                    self.messages.push(ChatMessage::Info(notice));
                }
                if replay_truncated {
                    self.messages.push(ChatMessage::Info(
                        "Reconnect replay exceeded 4 MiB; older in-flight details were omitted"
                            .into(),
                    ));
                }
                self.welcomed = !self.messages.is_empty();
                self.status = if recovery {
                    format!("Reattached {}", session.label)
                } else {
                    format!("Resumed {}", session.label)
                };
                if let Some(existing) = self.sessions.iter_mut().find(|item| item.id == session.id)
                {
                    existing.label = session.label;
                    existing.workspace = session.workspace;
                    existing.last_seen_secs = session.last_seen_secs;
                }
            }
            DomainEvent::SessionUpdated {
                session_id,
                label,
                archived,
            } => {
                if archived {
                    self.sessions.retain(|session| session.id != session_id);
                    if self.session_id.as_deref() == Some(session_id.as_str()) {
                        self.session_id = None;
                        self.messages.clear();
                        self.welcomed = false;
                    }
                    self.status = "Session archived".into();
                } else if let Some(label) = label {
                    if let Some(session) = self
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == session_id)
                    {
                        session.label = label.clone();
                    }
                    self.status = format!("Renamed session to {label}");
                } else {
                    self.status = "Archived session restored".into();
                }
            }
            DomainEvent::SessionDeleted { session_id } => {
                self.sessions.retain(|session| session.id != session_id);
                self.last_archived_session = self
                    .last_archived_session
                    .take()
                    .filter(|session| session.id != session_id);
                if self.session_id.as_deref() == Some(session_id.as_str()) {
                    self.session_id = None;
                    self.messages.clear();
                    self.welcomed = false;
                }
                self.status = "Session permanently deleted".into();
            }
            DomainEvent::OperationFailed { operation, message } => {
                self.status = format!("{operation} failed: {message}");
                self.messages.push(ChatMessage::Info(self.status.clone()));
            }
            DomainEvent::TextChunk { delta } => {
                self.turn_active = true;
                self.streaming.push_str(&delta);
            }
            DomainEvent::ThinkingChunk { delta } => {
                self.turn_active = true;
                self.streaming_thinking.push_str(&delta);
            }
            DomainEvent::ModelRetry {
                attempt,
                max_attempts,
                delay_ms,
                reason,
                cause,
            } => {
                self.turn_active = true;
                self.status = format!(
                    "{} · retry {attempt}/{max_attempts}",
                    retry_cause_label(cause)
                );
                self.messages.push(ChatMessage::Info(format!(
                    "{} · retry {attempt}/{max_attempts} in {delay_ms}ms · {}",
                    retry_cause_label(cause),
                    compact_runtime_reason(&reason)
                )));
            }
            DomainEvent::InteractionTimedOut {
                kind,
                subject_id,
                timeout_secs,
                recovery,
            } => {
                let modal_title = match kind {
                    sylvander_protocol::InteractionTimeoutKind::Approval => Some("Tool Approval"),
                    sylvander_protocol::InteractionTimeoutKind::Question => Some("Agent asks"),
                    sylvander_protocol::InteractionTimeoutKind::Plan => Some("Plan review"),
                    _ => None,
                };
                if modal_title.is_some_and(|title| {
                    self.modals.top().is_some_and(|modal| modal.title() == title)
                }) {
                    self.modals.pop();
                    self.mode = AppMode::Normal;
                }
                let short_id = subject_id.chars().take(8).collect::<String>();
                let label = timeout_kind_label(kind);
                let recovery = timeout_recovery_label(recovery);
                self.status = format!("{label} timed out · {recovery}");
                self.messages.push(ChatMessage::Info(format!(
                    "timeout · {label} · {short_id} · {timeout_secs}s\nrecovery · {recovery}"
                )));
            }
            DomainEvent::WorkspaceRollbackPreviewed {
                session_id,
                preview,
            } => {
                self.status = format!("Rollback ready · {} files", preview.files.len());
                self.modals
                    .push(Box::new(WorkspaceRollbackModal::new(session_id, preview)));
            }
            DomainEvent::WorkspaceRollbackCompleted { report } => {
                self.status = format!("Files restored · {}", report.restored.len());
                self.messages.push(ChatMessage::Info(format!(
                    "workspace rollback complete · {} file{} restored\n{}\nconversation history unchanged",
                    report.restored.len(),
                    if report.restored.len() == 1 { "" } else { "s" },
                    report.restored.join("\n")
                )));
            }
            DomainEvent::WorkspaceRollbackFailed { reason } => {
                self.status = "Workspace rollback failed".into();
                self.messages.push(ChatMessage::Info(format!(
                    "workspace rollback failed · {}",
                    compact_runtime_reason(&reason)
                )));
            }
            DomainEvent::WorkspaceDiffLoaded { scope, diff } => {
                if diff.is_empty() {
                    self.status = format!("No {}", scope.label());
                } else {
                    self.status = format!("Loaded {}", scope.label());
                    self.modals.push(Box::new(ToolInspector::new(
                        format!("workspace-diff-{scope:?}").to_ascii_lowercase(),
                        format!("git diff · {}", scope.label()),
                        diff,
                    )));
                }
            }
            DomainEvent::WorkspaceDiffFailed { reason } => {
                self.status = format!(
                    "Workspace diff failed: {}",
                    compact_runtime_reason(&reason)
                );
                self.messages.push(ChatMessage::Info(self.status.clone()));
            }
            DomainEvent::WorkspaceReviewLoaded { scope, diff } => {
                if diff.is_empty() {
                    self.status = format!("No {} to review", scope.label());
                } else if diff.len() > self.metadata.max_attachment_bytes {
                    self.status = format!(
                        "Review diff is too large · {} bytes exceeds model limit {}",
                        diff.len(),
                        self.metadata.max_attachment_bytes
                    );
                    self.messages.push(ChatMessage::Info(self.status.clone()));
                } else {
                    let prompt = "Review the attached workspace changes. Report actionable findings first, ordered by severity, with file and line references. Focus on correctness, regressions, security, and missing tests; keep the summary brief after the findings.".to_string();
                    let attachment = sylvander_protocol::MessageAttachment {
                        id: "workspace-review-diff".into(),
                        kind: sylvander_protocol::AttachmentKind::Diff,
                        name: format!("workspace-{}.diff", scope.label().replace(' ', "-")),
                        mime_type: "text/x-diff".into(),
                        byte_count: diff.len(),
                        content: sylvander_protocol::AttachmentContent::Text { text: diff },
                    };
                    self.messages.push(ChatMessage::User(prompt.clone()));
                    self.turn_active = true;
                    self.status = "Reviewing workspace changes".into();
                    return Some(Action::SendChat {
                        text: prompt,
                        attachments: vec![attachment],
                        session_id: self.session_id.clone(),
                        workspace: self.metadata.workspace.display().to_string(),
                    });
                }
            }
            DomainEvent::WorkspaceReviewFailed { reason } => {
                self.status = format!(
                    "Workspace review failed: {}",
                    compact_runtime_reason(&reason)
                );
                self.messages.push(ChatMessage::Info(self.status.clone()));
            }
            DomainEvent::ConfigInspected { report } => {
                self.status = "Resolved configuration loaded".into();
                self.modals.push(Box::new(ToolInspector::new(
                    "tui-config".into(),
                    "TUI configuration".into(),
                    report,
                )));
            }
            DomainEvent::DoctorCompleted { report, message } => {
                self.status = message;
                if let Some(report) = report {
                    self.modals.push(Box::new(ToolInspector::new(
                        "tui-doctor".into(),
                        "Sylvander doctor · redacted".into(),
                        report,
                    )));
                }
            }
            DomainEvent::DoctorFailed { reason } => {
                self.status = format!("Doctor failed: {}", compact_runtime_reason(&reason));
                self.messages.push(ChatMessage::Info(self.status.clone()));
            }
            DomainEvent::ToolStarted {
                call_id,
                tool_name,
                input,
            } => {
                self.turn_active = true;
                // Group consecutive tool events into a single ToolStep
                // block per UX §6. A new step starts when the last
                // trailing message is not a ToolStep, or when a previous
                // step was already finalized by AgentDone / AgentError.
                let need_new_step =
                    !matches!(self.messages.last(), Some(ChatMessage::ToolStep { .. }));
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
                if let Some(ChatMessage::ToolStep { children, .. }) = self.messages.last_mut() {
                    children.push(ToolStepChild {
                        call_id,
                        name: tool_name,
                        status: ToolStatus::Pending,
                        input,
                        output: None,
                        is_error: None,
                    });
                }
            }
            DomainEvent::ToolOutputDelta {
                call_id,
                tool_name,
                delta,
            } => {
                self.turn_active = true;
                for message in self.messages.iter_mut().rev() {
                    let ChatMessage::ToolStep { children, .. } = message else {
                        continue;
                    };
                    if let Some(child) = children.iter_mut().rev().find(|child| {
                        (!call_id.is_empty() && child.call_id == call_id)
                            || (call_id.is_empty() && child.name == tool_name)
                    }) {
                        child.output.get_or_insert_with(String::new).push_str(&delta);
                        break;
                    }
                }
            }
            DomainEvent::ToolFinished {
                call_id,
                tool_name,
                output,
                is_error,
            } => {
                if let Some(ChatMessage::ToolStep { children, .. }) = self.messages.last_mut() {
                    if let Some(child) = children.iter_mut().rev().find(|child| {
                        (!call_id.is_empty() && child.call_id == call_id)
                            || (call_id.is_empty() && child.name == tool_name)
                    }) {
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
            DomainEvent::UsageUpdated {
                iteration,
                input_tokens,
                output_tokens,
                cost_nano_usd,
            } => {
                self.iteration = iteration;
                self.input_tokens = input_tokens;
                self.output_tokens = output_tokens;
                self.cost_nano_usd = cost_nano_usd;
            }
            DomainEvent::AgentDone { final_text } => {
                self.turn_active = false;
                self.interrupt_requested = false;
                if !self.streaming.is_empty() {
                    self.messages
                        .push(ChatMessage::Agent(self.streaming.clone()));
                    self.streaming.clear();
                } else if !final_text.is_empty() {
                    self.messages.push(ChatMessage::Agent(final_text));
                }
                self.streaming_thinking.clear();
                return self.start_next_queued_prompt();
            }
            DomainEvent::AgentError { message } => {
                self.turn_active = false;
                self.interrupt_requested = false;
                self.messages
                    .push(ChatMessage::Info(format!("Error: {message}")));
                self.streaming.clear();
                self.streaming_thinking.clear();
                return self.start_next_queued_prompt();
            }
            DomainEvent::TurnInterrupted { reason } => {
                self.turn_active = false;
                self.interrupt_requested = false;
                if !self.streaming.is_empty() {
                    self.messages
                        .push(ChatMessage::Agent(std::mem::take(&mut self.streaming)));
                }
                self.streaming_thinking.clear();
                for message in &mut self.messages {
                    if let ChatMessage::ToolStep { children, .. } = message {
                        for child in children {
                            if child.status == ToolStatus::Pending {
                                child.status = ToolStatus::Error;
                                child.output = Some("interrupted".into());
                                child.is_error = Some(true);
                            }
                        }
                    }
                }
                while self.modals.top().is_some_and(|modal| {
                    matches!(
                        modal.title(),
                        "Tool Approval" | "Agent asks" | "Plan review" | "Plan · Edit step"
                    )
                }) {
                    self.modals.pop();
                }
                self.mode = AppMode::Normal;
                self.status = "Interrupted".into();
                self.messages
                    .push(ChatMessage::Info(format!("Turn interrupted: {reason}")));
                return self.start_next_queued_prompt();
            }
            DomainEvent::ApprovalRequested {
                batch_id,
                tools,
                allowed_scopes,
            } => {
                if tools.is_empty() {
                    self.messages.push(ChatMessage::Info(
                        "approval request contained no tools".into(),
                    ));
                    return None;
                }
                use crate::modal::approval::ApprovalModal;
                let mut modal =
                    ApprovalModal::new(batch_id, tools).with_allowed_scopes(allowed_scopes);
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
            DomainEvent::PlanUpdated { plan_id, steps, current } => {
                if let Some(ChatMessage::Plan {
                    steps: visible_steps,
                    current: visible_current,
                    ..
                }) = self.messages.iter_mut().rev().find(|message| {
                    matches!(message, ChatMessage::Plan { plan_id: id, .. } if id == &plan_id)
                }) {
                    *visible_steps = steps;
                    *visible_current = current.min(visible_steps.len().saturating_sub(1));
                } else {
                    self.messages.push(ChatMessage::Plan {
                        plan_id,
                        current: current.min(steps.len().saturating_sub(1)),
                        steps,
                    });
                }
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
                    detail: "started".into(),
                };
                match self.messages.last_mut() {
                    Some(ChatMessage::TaskList { tasks }) => {
                        if let Some(existing) = tasks
                            .iter_mut()
                            .find(|task| task.task_id == entry.task_id)
                        {
                            *existing = entry;
                        } else {
                            tasks.push(entry);
                        }
                    }
                    _ => {
                        self.messages.push(ChatMessage::TaskList {
                            tasks: vec![entry],
                        });
                    }
                }
            }
            DomainEvent::TaskProgress { task_id, message } => {
                update_task(&mut self.messages, &task_id, TaskState::Running, message);
            }
            DomainEvent::TaskCompleted { task_id, summary } => {
                update_task(&mut self.messages, &task_id, TaskState::Done, summary);
            }
            DomainEvent::TaskFailed { task_id, error } => {
                update_task(&mut self.messages, &task_id, TaskState::Failed, error);
            }
            DomainEvent::TaskCancelled { task_id, reason } => {
                update_task(&mut self.messages, &task_id, TaskState::Cancelled, reason);
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
                    self.dirty.mark();
                    return None;
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

        // 2. Global keys — Ctrl+C interrupts active work before it can quit.
        let is_undo_archive = key.code == crossterm::event::KeyCode::Char('z')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL);
        if is_undo_archive {
            if let Some(session) = self.last_archived_session.take() {
                let session_id = session.id.clone();
                self.sessions.insert(0, session);
                self.status = "Restoring archived session…".into();
                self.dirty.mark();
                return Some(Action::RestoreSession { session_id });
            }
        }
        let is_ctrl_c = key.code == crossterm::event::KeyCode::Char('c')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL);
        if is_ctrl_c && self.turn_active {
            if self.interrupt_requested {
                return None;
            }
            self.interrupt_requested = true;
            if let Some(session_id) = self.session_id.clone() {
                self.pending_actions
                    .push(Action::InterruptTurn { session_id });
                self.status = "Interrupting…".into();
            } else {
                self.status = "Interrupt queued until session is created".into();
            }
            self.dirty.mark();
            return None;
        }
        let is_ctrl_x = key.code == crossterm::event::KeyCode::Char('x')
            && key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL);
        if (is_ctrl_c || is_ctrl_x) && !self.turn_active {
            if let Some(text) = self.composer.selected_text() {
                if is_ctrl_x {
                    self.composer.delete_selection();
                }
                self.status = if is_ctrl_x {
                    "Cut selection".into()
                } else {
                    "Copying selection…".into()
                };
                self.dirty.mark();
                return Some(Action::CopyText { text });
            }
        }
        if is_ctrl_c && self.composer.is_empty() {
            self.should_quit = true;
            self.dirty.mark();
            return None;
        }
        // With a draft present Ctrl+C is currently left to the Composer. A
        // dedicated draft-stash interaction is tracked in the readiness list.

        // 3. Ctrl+P toggles the sessions overlay (UX §10).
        if self.keymap.matches(KeyAction::Sessions, key) {
            // If there's an existing modal on top, the modal's own keymap
            // handles Ctrl+P (closes itself). So we only open when nothing
            // is on top.
            if self.modals.is_empty() {
                self.pending_actions.push(Action::RequestSessions);
                let overlay = SessionsOverlay::new(self.sessions.clone());
                self.modals.push(Box::new(overlay));
                self.mode = AppMode::Normal; // overlay isn't modal-blocked
                self.dirty.mark();
            }
            return None;
        }

        if self.keymap.matches(KeyAction::ToolDetails, key) {
            self.tool_details_expanded = !self.tool_details_expanded;
            self.status = if self.tool_details_expanded {
                "Expanded tool details".into()
            } else {
                "Collapsed tool details".into()
            };
            self.dirty.mark();
            return None;
        }

        // 3b. `/` opens the command palette (UX §12) — only when the composer
        //     is empty and no modal is on top.
        let opens_commands = (key.code == crossterm::event::KeyCode::Char('/')
            && key.modifiers == crossterm::event::KeyModifiers::NONE)
            || self.keymap.matches(KeyAction::Commands, key);
        if opens_commands && self.composer.is_empty() && self.modals.is_empty() {
            use crate::modal::palette::CommandPalette;
            let palette = CommandPalette::new(self);
            self.modals.push(Box::new(palette));
            self.dirty.mark();
            return None;
        }

        // 4. Esc interrupts an active turn. It never terminates the Agent.
        if key.code == crossterm::event::KeyCode::Esc {
            if self.turn_active {
                if self.interrupt_requested {
                    return None;
                }
                self.interrupt_requested = true;
                if let Some(session_id) = self.session_id.clone() {
                    self.pending_actions
                        .push(Action::InterruptTurn { session_id });
                    self.status = "Interrupting…".into();
                } else {
                    self.status = "Interrupt queued until session is created".into();
                }
                self.dirty.mark();
                return None;
            }
            self.should_quit = true;
            self.dirty.mark();
            return None;
        }

        if key.code == crossterm::event::KeyCode::Char('@')
            && key.modifiers.is_empty()
            && self.composer.can_open_file_mention()
        {
            self.modals
                .push(Box::new(crate::modal::FileMentionModal::new(
                    self.metadata.workspace.clone(),
                    self.metadata.max_attachment_bytes,
                    self.metadata.supports_vision(),
                )));
            self.dirty.mark();
            return None;
        }

        // Transcript navigation is global while no decision layer owns focus.
        // Returning to the bottom clears the stable unread indicator.
        if self.keymap.matches(KeyAction::TranscriptPageUp, key) {
            self.scroll_transcript(8);
            return None;
        }
        if self.keymap.matches(KeyAction::TranscriptPageDown, key) {
            self.scroll_transcript(-8);
            return None;
        }
        if self.keymap.matches(KeyAction::ReturnLive, key) {
            self.chat_scroll = 0;
            self.unread_events = 0;
            self.dirty.mark();
            return None;
        }

        // 4. Otherwise, the focused panel owns the key.
        // Currently we only have InputPanel as a focusable panel.
        let is_submit = key.code == crossterm::event::KeyCode::Enter && key.modifiers.is_empty();
        if is_submit {
            if let Err(error) = self.composer.validate_attachments(
                self.metadata.max_attachment_bytes,
                self.metadata.supports_vision(),
            ) {
                self.status = error;
                self.dirty.mark();
                return None;
            }
        }
        if let Some(text) = self.composer.handle_key(key) {
            let attachments = self.composer.take_submitted_attachments();
            self.save_history();
            // First submission establishes the Welcome as this session's
            // transcript prelude. It remains above the appended turn.
            self.welcomed = true;
            // The user's turn belongs in the transcript immediately. The
            // server assigns identity and streams the response, but it does
            // not echo the submitted prompt back to this client.
            if self.turn_active {
                self.messages.push(ChatMessage::QueuedUser(text.clone()));
                self.queued_prompts.push_back(text);
                self.queued_prompt_attachments.push_back(attachments);
                self.status = format!("Working · {} queued", self.queued_prompts.len());
                self.chat_scroll = 0;
                self.unread_events = 0;
                self.dirty.mark();
                return None;
            }
            self.messages.push(ChatMessage::User(text.clone()));
            self.turn_active = true;
            self.interrupt_requested = false;
            self.chat_scroll = 0;
            self.unread_events = 0;
            self.dirty.mark();
            return Some(Action::SendChat {
                text,
                attachments,
                session_id: self.session_id.clone(),
                workspace: self.metadata.workspace.display().to_string(),
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

fn update_task(messages: &mut [ChatMessage], task_id: &str, state: TaskState, detail: String) {
    for message in messages.iter_mut().rev() {
        let ChatMessage::TaskList { tasks } = message else {
            continue;
        };
        if let Some(task) = tasks.iter_mut().find(|task| task.task_id == task_id) {
            task.state = state;
            task.detail = detail;
            return;
        }
    }
}

fn event_adds_transcript_content(event: &DomainEvent) -> bool {
    matches!(
        event,
        DomainEvent::TextChunk { .. }
            | DomainEvent::ThinkingChunk { .. }
            | DomainEvent::ModelRetry { .. }
            | DomainEvent::InteractionTimedOut { .. }
            | DomainEvent::AgentDone { .. }
            | DomainEvent::TurnInterrupted { .. }
            | DomainEvent::ToolStarted { .. }
            | DomainEvent::ToolOutputDelta { .. }
            | DomainEvent::ToolFinished { .. }
            | DomainEvent::PlanReceived { .. }
            | DomainEvent::PlanUpdated { .. }
            | DomainEvent::TaskStarted { .. }
            | DomainEvent::TaskProgress { .. }
            | DomainEvent::TaskCompleted { .. }
            | DomainEvent::TaskFailed { .. }
            | DomainEvent::TaskCancelled { .. }
    )
}

fn compact_runtime_reason(reason: &str) -> String {
    const LIMIT: usize = 120;
    let one_line = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= LIMIT {
        return one_line;
    }
    let mut compact = one_line.chars().take(LIMIT - 1).collect::<String>();
    compact.push('…');
    compact
}

pub(crate) fn format_cost(nano_usd: u64) -> String {
    let micro_usd = nano_usd.saturating_add(500) / 1_000;
    format!("${}.{:06}", micro_usd / 1_000_000, micro_usd % 1_000_000)
}

fn retry_cause_label(cause: sylvander_protocol::RetryCause) -> &'static str {
    match cause {
        sylvander_protocol::RetryCause::RateLimit => "Rate limited",
        sylvander_protocol::RetryCause::Server => "Provider unavailable",
        sylvander_protocol::RetryCause::Network => "Network interrupted",
        sylvander_protocol::RetryCause::Stream => "Response stream interrupted",
        sylvander_protocol::RetryCause::Other => "Model retry",
    }
}

pub(crate) fn reasoning_label(effort: sylvander_protocol::ReasoningEffort) -> &'static str {
    match effort {
        sylvander_protocol::ReasoningEffort::Off => "off",
        sylvander_protocol::ReasoningEffort::Low => "low",
        sylvander_protocol::ReasoningEffort::Medium => "medium",
        sylvander_protocol::ReasoningEffort::High => "high",
    }
}

fn model_migration_label(model: &sylvander_protocol::ModelDescriptor) -> Option<String> {
    match &model.lifecycle {
        sylvander_protocol::ModelLifecycle::Active => None,
        sylvander_protocol::ModelLifecycle::Deprecated { replacement } => {
            Some(replacement.as_ref().map_or_else(
                || format!("Model deprecated · {} · choose a supported model", model.id),
                |replacement| format!("Model deprecated · {} → {replacement}", model.id),
            ))
        }
    }
}

fn timeout_kind_label(kind: sylvander_protocol::InteractionTimeoutKind) -> &'static str {
    match kind {
        sylvander_protocol::InteractionTimeoutKind::Approval => "approval",
        sylvander_protocol::InteractionTimeoutKind::Question => "question",
        sylvander_protocol::InteractionTimeoutKind::Plan => "plan review",
        sylvander_protocol::InteractionTimeoutKind::Tool => "tool",
        sylvander_protocol::InteractionTimeoutKind::Task => "background task",
    }
}

fn timeout_recovery_label(recovery: sylvander_protocol::TimeoutRecovery) -> &'static str {
    match recovery {
        sylvander_protocol::TimeoutRecovery::RetryRequest => "ask the Agent to retry the request",
        sylvander_protocol::TimeoutRecovery::NarrowScope => "retry with a narrower scope",
        sylvander_protocol::TimeoutRecovery::ContinueWithout => "continue without this result",
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
    fn connection_requests_and_applies_runtime_truth() {
        let mut state = AppState::new();
        assert!(matches!(
            state.apply(DomainEvent::Connected),
            Some(Action::RequestRuntimeInfo)
        ));
        state.apply(DomainEvent::RuntimeInfo {
            model: "claude-test".into(),
            reasoning_effort: sylvander_protocol::ReasoningEffort::Low,
            models: vec![sylvander_protocol::ModelDescriptor {
                id: "claude-test".into(),
                provider: "test".into(),
                capabilities: 0b10001,
                reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
                lifecycle: sylvander_protocol::ModelLifecycle::Active,
                pricing: None,
            }],
            permissions: sylvander_protocol::PermissionProfile {
                file_access: sylvander_protocol::FileAccess::ReadOnly,
                network_access: sylvander_protocol::NetworkAccess::Denied,
                approval_policy: sylvander_protocol::ApprovalPolicy::Ask,
            },
            capabilities: 0b10001,
            approval_enabled: true,
            max_attachment_bytes: 4096,
            platform: sylvander_protocol::PlatformSnapshot::default(),
        });
        assert_eq!(state.metadata.model, "claude-test");
        assert_eq!(
            state.metadata.reasoning_effort,
            sylvander_protocol::ReasoningEffort::Low
        );
        assert_eq!(state.metadata.models.len(), 1);
        assert_eq!(
            state.metadata.permissions.file_access,
            sylvander_protocol::FileAccess::ReadOnly
        );
        assert_eq!(state.metadata.capabilities, 0b10001);
        assert!(state.metadata.approval_enabled);
        assert_eq!(state.metadata.max_attachment_bytes, 4096);
    }

    #[test]
    fn protocol_negotiation_records_server_truth() {
        let mut state = AppState::new();
        let action = state.apply(DomainEvent::ProtocolNegotiated {
            version: 1,
            server_name: "test-server".into(),
            capabilities: vec!["diagnostics".into()],
        });
        assert!(matches!(action, Some(Action::RequestRuntimeInfo)));
        assert!(state.connected);
        assert_eq!(state.protocol_version, Some(1));
        assert_eq!(state.protocol_capabilities, ["diagnostics"]);
        assert!(state.status.contains("test-server"));
    }

    #[test]
    fn reconnect_requests_reconciliation_and_preserves_the_local_queue() {
        let mut state = AppState::new();
        state.session_id = Some("session-1".into());
        state.queued_prompts.push_back("follow up".into());
        let action = state.apply(DomainEvent::ProtocolNegotiated {
            version: 1,
            server_name: "test-server".into(),
            capabilities: vec!["session_replay".into()],
        });
        assert!(matches!(
            action,
            Some(Action::ReconcileSession { session_id }) if session_id == "session-1"
        ));
        assert!(matches!(
            state.pending_actions.as_slice(),
            [Action::RequestRuntimeInfo]
        ));

        state.apply(DomainEvent::SessionHistoryLoaded {
            session: crate::model::SessionSummary {
                id: "session-1".into(),
                label: "Recovered".into(),
                workspace: "/workspace/project".into(),
                last_seen_secs: 0,
            },
            messages: vec![crate::model::HistoryEntry {
                role: HistoryRole::User,
                text: "active prompt".into(),
            }],
            iterations: 1,
            input_tokens: 10,
            output_tokens: 0,
            cost_nano_usd: Some(0),
            notice: None,
            source_session_id: None,
            recovery: true,
            replay_truncated: false,
        });
        assert_eq!(
            state.queued_prompts.front().map(String::as_str),
            Some("follow up")
        );
        assert!(
            matches!(state.messages.last(), Some(ChatMessage::QueuedUser(text)) if text == "follow up")
        );
        assert_eq!(state.status, "Reattached Recovered");
    }

    #[test]
    fn current_deprecated_model_surfaces_migration_target() {
        let mut state = AppState::new();
        state.apply(DomainEvent::RuntimeInfo {
            model: "old-model".into(),
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            models: vec![sylvander_protocol::ModelDescriptor {
                id: "old-model".into(),
                provider: "test".into(),
                capabilities: 0,
                reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
                lifecycle: sylvander_protocol::ModelLifecycle::Deprecated {
                    replacement: Some("new-model".into()),
                },
                pricing: None,
            }],
            permissions: sylvander_protocol::PermissionProfile::default(),
            capabilities: 0,
            approval_enabled: false,
            max_attachment_bytes: 4096,
            platform: sylvander_protocol::PlatformSnapshot::default(),
        });
        assert_eq!(state.status, "Model deprecated · old-model → new-model");
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::Info(message)) if message.contains("old-model → new-model")
        ));
    }

    #[test]
    fn context_report_renders_provider_usage_cache_and_sources() {
        let mut state = AppState::new();
        state.apply(DomainEvent::ContextReported {
            report: sylvander_protocol::ContextReport {
                model: "deep-code".into(),
                context_window: 200_000,
                used_tokens: 50_000,
                remaining_tokens: 150_000,
                cache_read_tokens: 40_000,
                cache_write_tokens: 2_000,
                sources: vec![sylvander_protocol::ContextSource {
                    kind: sylvander_protocol::ContextSourceKind::Conversation,
                    label: "conversation messages".into(),
                    items: 8,
                }],
            },
        });
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::Info(text))
                if text.contains("50000 / 200000 tokens (25%)")
                    && text.contains("40000 read")
                    && text.contains("conversation messages (8)")
        ));
    }

    #[test]
    fn compaction_lifecycle_is_visible_with_a_bounded_summary() {
        let mut state = AppState::new();
        state.apply(DomainEvent::CompactionStarted { automatic: false });
        assert_eq!(state.status, "Compacting context…");
        state.apply(DomainEvent::CompactionCompleted {
            report: sylvander_protocol::CompactionReport {
                automatic: false,
                removed_messages: 12,
                condensed_blocks: 3,
                freed_tokens: 4_200,
                summary: Some("Kept the architecture decisions and pending tests".into()),
            },
        });
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::Info(text))
                if text.contains("12 messages removed")
                    && text.contains("~4200 tokens freed")
                    && text.contains("architecture decisions")
        ));
    }

    #[test]
    fn composer_copy_and_cut_use_local_clipboard_effects() {
        let mut state = AppState::new();
        for character in "hello".chars() {
            state
                .composer
                .handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        state
            .composer
            .handle_key(&KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
        assert!(matches!(
            state.handle_key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::CopyText { text }) if text == "hello"
        ));
        state
            .composer
            .handle_key(&KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT));
        assert!(matches!(
            state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Action::CopyText { text }) if text == "hello"
        ));
        assert!(state.composer.is_empty());
    }

    #[test]
    fn apply_text_chunks_accumulate_into_streaming() {
        let mut s = AppState::new();
        s.apply(DomainEvent::TextChunk {
            delta: "hel".into(),
        });
        s.apply(DomainEvent::TextChunk {
            delta: "lo!".into(),
        });
        assert_eq!(s.streaming, "hello!");
        assert!(s.messages.is_empty());
    }

    #[test]
    fn model_retry_is_visible_and_bounded_in_transcript() {
        let mut state = AppState::new();
        state.apply(DomainEvent::ModelRetry {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 100,
            reason: format!("provider unavailable {}", "x".repeat(200)),
            cause: sylvander_protocol::RetryCause::RateLimit,
        });
        assert_eq!(state.status, "Rate limited · retry 1/3");
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::Info(text))
                if text.starts_with("Rate limited · retry 1/3 in 100ms")
                    && text.chars().count() < 170
        ));
    }

    #[test]
    fn usage_updates_cost_and_formats_sub_cent_amounts() {
        let mut state = AppState::new();
        state.apply(DomainEvent::UsageUpdated {
            iteration: 2,
            input_tokens: 1_000,
            output_tokens: 100,
            cost_nano_usd: Some(7_500_000),
        });
        assert_eq!(state.cost_nano_usd, Some(7_500_000));
        assert_eq!(format_cost(7_500_000), "$0.007500");
    }

    #[test]
    fn rollback_lifecycle_requires_preview_and_reports_restored_files() {
        let mut state = AppState::new();
        state.apply(DomainEvent::WorkspaceRollbackPreviewed {
            session_id: "s1".into(),
            preview: sylvander_protocol::WorkspaceRollbackPreview {
                turn_id: "turn-1".into(),
                files: vec!["src/lib.rs".into()],
            },
        });
        assert_eq!(
            state.modals.top().map(|modal| modal.title()),
            Some("Rollback files")
        );
        state.apply(DomainEvent::WorkspaceRollbackCompleted {
            report: sylvander_protocol::WorkspaceRollbackReport {
                turn_id: "turn-1".into(),
                restored: vec!["src/lib.rs".into()],
            },
        });
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::Info(text))
                if text.contains("src/lib.rs") && text.contains("conversation history unchanged")
        ));
    }

    #[test]
    fn workspace_review_sends_one_typed_diff_attachment() {
        let mut state = AppState::new();
        state.session_id = Some("s1".into());
        let action = state.apply(DomainEvent::WorkspaceReviewLoaded {
            scope: crate::event::WorkspaceDiffScope::Staged,
            diff: "diff --git a/a.rs b/a.rs\n+fixed\n".into(),
        });
        let Some(Action::SendChat {
            text,
            attachments,
            session_id,
            ..
        }) = action
        else {
            panic!("review send action");
        };
        assert!(text.contains("actionable findings first"));
        assert_eq!(session_id.as_deref(), Some("s1"));
        assert!(matches!(
            attachments.as_slice(),
            [sylvander_protocol::MessageAttachment {
                kind: sylvander_protocol::AttachmentKind::Diff,
                content: sylvander_protocol::AttachmentContent::Text { text },
                ..
            }] if text.contains("+fixed")
        ));
        assert!(state.turn_active);
        assert!(matches!(state.messages.last(), Some(ChatMessage::User(_))));
    }

    #[test]
    fn apply_agent_done_promotes_streaming_to_messages() {
        let mut s = AppState::new();
        s.apply(DomainEvent::TextChunk { delta: "hi".into() });
        s.apply(DomainEvent::AgentDone {
            final_text: "hi".into(),
        });
        assert_eq!(s.streaming, "");
        assert_eq!(s.messages.len(), 1);
        assert!(matches!(s.messages[0], ChatMessage::Agent(ref t) if t == "hi"));
    }

    #[test]
    fn apply_agent_done_with_empty_streaming_uses_final_text() {
        let mut s = AppState::new();
        s.apply(DomainEvent::AgentDone {
            final_text: "bye".into(),
        });
        assert_eq!(s.messages.len(), 1);
    }

    #[test]
    fn apply_tool_started_then_finished_groups_into_step() {
        // Per UX §6 / M-T14.E: consecutive `ToolStarted` + `ToolFinished`
        // events fold into a single `ToolStep` block, not two flat rows.
        // The reducer stores the children inside the step and updates
        // the child's status when the finish lands.
        let mut s = AppState::new();
        s.apply(DomainEvent::ToolStarted {
            call_id: "call-1".into(),
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
            call_id: "call-1".into(),
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
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "ls src"}),
        });
        s.apply(DomainEvent::ToolFinished {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            output: "a.rs".into(),
            is_error: false,
        });
        s.apply(DomainEvent::ToolStarted {
            call_id: "call-2".into(),
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
    fn same_named_tool_results_match_by_call_id() {
        let mut state = AppState::new();
        for call_id in ["first", "second"] {
            state.apply(DomainEvent::ToolStarted {
                call_id: call_id.into(),
                tool_name: "read".into(),
                input: serde_json::json!({"path": format!("{call_id}.rs")}),
            });
        }
        state.apply(DomainEvent::ToolFinished {
            call_id: "first".into(),
            tool_name: "read".into(),
            output: "first result".into(),
            is_error: false,
        });

        let Some(ChatMessage::ToolStep { children, .. }) = state.messages.last() else {
            panic!("expected tool step");
        };
        assert_eq!(children[0].output.as_deref(), Some("first result"));
        assert!(children[1].output.is_none());
    }

    #[test]
    fn partial_tool_output_appends_to_the_matching_pending_call() {
        let mut state = AppState::new();
        state.apply(DomainEvent::ToolStarted {
            call_id: "call-1".into(),
            tool_name: "read".into(),
            input: serde_json::json!({"path": "a.rs"}),
        });
        for delta in ["first ", "second"] {
            state.apply(DomainEvent::ToolOutputDelta {
                call_id: "call-1".into(),
                tool_name: "read".into(),
                delta: delta.into(),
            });
        }
        let ChatMessage::ToolStep { children, .. } = state.messages.last().unwrap() else {
            panic!("expected tool step");
        };
        assert_eq!(children[0].status, ToolStatus::Pending);
        assert_eq!(children[0].output.as_deref(), Some("first second"));
    }

    #[test]
    fn apply_approval_request_pushes_modal() {
        let mut s = AppState::new();
        s.apply(DomainEvent::ApprovalRequested {
            batch_id: "b1".into(),
            allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
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
    fn decision_timeout_closes_stale_modal_and_explains_recovery() {
        let mut state = AppState::new();
        state.apply(DomainEvent::ApprovalRequested {
            batch_id: "batch-1".into(),
            allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
            tools: vec![ToolInfo {
                call_id: "call-123456".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({"command":"cargo test"}),
            }],
        });
        state.apply(DomainEvent::InteractionTimedOut {
            kind: sylvander_protocol::InteractionTimeoutKind::Approval,
            subject_id: "call-123456".into(),
            timeout_secs: 120,
            recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
        });
        assert!(state.modals.is_empty());
        assert_eq!(state.mode, AppMode::Normal);
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::Info(message))
                if message.contains("approval") && message.contains("120s") && message.contains("retry")
        ));
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
    fn submitted_prompt_is_visible_before_the_server_replies() {
        let mut s = AppState::new();
        s.handle_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        s.handle_key(&KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let action = s.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(action, Some(Action::SendChat { .. })));
        assert!(matches!(s.messages.last(), Some(ChatMessage::User(text)) if text == "hi"));
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
    fn esc_interrupts_active_turn_without_quitting() {
        let mut state = AppState::new();
        state.session_id = Some("session-1".into());
        state.turn_active = true;

        state.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(!state.should_quit);
        assert!(state.interrupt_requested);
        assert!(matches!(
            state.pending_actions.as_slice(),
            [Action::InterruptTurn { session_id }] if session_id == "session-1"
        ));
    }

    #[test]
    fn interrupted_turn_settles_partial_output_and_pending_tools() {
        let mut state = AppState::new();
        state.turn_active = true;
        state.streaming = "partial answer".into();
        state.messages.push(ChatMessage::ToolStep {
            name: "Read file".into(),
            started_at_secs: 0,
            children: vec![ToolStepChild {
                call_id: "call-1".into(),
                name: "Read".into(),
                status: ToolStatus::Pending,
                input: serde_json::json!({"path": "README.md"}),
                output: None,
                is_error: None,
            }],
        });

        state.apply(DomainEvent::TurnInterrupted {
            reason: "interrupted by user".into(),
        });

        assert!(!state.turn_active);
        assert!(state.messages.iter().any(
            |message| matches!(message, ChatMessage::Agent(text) if text == "partial answer")
        ));
        assert!(state.messages.iter().any(|message| matches!(
            message,
            ChatMessage::ToolStep { children, .. }
                if children[0].status == ToolStatus::Error
        )));
    }

    #[test]
    fn submit_during_active_turn_queues_without_sending_concurrently() {
        let mut state = AppState::new();
        state.turn_active = true;
        for character in "next request".chars() {
            state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let action = state.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(action.is_none());
        assert_eq!(
            state.queued_prompts.front().map(String::as_str),
            Some("next request")
        );
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::QueuedUser(text)) if text == "next request"
        ));
    }

    #[test]
    fn persisted_session_history_replaces_the_visible_transcript() {
        let mut state = AppState::new();
        state.messages.push(ChatMessage::User("old session".into()));
        state.apply(DomainEvent::SessionHistoryLoaded {
            session: crate::model::SessionSummary {
                id: "s2".into(),
                label: "Restored".into(),
                workspace: "/workspace/project".into(),
                last_seen_secs: 1,
            },
            messages: vec![
                crate::model::HistoryEntry {
                    role: HistoryRole::User,
                    text: "restored question".into(),
                },
                crate::model::HistoryEntry {
                    role: HistoryRole::Assistant,
                    text: "restored answer".into(),
                },
            ],
            iterations: 4,
            input_tokens: 800,
            output_tokens: 120,
            cost_nano_usd: Some(7_500_000),
            notice: None,
            source_session_id: None,
            recovery: false,
            replay_truncated: false,
        });

        assert_eq!(state.session_id.as_deref(), Some("s2"));
        assert_eq!(state.messages.len(), 2);
        assert!(
            matches!(state.messages[0], ChatMessage::User(ref text) if text == "restored question")
        );
        assert!(
            matches!(state.messages[1], ChatMessage::Agent(ref text) if text == "restored answer")
        );
        assert_eq!(
            (state.iteration, state.input_tokens, state.output_tokens),
            (4, 800, 120)
        );
        assert_eq!(state.cost_nano_usd, Some(7_500_000));
    }

    #[test]
    fn rewind_notice_is_kept_in_the_restored_transcript() {
        let mut state = AppState::new();
        state.apply(DomainEvent::SessionHistoryLoaded {
            session: crate::model::SessionSummary {
                id: "rewind-1".into(),
                label: "Work (rewind 1)".into(),
                workspace: "/workspace/project".into(),
                last_seen_secs: 1,
            },
            messages: Vec::new(),
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_nano_usd: Some(0),
            notice: Some("Conversation rewound · workspace files unchanged".into()),
            source_session_id: Some("source-1".into()),
            recovery: false,
            replay_truncated: false,
        });
        assert!(matches!(
            state.messages.last(),
            Some(ChatMessage::Info(text)) if text.contains("workspace files unchanged")
        ));
        assert_eq!(
            state.last_branch_source_session_id.as_deref(),
            Some("source-1")
        );
    }

    #[test]
    fn esc_dismisses_modal_first() {
        let mut s = AppState::new();
        s.apply(DomainEvent::ApprovalRequested {
            batch_id: "b".into(),
            allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
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
            allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
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
            Action::SendApprove { ref call_id, approved: true, .. } if call_id == "c1"
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
    fn transcript_navigation_detaches_and_returns_to_live() {
        let mut s = AppState::new();
        s.handle_key(&KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(s.chat_scroll, 8);

        s.apply(DomainEvent::TextChunk {
            delta: "new output".into(),
        });
        assert_eq!(s.chat_scroll, 8, "streaming must not steal the viewport");
        assert_eq!(s.unread_events, 1);

        s.handle_key(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(s.chat_scroll, 0);
        assert_eq!(s.unread_events, 0);
    }

    #[test]
    fn ctrl_end_returns_directly_to_live() {
        let mut s = AppState::new();
        s.chat_scroll = 40;
        s.unread_events = 7;
        s.handle_key(&KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(s.chat_scroll, 0);
        assert_eq!(s.unread_events, 0);
    }

    #[test]
    fn idle_tick_does_not_schedule_a_repaint() {
        let mut s = AppState::new();
        assert!(s.dirty.take(), "initial frame must render");
        s.apply(DomainEvent::Tick);
        assert!(!s.dirty.take(), "idle terminal must remain still");

        s.streaming.push_str("working");
        s.apply(DomainEvent::Tick);
        assert!(s.dirty.take(), "live output may animate on a tick");
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

    #[test]
    fn background_task_lifecycle_updates_one_stable_transcript_entry() {
        let mut s = AppState::new();
        s.apply(DomainEvent::TaskStarted {
            task_id: "task-1".into(),
            owner: "sylvander".into(),
            purpose: "Inspect tests".into(),
        });
        s.apply(DomainEvent::TaskProgress {
            task_id: "task-1".into(),
            message: "running read".into(),
        });
        s.apply(DomainEvent::TaskCompleted {
            task_id: "task-1".into(),
            summary: "No failures".into(),
        });

        let ChatMessage::TaskList { tasks } = &s.messages[0] else {
            panic!("task list");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::Done);
        assert_eq!(tasks[0].detail, "No failures");
    }

    #[test]
    fn plan_progress_updates_existing_block_without_opening_another_modal() {
        let mut s = AppState::new();
        s.messages.push(ChatMessage::Plan {
            plan_id: "plan-1".into(),
            steps: vec!["inspect".into(), "verify".into()],
            current: 0,
        });
        s.apply(DomainEvent::PlanUpdated {
            plan_id: "plan-1".into(),
            steps: vec!["inspect".into(), "verify".into()],
            current: 1,
        });
        assert_eq!(s.messages.len(), 1);
        assert!(matches!(
            s.messages[0],
            ChatMessage::Plan { current: 1, .. }
        ));
        assert!(s.modals.is_empty());
    }

    #[test]
    fn at_sign_at_token_boundary_opens_file_picker_instead_of_mutating_draft() {
        let mut s = AppState::new();
        s.handle_key(&KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE));
        assert_eq!(
            s.modals.top().map(|modal| modal.title()),
            Some("Mention file")
        );
        assert!(s.composer.is_empty());
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
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("file");
            format!("Read {path}")
        }
        "write" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("file");
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
