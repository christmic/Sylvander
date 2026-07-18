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
