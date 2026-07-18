use super::*;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn build_modal(n: usize) -> PlanReviewModal {
    PlanReviewModal::new(
        "p1".into(),
        (1..=n).map(|index| format!("step {index}")).collect(),
        0,
        Some("s1".into()),
    )
}

#[test]
fn enter_approves_through_typed_action() {
    let mut state = AppState::new();
    let mut modal = build_modal(3);
    let consumed = modal.handle_key(&key(KeyCode::Enter), &mut state);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(matches!(
        state.pending_actions[0],
        Action::ResolvePlan {
            decision: sylvander_protocol::PlanDecision::Approved,
            ..
        }
    ));
}

#[test]
fn revision_is_explicit_and_returns_to_decision() {
    let mut state = AppState::new();
    let mut modal = build_modal(2);
    modal.handle_key(&key(KeyCode::Char('e')), &mut state);
    assert_eq!(modal.mode, PlanMode::Review);
    modal.handle_key(&key(KeyCode::Char('e')), &mut state);
    assert_eq!(modal.mode, PlanMode::EditStep);
    for _ in 0..6 {
        modal.handle_key(&key(KeyCode::Backspace), &mut state);
    }
    for character in "safer step".chars() {
        modal.handle_key(&key(KeyCode::Char(character)), &mut state);
    }
    modal.handle_key(&key(KeyCode::Enter), &mut state);
    modal.handle_key(&key(KeyCode::Enter), &mut state);
    assert_eq!(modal.mode, PlanMode::Decision);
    modal.handle_key(&key(KeyCode::Enter), &mut state);
    assert!(matches!(
        &state.pending_actions[0],
        Action::ResolvePlan {
            decision: sylvander_protocol::PlanDecision::Revised { steps },
            ..
        } if steps[0] == "safer step"
    ));
}

#[test]
fn escape_rejects_instead_of_abandoning_waiter() {
    let mut state = AppState::new();
    let mut modal = build_modal(2);
    modal.handle_key(&key(KeyCode::Esc), &mut state);
    assert!(matches!(
        &state.pending_actions[0],
        Action::ResolvePlan {
            decision: sylvander_protocol::PlanDecision::Rejected { .. },
            ..
        }
    ));
}

#[test]
fn review_can_add_and_remove_steps_without_resolving_gate() {
    let mut state = AppState::new();
    let mut modal = build_modal(2);
    modal.handle_key(&key(KeyCode::Char('e')), &mut state);
    modal.handle_key(&key(KeyCode::Char('a')), &mut state);
    assert_eq!(modal.steps.len(), 3);
    modal.handle_key(&key(KeyCode::Char('d')), &mut state);
    assert_eq!(modal.steps.len(), 2);
    assert!(state.pending_actions.is_empty());
}
