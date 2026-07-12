//! Crossterm input adapter.
//!
//! Keyboard, paste, resize, and mouse events are captured here and converted
//! into application intents. Mouse wheel events never masquerade as arrow keys.

use crossterm::event::{Event, KeyEventKind, MouseEventKind};
use tokio::sync::mpsc;

use crate::application::UserIntent;

pub fn spawn(mouse_scroll_lines: usize) -> mpsc::UnboundedReceiver<UserIntent> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            let intent = translate(event, mouse_scroll_lines);
            if intent.is_some_and(|intent| tx.send(intent).is_err()) {
                break;
            }
        }
    });
    rx
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
}
