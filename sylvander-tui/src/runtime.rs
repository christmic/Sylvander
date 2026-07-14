//! Event-driven terminal runtime.
//!
//! User input renders immediately. Service streaming is coalesced to the
//! configured frame rate, while lower-frequency animation ticks are isolated.

use std::io::{Write, stdout};
use std::time::{Duration, Instant};

use base64::Engine as _;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
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
const DRAFT_SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct DraftSaveSchedule {
    due: Option<Instant>,
}

impl DraftSaveSchedule {
    fn mark_changed(&mut self, now: Instant) {
        self.due = Some(now + DRAFT_SAVE_DEBOUNCE);
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.due.is_some_and(|due| due <= now) {
            self.due = None;
            true
        } else {
            false
        }
    }
}

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
    let reduced_motion = config.reduced_motion;
    let mut terminal = ratatui::init();
    let _terminal_mode = TerminalModeGuard;
    // Ratatui 0.30 preserves the cursor around `Terminal::clear`, which requires
    // a DSR round trip. Some valid PTYs do not answer that query, so clear the
    // newly entered alternate screen directly before the first full render.
    execute!(
        stdout(),
        Clear(ClearType::All),
        EnableBracketedPaste,
        EnableMouseCapture
    )?;

    let mut state = AppState::with_metadata(config.history_path.clone(), config.metadata.clone());
    state.keymap = config.keymap.clone();
    state.composer.set_editing_style(config.editing_style);
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
    let mut draft_save = DraftSaveSchedule::default();

    loop {
        let wake = tokio::select! {
            intent = input.recv(), if input_open => Wake::Input(intent),
            event = service.recv(), if service_open => Wake::Service(event),
            _ = frame_clock.tick() => Wake::Frame,
            _ = animation_clock.tick(), if !reduced_motion => Wake::Animation,
            _ = reconnect_clock.tick() => Wake::Reconnect,
        };

        let render_immediately = matches!(&wake, Wake::Input(Some(_)));
        let frame_due = matches!(&wake, Wake::Frame);
        let mut draft_changed = false;
        match wake {
            Wake::Input(Some(intent)) => {
                draft_changed = affects_draft(&intent);
                application.handle(intent);
            }
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
            draft_changed |= affects_draft(&intent);
            application.handle(intent);
        }
        if draft_changed {
            draft_save.mark_changed(Instant::now());
        }

        // Keyboard input owns the latency budget. Draw it before draining a
        // potentially large service burst or performing any persistence I/O.
        if had_input && application.state.dirty.take() {
            draw(&mut terminal, &mut application)?;
        }
        for _ in 0..MAX_SERVICE_BATCH {
            let Some(event) = service.try_recv() else {
                break;
            };
            application.apply(event);
        }

        for effect in application.take_effects() {
            if let crate::event::Action::RunDoctor { destination } = effect {
                let report = crate::diagnostics::report(&config, &application.state);
                let event = match destination {
                    crate::event::DoctorDestination::Inspect => DomainEvent::DoctorCompleted {
                        report: Some(report),
                        message: "Redacted diagnostics loaded".into(),
                    },
                    crate::event::DoctorDestination::Copy => match copy_osc52(&report) {
                        Ok(()) => DomainEvent::DoctorCompleted {
                            report: None,
                            message: "Copied redacted diagnostics".into(),
                        },
                        Err(error) => DomainEvent::DoctorFailed {
                            reason: error.to_string(),
                        },
                    },
                    crate::event::DoctorDestination::Export(path) => {
                        match crate::diagnostics::export(
                            &report,
                            &path,
                            &application.state.metadata.workspace,
                        ) {
                            Ok(path) => DomainEvent::DoctorCompleted {
                                report: None,
                                message: format!(
                                    "Exported redacted diagnostics to {}",
                                    path.display()
                                ),
                            },
                            Err(reason) => DomainEvent::DoctorFailed { reason },
                        }
                    }
                };
                application.apply(event);
                continue;
            }
            if matches!(effect, crate::event::Action::InspectConfig) {
                application.apply(DomainEvent::ConfigInspected {
                    report: config.report(&application.state.metadata),
                });
                continue;
            }
            if let crate::event::Action::InspectWorkspaceDiff { scope, workspace } = effect {
                let event = match crate::workspace_service::load_diff(&workspace, scope) {
                    Ok(diff) => DomainEvent::WorkspaceDiffLoaded { scope, diff },
                    Err(reason) => DomainEvent::WorkspaceDiffFailed { reason },
                };
                application.apply(event);
                continue;
            }
            if let crate::event::Action::ReviewWorkspaceChanges { scope, workspace } = effect {
                let event = match crate::workspace_service::load_diff(&workspace, scope) {
                    Ok(diff) => DomainEvent::WorkspaceReviewLoaded { scope, diff },
                    Err(reason) => DomainEvent::WorkspaceReviewFailed { reason },
                };
                application.apply(event);
                continue;
            }
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
                        let truncated = application.state.composer.replace_text(&text);
                        application.state.save_draft();
                        application.state.status = if truncated {
                            "Draft updated from editor · content truncated to local limit".into()
                        } else {
                            "Draft updated from external editor".into()
                        };
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

        if draft_save.take_due(Instant::now()) {
            application.state.save_draft();
        }

        if frame_due && application.state.dirty.take() {
            draw(&mut terminal, &mut application)?;
        }

        if application.state.should_quit {
            application.state.save_draft();
            application.state.save_history();
            return Ok(());
        }

        if !input_open && !service_open {
            return Ok(());
        }
    }
}

fn draw(
    terminal: &mut ratatui::DefaultTerminal,
    application: &mut Application,
) -> std::io::Result<()> {
    let metrics = std::cell::Cell::new(ui::FrameMetrics::default());
    terminal.draw(|frame| metrics.set(ui::dispatch_with_metrics(frame, &application.state)))?;
    application
        .state
        .set_chat_scroll_limit(metrics.get().transcript_scroll_limit);
    Ok(())
}

fn affects_draft(intent: &UserIntent) -> bool {
    matches!(intent, UserIntent::Key(_) | UserIntent::Paste(_))
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
    let command = shell_words::split(editor).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid editor command: {error}"),
        )
    })?;
    let (program, arguments) = command.split_first().ok_or_else(|| {
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
            .args(arguments)
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

    #[test]
    fn draft_persistence_waits_for_an_input_pause() {
        let start = Instant::now();
        let mut schedule = DraftSaveSchedule::default();
        schedule.mark_changed(start);
        assert!(!schedule.take_due(start + Duration::from_millis(249)));

        schedule.mark_changed(start + Duration::from_millis(200));
        assert!(!schedule.take_due(start + Duration::from_millis(449)));
        assert!(schedule.take_due(start + Duration::from_millis(450)));
        assert!(!schedule.take_due(start + Duration::from_secs(1)));
    }

    #[test]
    fn only_text_input_schedules_draft_persistence() {
        assert!(affects_draft(&UserIntent::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('中'),
                crossterm::event::KeyModifiers::NONE,
            )
        )));
        assert!(affects_draft(&UserIntent::Paste("中文".into())));
        assert!(!affects_draft(&UserIntent::Redraw));
        assert!(!affects_draft(&UserIntent::ScrollTranscript { lines: 4 }));
    }
}
