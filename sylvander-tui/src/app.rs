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
    ModalStack, ProfileDeleteModal, ProfileEditMode, ProfileEditor, SessionEntry, SessionStatus,
    SessionsOverlay, ToolInspector, WorkspaceRollbackModal,
};

const MAX_TRANSCRIPT_ENTRIES: usize = 2_000;
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_TOOL_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_GROUP_ITEMS: usize = 64;
const MAX_QUEUED_PROMPTS: usize = 100;
const MAX_SESSION_CACHE: usize = 5_000;
const TRANSCRIPT_PRUNED_NOTICE: &str =
    "transcript · older entries omitted from the local view; /resume reloads persisted history";
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
    pub agents: Vec<sylvander_protocol::AgentDescriptor>,
    pub selected_agent_id: Option<sylvander_protocol::AgentId>,
    pub session_config: Option<sylvander_protocol::SessionConfigState>,
    pub pending_session_prompt: Option<(String, Vec<sylvander_protocol::MessageAttachment>)>,
    pub session_creation_pending: bool,
    pub session_model_override: Option<(
        sylvander_protocol::ModelSelection,
        sylvander_protocol::ReasoningEffort,
    )>,
    pub connected: bool,
    pub protocol_version: Option<u16>,
    pub protocol_capabilities: Vec<String>,
    /// Latest owner-scoped profile returned by Runtime. Mutations always bind
    /// to this server revision; conflict responses invalidate the cache.
    pub user_profile: Option<sylvander_protocol::UserProfileView>,
    /// Server-issued opaque handle for feedback about the most recently
    /// completed turn in this single-session view.
    pub feedback_target: Option<sylvander_protocol::FeedbackTarget>,
    pub(crate) pending_profile_intent: Option<PendingProfileIntent>,
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
    /// Set only when a desktop host supplied a complete local capability bridge.
    pub host_preview_available: bool,

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
    /// Maximum visual-row offset reported by the latest terminal layout.
    pub chat_scroll_limit: usize,
    /// Events received while the viewport is detached from live output.
    pub unread_events: usize,
    /// Quit signal — set by `handle_key` on Ctrl+C / Esc.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingProfileIntent {
    Edit { correction: bool },
    SetDoNotLearn(bool),
    Delete,
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
            agents: Vec::new(),
            selected_agent_id: None,
            session_config: None,
            pending_session_prompt: None,
            session_creation_pending: false,
            session_model_override: None,
            connected: false,
            protocol_version: None,
            protocol_capabilities: Vec::new(),
            user_profile: None,
            feedback_target: None,
            pending_profile_intent: None,
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
            host_preview_available: false,
            sessions: Vec::new(),
            last_archived_session: None,
            modals: ModalStack::new(),
            composer,
            chat_scroll: 0,
            chat_scroll_limit: 0,
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
        if let Some(path) = self.history_path.clone()
            && let Err(e) = self.composer.save_history_to(&path)
        {
            self.status = format!("history save failed: {e}");
        }
    }

    pub(crate) fn sync_decision_dock_mode(&mut self) {
        self.mode = if self
            .modals
            .iter()
            .any(|modal| matches!(modal.title(), "Tool Approval" | "Memory confirmation"))
        {
            AppMode::ApprovalPending
        } else {
            AppMode::Normal
        };
    }

    pub fn save_draft(&mut self) {
        if let Some(path) = self.draft_path.clone()
            && let Err(error) = self.composer.save_draft_to(&path)
        {
            self.status = format!("draft save failed: {error}");
        }
    }

    /// Move the transcript viewport without touching composer history.
    /// Positive values review older content; negative values move toward live.
    pub fn scroll_transcript(&mut self, lines: isize) {
        let previous = self.chat_scroll;
        if lines >= 0 {
            self.chat_scroll = self
                .chat_scroll
                .saturating_add(lines as usize)
                .min(self.chat_scroll_limit);
        } else {
            self.chat_scroll = self.chat_scroll.saturating_sub(lines.unsigned_abs());
        }
        if self.chat_scroll == 0 {
            self.unread_events = 0;
        }
        if self.chat_scroll != previous {
            self.dirty.mark();
        }
    }

    /// Apply presentation-owned scroll bounds without letting repeated input at
    /// the top accumulate an invisible offset. If the viewport was pinned to
    /// the oldest row, keep it pinned when wrapping or incoming content changes
    /// the measured line count.
    pub fn set_chat_scroll_limit(&mut self, limit: usize) {
        let was_at_top = self.chat_scroll_limit > 0 && self.chat_scroll == self.chat_scroll_limit;
        self.chat_scroll_limit = limit;
        self.chat_scroll = if was_at_top {
            limit
        } else {
            self.chat_scroll.min(limit)
        };
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
        self.enforce_memory_budget();
        self.dirty.mark();
        action
    }

    pub(crate) fn enforce_memory_budget(&mut self) {
        for message in &mut self.messages {
            normalize_message(message);
        }
        truncate_utf8(&mut self.streaming, MAX_MESSAGE_BYTES);
        truncate_utf8(&mut self.streaming_thinking, MAX_MESSAGE_BYTES);

        let mut pruned = false;
        if self.messages.len() > MAX_TRANSCRIPT_ENTRIES {
            let remove = self.messages.len() - MAX_TRANSCRIPT_ENTRIES;
            self.messages.drain(..remove);
            pruned = true;
        }
        let byte_budget = MAX_TRANSCRIPT_BYTES.saturating_sub(TRANSCRIPT_PRUNED_NOTICE.len());
        let mut bytes = self.messages.iter().map(message_bytes).sum::<usize>();
        let mut remove = 0;
        while bytes > byte_budget && self.messages.len().saturating_sub(remove) > 1 {
            bytes = bytes.saturating_sub(message_bytes(&self.messages[remove]));
            remove += 1;
        }
        if remove > 0 {
            self.messages.drain(..remove);
            pruned = true;
        }
        if pruned {
            if !matches!(self.messages.first(), Some(ChatMessage::Info(text)) if text == TRANSCRIPT_PRUNED_NOTICE)
            {
                if self.messages.len() == MAX_TRANSCRIPT_ENTRIES {
                    self.messages.remove(0);
                }
                self.messages
                    .insert(0, ChatMessage::Info(TRANSCRIPT_PRUNED_NOTICE.into()));
            }
            self.chat_scroll = 0;
            self.unread_events = 0;
        }
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
        self.feedback_target = None;
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

    /// Enter one prompt through the same transcript, queue, and transport
    /// boundary used by the composer. Dynamic commands call this after their
    /// typed UI effect has been validated.
    pub(crate) fn submit_prompt(
        &mut self,
        text: String,
        attachments: Vec<sylvander_protocol::MessageAttachment>,
    ) -> Option<Action> {
        self.welcomed = true;
        if self.turn_active {
            if self.queued_prompts.len() >= MAX_QUEUED_PROMPTS {
                self.status =
                    "Prompt queue full · remove an item with /queue remove before adding more"
                        .into();
                self.dirty.mark();
                return None;
            }
            self.messages.push(ChatMessage::QueuedUser(text.clone()));
            self.queued_prompts.push_back(text);
            self.queued_prompt_attachments.push_back(attachments);
            self.status = format!("Working · {} queued", self.queued_prompts.len());
            self.chat_scroll = 0;
            self.unread_events = 0;
            self.dirty.mark();
            self.enforce_memory_budget();
            return None;
        }
        self.messages.push(ChatMessage::User(text.clone()));
        self.turn_active = true;
        self.interrupt_requested = false;
        self.feedback_target = None;
        self.chat_scroll = 0;
        self.unread_events = 0;
        self.dirty.mark();
        self.enforce_memory_budget();
        if self.session_id.is_none() {
            self.pending_session_prompt = Some((text, attachments));
            self.status = "Creating session…".into();
            let action = self.create_session_action();
            self.session_creation_pending = action.is_some();
            return action;
        }
        Some(Action::SendChat {
            text,
            attachments,
            session_id: self.session_id.clone(),
            workspace: self.metadata.workspace.display().to_string(),
        })
    }

    pub(crate) fn create_session_action(&self) -> Option<Action> {
        let agent_id = self.selected_agent_id.clone()?;
        let mut overrides = sylvander_protocol::SessionConfigOverrides {
            user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: self.metadata.workspace.clone(),
                read_only: false,
                instruction_focus: None,
            }),
            ..Default::default()
        };
        if let Some((model, effort)) = &self.session_model_override {
            overrides.model = Some(model.clone());
            overrides.reasoning_effort = Some(*effort);
        }
        Some(Action::CreateSession {
            request: Box::new(sylvander_protocol::SessionCreateRequest {
                agent_id,
                label: "New session".into(),
                channel_id: None,
                overrides,
            }),
        })
    }

    fn apply_inner(&mut self, event: DomainEvent) -> Option<Action> {
        match event {
            DomainEvent::Connected => {
                self.connected = true;
                self.status = "Connected".into();
                self.pending_actions.push(Action::DiscoverAgents);
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
                self.pending_actions.push(Action::DiscoverAgents);
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
                if (first_runtime_info || changed)
                    && let Some(migration) = migration
                {
                    self.status.clone_from(&migration);
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
                    .as_deref().map_or_else(|| "structural cleanup only".into(), compact_runtime_reason);
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
                self.user_profile = None;
                self.pending_profile_intent = None;
                self.turn_active = false;
                self.interrupt_requested = false;
                while self.modals.top().is_some_and(|modal| {
                    matches!(
                        modal.title(),
                        "Tool Approval"
                            | "Agent asks"
                            | "Plan review"
                            | "Plan editor"
                            | "Plan · Edit step"
                    )
                }) {
                    self.modals.pop();
                }
                self.status = format!("Disconnected: {reason}");
                self.messages
                    .push(ChatMessage::Info(format!("Disconnected: {reason}")));
            }
            DomainEvent::AgentsDiscovered { agents } => {
                self.agents = agents;
                if self
                    .selected_agent_id
                    .as_ref()
                    .is_none_or(|selected| !self.agents.iter().any(|agent| &agent.id == selected))
                {
                    self.selected_agent_id = self.agents.first().map(|agent| agent.id.clone());
                }
                if self.pending_session_prompt.is_some() && !self.session_creation_pending {
                    let action = self.create_session_action();
                    self.session_creation_pending = action.is_some();
                    return action;
                }
                self.status = match self.agents.len() {
                    0 => "No Agents available".into(),
                    count => format!("{count} Agent{} available", if count == 1 { "" } else { "s" }),
                };
            }
            DomainEvent::SessionConfigLoaded { state } => {
                self.selected_agent_id = Some(state.effective.agent_id.clone());
                self.metadata.model.clone_from(&state.effective.model_id);
                self.metadata.reasoning_effort = state.effective.reasoning_effort;
                self.session_config = Some(state);
            }
            DomainEvent::SessionCreated { session_id, config } => {
                self.session_creation_pending = false;
                // First time we see this id — push a local session entry.
                // De-dup by id so reconnects don't create dup rows.
                if self.sessions.iter().any(|e| e.id == session_id) {
                    // Mark existing as working + refresh its seen-time.
                    if let Some(e) = self.sessions.iter_mut().find(|e| e.id == session_id) {
                        e.status = SessionStatus::Working;
                        e.last_seen_secs = 0;
                    }
                } else {
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
                    self.sessions.truncate(MAX_SESSION_CACHE);
                }
                self.session_id = Some(session_id);
                if let Some(config) = config {
                    self.selected_agent_id = Some(config.effective.agent_id.clone());
                    self.metadata.model.clone_from(&config.effective.model_id);
                    self.metadata.reasoning_effort = config.effective.reasoning_effort;
                    self.session_config = Some(config);
                }
                if let Some((text, attachments)) = self.pending_session_prompt.take() {
                    return Some(Action::SendChat {
                        text,
                        attachments,
                        session_id: self.session_id.clone(),
                        workspace: self.metadata.workspace.display().to_string(),
                    });
                }
                if self.interrupt_requested
                    && let Some(session_id) = self.session_id.clone() {
                        self.pending_actions
                            .push(Action::InterruptTurn { session_id });
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
                self.sessions.truncate(MAX_SESSION_CACHE);
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
                if self
                    .protocol_capabilities
                    .iter()
                    .any(|capability| {
                        capability == sylvander_protocol::MEMORY_CONFIRMATION_CAPABILITY
                    })
                {
                    self.pending_actions
                        .push(Action::RequestMemoryConfirmations {
                            session_id: session.id,
                        });
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
                        session.label.clone_from(&label);
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
                if operation == "create_session" {
                    self.session_creation_pending = false;
                    self.pending_session_prompt = None;
                    self.turn_active = false;
                }
                self.status = format!("{operation} failed: {message}");
                self.messages.push(ChatMessage::Info(self.status.clone()));
            }
            DomainEvent::UserProfileReceived { response } => {
                self.apply_user_profile_response(response);
            }
            DomainEvent::FeedbackRecorded { feedback_id } => {
                self.status = "Feedback recorded".into();
                self.messages.push(ChatMessage::Info(format!(
                    "feedback · recorded · {}",
                    feedback_id.chars().take(8).collect::<String>()
                )));
            }
            DomainEvent::MemoryConfirmationsLoaded {
                session_id,
                confirmations,
            } => {
                if self.session_id.as_deref() != Some(session_id.as_str()) {
                    return None;
                }
                let mut added = 0usize;
                for confirmation in confirmations.into_iter().rev() {
                    if self.modals.is_full() {
                        self.status =
                            "Memory confirmation queue is full; pending decisions remain".into();
                        break;
                    }
                    self.modals.push(Box::new(
                        crate::modal::MemoryConfirmationModal::new(
                            session_id.clone(),
                            confirmation,
                        ),
                    ));
                    added += 1;
                }
                if added > 0 {
                    self.mode = AppMode::ApprovalPending;
                    self.status = format!(
                        "{added} memory confirmation{} pending",
                        if added == 1 { "" } else { "s" }
                    );
                }
            }
            DomainEvent::MemoryConfirmationRecorded {
                candidate_id,
                decision,
            } => {
                let verb = match decision {
                    sylvander_protocol::MemoryConfirmationDecision::Confirm => "saved",
                    sylvander_protocol::MemoryConfirmationDecision::Reject => "not saved",
                };
                self.status = format!("Memory {verb}");
                self.messages.push(ChatMessage::Info(format!(
                    "memory · {verb} · {}",
                    candidate_id.chars().take(8).collect::<String>()
                )));
            }
            DomainEvent::MemoryConfirmationFailed { message } => {
                self.status.clone_from(&message);
                self.messages
                    .push(ChatMessage::Info(format!("memory decision · {message}")));
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
                    self.modals.top().is_some_and(|modal| {
                        modal.title() == title
                            || (kind == sylvander_protocol::InteractionTimeoutKind::Plan
                                && matches!(modal.title(), "Plan editor" | "Plan · Edit step"))
                    })
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
            DomainEvent::CodingSessionDiffLoaded { status, patch } => {
                let output = match (status.trim().is_empty(), patch.trim().is_empty()) {
                    (true, true) => String::new(),
                    (false, true) => format!("git status --short\n{status}"),
                    (true, false) => patch,
                    (false, false) => format!("git status --short\n{status}\n\ngit diff HEAD\n{patch}"),
                };
                if output.is_empty() {
                    self.status = "Coding session has no changes".into();
                } else {
                    self.status = "Loaded coding session changes".into();
                    self.modals.push(Box::new(ToolInspector::new(
                        "coding-session-diff".into(),
                        "coding session · git diff".into(),
                        output,
                    )));
                }
            }
            DomainEvent::CodingSessionAccepted => {
                self.status = "Coding changes merged".into();
                self.messages.push(ChatMessage::Info(
                    "coding session accepted · reviewed changes merged".into(),
                ));
            }
            DomainEvent::CodingSessionDiscarded => {
                self.status = "Coding session discarded · new session ready".into();
                self.session_id = None;
                self.messages.clear();
                self.streaming.clear();
                self.streaming_thinking.clear();
                self.welcomed = false;
            }
            DomainEvent::CodingSessionOperationFailed { operation, reason } => {
                self.status = format!(
                    "Coding session {operation} failed: {}",
                    compact_runtime_reason(&reason)
                );
                self.messages.push(ChatMessage::Info(self.status.clone()));
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
                let mut found = false;
                for message in self.messages.iter_mut().rev() {
                    let ChatMessage::ToolStep { children, .. } = message else {
                        continue;
                    };
                    if let Some(child) = children.iter_mut().rev().find(|child| {
                        (!call_id.is_empty() && child.call_id == call_id)
                            || (call_id.is_empty() && child.name == tool_name)
                    }) {
                        append_live_tool_output(
                            child.output.get_or_insert_with(String::new),
                            &delta,
                        );
                        found = true;
                        break;
                    }
                }
                if !found {
                    self.messages.push(ChatMessage::ToolStep {
                        name: step_name_for(&tool_name, &serde_json::Value::Null),
                        started_at_secs: now_secs(),
                        children: vec![ToolStepChild {
                            call_id,
                            name: tool_name,
                            status: ToolStatus::Pending,
                            input: serde_json::Value::Null,
                            output: Some(bounded_live_tool_output(delta)),
                            is_error: None,
                        }],
                    });
                }
            }
            DomainEvent::ToolFinished {
                call_id,
                tool_name,
                output,
                is_error,
            } => {
                let mut result = Some(output);
                let mut found = false;
                for message in self.messages.iter_mut().rev() {
                    let ChatMessage::ToolStep { children, .. } = message else {
                        continue;
                    };
                    if let Some(child) = children.iter_mut().rev().find(|child| {
                        (!call_id.is_empty() && child.call_id == call_id)
                            || (call_id.is_empty() && child.name == tool_name)
                    }) {
                        child.status = if is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Done
                        };
                        child.output = result.take();
                        child.is_error = Some(is_error);
                        found = true;
                        break;
                    }
                }
                if !found {
                    self.messages.push(ChatMessage::ToolStep {
                        name: step_name_for(&tool_name, &serde_json::Value::Null),
                        started_at_secs: now_secs(),
                        children: vec![ToolStepChild {
                            call_id,
                            name: tool_name,
                            status: if is_error {
                                ToolStatus::Error
                            } else {
                                ToolStatus::Done
                            },
                            input: serde_json::Value::Null,
                            output: result,
                            is_error: Some(is_error),
                        }],
                    });
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
            DomainEvent::AgentDone {
                final_text,
                feedback_target,
            } => {
                self.turn_active = false;
                self.interrupt_requested = false;
                self.feedback_target = feedback_target;
                if !self.streaming.is_empty() {
                    self.messages
                        .push(ChatMessage::Agent(self.streaming.clone()));
                    self.streaming.clear();
                } else if !final_text.is_empty() {
                    self.messages.push(ChatMessage::Agent(final_text));
                }
                self.streaming_thinking.clear();
                if self
                    .protocol_capabilities
                    .iter()
                    .any(|capability| {
                        capability == sylvander_protocol::MEMORY_CONFIRMATION_CAPABILITY
                    })
                    && let Some(session_id) = self.session_id.clone()
                {
                    self.pending_actions
                        .push(Action::RequestMemoryConfirmations { session_id });
                }
                return self.start_next_queued_prompt();
            }
            DomainEvent::AgentError {
                message,
                feedback_target,
            } => {
                self.turn_active = false;
                self.interrupt_requested = false;
                self.feedback_target = feedback_target;
                self.messages
                    .push(ChatMessage::Info(format!("Error: {message}")));
                self.streaming.clear();
                self.streaming_thinking.clear();
                if self
                    .protocol_capabilities
                    .iter()
                    .any(|capability| {
                        capability == sylvander_protocol::MEMORY_CONFIRMATION_CAPABILITY
                    })
                    && let Some(session_id) = self.session_id.clone()
                {
                    self.pending_actions
                        .push(Action::RequestMemoryConfirmations { session_id });
                }
                return self.start_next_queued_prompt();
            }
            DomainEvent::TurnInterrupted {
                reason,
                feedback_target,
            } => {
                self.turn_active = false;
                self.interrupt_requested = false;
                self.feedback_target = feedback_target;
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
                        "Tool Approval"
                            | "Agent asks"
                            | "Plan review"
                            | "Plan editor"
                            | "Plan · Edit step"
                    )
                }) {
                    self.modals.pop();
                }
                self.mode = AppMode::Normal;
                self.status = "Interrupted".into();
                self.messages
                    .push(ChatMessage::Info(format!("Turn interrupted: {reason}")));
                if self
                    .protocol_capabilities
                    .iter()
                    .any(|capability| {
                        capability == sylvander_protocol::MEMORY_CONFIRMATION_CAPABILITY
                    })
                    && let Some(session_id) = self.session_id.clone()
                {
                    self.pending_actions
                        .push(Action::RequestMemoryConfirmations { session_id });
                }
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
                if self.modals.is_full() {
                    for tool in tools {
                        self.pending_actions.push(Action::SendApprove {
                            session_id: self.session_id.clone().unwrap_or_default(),
                            call_id: tool.call_id,
                            approved: false,
                            scope: sylvander_protocol::ApprovalScope::Once,
                            reason: Some("TUI decision queue is full".into()),
                        });
                    }
                    self.messages.push(ChatMessage::Info(
                        "approval rejected · TUI decision queue is full".into(),
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
                if self.modals.is_full() {
                    self.pending_actions.push(Action::SendAnswer {
                        session_id: self.session_id.clone().unwrap_or_default(),
                        call_id,
                        answer: String::new(),
                    });
                    self.messages.push(ChatMessage::Info(
                        "question skipped · TUI decision queue is full".into(),
                    ));
                    return None;
                }
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
                if self.modals.is_full() {
                    self.pending_actions.push(Action::ResolvePlan {
                        session_id: self.session_id.clone().unwrap_or_default(),
                        plan_id,
                        decision: sylvander_protocol::PlanDecision::Rejected {
                            reason: "TUI decision queue is full".into(),
                        },
                    });
                    self.messages.push(ChatMessage::Info(
                        "plan rejected · TUI decision queue is full".into(),
                    ));
                    return None;
                }
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
        if self.composer.paste(text) == crate::input::PasteOutcome::Rejected {
            self.status = "Paste rejected · local Composer limit exceeded".into();
        }
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
        if is_undo_archive && let Some(session) = self.last_archived_session.take() {
            let session_id = session.id.clone();
            self.sessions.insert(0, session);
            self.status = "Restoring archived session…".into();
            self.dirty.mark();
            return Some(Action::RestoreSession { session_id });
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
        if (is_ctrl_c || is_ctrl_x)
            && !self.turn_active
            && let Some(text) = self.composer.selected_text()
        {
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
            self.composer.replace_text("/");
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
            if self.composer.handle_escape() {
                self.status = "Vim NORMAL · i insert · Enter send".into();
                self.dirty.mark();
                return None;
            }
            self.should_quit = true;
            self.dirty.mark();
            return None;
        }

        if key.code == crossterm::event::KeyCode::Char('@')
            && key.modifiers.is_empty()
            && self.composer.accepts_text_input()
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
        if is_submit && self.turn_active && self.queued_prompts.len() >= MAX_QUEUED_PROMPTS {
            self.status =
                "Prompt queue full · current draft preserved; use /queue remove first".into();
            self.dirty.mark();
            return None;
        }
        if is_submit
            && let Err(error) = self.composer.validate_attachments(
                self.metadata.max_attachment_bytes,
                self.metadata.supports_vision(),
            )
        {
            self.status = error;
            self.dirty.mark();
            return None;
        }
        if let Some(text) = self.composer.handle_key(key) {
            let attachments = self.composer.take_submitted_attachments();
            self.save_history();
            return self.submit_prompt(text, attachments);
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

impl AppState {
    fn apply_user_profile_response(&mut self, response: sylvander_protocol::UserProfileResponse) {
        use sylvander_protocol::UserProfileResponse;
        match response {
            UserProfileResponse::Created { profile, .. } => {
                self.user_profile = Some(profile);
                self.pending_profile_intent = None;
                self.status = "User profile created".into();
                self.push_user_profile_summary("created");
            }
            UserProfileResponse::Read { profile, .. } => {
                self.user_profile = Some(profile);
                if !self.resume_pending_profile_intent() {
                    self.status = "User profile loaded".into();
                    self.push_user_profile_summary("current");
                }
            }
            UserProfileResponse::Updated { profile, .. } => {
                self.user_profile = Some(profile);
                self.pending_profile_intent = None;
                self.status = "User profile updated".into();
                self.push_user_profile_summary("updated");
            }
            UserProfileResponse::Corrected { profile, .. } => {
                self.user_profile = Some(profile);
                self.pending_profile_intent = None;
                self.status = "User profile corrected".into();
                self.push_user_profile_summary("corrected");
            }
            UserProfileResponse::DoNotLearnUpdated { profile, .. } => {
                let enabled = profile.do_not_learn;
                self.user_profile = Some(profile);
                self.pending_profile_intent = None;
                self.status = format!(
                    "Do-not-learn {}",
                    if enabled { "enabled" } else { "disabled" }
                );
                self.push_user_profile_summary("privacy updated");
            }
            UserProfileResponse::Exported { export, .. } => {
                self.user_profile = Some(export.profile.clone());
                self.pending_profile_intent = None;
                match serde_json::to_string_pretty(&export) {
                    Ok(text) => {
                        self.pending_actions.push(Action::CopyText { text });
                        self.status = "User profile export copied".into();
                        self.messages.push(ChatMessage::Info(format!(
                            "user profile · export copied · revision r{} · JSON v{}",
                            export.profile.revision, export.schema_version
                        )));
                    }
                    Err(error) => {
                        self.status = format!("User profile export failed: {error}");
                        self.messages.push(ChatMessage::Info(self.status.clone()));
                    }
                }
            }
            UserProfileResponse::Deleted {
                deleted_revision,
                do_not_learn_preserved,
                ..
            } => {
                self.user_profile = None;
                self.pending_profile_intent = None;
                self.status = "User profile deleted".into();
                self.messages.push(ChatMessage::Info(format!(
                    "user profile · deleted r{deleted_revision} · do-not-learn {}",
                    if do_not_learn_preserved {
                        "preserved"
                    } else {
                        "not set"
                    }
                )));
            }
            UserProfileResponse::NotFound { .. } => {
                self.user_profile = None;
                match self.pending_profile_intent.take() {
                    Some(PendingProfileIntent::Edit { correction: false }) => {
                        self.modals
                            .push(Box::new(ProfileEditor::new(ProfileEditMode::Create, None)));
                        self.status = "No stored profile · creating a new one".into();
                    }
                    Some(PendingProfileIntent::Edit { correction: true }) => {
                        self.status = "No stored profile to correct".into();
                        self.messages.push(ChatMessage::Info(self.status.clone()));
                    }
                    Some(PendingProfileIntent::SetDoNotLearn(_)) => {
                        self.status = "Create a profile before changing do-not-learn".into();
                        self.messages.push(ChatMessage::Info(self.status.clone()));
                    }
                    Some(PendingProfileIntent::Delete) => {
                        self.status = "No stored user profile to delete".into();
                        self.messages.push(ChatMessage::Info(self.status.clone()));
                    }
                    None => {
                        self.status = "No stored user profile".into();
                        self.messages.push(ChatMessage::Info(
                            "user profile · not created · use /profile edit".into(),
                        ));
                    }
                }
            }
            UserProfileResponse::Error { error, .. } => {
                self.pending_profile_intent = None;
                let operation = format!("{:?}", error.operation).to_ascii_lowercase();
                if error.code == sylvander_protocol::UserProfileErrorCode::Conflict {
                    self.user_profile = None;
                    self.pending_actions.push(Action::UserProfile {
                        request: sylvander_protocol::UserProfileRequest {
                            version: sylvander_protocol::USER_PROFILE_PROTOCOL_VERSION,
                            action: sylvander_protocol::UserProfileAction::Read {},
                        },
                    });
                    self.status = "User profile changed elsewhere · reloading".into();
                    self.messages.push(ChatMessage::Info(format!(
                        "user profile · conflict on {operation}{} · your stale edit was not applied",
                        error
                            .current_revision
                            .map_or_else(String::new, |revision| format!(" · current r{revision}"))
                    )));
                } else {
                    self.status = format!("User profile {operation} failed");
                    self.messages.push(ChatMessage::Info(format!(
                        "user profile · {operation} failed · {:?}{}",
                        error.code,
                        error
                            .retry_after_ms
                            .map_or_else(String::new, |delay| format!(" · retry in {delay} ms"))
                    )));
                }
            }
        }
    }

    fn resume_pending_profile_intent(&mut self) -> bool {
        let Some(intent) = self.pending_profile_intent.take() else {
            return false;
        };
        let Some(profile) = self.user_profile.as_ref() else {
            return false;
        };
        match intent {
            PendingProfileIntent::Edit { correction } => {
                let mode = if correction {
                    ProfileEditMode::Correct
                } else {
                    ProfileEditMode::Update
                };
                let modal = ProfileEditor::new(mode, Some(profile));
                self.modals.push(Box::new(modal));
                self.status = if correction {
                    "Correcting server profile".into()
                } else {
                    "Editing server profile".into()
                };
            }
            PendingProfileIntent::SetDoNotLearn(enabled) => {
                self.pending_actions.push(Action::UserProfile {
                    request: sylvander_protocol::UserProfileRequest {
                        version: sylvander_protocol::USER_PROFILE_PROTOCOL_VERSION,
                        action: sylvander_protocol::UserProfileAction::SetDoNotLearn {
                            expected_revision: profile.revision,
                            enabled,
                        },
                    },
                });
                self.status = "Updating do-not-learn…".into();
            }
            PendingProfileIntent::Delete => {
                self.modals
                    .push(Box::new(ProfileDeleteModal::new(profile.revision)));
                self.status = "Confirm user profile deletion".into();
            }
        }
        true
    }

    fn push_user_profile_summary(&mut self, state: &str) {
        if let Some(profile) = &self.user_profile {
            self.messages.push(ChatMessage::Info(format!(
                "user profile · {state} · r{} · do-not-learn {}\n{}",
                profile.revision,
                if profile.do_not_learn { "on" } else { "off" },
                user_profile_summary(&profile.profile)
            )));
        }
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
            | DomainEvent::UserProfileReceived { .. }
            | DomainEvent::MemoryConfirmationRecorded { .. }
            | DomainEvent::MemoryConfirmationFailed { .. }
    )
}

fn user_profile_summary(profile: &sylvander_protocol::UserProfileData) -> String {
    let language = profile
        .preferred_language
        .as_ref()
        .map_or("not set", |value| value.value.as_str());
    let locale = profile
        .locale
        .as_ref()
        .map_or("not set", |value| value.value.as_str());
    let detail = profile
        .response_detail
        .as_ref()
        .map_or("not set", |value| match value.value {
            sylvander_protocol::ResponseDetail::Concise => "concise",
            sylvander_protocol::ResponseDetail::Balanced => "balanced",
            sylvander_protocol::ResponseDetail::Detailed => "detailed",
        });
    let tone = profile
        .communication_tone
        .as_ref()
        .map_or("not set", |value| match value.value {
            sylvander_protocol::CommunicationTone::Direct => "direct",
            sylvander_protocol::CommunicationTone::Warm => "warm",
            sylvander_protocol::CommunicationTone::Formal => "formal",
        });
    let accessibility = profile.accessibility.as_ref().map_or_else(
        || "default".into(),
        |value| {
            let mut enabled = Vec::new();
            if value.value.screen_reader_optimized {
                enabled.push("screen-reader");
            }
            if value.value.reduce_motion {
                enabled.push("reduced-motion");
            }
            if value.value.high_contrast {
                enabled.push("high-contrast");
            }
            if enabled.is_empty() {
                "default".into()
            } else {
                enabled.join(", ")
            }
        },
    );
    let constraints = if profile.constraints.is_empty() {
        "none".into()
    } else {
        profile
            .constraints
            .iter()
            .map(|value| value.value.as_str())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    format!(
        "language {language} · locale {locale} · detail {detail} · tone {tone}\naccessibility {accessibility}\nconstraints {constraints}"
    )
}

fn normalize_message(message: &mut ChatMessage) {
    match message {
        ChatMessage::User(text)
        | ChatMessage::QueuedUser(text)
        | ChatMessage::Agent(text)
        | ChatMessage::Thinking(text)
        | ChatMessage::Info(text) => truncate_utf8(text, MAX_MESSAGE_BYTES),
        ChatMessage::ToolCall { name, input, .. } => {
            truncate_utf8(name, MAX_TOOL_PAYLOAD_BYTES);
            normalize_json(input);
        }
        ChatMessage::ToolResult { name, output, .. } => {
            truncate_utf8(name, MAX_TOOL_PAYLOAD_BYTES);
            truncate_utf8(output, MAX_TOOL_PAYLOAD_BYTES);
        }
        ChatMessage::ToolStep { name, children, .. } => {
            truncate_utf8(name, MAX_TOOL_PAYLOAD_BYTES);
            if children.len() > MAX_GROUP_ITEMS {
                children.drain(..children.len() - MAX_GROUP_ITEMS);
            }
            for child in children {
                truncate_utf8(&mut child.call_id, MAX_TOOL_PAYLOAD_BYTES);
                truncate_utf8(&mut child.name, MAX_TOOL_PAYLOAD_BYTES);
                normalize_json(&mut child.input);
                if let Some(output) = &mut child.output {
                    if child.status == ToolStatus::Pending {
                        truncate_utf8_tail(output, MAX_TOOL_PAYLOAD_BYTES);
                    } else {
                        truncate_utf8(output, MAX_TOOL_PAYLOAD_BYTES);
                    }
                }
            }
        }
        ChatMessage::Plan {
            plan_id,
            steps,
            current,
        } => {
            truncate_utf8(plan_id, MAX_TOOL_PAYLOAD_BYTES);
            steps.truncate(MAX_GROUP_ITEMS);
            for step in steps.iter_mut() {
                truncate_utf8(step, MAX_TOOL_PAYLOAD_BYTES);
            }
            *current = (*current).min(steps.len().saturating_sub(1));
        }
        ChatMessage::TaskList { tasks } => {
            if tasks.len() > MAX_GROUP_ITEMS {
                tasks.drain(..tasks.len() - MAX_GROUP_ITEMS);
            }
            for task in tasks {
                truncate_utf8(&mut task.task_id, MAX_TOOL_PAYLOAD_BYTES);
                truncate_utf8(&mut task.owner, MAX_TOOL_PAYLOAD_BYTES);
                truncate_utf8(&mut task.purpose, MAX_TOOL_PAYLOAD_BYTES);
                truncate_utf8(&mut task.detail, MAX_TOOL_PAYLOAD_BYTES);
            }
        }
    }
}

fn normalize_json(value: &mut serde_json::Value) {
    let oversized =
        serde_json::to_vec(value).map_or(true, |encoded| encoded.len() > MAX_TOOL_PAYLOAD_BYTES);
    if oversized {
        *value = serde_json::json!({
            "_sylvander": "tool input omitted from local view because it exceeded 64 KiB"
        });
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    const MARKER: &str = "\n… local view truncated …";
    if value.len() <= max_bytes {
        return;
    }
    let mut keep = max_bytes.saturating_sub(MARKER.len());
    while keep > 0 && !value.is_char_boundary(keep) {
        keep -= 1;
    }
    value.truncate(keep);
    value.push_str(MARKER);
}

fn append_live_tool_output(output: &mut String, delta: &str) {
    output.push_str(delta);
    truncate_utf8_tail(output, MAX_TOOL_PAYLOAD_BYTES);
}

fn bounded_live_tool_output(mut output: String) -> String {
    truncate_utf8_tail(&mut output, MAX_TOOL_PAYLOAD_BYTES);
    output
}

fn truncate_utf8_tail(value: &mut String, max_bytes: usize) {
    const MARKER: &str = "… earlier live output omitted …\n";
    if value.len() <= max_bytes {
        return;
    }
    let mut start = value
        .len()
        .saturating_sub(max_bytes.saturating_sub(MARKER.len()));
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    let tail = value[start..].to_owned();
    value.clear();
    value.push_str(MARKER);
    value.push_str(&tail);
}

fn message_bytes(message: &ChatMessage) -> usize {
    match message {
        ChatMessage::User(text)
        | ChatMessage::QueuedUser(text)
        | ChatMessage::Agent(text)
        | ChatMessage::Thinking(text)
        | ChatMessage::Info(text) => text.len(),
        ChatMessage::ToolCall { name, input, .. } => name.len() + json_bytes(input),
        ChatMessage::ToolResult { name, output, .. } => name.len() + output.len(),
        ChatMessage::ToolStep { name, children, .. } => {
            name.len()
                + children
                    .iter()
                    .map(|child| {
                        child.call_id.len()
                            + child.name.len()
                            + json_bytes(&child.input)
                            + child.output.as_ref().map_or(0, String::len)
                    })
                    .sum::<usize>()
        }
        ChatMessage::Plan { plan_id, steps, .. } => {
            plan_id.len() + steps.iter().map(String::len).sum::<usize>()
        }
        ChatMessage::TaskList { tasks } => tasks
            .iter()
            .map(|task| {
                task.task_id.len() + task.owner.len() + task.purpose.len() + task.detail.len()
            })
            .sum(),
    }
}

fn json_bytes(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).map_or(0, |encoded| encoded.len())
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
#[path = "../tests/unit/app.rs"]
mod tests;

/// Build a short human label from a session uuid.
fn short_session_label(id: &str) -> String {
    let first8: String = id.chars().take(8).collect();
    first8
}

/// Monotonic seconds since UNIX epoch. Used for `ToolStep` `started_at`
/// timestamps; the renderer derives elapsed time at draw time.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Derive a human-readable step name from the leading tool verb +
/// target.  Falls back to the bare tool name when no recognizable
/// input shape is available.
fn step_name_for(tool: &str, input: &serde_json::Value) -> String {
    match tool.to_ascii_lowercase().as_str() {
        "read" => {
            let path = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            format!("Read {path}")
        }
        "write" => {
            let path = input
                .get("path")
                .or_else(|| input.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            format!("Write {path}")
        }
        "edit" => "Edit file".into(),
        "bash" | "shell" | "exec" | "command" => {
            let cmd = input
                .get("command")
                .or_else(|| input.get("cmd"))
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
        _ => crate::markdown::sanitize_terminal_text(tool),
    }
}
