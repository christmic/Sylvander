use super::*;

#[test]
fn unavailable_ask_is_skipped_and_selection_is_typed() {
    let mut state = AppState::new();
    state.session_id = Some("session-1".into());
    state.metadata.approval_enabled = false;
    let mut picker = PermissionsPicker::new(&state);
    picker.row = 2;
    picker.profile.approval_policy = sylvander_protocol::ApprovalPolicy::Allow;
    picker.handle_key(&KeyEvent::from(KeyCode::Left), &mut state);
    assert_eq!(
        picker.profile.approval_policy,
        sylvander_protocol::ApprovalPolicy::Deny
    );
    picker.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state);
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::SelectPermissions { session_id, profile }]
            if session_id == "session-1"
                && profile.approval_policy == sylvander_protocol::ApprovalPolicy::Deny
    ));
}
