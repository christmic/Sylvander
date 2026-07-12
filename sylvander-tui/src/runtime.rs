//! Event-driven terminal runtime.
//!
//! User input renders immediately. Service streaming is coalesced to the
//! configured frame rate, while lower-frequency animation ticks are isolated.

use std::io::{Write, stdout};

use base64::Engine as _;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::app::AppState;
use crate::application::{Application, UserIntent};
use crate::config::TuiConfig;
use crate::event::DomainEvent;
use crate::service::AgentService;
use crate::{terminal_input, ui};

enum Wake {
    Input(Option<UserIntent>),
    Service(Option<DomainEvent>),
    Frame,
    Animation,
    Reconnect,
}

const MAX_INPUT_BATCH: usize = 64;
const MAX_SERVICE_BATCH: usize = 256;

struct TerminalModeGuard;

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        disable_raw_mode().ok();
        execute!(
            stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        )
        .ok();
    }
}

pub async fn run(config: TuiConfig) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let _terminal_mode = TerminalModeGuard;
    terminal.clear()?;
    execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;

    let state = AppState::with_metadata(config.history_path.clone(), config.metadata.clone());
    let mut application = Application::new(state);
    let mut input = terminal_input::spawn(config.mouse_scroll_lines);
    let mut service = AgentService::new(&config.socket_path);
    let connection = service.connect().await;
    application.apply(connection);

    let mut frame_clock = tokio::time::interval(config.render_interval);
    frame_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut animation_clock = tokio::time::interval(config.animation_interval);
    animation_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut reconnect_clock = tokio::time::interval(config.reconnect_interval);
    reconnect_clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut input_open = true;
    let mut service_open = true;

    loop {
        let wake = tokio::select! {
            intent = input.recv(), if input_open => Wake::Input(intent),
            event = service.recv(), if service_open => Wake::Service(event),
            _ = frame_clock.tick() => Wake::Frame,
            _ = animation_clock.tick() => Wake::Animation,
            _ = reconnect_clock.tick() => Wake::Reconnect,
        };

        let render_immediately = matches!(&wake, Wake::Input(Some(_)));
        let frame_due = matches!(&wake, Wake::Frame);
        match wake {
            Wake::Input(Some(intent)) => application.handle(intent),
            Wake::Input(None) => input_open = false,
            Wake::Service(Some(event)) => application.apply(event),
            Wake::Service(None) => service_open = false,
            Wake::Frame => {}
            Wake::Animation => application.apply(DomainEvent::Tick),
            Wake::Reconnect => {
                if !application.state.connected {
                    let connection = service.connect().await;
                    application.apply(connection);
                }
            }
        }

        // Coalesce bursts without delaying the first input event.
        let mut had_input = render_immediately;
        for _ in 0..MAX_INPUT_BATCH {
            let Ok(intent) = input.try_recv() else {
                break;
            };
            had_input = true;
            application.handle(intent);
        }
        if had_input {
            application.state.save_draft();
        }
        for _ in 0..MAX_SERVICE_BATCH {
            let Some(event) = service.try_recv() else {
                break;
            };
            application.apply(event);
        }

        for effect in application.take_effects() {
            if let crate::event::Action::CopyText { text } = effect {
                match copy_osc52(&text) {
                    Ok(()) => application.state.status = "Copied tool output".into(),
                    Err(error) => application.state.status = format!("Copy failed: {error}"),
                }
                application.state.dirty.mark();
                continue;
            }
            if matches!(effect, crate::event::Action::EditDraft) {
                disable_raw_mode()?;
                execute!(
                    stdout(),
                    DisableMouseCapture,
                    DisableBracketedPaste,
                    LeaveAlternateScreen
                )?;
                let original = application.state.composer.text();
                let editor_task =
                    tokio::task::spawn_blocking(move || edit_draft_in_external_editor(&original))
                        .await;
                enable_raw_mode()?;
                execute!(
                    stdout(),
                    EnterAlternateScreen,
                    EnableBracketedPaste,
                    EnableMouseCapture
                )?;
                terminal.clear()?;
                let result = editor_task.unwrap_or_else(|error| {
                    Err(std::io::Error::other(format!(
                        "editor task failed: {error}"
                    )))
                });
                match result {
                    Ok(text) => {
                        application.state.composer.replace_text(&text);
                        application.state.save_draft();
                        application.state.status = "Draft updated from external editor".into();
                    }
                    Err(error) => {
                        application.state.status = format!("Editor failed: {error}");
                    }
                }
                application.state.dirty.mark();
                continue;
            }
            if let Err(error) = service.execute(effect).await {
                application.apply(DomainEvent::Disconnected {
                    reason: error.to_string(),
                });
            }
        }
        application.state.modals.reap();

        if (render_immediately || frame_due) && application.state.dirty.take() {
            terminal.draw(|frame| ui::dispatch(frame, &application.state))?;
        }

        if application.state.should_quit {
            application.state.save_history();
            return Ok(());
        }

        if !input_open && !service_open {
            return Ok(());
        }
    }
}

fn copy_osc52(text: &str) -> std::io::Result<()> {
    let sequence = osc52_sequence(text)?;
    let mut output = stdout().lock();
    output.write_all(sequence.as_bytes())?;
    output.flush()
}

fn osc52_sequence(text: &str) -> std::io::Result<String> {
    const MAX_CLIPBOARD_BYTES: usize = 100 * 1024;
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output exceeds the 100 KiB terminal clipboard limit",
        ));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    Ok(format!("\x1b]52;c;{encoded}\x07"))
}

fn edit_draft_in_external_editor(initial: &str) -> std::io::Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    edit_draft_with_command(initial, &editor)
}

fn edit_draft_with_command(initial: &str, editor: &str) -> std::io::Result<String> {
    let argv = shell_words::split(&editor).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid editor command: {error}"),
        )
    })?;
    let (program, args) = argv.split_first().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "editor command is empty")
    })?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "sylvander-draft-{}-{unique}.md",
        std::process::id()
    ));
    let operation = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(initial.as_bytes())?;
        file.flush()?;
        let status = std::process::Command::new(program)
            .args(args)
            .arg(&path)
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "editor exited with {status}"
            )));
        }
        std::fs::read_to_string(&path)
    })();
    let _ = std::fs::remove_file(path);
    operation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_clipboard_is_bounded_and_round_trips_utf8() {
        let sequence = osc52_sequence("蟹 helper").unwrap();
        let encoded = sequence
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            "蟹 helper".as_bytes()
        );
        assert!(osc52_sequence(&"x".repeat(100 * 1024 + 1)).is_err());
    }

    #[test]
    fn external_editor_replaces_text_only_after_success() {
        let edited =
            edit_draft_with_command("before", "sh -c 'printf after > \"$1\"' sylvander-editor")
                .unwrap();
        assert_eq!(edited, "after");
        assert!(edit_draft_with_command("before", "sh -c 'exit 7'").is_err());
    }
}
