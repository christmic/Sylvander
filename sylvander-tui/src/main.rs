//! Sylvander TUI — terminal client for Sylvander agents.
//!
//! Connects to a Sylvander server over Unix socket. Library surface in
//! `lib.rs`; this file is the binary entry point.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{Event, KeyEvent, KeyEventKind};
use tokio::sync::mpsc;

use sylvander_tui::{
    app::AppState,
    client::{parse_server_msg, ClientEvent, ClientMsg, UnixClient},
    event::{Action, DomainEvent},
    ui,
};

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

    // App state (single-threaded, owned by main)
    let mut state = AppState::new();

    // ---- Keyboard input channel ----
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEvent>();
    std::thread::spawn(move || loop {
        if let Ok(Event::Key(key)) = crossterm::event::read() {
            if key.kind == KeyEventKind::Press && key_tx.send(key).is_err() {
                break;
            }
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
        // 1. Drain keyboard events.
        while let Ok(key) = key_rx.try_recv() {
            if let Some(action) = state.handle_key(&key) {
                dispatch_action(action, &mut client, &mut state).await;
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
            ratatui::restore();
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
        Action::Quit => {
            // handle_key sets state.should_quit instead.
        }
    }
}