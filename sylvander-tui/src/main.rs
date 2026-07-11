//! Sylvander TUI — terminal client for Sylvander agents.
//!
//! Connects to a Sylvander server over Unix socket. Library surface in
//! `lib.rs`; this file is the binary entry point.

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{Event, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use tokio::sync::mpsc;

use sylvander_tui::{
    app::AppState,
    client::{parse_server_msg, ClientEvent, ClientMsg, UnixClient},
    event::{Action, DomainEvent},
    ui,
};

/// Input event from the terminal — either a key press or a bracketed paste.
enum InputEvent {
    Key(KeyEvent),
    Paste(String),
}

const SOCKET_PATH: &str = "/tmp/sylvander.sock";
const TICK_MS: u64 = 50;

#[tokio::main]
async fn main() {
    let socket_path: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| SOCKET_PATH.into())
        .into();

    // Terminal setup
    let mut terminal = ratatui::init();
    terminal.clear().unwrap();

    // Enable bracketed paste so pasted text arrives as `Event::Paste`
    // instead of flooding the app with synthetic Key events. Required
    // for the design's inline-vs-attachment paste policy (M-T2).
    if let Err(e) = execute!(stdout(), EnableBracketedPaste) {
        eprintln!("warning: failed to enable bracketed paste: {e}");
    }

    // App state (single-threaded, owned by main)
    let mut state = AppState::new();

    // ---- Input channel (keys + bracketed paste) ----
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<InputEvent>();
    std::thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(Event::Key(key)) => {
                if key.kind == KeyEventKind::Press && input_tx.send(InputEvent::Key(key)).is_err() {
                    break;
                }
            }
            Ok(Event::Paste(text)) => {
                if input_tx.send(InputEvent::Paste(text)).is_err() {
                    break;
                }
            }
            Ok(_) => {} // Focus / Resize / Mouse — currently ignored.
            Err(_) => break,
        }
    });

    // ---- Socket client (M21e will use this) ----
    let (client, event_rx) = UnixClient::new(&socket_path);
    let mut client = client;
    let mut event_rx = event_rx;
    // Try to connect; failure is non-fatal — we surface "Disconnected" in UI.
    if let Err(e) = client.connect().await {
        let _ = state.apply(DomainEvent::Disconnected {
            reason: e.to_string(),
        });
    }

    // ---- Main loop ----
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // 1. Drain terminal input.
        while let Ok(input) = input_rx.try_recv() {
            match input {
                InputEvent::Key(key) => {
                    if let Some(action) = state.handle_key(&key) {
                        dispatch_action(action, &mut client, &mut state).await;
                    }
                }
                InputEvent::Paste(text) => {
                    state.handle_paste(&text);
                }
            }
        }

        // 2. Drain socket events.
        while let Ok(ev) = event_rx.try_recv() {
            handle_client_event(ev, &mut state, &mut client).await;
        }

        // 3. Drain pending outbound actions (from modals).
        let actions = std::mem::take(&mut state.pending_actions);
        for action in actions {
            dispatch_action(action, &mut client, &mut state).await;
        }

        // 4. Reap expired modals (e.g. Toasts).
        state.modals.reap();

        // 5. Render if dirty.
        if state.dirty.take() {
            terminal.draw(|f| ui::dispatch(f, &state)).unwrap();
        }

        // 6. Quit check.
        if state.should_quit {
            // Restore terminal cleanly: ratatui::restore() runs its
            // shutdown hooks (which already includes LeaveAlternateScreen),
            // but bracketed paste and raw mode are managed by us so we
            // undo them here in the right order.
            disable_raw_mode().ok();
            execute!(stdout(), DisableBracketedPaste).ok();
            execute!(stdout(), LeaveAlternateScreen).ok();
            return;
        }

        // 7. Smart wait: wake on next event, fallback tick for animations.
        let _ = ticker.tick().await;
        // Heartbeat so spinners can advance.
        state.apply(DomainEvent::Tick);
    }
}

async fn handle_client_event(
    ev: ClientEvent,
    state: &mut AppState,
    _client: &mut UnixClient,
) {
    match ev {
        ClientEvent::Connected => {
            state.apply(DomainEvent::Connected);
        }
        ClientEvent::Disconnected => {
            state.apply(DomainEvent::Disconnected {
                reason: "server closed".into(),
            });
        }
        ClientEvent::Message(msg) => {
            if let Some(ev) = parse_server_msg(msg) {
                state.apply(ev);
            }
        }
    }
}

async fn dispatch_action(action: Action, client: &mut UnixClient, _state: &mut AppState) {
    match action {
        Action::SendChat { text, session_id } => {
            let _ = client.send(&ClientMsg::Chat { text, session_id }).await;
        }
        Action::SendApprove { call_id, approved } => {
            let _ = client.send(&ClientMsg::Approve { call_id, approved }).await;
        }
        Action::SendAnswer { call_id, answer } => {
            let _ = client.send(&ClientMsg::Answer { call_id, answer }).await;
        }
        Action::SendFeedback { text, session_id } => {
            // Send the rejection reason as a plain chat message. The agent
            // loop does not parse any special prefix — feedback is just
            // the next user turn, presented to the LLM with the rejected
            // tool's transcript still in context. Avoids inventing a wire
            // protocol for an annotation that's already represented as
            // (call_id, approved=false) on the upstream server side.
            let _ = client
                .send(&ClientMsg::Chat { text, session_id })
                .await;
        }
        Action::Quit => {
            // handle_key sets state.should_quit instead.
        }
    }
}