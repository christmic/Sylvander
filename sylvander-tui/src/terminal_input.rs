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
            if let Some(intent) = translate(event, mouse_scroll_lines) {
                if !enqueue(&tx, intent) {
                    break;
                }
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
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};

    #[test]
    fn arrow_up_stays_a_key_while_wheel_becomes_transcript_scroll() {
        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(translate(Event::Key(key), 4), Some(UserIntent::Key(key)));

        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            translate(Event::Mouse(mouse), 4),
            Some(UserIntent::ScrollTranscript { lines: 4 })
        );
    }

    #[test]
    fn redraw_flood_is_bounded_without_dropping_a_later_key() {
        let (tx, mut rx) = mpsc::channel(INPUT_EVENT_CAPACITY);
        for _ in 0..100_000 {
            assert!(enqueue(&tx, UserIntent::Redraw));
        }
        assert_eq!(rx.len(), INPUT_EVENT_CAPACITY);

        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        let key_tx = tx.clone();
        let sender = std::thread::spawn(move || enqueue(&key_tx, UserIntent::Key(key)));
        assert_eq!(rx.blocking_recv(), Some(UserIntent::Redraw));
        assert!(sender.join().expect("key sender"));

        let mut delivered_key = false;
        while let Ok(intent) = rx.try_recv() {
            delivered_key |= intent == UserIntent::Key(key);
        }
        assert!(delivered_key);
    }
}
