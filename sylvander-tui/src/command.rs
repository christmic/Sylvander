//! Application commands exposed through the `/` command line.

use crate::app::{AppMode, AppState, ChatMessage};
use crate::input::AttachmentKind;
use crate::modal::{
    AgentPicker, FileMentionModal, HelpModal, ModelPicker, PermissionsPicker, SessionsOverlay,
    ToolInspector,
};
use crate::theme::ThemeName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    New,
    Agent,
    Sessions,
    Resume,
    Rename,
    Fork,
    Rewind,
    Checkpoint,
    Undo,
    Clear,
    Help,
    Theme,
    Tools,
    Queue,
    Tasks,
    Attachments,
    Mention,
    Diff,
    Accept,
    Discard,
    Review,
    Config,
    Doctor,
    Mcp,
    Skills,
    Hooks,
    Extensions,
    Memory,
    Profile,
    Feedback,
    Inspect,
    Copy,
    Editor,
    Model,
    Permissions,
    Context,
    Compact,
    Rollback,
    Preview,
    Status,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    pub hint: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: CommandId::New,
        name: "new",
        usage: "/new",
        description: "Start a clean local session",
        hint: "next prompt creates it",
    },
    CommandSpec {
        id: CommandId::Agent,
        name: "agent",
        usage: "/agent [id]",
        description: "Choose the Agent for a new session",
        hint: "switching starts fresh",
    },
    CommandSpec {
        id: CommandId::Sessions,
        name: "sessions",
        usage: "/sessions",
        description: "Browse and switch sessions",
        hint: "ctrl+p",
    },
    CommandSpec {
        id: CommandId::Resume,
        name: "resume",
        usage: "/resume",
        description: "Resume a persisted session",
        hint: "session browser",
    },
    CommandSpec {
        id: CommandId::Rename,
        name: "rename",
        usage: "/rename <name>",
        description: "Rename the current persisted session",
        hint: "server-backed",
    },
    CommandSpec {
        id: CommandId::Fork,
        name: "fork",
        usage: "/fork",
        description: "Fork the current persisted session",
        hint: "copies history",
    },
    CommandSpec {
        id: CommandId::Rewind,
        name: "rewind",
        usage: "/rewind <completed-turn>",
        description: "Branch from a completed conversation turn",
        hint: "workspace unchanged",
    },
    CommandSpec {
        id: CommandId::Checkpoint,
        name: "checkpoint",
        usage: "/checkpoint",
        description: "Create a conversation checkpoint branch",
        hint: "workspace unchanged",
    },
    CommandSpec {
        id: CommandId::Undo,
        name: "undo",
        usage: "/undo",
        description: "Return to the source conversation",
        hint: "does not revert files",
    },
    CommandSpec {
        id: CommandId::Clear,
        name: "clear",
        usage: "/clear",
        description: "Clear the local transcript",
        hint: "keeps session",
    },
    CommandSpec {
        id: CommandId::Help,
        name: "help",
        usage: "/help [commands|approval|tools|vim]",
        description: "Show interaction help",
        hint: "ui-only",
    },
    CommandSpec {
        id: CommandId::Theme,
        name: "theme",
        usage: "/theme <sylvander|midnight|high-contrast>",
        description: "Change the active TUI theme",
        hint: "local",
    },
    CommandSpec {
        id: CommandId::Tools,
        name: "tools",
        usage: "/tools [expand|collapse]",
        description: "Toggle tool details",
        hint: "ctrl+o",
    },
    CommandSpec {
        id: CommandId::Queue,
        name: "queue",
        usage: "/queue [drop <n>|edit <n> <text>|clear]",
        description: "Inspect or edit prompts waiting behind active work",
        hint: "working turns",
    },
    CommandSpec {
        id: CommandId::Tasks,
        name: "tasks",
        usage: "/tasks [cancel <id-or-prefix>]",
        description: "Inspect or cancel one background task",
        hint: "read-only workers",
    },
    CommandSpec {
        id: CommandId::Attachments,
        name: "attachments",
        usage: "/attachments [selection|tool <id>|diff <id>|drop <n>|up <n>|down <n>|clear]",
        description: "Inspect, reorder, or remove draft attachments",
        hint: "@ adds files",
    },
    CommandSpec {
        id: CommandId::Mention,
        name: "mention",
        usage: "/mention",
        description: "Find and attach a workspace file",
        hint: "same picker as @",
    },
    CommandSpec {
        id: CommandId::Diff,
        name: "diff",
        usage: "/diff",
        description: "Inspect this coding session's changes",
        hint: "server worktree",
    },
    CommandSpec {
        id: CommandId::Accept,
        name: "accept",
        usage: "/accept",
        description: "Merge this coding session's reviewed changes",
        hint: "confirmation required",
    },
    CommandSpec {
        id: CommandId::Discard,
        name: "discard",
        usage: "/discard",
        description: "Delete this coding session and its changes",
        hint: "confirmation required",
    },
    CommandSpec {
        id: CommandId::Review,
        name: "review",
        usage: "/review [staged|unstaged]",
        description: "Ask Sylvander to review workspace changes",
        hint: "findings first",
    },
    CommandSpec {
        id: CommandId::Config,
        name: "config",
        usage: "/config",
        description: "Inspect resolved TUI configuration",
        hint: "read-only",
    },
    CommandSpec {
        id: CommandId::Doctor,
        name: "doctor",
        usage: "/doctor [copy|export <path>]",
        description: "Inspect or export redacted diagnostics",
        hint: "no secrets",
    },
    CommandSpec {
        id: CommandId::Mcp,
        name: "mcp",
        usage: "/mcp",
        description: "Inspect Agent-advertised MCP configuration state",
        hint: "server truth",
    },
    CommandSpec {
        id: CommandId::Skills,
        name: "skills",
        usage: "/skills",
        description: "Inspect advertised skills and activation state",
        hint: "server truth",
    },
    CommandSpec {
        id: CommandId::Hooks,
        name: "hooks",
        usage: "/hooks",
        description: "Inspect before-tool hooks and blocking policy",
        hint: "server truth",
    },
    CommandSpec {
        id: CommandId::Extensions,
        name: "extensions",
        usage: "/extensions",
        description: "Inspect declarative extension contributions",
        hint: "no UI callbacks",
    },
    CommandSpec {
        id: CommandId::Memory,
        name: "memory",
        usage: "/memory",
        description: "Inspect long-term memory availability",
        hint: "server truth",
    },
    CommandSpec {
        id: CommandId::Profile,
        name: "profile",
        usage: "/profile [show|create|edit|correct|do-not-learn on|off|export|delete]",
        description: "View or edit your owner-scoped user profile",
        hint: "server revision",
    },
    CommandSpec {
        id: CommandId::Feedback,
        name: "feedback",
        usage: "/feedback <positive|negative> [note] | correction <text> | note <positive|negative> <text>",
        description: "Rate or correct the most recently completed turn",
        hint: "durable evidence",
    },
    CommandSpec {
        id: CommandId::Preview,
        name: "preview",
        usage: "/preview <image|web> <path-or-url>",
        description: "Open a resource in the trusted desktop host",
        hint: "Ghostty host only",
    },
    CommandSpec {
        id: CommandId::Status,
        name: "status",
        usage: "/status",
        description: "Show runtime and token usage",
        hint: "local",
    },
    CommandSpec {
        id: CommandId::Inspect,
        name: "inspect",
        usage: "/inspect [call-id-prefix]",
        description: "Open searchable output for a completed tool call",
        hint: "long output",
    },
    CommandSpec {
        id: CommandId::Copy,
        name: "copy",
        usage: "/copy [call-id-prefix]",
        description: "Copy a tool result through the terminal clipboard",
        hint: "OSC 52",
    },
    CommandSpec {
        id: CommandId::Editor,
        name: "editor",
        usage: "/editor",
        description: "Edit the current draft in $VISUAL or $EDITOR",
        hint: "keeps attachments",
    },
    CommandSpec {
        id: CommandId::Model,
        name: "model",
        usage: "/model [provider/model|model-id] [off|low|medium|high]",
        description: "Select a server-advertised model and reasoning effort",
        hint: "next turn",
    },
    CommandSpec {
        id: CommandId::Permissions,
        name: "permissions",
        usage: "/permissions",
        description: "Edit workspace, network, and approval policy",
        hint: "next turn",
    },
    CommandSpec {
        id: CommandId::Context,
        name: "context",
        usage: "/context",
        description: "Show server-confirmed context and cache usage",
        hint: "live report",
    },
    CommandSpec {
        id: CommandId::Compact,
        name: "compact",
        usage: "/compact",
        description: "Summarize older context while preserving recent turns",
        hint: "server-backed",
    },
    CommandSpec {
        id: CommandId::Rollback,
        name: "rollback",
        usage: "/rollback",
        description: "Inspect and restore the latest Agent file turn",
        hint: "conflict checked",
    },
    CommandSpec {
        id: CommandId::Quit,
        name: "quit",
        usage: "/quit",
        description: "Quit sylvander-tui",
        hint: "ctrl+c",
    },
];

/// Familiar spellings accepted by the parser and ranked by the palette. An
/// alias never owns behavior; it resolves to a typed core command first.
pub const ALIASES: &[(&str, CommandId)] = &[
    ("history", CommandId::Sessions),
    ("session", CommandId::Sessions),
    ("files", CommandId::Mention),
    ("settings", CommandId::Config),
    ("diagnostics", CommandId::Doctor),
    ("perm", CommandId::Permissions),
    ("ctx", CommandId::Context),
    ("exit", CommandId::Quit),
    ("q", CommandId::Quit),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAvailability {
    Available,
    Unavailable(String),
}

impl CommandAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMatch {
    pub index: usize,
    pub dynamic: bool,
    pub score: usize,
    pub availability: CommandAvailability,
}

pub fn match_name<'a>(entry: &CommandMatch, state: &'a AppState) -> &'a str {
    if entry.dynamic {
        state
            .platform
            .commands
            .get(entry.index)
            .map_or("command-catalog-changed", |command| command.name.as_str())
    } else {
        COMMANDS[entry.index].name
    }
}

pub fn match_description<'a>(entry: &CommandMatch, state: &'a AppState) -> &'a str {
    if entry.dynamic {
        state.platform.commands.get(entry.index).map_or(
            "Server command catalog changed; type to refresh",
            |command| command.description.as_str(),
        )
    } else {
        COMMANDS[entry.index].description
    }
}

pub fn match_source<'a>(entry: &CommandMatch, state: &'a AppState) -> Option<&'a str> {
    if !entry.dynamic {
        return None;
    }
    state
        .platform
        .commands
        .get(entry.index)
        .map(|command| command.source.as_str())
}

pub fn aliases_for(id: CommandId) -> impl Iterator<Item = &'static str> {
    ALIASES
        .iter()
        .filter(move |(_, target)| *target == id)
        .map(|(alias, _)| *alias)
}

pub fn resolve(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(name))
        .or_else(|| {
            let id = ALIASES
                .iter()
                .find(|(alias, _)| alias.eq_ignore_ascii_case(name))?
                .1;
            COMMANDS.iter().find(|spec| spec.id == id)
        })
}

pub fn availability(spec: &CommandSpec, state: &AppState) -> CommandAvailability {
    use CommandAvailability::{Available, Unavailable};

    let needs_connection = matches!(
        spec.id,
        CommandId::Resume
            | CommandId::Agent
            | CommandId::Rename
            | CommandId::Fork
            | CommandId::Rewind
            | CommandId::Checkpoint
            | CommandId::Context
            | CommandId::Compact
            | CommandId::Rollback
            | CommandId::Diff
            | CommandId::Accept
            | CommandId::Discard
            | CommandId::Model
            | CommandId::Permissions
            | CommandId::Mcp
            | CommandId::Skills
            | CommandId::Hooks
            | CommandId::Extensions
            | CommandId::Memory
            | CommandId::Profile
            | CommandId::Feedback
    );
    if needs_connection && !state.connected {
        return Unavailable("connect to the Agent first".into());
    }
    if state.turn_active
        && matches!(
            spec.id,
            CommandId::New
                | CommandId::Agent
                | CommandId::Clear
                | CommandId::Fork
                | CommandId::Rewind
                | CommandId::Checkpoint
                | CommandId::Review
                | CommandId::Compact
                | CommandId::Rollback
                | CommandId::Accept
                | CommandId::Discard
        )
    {
        return Unavailable("interrupt active work first".into());
    }
    if matches!(
        spec.id,
        CommandId::Rename
            | CommandId::Fork
            | CommandId::Rewind
            | CommandId::Checkpoint
            | CommandId::Compact
            | CommandId::Rollback
            | CommandId::Diff
            | CommandId::Accept
            | CommandId::Discard
    ) && state.session_id.is_none()
    {
        return Unavailable("requires a persisted session".into());
    }
    if spec.id == CommandId::Undo && state.last_branch_source_session_id.is_none() {
        return Unavailable("no conversation branch to undo".into());
    }
    if spec.id == CommandId::Model && state.metadata.models.is_empty() {
        return Unavailable("model catalog is still loading".into());
    }
    if spec.id == CommandId::Preview && !state.host_preview_available {
        return Unavailable("requires a trusted desktop host".into());
    }
    if spec.id == CommandId::Profile
        && !state
            .protocol_capabilities
            .iter()
            .any(|capability| capability == sylvander_protocol::USER_PROFILE_CAPABILITY)
    {
        return Unavailable("server does not advertise user_profile_v1".into());
    }
    if spec.id == CommandId::Feedback {
        if !state
            .protocol_capabilities
            .iter()
            .any(|capability| capability == sylvander_protocol::FEEDBACK_CAPABILITY)
        {
            return Unavailable("server does not advertise feedback_v1".into());
        }
        if state.feedback_target.is_none() {
            return Unavailable("complete a turn before recording feedback".into());
        }
    }
    if matches!(spec.id, CommandId::Inspect | CommandId::Copy)
        && find_tool_output(state, None).is_err()
    {
        return Unavailable("no completed tool output".into());
    }
    Available
}

/// Rank commands with a deterministic subsequence matcher. Exact/prefix/name
/// matches beat aliases and descriptions; recency only breaks comparable
/// results so discovery remains predictable.
pub fn ranked_commands(query: &str, state: &AppState) -> Vec<CommandMatch> {
    let needle = query
        .trim()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let recent_rank = |id| {
        state
            .recent_commands
            .iter()
            .position(|recent| *recent == id)
            .unwrap_or(usize::MAX)
    };
    let mut matches = COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(index, spec)| {
            let score = if needle.is_empty() {
                0
            } else {
                let name = fuzzy_score(&needle, spec.name);
                let alias = aliases_for(spec.id)
                    .filter_map(|alias| fuzzy_score(&needle, alias))
                    .min()
                    .map(|score| score + 8);
                let description = fuzzy_score(&needle, spec.description).map(|score| score + 80);
                name.into_iter().chain(alias).chain(description).min()?
            };
            Some(CommandMatch {
                index,
                dynamic: false,
                score,
                availability: availability(spec, state),
            })
        })
        .collect::<Vec<_>>();
    matches.extend(
        state
            .platform
            .commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                let score = if needle.is_empty() {
                    0
                } else if command.name.len() <= 32 && command.description.len() <= 160 {
                    fuzzy_score(&needle, &command.name)
                        .into_iter()
                        .chain(fuzzy_score(&needle, &command.description).map(|score| score + 80))
                        .min()?
                } else {
                    return None;
                };
                Some(CommandMatch {
                    index,
                    dynamic: true,
                    score,
                    availability: dynamic_availability(index, state),
                })
            }),
    );
    matches.sort_by_key(|entry| {
        (
            entry.score,
            if entry.dynamic {
                usize::MAX
            } else {
                recent_rank(COMMANDS[entry.index].id)
            },
            usize::from(entry.dynamic),
            entry.index,
        )
    });
    matches
}

fn dynamic_availability(index: usize, state: &AppState) -> CommandAvailability {
    use CommandAvailability::{Available, Unavailable};
    if !state.connected {
        return Unavailable("connect to the Agent first".into());
    }
    dynamic_command_issue(index, state).map_or(Available, Unavailable)
}

fn dynamic_command_issue(index: usize, state: &AppState) -> Option<String> {
    let command = state.platform.commands.get(index)?;
    let valid_name = !command.name.is_empty()
        && command.name.len() <= 32
        && command.name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit() && index > 0
                || byte == b'-' && index > 0
        });
    if !valid_name {
        return Some("invalid extension command name".into());
    }
    if command.id.is_empty()
        || command.id.len() > 64
        || !command
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Some("invalid extension command id".into());
    }
    if command.usage.len() > 96
        || command.description.is_empty()
        || command.hint.len() > 64
        || command.source.is_empty()
        || command.source.len() > 64
        || [
            &command.usage,
            &command.description,
            &command.hint,
            &command.source,
        ]
        .into_iter()
        .any(|value| value.chars().any(char::is_control))
    {
        return Some("extension command metadata exceeds UI limits".into());
    }
    let expected_usage = format!("/{}", command.name);
    if command.usage.split_whitespace().next() != Some(expected_usage.as_str()) {
        return Some("extension command usage must begin with its name".into());
    }
    if !matches!(
        command.trust,
        sylvander_protocol::PlatformTrust::Workspace | sylvander_protocol::PlatformTrust::User
    ) {
        return Some(format!(
            "{} source is not trusted for commands",
            platform_trust_label(command.trust)
        ));
    }
    if resolve(&command.name).is_some() {
        return Some("conflicts with a built-in command or alias".into());
    }
    if state.platform.commands[..index]
        .iter()
        .any(|other| other.id == command.id || other.name.eq_ignore_ascii_case(&command.name))
    {
        return Some("duplicates an earlier extension command".into());
    }
    match &command.effect {
        sylvander_protocol::UiCommandEffect::SubmitPrompt { template }
            if template.is_empty()
                || template.len() > 16 * 1024
                || template.chars().any(|character| {
                    character.is_control() && !matches!(character, '\n' | '\t')
                }) =>
        {
            Some("prompt template is empty or too large".into())
        }
        sylvander_protocol::UiCommandEffect::SubmitPrompt { .. } => None,
    }
}

fn fuzzy_score(needle: &str, candidate: &str) -> Option<usize> {
    let candidate = candidate.to_ascii_lowercase();
    if candidate == needle {
        return Some(0);
    }
    if candidate.starts_with(needle) {
        return Some(4 + candidate.len().saturating_sub(needle.len()));
    }
    if let Some(position) = candidate.find(needle) {
        return Some(20 + position);
    }
    let mut cursor = 0;
    let mut gaps = 0;
    for wanted in needle.chars() {
        let offset = candidate[cursor..].find(wanted)?;
        gaps += offset;
        cursor += offset + wanted.len_utf8();
    }
    Some(40 + gaps)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation<'a> {
    pub spec: &'static CommandSpec,
    pub args: Vec<&'a str>,
}

pub fn parse(line: &str) -> Result<Invocation<'_>, String> {
    let mut parts = line.trim().trim_start_matches('/').split_whitespace();
    let name = parts.next().ok_or_else(|| "Choose a command".to_string())?;
    let spec = resolve(name).ok_or_else(|| format!("Unknown command /{name}"))?;
    Ok(Invocation {
        spec,
        args: parts.collect(),
    })
}

pub fn execute_line(line: &str, state: &mut AppState) -> Result<(), String> {
    let mut parts = line.trim().trim_start_matches('/').split_whitespace();
    let name = parts.next().ok_or_else(|| "Choose a command".to_string())?;
    if resolve(name).is_some() {
        return execute(parse(line)?, state);
    }
    let Some(index) = state
        .platform
        .commands
        .iter()
        .position(|command| command.name.eq_ignore_ascii_case(name))
    else {
        return Err(format!("Unknown command /{name}"));
    };
    if let CommandAvailability::Unavailable(reason) = dynamic_availability(index, state) {
        return Err(format!("/{name} unavailable: {reason}"));
    }
    let command = state.platform.commands[index].clone();
    let args = parts.collect::<Vec<_>>().join(" ");
    let sylvander_protocol::UiCommandEffect::SubmitPrompt { template } = command.effect;
    let prompt = if template.contains("{{args}}") {
        template.replace("{{args}}", &args)
    } else if args.is_empty() {
        template
    } else {
        format!("{template}\n\nArguments: {args}")
    };
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(format!("/{name} produced an empty prompt"));
    }
    state.status = format!("Ran /{name} · {}", command.source);
    if let Some(action) = state.submit_prompt(prompt, Vec::new()) {
        state.pending_actions.push(action);
    }
    Ok(())
}

pub fn execute(invocation: Invocation<'_>, state: &mut AppState) -> Result<(), String> {
    if let CommandAvailability::Unavailable(reason) = availability(invocation.spec, state) {
        return Err(format!("/{} unavailable: {reason}", invocation.spec.name));
    }
    let invoked_id = invocation.spec.id;
    match invocation.spec.id {
        CommandId::New => {
            require_no_args(&invocation)?;
            state.session_id = None;
            state.session_config = None;
            state.pending_session_prompt = None;
            state.session_creation_pending = false;
            state.messages.clear();
            state.streaming.clear();
            state.streaming_thinking.clear();
            state.iteration = 0;
            state.input_tokens = 0;
            state.output_tokens = 0;
            state.cost_nano_usd = None;
            state.feedback_target = None;
            state.turn_active = false;
            state.interrupt_requested = false;
            state.queued_prompts.clear();
            state.queued_prompt_attachments.clear();
            state.chat_scroll = 0;
            state.unread_events = 0;
            state.welcomed = false;
            state.mode = AppMode::Normal;
            state.status = "New session ready".into();
        }
        CommandId::Agent => {
            if state.agents.is_empty() {
                state
                    .pending_actions
                    .push(crate::event::Action::DiscoverAgents);
                return Err("Agent catalog is still loading".into());
            }
            match invocation.args.as_slice() {
                [] => {
                    state.modals.push(Box::new(AgentPicker::new(state)));
                }
                [id] => {
                    let index = state
                        .agents
                        .iter()
                        .position(|agent| agent.id.0 == *id || agent.name.eq_ignore_ascii_case(id))
                        .ok_or_else(|| format!("Agent `{id}` is not advertised by the server"))?;
                    let agent = state.agents[index].clone();
                    let changed = state.selected_agent_id.as_ref() != Some(&agent.id);
                    state.selected_agent_id = Some(agent.id);
                    state.metadata.models = agent.models;
                    state.metadata.model = agent.default_model_id;
                    state.session_model_override = None;
                    if changed && state.session_id.is_some() {
                        state.session_id = None;
                        state.session_config = None;
                        state.session_creation_pending = false;
                        state.messages.clear();
                        state.welcomed = false;
                    }
                    state.status =
                        format!("{} selected · next prompt starts a new session", agent.name);
                }
                _ => return Err(format!("Usage: {}", invocation.spec.usage)),
            }
        }
        CommandId::Sessions | CommandId::Resume => {
            require_no_args(&invocation)?;
            state
                .pending_actions
                .push(crate::event::Action::RequestSessions);
            state
                .modals
                .push(Box::new(SessionsOverlay::new(state.sessions.clone())));
        }
        CommandId::Rename => {
            if invocation.args.is_empty() {
                return Err(format!("Usage: {}", invocation.spec.usage));
            }
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no persisted session to rename".to_string())?;
            let label = invocation.args.join(" ");
            state
                .pending_actions
                .push(crate::event::Action::RenameSession { session_id, label });
            state.status = "Renaming session…".into();
        }
        CommandId::Fork => {
            require_no_args(&invocation)?;
            if state.turn_active {
                return Err("Interrupt active work before forking".into());
            }
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no persisted session to fork".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::ForkSession {
                    session_id,
                    completed_turns: None,
                    checkpoint: false,
                });
            state.status = "Forking session…".into();
        }
        CommandId::Rewind => {
            if state.turn_active {
                return Err("Interrupt active work before rewinding".into());
            }
            let [turn] = invocation.args.as_slice() else {
                return Err(format!("Usage: {}", invocation.spec.usage));
            };
            let completed_turns = turn
                .parse::<usize>()
                .ok()
                .filter(|turn| *turn > 0)
                .ok_or_else(|| "Completed turn must be a positive integer".to_string())?;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no persisted session to rewind".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::ForkSession {
                    session_id,
                    completed_turns: Some(completed_turns),
                    checkpoint: false,
                });
            state.status = "Rewinding into a new conversation branch · workspace unchanged…".into();
        }
        CommandId::Checkpoint => {
            require_no_args(&invocation)?;
            if state.turn_active {
                return Err("Interrupt active work before checkpointing".into());
            }
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no persisted session to checkpoint".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::ForkSession {
                    session_id,
                    completed_turns: None,
                    checkpoint: true,
                });
            state.status = "Creating conversation checkpoint · workspace unchanged…".into();
        }
        CommandId::Undo => {
            require_no_args(&invocation)?;
            if state.turn_active {
                return Err("Interrupt active work before returning to the source session".into());
            }
            let session_id = state
                .last_branch_source_session_id
                .take()
                .ok_or_else(|| "There is no conversation branch to undo".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::LoadSession { session_id });
            state.status = "Returning to source conversation · workspace files unchanged…".into();
        }
        CommandId::Clear => {
            require_no_args(&invocation)?;
            state.messages.clear();
            state.streaming.clear();
            state.streaming_thinking.clear();
            state.chat_scroll = 0;
            state.unread_events = 0;
            state.welcomed = false;
            state.status = "Cleared local transcript".into();
        }
        CommandId::Help => {
            let topic = invocation.args.first().copied();
            if invocation.args.len() > 1 {
                return Err(format!("Usage: {}", invocation.spec.usage));
            }
            state.modals.push(Box::new(HelpModal::new(topic)?));
        }
        CommandId::Theme => {
            let name = exactly_one_arg(&invocation)?;
            let theme = name.parse::<ThemeName>()?;
            crate::theme::configure(theme);
            state.status = format!("Theme: {}", crate::theme::active_name());
        }
        CommandId::Tools => {
            if invocation.args.len() > 1 {
                return Err(format!("Usage: {}", invocation.spec.usage));
            }
            state.tool_details_expanded = match invocation.args.first().copied() {
                None => !state.tool_details_expanded,
                Some("expand" | "expanded" | "on") => true,
                Some("collapse" | "collapsed" | "off") => false,
                Some(_) => return Err(format!("Usage: {}", invocation.spec.usage)),
            };
            state.status = if state.tool_details_expanded {
                "Expanded tool details".into()
            } else {
                "Collapsed tool details".into()
            };
        }
        CommandId::Queue => match invocation.args.as_slice() {
            [] => {
                if state.queued_prompts.is_empty() {
                    state
                        .messages
                        .push(ChatMessage::Info("No queued prompts".into()));
                } else {
                    let summary = state
                        .queued_prompts
                        .iter()
                        .enumerate()
                        .map(|(index, prompt)| format!("{}. {}", index + 1, compact_prompt(prompt)))
                        .collect::<Vec<_>>()
                        .join("\n");
                    state
                        .messages
                        .push(ChatMessage::Info(format!("Queued prompts:\n{summary}")));
                }
            }
            ["clear"] => {
                state.queued_prompts.clear();
                state.queued_prompt_attachments.clear();
                state
                    .messages
                    .retain(|message| !matches!(message, ChatMessage::QueuedUser(_)));
                state.status = "Cleared queued prompts".into();
            }
            ["drop", index] => {
                let index = queue_index(index, state.queued_prompts.len(), invocation.spec.usage)?;
                let removed = state.queued_prompts.remove(index).expect("validated index");
                state.queued_prompt_attachments.remove(index);
                remove_queued_message(&mut state.messages, &removed);
                state.status = format!("Removed queued prompt {}", index + 1);
            }
            ["edit", index, replacement @ ..] if !replacement.is_empty() => {
                let index = queue_index(index, state.queued_prompts.len(), invocation.spec.usage)?;
                let replacement = replacement.join(" ");
                let previous = std::mem::replace(
                    state
                        .queued_prompts
                        .get_mut(index)
                        .expect("validated index"),
                    replacement.clone(),
                );
                if let Some(message) = state.messages.iter_mut().find(
                    |message| matches!(message, ChatMessage::QueuedUser(text) if text == &previous),
                ) {
                    *message = ChatMessage::QueuedUser(replacement);
                }
                state.status = format!("Edited queued prompt {}", index + 1);
            }
            _ => return Err(format!("Usage: {}", invocation.spec.usage)),
        },
        CommandId::Tasks => match invocation.args.as_slice() {
            [] => {
                let tasks = state
                    .messages
                    .iter()
                    .flat_map(|message| match message {
                        ChatMessage::TaskList { tasks } => tasks.as_slice(),
                        _ => &[],
                    })
                    .collect::<Vec<_>>();
                if tasks.is_empty() {
                    state
                        .messages
                        .push(ChatMessage::Info("No background tasks".into()));
                } else {
                    let text = tasks
                        .iter()
                        .map(|task| {
                            format!(
                                "{} · {:?} · {}\n  {}",
                                task.task_id, task.state, task.purpose, task.detail
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    state
                        .messages
                        .push(ChatMessage::Info(format!("Background tasks:\n{text}")));
                }
            }
            ["cancel", prefix] => {
                let session_id = state
                    .session_id
                    .clone()
                    .ok_or_else(|| "There is no active session".to_string())?;
                let matches = state
                    .messages
                    .iter()
                    .flat_map(|message| match message {
                        ChatMessage::TaskList { tasks } => tasks.as_slice(),
                        _ => &[],
                    })
                    .filter(|task| {
                        task.task_id.starts_with(prefix)
                            && task.state == crate::app::TaskState::Running
                    })
                    .collect::<Vec<_>>();
                let task = match matches.as_slice() {
                    [task] => *task,
                    [] => return Err(format!("No running task matches `{prefix}`")),
                    _ => return Err(format!("Task prefix `{prefix}` is ambiguous")),
                };
                let task_id = task.task_id.clone();
                state
                    .pending_actions
                    .push(crate::event::Action::CancelTask {
                        session_id,
                        task_id: task_id.clone(),
                    });
                state.status = format!("Cancelling task {}…", &task_id[..8.min(task_id.len())]);
            }
            _ => return Err(format!("Usage: {}", invocation.spec.usage)),
        },
        CommandId::Attachments => match invocation.args.as_slice() {
            [] => {
                if state.composer.attachments.is_empty() {
                    state
                        .messages
                        .push(ChatMessage::Info("No draft attachments".into()));
                } else {
                    let text = state
                        .composer
                        .attachments
                        .iter()
                        .enumerate()
                        .map(|(index, attachment)| format!("{}. {}", index + 1, attachment.label()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    state
                        .messages
                        .push(ChatMessage::Info(format!("Draft attachments:\n{text}")));
                }
            }
            ["clear"] => {
                state.composer.attachments.clear();
                state.status = "Cleared draft attachments".into();
            }
            ["drop", raw] => {
                let index = attachment_index(
                    raw,
                    state.composer.attachment_count(),
                    invocation.spec.usage,
                )?;
                state.composer.remove_attachment(index);
                state.status = format!("Removed attachment {}", index + 1);
            }
            [direction @ ("up" | "down"), raw] => {
                let index = attachment_index(
                    raw,
                    state.composer.attachment_count(),
                    invocation.spec.usage,
                )?;
                let target = if *direction == "up" {
                    index
                        .checked_sub(1)
                        .ok_or_else(|| "Attachment is already first".to_string())?
                } else {
                    let target = index + 1;
                    if target >= state.composer.attachment_count() {
                        return Err("Attachment is already last".into());
                    }
                    target
                };
                state.composer.move_attachment(index, target);
                state.status = format!("Moved attachment {}", index + 1);
            }
            ["selection"] => {
                let selected = state
                    .composer
                    .selected_text()
                    .ok_or_else(|| "Select composer text with Shift+Arrow first".to_string())?;
                state.composer.attach_text(
                    AttachmentKind::Selection,
                    "composer selection",
                    "text/plain",
                    selected,
                )?;
                state.status = "Attached composer selection".into();
            }
            [kind @ ("tool" | "diff"), prefix] => {
                let (call_id, tool_name, output) = find_tool_output(state, Some(prefix))?;
                let (attachment_kind, mime_type) = if *kind == "diff" {
                    (AttachmentKind::Diff, "text/x-diff")
                } else {
                    (AttachmentKind::TerminalOutput, "text/plain")
                };
                state.composer.attach_text(
                    attachment_kind,
                    format!("{tool_name} {}", &call_id[..8.min(call_id.len())]),
                    mime_type,
                    output,
                )?;
                state.status = format!("Attached {kind} output");
            }
            _ => return Err(format!("Usage: {}", invocation.spec.usage)),
        },
        CommandId::Mention => {
            require_no_args(&invocation)?;
            state.modals.push(Box::new(FileMentionModal::new(
                state.metadata.workspace.clone(),
                state.metadata.max_attachment_bytes,
                state.metadata.supports_vision(),
            )));
        }
        CommandId::Diff => {
            require_no_args(&invocation)?;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no coding session to inspect".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::InspectCodingSession { session_id });
            state.status = "Loading coding session changes…".into();
        }
        CommandId::Accept => {
            require_no_args(&invocation)?;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no coding session to accept".to_string())?;
            state.modals.push(Box::new(
                crate::modal::CodingSessionConfirmationModal::accept(session_id),
            ));
            state.status = "Review merge confirmation".into();
        }
        CommandId::Discard => {
            require_no_args(&invocation)?;
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no coding session to discard".to_string())?;
            state.modals.push(Box::new(
                crate::modal::CodingSessionConfirmationModal::discard(session_id),
            ));
            state.status = "Review discard confirmation".into();
        }
        CommandId::Review => {
            if state.turn_active {
                return Err("Wait for active work to finish before starting a review".into());
            }
            let scope = diff_scope(&invocation)?;
            state
                .pending_actions
                .push(crate::event::Action::ReviewWorkspaceChanges {
                    scope,
                    workspace: state.metadata.workspace.clone(),
                });
            state.status = format!("Preparing review of {}…", scope.label());
        }
        CommandId::Config => {
            require_no_args(&invocation)?;
            state
                .pending_actions
                .push(crate::event::Action::InspectConfig);
            state.status = "Loading resolved configuration…".into();
        }
        CommandId::Doctor => {
            let destination = match invocation.args.as_slice() {
                [] => crate::event::DoctorDestination::Inspect,
                ["copy"] => crate::event::DoctorDestination::Copy,
                ["export", path @ ..] if !path.is_empty() => {
                    crate::event::DoctorDestination::Export(path.join(" ").into())
                }
                _ => return Err(format!("Usage: {}", invocation.spec.usage)),
            };
            state
                .pending_actions
                .push(crate::event::Action::RunDoctor { destination });
            state.status = "Preparing redacted diagnostics…".into();
        }
        CommandId::Mcp => {
            require_no_args(&invocation)?;
            state.messages.push(ChatMessage::Info(platform_report(
                "MCP servers",
                sylvander_protocol::PlatformFeatureKind::Mcp,
                &state.platform,
            )));
        }
        CommandId::Skills => {
            require_no_args(&invocation)?;
            state.messages.push(ChatMessage::Info(platform_report(
                "Skills",
                sylvander_protocol::PlatformFeatureKind::Skill,
                &state.platform,
            )));
        }
        CommandId::Hooks => {
            require_no_args(&invocation)?;
            state.messages.push(ChatMessage::Info(platform_report(
                "Hooks",
                sylvander_protocol::PlatformFeatureKind::Hook,
                &state.platform,
            )));
        }
        CommandId::Extensions => {
            require_no_args(&invocation)?;
            state.messages.push(ChatMessage::Info(platform_report(
                "Extensions",
                sylvander_protocol::PlatformFeatureKind::Extension,
                &state.platform,
            )));
        }
        CommandId::Memory => {
            require_no_args(&invocation)?;
            state.messages.push(ChatMessage::Info(platform_report(
                "Memory",
                sylvander_protocol::PlatformFeatureKind::Memory,
                &state.platform,
            )));
        }
        CommandId::Profile => execute_profile(&invocation, state)?,
        CommandId::Feedback => execute_feedback(&invocation, state)?,
        CommandId::Status => {
            require_no_args(&invocation)?;
            let session = state.session_id.as_deref().unwrap_or("new");
            state.messages.push(ChatMessage::Info(format!(
                "model {} · permissions {} · branch {} · session {} · iteration {} · {} input + {} output tokens · {}",
                state.metadata.model_label(),
                permission_summary(&state.metadata.permissions),
                state.metadata.branch,
                session,
                state.iteration,
                state.input_tokens,
                state.output_tokens,
                state.cost_nano_usd.map_or_else(
                    || "cost unavailable".into(),
                    |cost| format!("estimated cost {}", crate::app::format_cost(cost)),
                )
            )));
        }
        CommandId::Context => {
            require_no_args(&invocation)?;
            state
                .pending_actions
                .push(crate::event::Action::RequestContext {
                    session_id: state.session_id.clone(),
                });
            state.status = "Loading context report…".into();
        }
        CommandId::Compact => {
            require_no_args(&invocation)?;
            if state.turn_active {
                return Err("Interrupt active work before compacting".into());
            }
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no session to compact".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::CompactSession { session_id });
            state.status = "Requesting context compaction…".into();
        }
        CommandId::Rollback => {
            require_no_args(&invocation)?;
            if state.turn_active {
                return Err("Interrupt active work before rolling back files".into());
            }
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "There is no session to roll back".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::PreviewWorkspaceRollback { session_id });
            state.status = "Inspecting latest Agent file changes…".into();
        }
        CommandId::Inspect => {
            let prefix = optional_one_arg(&invocation)?;
            let (call_id, tool_name, output) = find_tool_output(state, prefix)?;
            state
                .modals
                .push(Box::new(ToolInspector::new(call_id, tool_name, output)));
        }
        CommandId::Copy => {
            let prefix = optional_one_arg(&invocation)?;
            let (call_id, _, output) = find_tool_output(state, prefix)?;
            state
                .pending_actions
                .push(crate::event::Action::CopyText { text: output });
            state.status = format!("Copying tool output {}…", &call_id[..8.min(call_id.len())]);
        }
        CommandId::Editor => {
            require_no_args(&invocation)?;
            state.pending_actions.push(crate::event::Action::EditDraft);
            state.status = "Opening external editor…".into();
        }
        CommandId::Model => match invocation.args.as_slice() {
            [] => {
                if state.metadata.models.is_empty() {
                    state
                        .pending_actions
                        .push(crate::event::Action::RequestRuntimeInfo);
                    return Err("Model catalog is still loading".into());
                }
                state.modals.push(Box::new(ModelPicker::new(state)));
            }
            [model] | [model, _] => {
                let matches = state
                    .metadata
                    .models
                    .iter()
                    .filter(|entry| {
                        model
                            .split_once('/')
                            .map_or(entry.id == *model, |(provider, id)| {
                                entry.provider == provider && entry.id == id
                            })
                    })
                    .collect::<Vec<_>>();
                let descriptor = match matches.as_slice() {
                    [] => {
                        return Err(format!("Model `{model}` is not advertised by the server"));
                    }
                    [descriptor] => *descriptor,
                    _ => {
                        return Err(format!(
                            "Model `{model}` is available from multiple providers; use provider/model"
                        ));
                    }
                };
                let effort = invocation
                    .args
                    .get(1)
                    .map(|value| parse_reasoning_effort(value))
                    .transpose()?
                    .unwrap_or(sylvander_protocol::ReasoningEffort::Off);
                if !descriptor.reasoning_efforts.contains(&effort) {
                    return Err(format!(
                        "Model `{model}` does not advertise reasoning `{}`",
                        crate::app::reasoning_label(effort)
                    ));
                }
                let selection = sylvander_protocol::ModelSelection {
                    provider_id: descriptor.provider.clone(),
                    model_id: descriptor.id.clone(),
                };
                if let Some(session_id) = state.session_id.clone() {
                    state
                        .pending_actions
                        .push(crate::event::Action::SelectModel {
                            session_id,
                            model: selection,
                            reasoning_effort: effort,
                        });
                    state.status = "Selecting model…".into();
                } else {
                    state.metadata.model.clone_from(&selection.model_id);
                    state.metadata.reasoning_effort = effort;
                    state.session_model_override = Some((selection, effort));
                    state.status = "Model override ready · applies when the session starts".into();
                }
            }
            _ => return Err(format!("Usage: {}", invocation.spec.usage)),
        },
        CommandId::Permissions => {
            require_no_args(&invocation)?;
            state.modals.push(Box::new(PermissionsPicker::new(state)));
        }
        CommandId::Preview => {
            let [kind, target @ ..] = invocation.args.as_slice() else {
                return Err(format!("Usage: {}", invocation.spec.usage));
            };
            let kind = match *kind {
                "image" => crate::host_bridge::PreviewKind::Image,
                "web" => crate::host_bridge::PreviewKind::Web,
                _ => return Err(format!("Usage: {}", invocation.spec.usage)),
            };
            let target = target.join(" ");
            if target.is_empty() {
                return Err(format!("Usage: {}", invocation.spec.usage));
            }
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "Host preview requires a persisted session".to_string())?;
            state
                .pending_actions
                .push(crate::event::Action::HostPreview {
                    session_id,
                    kind,
                    target,
                });
            state.status = "Requesting host preview…".into();
        }
        CommandId::Quit => {
            require_no_args(&invocation)?;
            state.should_quit = true;
        }
    }
    state.recent_commands.retain(|id| *id != invoked_id);
    state.recent_commands.push_front(invoked_id);
    state.recent_commands.truncate(8);
    state.dirty.mark();
    Ok(())
}

fn execute_feedback(invocation: &Invocation<'_>, state: &mut AppState) -> Result<(), String> {
    use sylvander_protocol::{FeedbackPrivacyClass, FeedbackRating, RunFeedback};

    let target = state
        .feedback_target
        .clone()
        .ok_or_else(|| "Complete a turn before recording feedback".to_string())?;
    let (rating, note, correction, task_result) = match invocation.args.as_slice() {
        [kind, rest @ ..] if matches!(*kind, "positive" | "up" | "+") => {
            (FeedbackRating::Positive, joined_feedback(rest), None, None)
        }
        [kind, rest @ ..] if matches!(*kind, "negative" | "down" | "-") => {
            (FeedbackRating::Negative, joined_feedback(rest), None, None)
        }
        ["correction" | "correct", rest @ ..] => {
            let correction = joined_feedback(rest)
                .ok_or_else(|| "Usage: /feedback correction <correct response>".to_string())?;
            (FeedbackRating::Negative, None, Some(correction), None)
        }
        ["note", rating, rest @ ..] => {
            let rating = match *rating {
                "positive" | "up" | "+" => FeedbackRating::Positive,
                "negative" | "down" | "-" => FeedbackRating::Negative,
                _ => {
                    return Err("Usage: /feedback note <positive|negative> <note>".into());
                }
            };
            let note =
                joined_feedback(rest).ok_or_else(|| "Feedback note cannot be empty".to_string())?;
            (rating, Some(note), None, None)
        }
        _ => return Err(format!("Usage: {}", invocation.spec.usage)),
    };
    if note.as_ref().is_some_and(|value| value.len() > 4096)
        || correction.as_ref().is_some_and(|value| value.len() > 4096)
    {
        return Err("Feedback text must not exceed 4096 bytes".into());
    }
    state
        .pending_actions
        .push(crate::event::Action::SubmitFeedback {
            feedback: RunFeedback {
                target,
                rating,
                note,
                correction,
                tags: vec!["tui".into()],
                task_result,
                artifacts: Vec::new(),
                validations: Vec::new(),
                privacy_class: FeedbackPrivacyClass::Private,
            },
        });
    state.status = "Recording feedback…".into();
    Ok(())
}

fn joined_feedback(parts: &[&str]) -> Option<String> {
    let text = parts.join(" ");
    (!text.is_empty()).then_some(text)
}

fn execute_profile(invocation: &Invocation<'_>, state: &mut AppState) -> Result<(), String> {
    use sylvander_protocol::{
        USER_PROFILE_PROTOCOL_VERSION, UserProfileAction, UserProfileExportFormat,
        UserProfileRequest,
    };

    let request = |action| crate::event::Action::UserProfile {
        request: UserProfileRequest {
            version: USER_PROFILE_PROTOCOL_VERSION,
            action,
        },
    };
    match invocation.args.as_slice() {
        [] | ["show" | "read"] => {
            state
                .pending_actions
                .push(request(UserProfileAction::Read {}));
            state.status = "Loading user profile…".into();
        }
        ["create"] => {
            state.modals.push(Box::new(crate::modal::ProfileEditor::new(
                crate::modal::ProfileEditMode::Create,
                None,
            )));
            state.status = "Creating a typed user profile".into();
        }
        ["edit" | "update"] => {
            state.pending_profile_intent =
                Some(crate::app::PendingProfileIntent::Edit { correction: false });
            state
                .pending_actions
                .push(request(UserProfileAction::Read {}));
            state.status = "Loading the current profile revision…".into();
        }
        ["correct"] => {
            state.pending_profile_intent =
                Some(crate::app::PendingProfileIntent::Edit { correction: true });
            state
                .pending_actions
                .push(request(UserProfileAction::Read {}));
            state.status = "Loading the current profile revision…".into();
        }
        ["do-not-learn" | "do_not_learn" | "dnl", enabled] => {
            let enabled = match enabled.to_ascii_lowercase().as_str() {
                "on" => true,
                "off" => false,
                _ => return Err("Usage: /profile do-not-learn <on|off>".into()),
            };
            state.pending_profile_intent =
                Some(crate::app::PendingProfileIntent::SetDoNotLearn(enabled));
            state
                .pending_actions
                .push(request(UserProfileAction::Read {}));
            state.status = "Loading the current profile revision…".into();
        }
        ["export"] => {
            state
                .pending_actions
                .push(request(UserProfileAction::Export {
                    format: UserProfileExportFormat::Json,
                }));
            state.status = "Exporting user profile…".into();
        }
        ["delete"] => {
            state.pending_profile_intent = Some(crate::app::PendingProfileIntent::Delete);
            state
                .pending_actions
                .push(request(UserProfileAction::Read {}));
            state.status = "Loading the current profile revision…".into();
        }
        _ => return Err(format!("Usage: {}", invocation.spec.usage)),
    }
    Ok(())
}

fn permission_summary(profile: &sylvander_protocol::PermissionProfile) -> String {
    let files = match profile.file_access {
        sylvander_protocol::FileAccess::None => "no-files",
        sylvander_protocol::FileAccess::ReadOnly => "read-only",
        sylvander_protocol::FileAccess::WorkspaceWrite => "workspace-write",
    };
    let network = match profile.network_access {
        sylvander_protocol::NetworkAccess::Denied => "net-deny",
        sylvander_protocol::NetworkAccess::Allowed => "net-allow",
    };
    let approval = match profile.approval_policy {
        sylvander_protocol::ApprovalPolicy::Ask => "ask",
        sylvander_protocol::ApprovalPolicy::Allow => "allow",
        sylvander_protocol::ApprovalPolicy::Deny => "deny",
    };
    format!("{files}/{network}/{approval}")
}

fn platform_report(
    title: &str,
    kind: sylvander_protocol::PlatformFeatureKind,
    snapshot: &sylvander_protocol::PlatformSnapshot,
) -> String {
    let features = snapshot
        .features
        .iter()
        .filter(|feature| feature.kind == kind)
        .collect::<Vec<_>>();
    if features.is_empty() {
        return format!("{title}\nNo {title} advertised by the Agent.");
    }
    let rows = features
        .iter()
        .map(|feature| {
            let status = platform_status_label(feature.status);
            let auth = platform_auth_label(feature.auth);
            let trust = feature
                .trust.map_or_else(|| "unspecified".into(), platform_trust_label);
            let source = feature
                .source
                .as_deref()
                .map_or(String::new(), |source| format!("\n  source {source}"));
            let capabilities = if feature.capabilities.is_empty() {
                String::new()
            } else {
                format!("\n  capabilities {}", feature.capabilities.join(", "))
            };
            format!(
                "{} · {status} · auth {auth} · trust {trust} · reload {}\n  {}{source}{capabilities}",
                feature.name,
                if feature.reloadable { "available" } else { "no" },
                feature.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{title}\n{rows}")
}

fn platform_status_label(status: sylvander_protocol::PlatformFeatureStatus) -> &'static str {
    match status {
        sylvander_protocol::PlatformFeatureStatus::Active => "active",
        sylvander_protocol::PlatformFeatureStatus::Configured => "configured",
        sylvander_protocol::PlatformFeatureStatus::Degraded => "degraded",
        sylvander_protocol::PlatformFeatureStatus::Unavailable => "unavailable",
    }
}

fn platform_auth_label(auth: sylvander_protocol::PlatformAuthStatus) -> &'static str {
    match auth {
        sylvander_protocol::PlatformAuthStatus::NotRequired => "not-required",
        sylvander_protocol::PlatformAuthStatus::Configured => "configured",
        sylvander_protocol::PlatformAuthStatus::Missing => "missing",
        sylvander_protocol::PlatformAuthStatus::Unknown => "unknown",
    }
}

fn platform_trust_label(trust: sylvander_protocol::PlatformTrust) -> String {
    match trust {
        sylvander_protocol::PlatformTrust::BuiltIn => "built-in",
        sylvander_protocol::PlatformTrust::Workspace => "workspace",
        sylvander_protocol::PlatformTrust::User => "user",
        sylvander_protocol::PlatformTrust::External => "external",
        sylvander_protocol::PlatformTrust::Unverified => "unverified",
    }
    .into()
}

fn parse_reasoning_effort(value: &str) -> Result<sylvander_protocol::ReasoningEffort, String> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Ok(sylvander_protocol::ReasoningEffort::Off),
        "low" => Ok(sylvander_protocol::ReasoningEffort::Low),
        "medium" => Ok(sylvander_protocol::ReasoningEffort::Medium),
        "high" => Ok(sylvander_protocol::ReasoningEffort::High),
        _ => Err("Reasoning must be off, low, medium, or high".into()),
    }
}

fn compact_prompt(prompt: &str) -> String {
    let single_line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= 72 {
        single_line
    } else {
        format!("{}…", single_line.chars().take(71).collect::<String>())
    }
}

fn queue_index(raw: &str, len: usize, usage: &str) -> Result<usize, String> {
    let one_based = raw
        .parse::<usize>()
        .map_err(|_| format!("Usage: {usage}"))?;
    let index = one_based.saturating_sub(1);
    if one_based == 0 || index >= len {
        return Err(format!("Queue item {one_based} does not exist"));
    }
    Ok(index)
}

fn attachment_index(raw: &str, len: usize, usage: &str) -> Result<usize, String> {
    queue_index(raw, len, usage).map_err(|_| format!("Attachment `{raw}` does not exist"))
}

fn remove_queued_message(messages: &mut Vec<ChatMessage>, prompt: &str) {
    if let Some(index) = messages
        .iter()
        .position(|message| matches!(message, ChatMessage::QueuedUser(text) if text == prompt))
    {
        messages.remove(index);
    }
}

fn require_no_args(invocation: &Invocation<'_>) -> Result<(), String> {
    if invocation.args.is_empty() {
        Ok(())
    } else {
        Err(format!("Usage: {}", invocation.spec.usage))
    }
}

fn exactly_one_arg<'a>(invocation: &'a Invocation<'a>) -> Result<&'a str, String> {
    if let [argument] = invocation.args.as_slice() {
        Ok(*argument)
    } else {
        Err(format!("Usage: {}", invocation.spec.usage))
    }
}

fn optional_one_arg<'a>(invocation: &'a Invocation<'a>) -> Result<Option<&'a str>, String> {
    match invocation.args.as_slice() {
        [] => Ok(None),
        [argument] => Ok(Some(*argument)),
        _ => Err(format!("Usage: {}", invocation.spec.usage)),
    }
}

fn diff_scope(invocation: &Invocation<'_>) -> Result<crate::event::WorkspaceDiffScope, String> {
    match invocation.args.as_slice() {
        [] => Ok(crate::event::WorkspaceDiffScope::All),
        ["staged"] => Ok(crate::event::WorkspaceDiffScope::Staged),
        ["unstaged"] => Ok(crate::event::WorkspaceDiffScope::Unstaged),
        _ => Err(format!("Usage: {}", invocation.spec.usage)),
    }
}

fn find_tool_output(
    state: &AppState,
    prefix: Option<&str>,
) -> Result<(String, String, String), String> {
    let matches = state
        .messages
        .iter()
        .rev()
        .flat_map(|message| match message {
            ChatMessage::ToolStep { children, .. } => children.as_slice(),
            _ => &[],
        })
        .filter(|child| {
            child.output.is_some() && prefix.is_none_or(|prefix| child.call_id.starts_with(prefix))
        })
        .collect::<Vec<_>>();
    let child = match (prefix, matches.as_slice()) {
        (_, []) => return Err("No completed tool output matches".into()),
        (Some(prefix), [_, _, ..]) => return Err(format!("Tool prefix `{prefix}` is ambiguous")),
        (_, [child, ..]) => *child,
    };
    Ok((
        child.call_id.clone(),
        child.name.clone(),
        child.output.clone().expect("filtered output"),
    ))
}

#[cfg(test)]
#[path = "../tests/unit/command.rs"]
mod tests;
