use super::*;

#[test]
fn search_wraps_and_copy_emits_a_local_effect() {
    let mut inspector = ToolInspector::new(
        "call-123456".into(),
        "bash".into(),
        "first\nneedle one\nlast needle".into(),
    );
    inspector.query = "needle".into();
    let lines = inspector.lines();
    assert_eq!(inspector.matches(&lines), [1, 2]);
    inspector.cursor = 2;
    let mut state = AppState::new();
    inspector.handle_key(
        &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(inspector.cursor, 1);
    inspector.handle_key(
        &KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        state.pending_actions.as_slice(),
        [Action::CopyText { text }] if text.contains("needle one")
    ));
}
