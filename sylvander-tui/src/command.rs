//! Application commands exposed through the `/` command line.

use crate::app::{AppMode, AppState, ChatMessage};
use crate::modal::{HelpModal, SessionsOverlay};
use crate::theme::ThemeName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    New,
    Sessions,
    Clear,
    Help,
    Theme,
    Tools,
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
}
