use super::*;

fn tool(call_id: &str) -> ToolInfo {
    ToolInfo {
        call_id: call_id.into(),
        tool_name: "bash".into(),
        input: serde_json::json!({}),
    }
}

fn build_modal_with_n_tools(n: usize) -> ApprovalModal {
    let mut m = ApprovalModal::new("b".into(), (0..n).map(|i| tool(&format!("c{i}"))).collect());
    m.queue_total = 1;
    m
}

#[test]
fn decision_tracks_pending_state() {
    let m = build_modal_with_n_tools(3);
    assert!(m.decisions.iter().all(|d| *d == Decision::Pending));
}

#[test]
fn critical_request_places_deny_first_and_selects_it() {
    let m = ApprovalModal::new(
        "b".into(),
        vec![ToolInfo {
            call_id: "c".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "rm -rf ./cache"}),
        }],
    );
    assert_eq!(m.choices()[0], ApprovalChoice::Reject);
    assert_eq!(m.choice_index, 0);
}

#[test]
fn approve_advances_to_next_tool() {
    let mut m = build_modal_with_n_tools(3);
    m.handle_navigate_key(
        &key(KeyCode::Char('y'), KeyModifiers::NONE),
        &mut AppState::new(),
    );
    assert_eq!(
        m.decisions[0],
        Decision::Approve(sylvander_protocol::ApprovalScope::Once)
    );
    assert_eq!(m.current, 1);
}

#[test]
fn approve_y_emits_action_when_last_tool() {
    let mut m = build_modal_with_n_tools(2);
    let mut s = AppState::new();
    // Navigate to the last tool.
    m.handle_navigate_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut s);
    // Decide last.
    let consumed = m.handle_navigate_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert_eq!(s.pending_actions.len(), 2);
    assert!(matches!(
        s.pending_actions[0],
        Action::SendApprove {
            ref call_id,
            approved: true,
            reason: None,
            ..
        } if call_id == "c0"
    ));
    assert!(matches!(
        s.pending_actions[1],
        Action::SendApprove { ref call_id, approved: true, .. } if call_id == "c1"
    ));
}

#[test]
fn session_scope_is_emitted_only_when_server_allows_it() {
    let mut modal = build_modal_with_n_tools(1).with_allowed_scopes(vec![
        sylvander_protocol::ApprovalScope::Once,
        sylvander_protocol::ApprovalScope::Session,
    ]);
    let mut state = AppState::new();
    let consumed =
        modal.handle_navigate_key(&key(KeyCode::Char('s'), KeyModifiers::NONE), &mut state);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(matches!(
        state.pending_actions.as_slice(),
        [Action::SendApprove {
            approved: true,
            scope: sylvander_protocol::ApprovalScope::Session,
            ..
        }]
    ));
}

#[test]
fn unavailable_persistent_scope_stays_open_and_sends_nothing() {
    let mut modal = build_modal_with_n_tools(1);
    let mut state = AppState::new();
    let consumed =
        modal.handle_navigate_key(&key(KeyCode::Char('p'), KeyModifiers::NONE), &mut state);
    assert!(matches!(consumed, Consumed::Yes { dismiss: false }));
    assert!(state.pending_actions.is_empty());
    assert_eq!(
        state.status,
        "persistent approval is disabled by the server"
    );
}

#[test]
fn escape_rejects_pending_calls_instead_of_abandoning_the_agent() {
    let mut modal = build_modal_with_n_tools(2);
    let mut state = AppState::new();
    let consumed = modal.handle_navigate_key(&key(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert_eq!(state.pending_actions.len(), 2);
    assert!(state.pending_actions.iter().all(|action| matches!(
        action,
        Action::SendApprove {
            approved: false,
            ..
        }
    )));
}

#[test]
fn reject_then_enter_feedback_does_not_yet_emit_action() {
    let mut m = build_modal_with_n_tools(1);
    let mut s = AppState::new();
    // Reject → enter feedback mode, but no SendApprove yet.
    let consumed = m.handle_navigate_key(&key(KeyCode::Char('n'), KeyModifiers::NONE), &mut s);
    assert_eq!(m.mode, ApprovalMode::RejectFeedback);
    assert!(matches!(consumed, Consumed::Yes { dismiss: false }));
    assert!(s.pending_actions.is_empty());
    // Type feedback + Enter.
    for c in "use docker".chars() {
        m.handle_feedback_key(&key(KeyCode::Char(c), KeyModifiers::NONE), &mut s);
    }
    let consumed = m.handle_feedback_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert_eq!(s.pending_actions.len(), 1);
    assert!(matches!(
        s.pending_actions[0],
        Action::SendApprove {
            approved: false,
            reason: Some(ref reason),
            ..
        } if reason == "use docker"
    ));
}

#[test]
fn rejection_reason_input_is_bounded_before_transport() {
    let mut modal = build_modal_with_n_tools(1);
    let mut state = AppState::new();
    modal.mode = ApprovalMode::RejectFeedback;
    for _ in 0..501 {
        let _ = modal.handle_feedback_key(&key(KeyCode::Char('x'), KeyModifiers::NONE), &mut state);
    }
    assert_eq!(modal.feedback.chars().count(), 500);
}

#[test]
fn shift_y_approves_all_remaining() {
    let mut m = build_modal_with_n_tools(3);
    let mut s = AppState::new();
    let consumed = m.handle_navigate_key(&key(KeyCode::Char('Y'), KeyModifiers::SHIFT), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    // All decisions are now Approve.
    assert_eq!(
        s.pending_actions.len(),
        3,
        "expected 3 SendApprove actions for 3-tool batch"
    );
}

#[test]
fn backspace_rewinds_and_clears_decision() {
    let mut m = build_modal_with_n_tools(3);
    let mut s = AppState::new();
    m.handle_navigate_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut s);
    // current=1, decisions[0]=Approve, decisions[1]=Pending
    let consumed = m.handle_navigate_key(&key(KeyCode::Backspace, KeyModifiers::NONE), &mut s);
    // cursor moved back, decision[0] should be Pending again.
    assert!(matches!(consumed, Consumed::Yes { dismiss: false }));
    assert_eq!(m.current, 0);
    assert_eq!(m.decisions[0], Decision::Pending);
}

#[test]
fn shift_n_rejects_all_and_enters_feedback() {
    let mut m = build_modal_with_n_tools(3);
    let mut s = AppState::new();
    let consumed = m.handle_navigate_key(&key(KeyCode::Char('N'), KeyModifiers::SHIFT), &mut s);
    // Reject-all keeps the modal open in RejectFeedback mode.
    assert!(matches!(consumed, Consumed::Yes { dismiss: false }));
    assert_eq!(m.mode, ApprovalMode::RejectFeedback);
    // All three decisions are now Reject.
    assert!(m.decisions.iter().all(|d| *d == Decision::Reject));
    assert!(
        s.pending_actions.is_empty(),
        "no SendApprove should fire yet — feedback still pending"
    );
    // Finish via Enter in feedback mode.
    for ch in "too destructive".chars() {
        let _ = m.handle_feedback_key(&key(KeyCode::Char(ch), KeyModifiers::NONE), &mut s);
    }
    let consumed = m.handle_feedback_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert_eq!(s.pending_actions.len(), 3);
    let reject_count = s
        .pending_actions
        .iter()
        .filter(|a| {
            matches!(
                a,
                Action::SendApprove {
                    approved: false,
                    reason: Some(reason),
                    ..
                } if reason == "too destructive"
            )
        })
        .count();
    assert_eq!(reject_count, 3);
}

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}
