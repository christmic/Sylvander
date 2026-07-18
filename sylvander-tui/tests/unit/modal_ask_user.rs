use super::*;

fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(c, m)
}

#[test]
fn free_text_mode_returns_typed_answer() {
    let mut m = AskUserModal::new("c".into(), "why?".into(), vec![], false);
    let mut s = AppState::new();
    for ch in "make it blue".chars() {
        m.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE), &mut s);
    }
    let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert_eq!(s.pending_actions.len(), 1);
    assert!(matches!(
        s.pending_actions[0],
        Action::SendAnswer { ref call_id, ref answer, .. } if call_id == "c" && answer == "make it blue"
    ));
}

#[test]
fn single_select_with_numeric() {
    let mut m = AskUserModal::new(
        "c".into(),
        "color?".into(),
        vec!["red".into(), "green".into(), "blue".into()],
        false,
    );
    let mut s = AppState::new();
    m.handle_key(&key(KeyCode::Char('2'), KeyModifiers::NONE), &mut s);
    let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(matches!(
        s.pending_actions[0],
        Action::SendAnswer { ref answer, .. } if answer == "green"
    ));
}

#[test]
fn multi_select_toggle_with_space() {
    let mut m = AskUserModal::new(
        "c".into(),
        "tags?".into(),
        vec!["urgent".into(), "bug".into(), "feature".into()],
        true,
    );
    let mut s = AppState::new();
    // Cursor on row 0; Space → toggle.
    m.handle_key(&key(KeyCode::Char(' '), KeyModifiers::NONE), &mut s);
    // Down to row 2.
    m.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut s);
    m.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut s);
    // Toggle row 2.
    m.handle_key(&key(KeyCode::Char(' '), KeyModifiers::NONE), &mut s);
    let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(matches!(
        s.pending_actions[0],
        Action::SendAnswer { ref answer, .. } if answer == "urgent, feature"
    ));
}

#[test]
fn option_plus_free_text_concatenates_with_semicolon() {
    let mut m = AskUserModal::new(
        "c".into(),
        "?".into(),
        vec!["red".into(), "green".into()],
        false,
    );
    let mut s = AppState::new();
    m.handle_key(&key(KeyCode::Char('1'), KeyModifiers::NONE), &mut s);
    for ch in " but smaller".chars() {
        m.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE), &mut s);
    }
    let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(matches!(
        s.pending_actions[0],
        Action::SendAnswer { ref answer, .. } if answer == "red; but smaller"
    ));
}

#[test]
fn esc_cancels_and_unblocks_the_agent() {
    let mut m = AskUserModal::new(
        "c".into(),
        "?".into(),
        vec!["yes".into(), "no".into()],
        false,
    );
    let mut s = AppState::new();
    let consumed = m.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(matches!(
        s.pending_actions.as_slice(),
        [Action::SendAnswer { call_id, answer, .. }] if call_id == "c" && answer.is_empty()
    ));
    assert_eq!(s.mode, AppMode::Normal);
}
