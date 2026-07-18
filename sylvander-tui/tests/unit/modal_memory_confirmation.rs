use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn confirmation() -> sylvander_protocol::PendingMemoryConfirmation {
    sylvander_protocol::PendingMemoryConfirmation {
        candidate_id: "candidate-1".into(),
        expected_revision: 3,
        scope: sylvander_protocol::MemoryConfirmationScope::UserProfile,
        summary: "prefers concise answers".into(),
    }
}

#[test]
fn enter_emits_typed_confirm_for_the_bound_session() {
    let mut state = AppState::new();
    let mut modal = MemoryConfirmationModal::new("session-1".into(), confirmation());

    assert_eq!(
        modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
        Consumed::Yes { dismiss: true }
    );
    assert!(matches!(
        state.pending_actions.as_slice(),
        [Action::ResolveMemoryConfirmation {
            session_id,
            candidate_id,
            expected_revision: 3,
            decision: sylvander_protocol::MemoryConfirmationDecision::Confirm,
        }] if session_id == "session-1" && candidate_id == "candidate-1"
    ));
}

#[test]
fn escape_is_an_explicit_rejection() {
    let mut state = AppState::new();
    let mut modal = MemoryConfirmationModal::new("session-1".into(), confirmation());

    assert_eq!(
        modal.handle_key(&KeyEvent::from(KeyCode::Esc), &mut state),
        Consumed::Yes { dismiss: true }
    );
    assert!(matches!(
        state.pending_actions.as_slice(),
        [Action::ResolveMemoryConfirmation {
            decision: sylvander_protocol::MemoryConfirmationDecision::Reject,
            ..
        }]
    ));
}

#[test]
fn keyboard_selection_can_reject_without_a_second_input_surface() {
    let mut state = AppState::new();
    let mut modal = MemoryConfirmationModal::new("session-1".into(), confirmation());
    assert_eq!(
        modal.handle_key(&KeyEvent::from(KeyCode::Down), &mut state),
        Consumed::Yes { dismiss: false }
    );
    modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state);
    assert!(matches!(
        state.pending_actions.as_slice(),
        [Action::ResolveMemoryConfirmation {
            decision: sylvander_protocol::MemoryConfirmationDecision::Reject,
            ..
        }]
    ));
}

#[test]
fn render_is_a_compact_below_composer_decision_dock() {
    let state = AppState::new();
    let modal = MemoryConfirmationModal::new("session-1".into(), confirmation());
    assert_eq!(
        modal.placement(&state, 80),
        ModalPlacement::BelowComposer { rows: 8 }
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| modal.render(frame, frame.area(), &state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = (0..buffer.area.height)
        .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol().to_owned()))
        .collect::<String>();
    assert!(rendered.contains("Save this for future conversations?"));
    assert!(rendered.contains("prefers concise answers"));
    assert!(rendered.contains("Don't save"));
}

#[test]
fn long_cjk_summary_is_bounded_and_keeps_both_decisions_visible() {
    let state = AppState::new();
    let mut confirmation = confirmation();
    confirmation.summary = "用户希望在所有后续编程会话中优先使用中文解释复杂概念，并保留关键英文术语、完整命令和清晰的验证证据。"
        .repeat(8);
    let modal = MemoryConfirmationModal::new("session-1".into(), confirmation);
    let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
    terminal
        .draw(|frame| modal.render(frame, frame.area(), &state))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let rows = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert!(rows[1..=2].iter().any(|row| row.contains('…')));
    assert!(rows[4].contains("Save memory"));
    assert!(rows[5].contains("Don't save"));
    assert!(rows[6].contains("confirm"));
}
