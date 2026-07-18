use super::*;

#[test]
fn confirmation_carries_the_preview_turn_id() {
    let mut state = AppState::new();
    let mut modal = WorkspaceRollbackModal::new(
        "s1".into(),
        sylvander_protocol::WorkspaceRollbackPreview {
            turn_id: "turn-7".into(),
            files: vec!["src/lib.rs".into()],
        },
    );
    modal.handle_key(&KeyEvent::from(KeyCode::Down), &mut state);
    assert_eq!(
        modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
        Consumed::Yes { dismiss: true }
    );
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::ConfirmWorkspaceRollback {
            expected_turn_id,
            ..
        }] if expected_turn_id == "turn-7"
    ));
}

#[test]
fn safe_choice_is_selected_by_default() {
    let mut state = AppState::new();
    let mut modal = WorkspaceRollbackModal::new(
        "s1".into(),
        sylvander_protocol::WorkspaceRollbackPreview {
            turn_id: "turn-7".into(),
            files: vec!["src/lib.rs".into()],
        },
    );
    let consumed = modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state);
    assert_eq!(consumed, Consumed::Yes { dismiss: true });
    assert!(state.pending_actions.is_empty());
}
