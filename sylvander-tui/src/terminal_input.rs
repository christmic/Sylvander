//! Crossterm input adapter.
//!
//! Keyboard, paste, resize, and mouse events are captured here and converted
//! into application intents. Mouse wheel events never masquerade as arrow keys.

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use tokio::sync::mpsc;

use crate::application::UserIntent;

const INPUT_EVENT_CAPACITY: usize = 256;

pub fn spawn(mouse_scroll_lines: usize) -> mpsc::Receiver<UserIntent> {
    let (tx, rx) = mpsc::channel(INPUT_EVENT_CAPACITY);
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if let Some(intent) = translate(event, mouse_scroll_lines)
                && !enqueue(&tx, intent)
            {
                break;
            }
        }
    });
    rx
}

fn enqueue(tx: &mpsc::Sender<UserIntent>, intent: UserIntent) -> bool {
    match intent {
        UserIntent::Key(_) | UserIntent::Paste(_) => tx.blocking_send(intent).is_ok(),
        UserIntent::ScrollTranscript { .. } | UserIntent::Redraw => match tx.try_send(intent) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        },
    }
}

pub fn translate(event: Event, mouse_scroll_lines: usize) -> Option<UserIntent> {
    let scroll_lines = isize::try_from(mouse_scroll_lines).unwrap_or(isize::MAX);
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(UserIntent::Key(key)),
        Event::Paste(text) => Some(UserIntent::Paste(text)),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => Some(UserIntent::ScrollTranscript {
                lines: scroll_lines,
            }),
            MouseEventKind::ScrollDown => Some(UserIntent::ScrollTranscript {
                lines: -scroll_lines,
            }),
            _ => None,
        },
        Event::Resize(_, _) => Some(UserIntent::Redraw),
        _ => None,
    }
}

#[cfg(test)]
#[path = "../tests/unit/terminal_input.rs"]
mod tests;
