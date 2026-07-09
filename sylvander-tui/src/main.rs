//! Sylvander TUI — terminal client for Sylvander agents.
//!
//! Connects to a Sylvander server via Unix socket.

mod app;
mod client;
mod input;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use app::{AppMode, AppState, ChatMessage};

const SOCKET_PATH: &str = "/tmp/sylvander.sock";

#[tokio::main]
async fn main() {
    let socket_path: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| SOCKET_PATH.into())
        .into();

    // Terminal setup
    let mut terminal = ratatui::init();
    terminal.clear().unwrap();

    // App state
    let mut state = AppState::new();

    // Event channels
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEvent>();

    // crossterm event reader (blocking thread)
    std::thread::spawn(move || loop {
        if let Ok(event) = event::read() {
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Press && key_tx.send(key).is_err() {
                    break;
                }
            }
        }
    });

    // Main loop
    loop {
        // Process keyboard events
        while let Ok(key) = key_rx.try_recv() {
            handle_key(&key, &mut state);
            if state.should_quit {
                ratatui::restore();
                return;
            }
        }

        // Render
        terminal
            .draw(|frame| ui::ui(frame, &state))
            .unwrap();

        // Tick: 30fps
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
}

fn handle_key(key: &KeyEvent, state: &mut AppState) {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
            state.should_quit = true;
            return;
        }
        _ => {}
    }

    match &state.mode {
        AppMode::Normal => match key.code {
            KeyCode::Esc => state.should_quit = true,
            _ => {
                if let Some(text) = state.input.handle_key(&key.code) {
                    // Submit message
                    state.messages.push(ChatMessage::User(text.clone()));
                    state.status = format!("Sent: {text}");

                    // Simulate agent response for M20a (no socket yet)
                    tokio::spawn(simulate_agent_response(text));
                }
            }
        },
        AppMode::Approval { .. } => {
            if let AppMode::Approval { tools, current, decisions, batch_id: _ } = &mut state.mode {
                match key.code {
                    KeyCode::Char('y') => {
                        decisions[*current] = true;
                        if *current + 1 < tools.len() {
                            *current += 1;
                        } else {
                            state.mode = AppMode::Normal;
                        }
                    }
                    KeyCode::Char('n') => {
                        decisions[*current] = false;
                        if *current + 1 < tools.len() {
                            *current += 1;
                        } else {
                            state.mode = AppMode::Normal;
                        }
                    }
                    KeyCode::Esc => {
                        state.mode = AppMode::Normal;
                    }
                    _ => {}
                }
            }
        },
        AppMode::AskUser { .. } => {
            if let AppMode::AskUser { answer, .. } = &mut state.mode {
                match key.code {
                    KeyCode::Enter => {
                        state.mode = AppMode::Normal;
                    }
                    KeyCode::Esc => {
                        state.mode = AppMode::Normal;
                    }
                    _ => {
                        if let Some(text) = state.input.handle_key(&key.code) {
                            *answer = text;
                        }
                    }
                }
            }
        },
    }
}

async fn simulate_agent_response(text: String) {
    // For M20a: simulate streaming response in chat history.
    // In M20b this will be replaced with real socket events.
    if text == "/approve" {
        // Simulate approval request
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            // In real app this comes from socket, so we'd need to update state
        });
    }
    if text == "/ask" {
        // Simulate AskUser
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
        });
    }
}
