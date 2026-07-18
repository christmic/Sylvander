use super::*;

#[test]
fn destructive_choice_is_never_selected_by_default() {
    let mut state = AppState::new();
    let mut modal = CodingSessionConfirmationModal::discard("s1".into());
    assert_eq!(
        modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
        Consumed::Yes { dismiss: true }
    );
    assert!(state.pending_actions.is_empty());
}

#[test]
fn explicit_confirmation_emits_accept_action() {
    let mut state = AppState::new();
    let mut modal = CodingSessionConfirmationModal::accept("s1".into());
    modal.handle_key(&KeyEvent::from(KeyCode::Down), &mut state);
    modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state);
    assert!(
        matches!(state.pending_actions.as_slice(), [crate::event::Action::AcceptCodingSession { session_id }] if session_id == "s1")
    );
}
