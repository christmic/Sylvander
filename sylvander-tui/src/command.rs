//! Application commands exposed through the `/` command line.

use crate::app::{AppMode, AppState, ChatMessage};
use crate::modal::{HelpModal, SessionsOverlay};
use crate::theme::ThemeName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    New,
    Sessions,
    Resume,
    Rename,
    Fork,
    Clear,
    Help,
    Theme,
    Tools,
    Queue,
    Tasks,
    Attachments,
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
        id: CommandId::Clear,
        name: "clear",
        usage: "/clear",
        description: "Clear the local transcript",
        hint: "keeps session",
    },
    CommandSpec {
        id: CommandId::Help,
        name: "help",
        usage: "/help [commands|approval|tools]",
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
        usage: "/attachments [drop <n>|up <n>|down <n>|clear]",
        description: "Inspect, reorder, or remove draft attachments",
        hint: "@ adds files",
    },
    CommandSpec {
        id: CommandId::Status,
        name: "status",
        usage: "/status",
        description: "Show runtime and token usage",
        hint: "local",
    },
    CommandSpec {
        id: CommandId::Quit,
        name: "quit",
        usage: "/quit",
        description: "Quit sylvander-tui",
        hint: "ctrl+c",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation<'a> {
    pub spec: &'static CommandSpec,
    pub args: Vec<&'a str>,
}

pub fn parse(line: &str) -> Result<Invocation<'_>, String> {
    let mut parts = line.trim().trim_start_matches('/').split_whitespace();
    let name = parts.next().ok_or_else(|| "Choose a command".to_string())?;
    let spec = COMMANDS
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("Unknown command /{name}"))?;
    Ok(Invocation {
        spec,
        args: parts.collect(),
    })
}

pub fn execute(invocation: Invocation<'_>, state: &mut AppState) -> Result<(), String> {
    match invocation.spec.id {
        CommandId::New => {
            require_no_args(&invocation)?;
            state.session_id = None;
            state.messages.clear();
            state.streaming.clear();
            state.streaming_thinking.clear();
            state.iteration = 0;
            state.input_tokens = 0;
            state.output_tokens = 0;
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
        CommandId::Sessions => {
            require_no_args(&invocation)?;
            state
                .pending_actions
                .push(crate::event::Action::RequestSessions);
            state
                .modals
                .push(Box::new(SessionsOverlay::new(state.sessions.clone())));
        }
        CommandId::Resume => {
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
            state.pending_actions.push(crate::event::Action::ForkSession {
                session_id,
            });
            state.status = "Forking session…".into();
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
                        .map(|(index, prompt)| {
                            format!("{}. {}", index + 1, compact_prompt(prompt))
                        })
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
                    state.queued_prompts.get_mut(index).expect("validated index"),
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
                let tasks = state.messages.iter().flat_map(|message| match message {
                    ChatMessage::TaskList { tasks } => tasks.as_slice(),
                    _ => &[],
                }).collect::<Vec<_>>();
                if tasks.is_empty() {
                    state.messages.push(ChatMessage::Info("No background tasks".into()));
                } else {
                    let text = tasks.iter().map(|task| {
                        format!(
                            "{} · {:?} · {}\n  {}",
                            task.task_id, task.state, task.purpose, task.detail
                        )
                    }).collect::<Vec<_>>().join("\n");
                    state.messages.push(ChatMessage::Info(format!("Background tasks:\n{text}")));
                }
            }
            ["cancel", prefix] => {
                let session_id = state.session_id.clone()
                    .ok_or_else(|| "There is no active session".to_string())?;
                let matches = state.messages.iter().flat_map(|message| match message {
                    ChatMessage::TaskList { tasks } => tasks.as_slice(),
                    _ => &[],
                }).filter(|task| {
                    task.task_id.starts_with(prefix)
                        && task.state == crate::app::TaskState::Running
                }).collect::<Vec<_>>();
                let task = match matches.as_slice() {
                    [task] => *task,
                    [] => return Err(format!("No running task matches `{prefix}`")),
                    _ => return Err(format!("Task prefix `{prefix}` is ambiguous")),
                };
                let task_id = task.task_id.clone();
                state.pending_actions.push(crate::event::Action::CancelTask {
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
                    state.messages.push(ChatMessage::Info("No draft attachments".into()));
                } else {
                    let text = state.composer.attachments.iter().enumerate()
                        .map(|(index, attachment)| format!("{}. {}", index + 1, attachment.label()))
                        .collect::<Vec<_>>().join("\n");
                    state.messages.push(ChatMessage::Info(format!("Draft attachments:\n{text}")));
                }
            }
            ["clear"] => {
                state.composer.attachments.clear();
                state.status = "Cleared draft attachments".into();
            }
            ["drop", raw] => {
                let index = attachment_index(raw, state.composer.attachment_count(), invocation.spec.usage)?;
                state.composer.remove_attachment(index);
                state.status = format!("Removed attachment {}", index + 1);
            }
            [direction @ ("up" | "down"), raw] => {
                let index = attachment_index(raw, state.composer.attachment_count(), invocation.spec.usage)?;
                let target = if *direction == "up" {
                    index.checked_sub(1).ok_or_else(|| "Attachment is already first".to_string())?
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
            _ => return Err(format!("Usage: {}", invocation.spec.usage)),
        },
        CommandId::Status => {
            require_no_args(&invocation)?;
            let session = state.session_id.as_deref().unwrap_or("new");
            state.messages.push(ChatMessage::Info(format!(
                "model {} · branch {} · session {} · iteration {} · {} input + {} output tokens",
                state.metadata.model,
                state.metadata.branch,
                session,
                state.iteration,
                state.input_tokens,
                state.output_tokens
            )));
        }
        CommandId::Quit => {
            require_no_args(&invocation)?;
            state.should_quit = true;
        }
    }
    state.dirty.mark();
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_arguments_and_leading_slash() {
        let invocation = parse("/theme midnight").unwrap();
        assert_eq!(invocation.spec.id, CommandId::Theme);
        assert_eq!(invocation.args, vec!["midnight"]);
    }

    #[test]
    fn new_resets_conversation_without_sending_an_empty_prompt() {
        let mut state = AppState::new();
        state.session_id = Some("old".into());
        state.messages.push(ChatMessage::User("hello".into()));
        execute(parse("new").unwrap(), &mut state).unwrap();
        assert!(state.session_id.is_none());
        assert!(state.messages.is_empty());
        assert!(state.pending_actions.is_empty());
    }

    #[test]
    fn tools_argument_is_validated() {
        let mut state = AppState::new();
        execute(parse("tools expand").unwrap(), &mut state).unwrap();
        assert!(state.tool_details_expanded);
        assert!(execute(parse("tools sideways").unwrap(), &mut state).is_err());
    }

    #[test]
    fn queue_commands_edit_and_remove_waiting_prompts() {
        let mut state = AppState::new();
        state.queued_prompts.push_back("first".into());
        state.messages.push(ChatMessage::QueuedUser("first".into()));

        execute(parse("queue edit 1 updated prompt").unwrap(), &mut state).unwrap();
        assert_eq!(state.queued_prompts[0], "updated prompt");
        assert!(matches!(
            state.messages[0],
            ChatMessage::QueuedUser(ref text) if text == "updated prompt"
        ));

        execute(parse("queue drop 1").unwrap(), &mut state).unwrap();
        assert!(state.queued_prompts.is_empty());
        assert!(state.messages.is_empty());
    }

    #[test]
    fn tasks_cancel_requires_one_running_task_and_keeps_session_scope() {
        let mut state = AppState::new();
        state.session_id = Some("session-1".into());
        state.messages.push(ChatMessage::TaskList {
            tasks: vec![crate::app::TaskEntry {
                task_id: "abcdef12-3456".into(),
                owner: "sylvander".into(),
                purpose: "Inspect".into(),
                state: crate::app::TaskState::Running,
                detail: "iteration 1".into(),
            }],
        });

        execute(parse("tasks cancel abcdef12").unwrap(), &mut state).unwrap();
        assert!(matches!(
            &state.pending_actions[0],
            crate::event::Action::CancelTask { session_id, task_id }
                if session_id == "session-1" && task_id == "abcdef12-3456"
        ));
    }

    #[test]
    fn attachments_commands_reorder_and_remove_draft_context() {
        let mut state = AppState::new();
        state.composer.attachments.push(crate::input::Attachment::new_paste("first".into()));
        state.composer.attachments.push(crate::input::Attachment::new_paste("second".into()));
        execute(parse("attachments up 2").unwrap(), &mut state).unwrap();
        assert_eq!(state.composer.attachments[0].content, "second");
        execute(parse("attachments drop 1").unwrap(), &mut state).unwrap();
        assert_eq!(state.composer.attachments.len(), 1);
        assert_eq!(state.composer.attachments[0].content, "first");
    }
}
