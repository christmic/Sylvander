//! Event-driven terminal runtime.
//!
//! User input renders immediately. Service streaming is coalesced to the
//! configured frame rate, while lower-frequency animation ticks are isolated.

use std::io::stdout;

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};

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
    let mut input_open = true;
    let mut service_open = true;

    loop {
        let wake = tokio::select! {
            intent = input.recv(), if input_open => Wake::Input(intent),
            event = service.recv(), if service_open => Wake::Service(event),
            _ = frame_clock.tick() => Wake::Frame,
            _ = animation_clock.tick() => Wake::Animation,
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
        }

        // Coalesce bursts without delaying the first input event.
        for _ in 0..MAX_INPUT_BATCH {
            let Ok(intent) = input.try_recv() else {
                break;
            };
            application.handle(intent);
        }
        for _ in 0..MAX_SERVICE_BATCH {
            let Some(event) = service.try_recv() else {
                break;
            };
            application.apply(event);
        }

        for effect in application.take_effects() {
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
