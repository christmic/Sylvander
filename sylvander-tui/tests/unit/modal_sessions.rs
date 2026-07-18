use super::*;

fn entry(id: &str, label: &str, status: SessionStatus, ago: u64) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        label: label.into(),
        status,
        workspace: format!("/p/{label}"),
        last_seen_secs: ago,
    }
}

fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(c, m)
}

#[test]
fn empty_session_list_renders_no_match_line() {
    let overlay = SessionsOverlay::new(vec![]);
    let filtered = overlay.filtered();
    assert!(filtered.is_empty());
}

#[test]
fn filter_is_case_insensitive_substring() {
    let overlay = SessionsOverlay::new(vec![
        entry("a", "Auth-Refactor", SessionStatus::Working, 120),
        entry("b", "JWT-Research", SessionStatus::Complete, 7200),
    ]);
    assert_eq!(overlay.filtered().len(), 2);
    let mut o = overlay;
    o.filter = "auth".into();
    assert_eq!(o.filtered().len(), 1);
    o.filter = "AUTH".into();
    assert_eq!(o.filtered().len(), 1);
    o.filter = "zzz".into();
    assert_eq!(o.filtered().len(), 0);
}

#[test]
fn sessions_group_by_workspace_and_keep_recent_first() {
    let overlay = SessionsOverlay::new(vec![
        SessionEntry {
            workspace: "/b".into(),
            ..entry("b1", "B", SessionStatus::Complete, 5)
        },
        SessionEntry {
            workspace: "/a".into(),
            ..entry("a2", "A2", SessionStatus::Complete, 60)
        },
        SessionEntry {
            workspace: "/a".into(),
            ..entry("a1", "A1", SessionStatus::Complete, 5)
        },
    ]);
    assert_eq!(
        overlay
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["a1", "a2", "b1"]
    );
}

#[test]
fn enter_requests_persisted_session_history() {
    let mut state = AppState::new();
    let mut overlay = SessionsOverlay::new(vec![
        entry("a", "Auth-Refactor", SessionStatus::Working, 120),
        entry("b", "Login-Tests", SessionStatus::Complete, 7200),
    ]);
    overlay.filter_focused = false;
    overlay.cursor = 1;
    let _ = overlay.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::LoadSession { session_id }] if session_id == "b"
    ));
}

#[test]
fn delete_confirm_cancels_on_n() {
    let mut state = AppState::new();
    let mut overlay = SessionsOverlay::new(vec![entry("a", "Foo", SessionStatus::Working, 60)]);
    overlay.pending_delete = Some(0);
    let result = overlay.handle_key(&key(KeyCode::Char('n'), KeyModifiers::NONE), &mut state);
    assert!(matches!(result, Consumed::Yes { dismiss: false }));
    assert!(overlay.pending_delete.is_none());
}

#[test]
fn delete_confirm_removes_entry_on_y() {
    let mut state = AppState::new();
    state.sessions = vec![entry("a", "Foo", SessionStatus::Working, 60)];
    let mut overlay = SessionsOverlay::new(state.sessions.clone());
    overlay.pending_delete = Some(0);
    let result = overlay.handle_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut state);
    assert!(matches!(result, Consumed::Yes { dismiss: false }));
    assert_eq!(overlay.entries.len(), 0);
    assert!(state.sessions.is_empty());
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::ArchiveSession { session_id }] if session_id == "a"
    ));
    assert_eq!(
        state
            .last_archived_session
            .as_ref()
            .map(|session| session.id.as_str()),
        Some("a")
    );
}

#[test]
fn ctrl_z_restores_the_last_archived_session() {
    let mut state = AppState::new();
    let archived = entry("a", "Foo", SessionStatus::Complete, 60);
    state.last_archived_session = Some(archived);
    let mut overlay = SessionsOverlay::new(vec![]);
    overlay.handle_key(&key(KeyCode::Char('z'), KeyModifiers::CONTROL), &mut state);
    assert_eq!(overlay.entries[0].id, "a");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::RestoreSession { session_id }] if session_id == "a"
    ));
}

#[test]
fn permanent_delete_requires_exact_typed_confirmation() {
    let mut state = AppState::new();
    let mut overlay = SessionsOverlay::new(vec![entry(
        "a",
        "Critical work",
        SessionStatus::Complete,
        60,
    )]);
    overlay.filter_focused = false;
    overlay.handle_key(&key(KeyCode::Char('D'), KeyModifiers::SHIFT), &mut state);
    for character in "DELETE".chars() {
        overlay.handle_key(
            &key(KeyCode::Char(character), KeyModifiers::SHIFT),
            &mut state,
        );
    }
    let result = overlay.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
    assert!(matches!(result, Consumed::Yes { dismiss: true }));
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::DeleteSession { session_id }] if session_id == "a"
    ));
    assert_eq!(
        overlay.entries.len(),
        1,
        "server confirmation owns final removal"
    );
}

#[test]
fn rename_updates_overlay_and_application_cache() {
    let mut state = AppState::new();
    state.sessions = vec![entry("a", "Old", SessionStatus::Working, 60)];
    let mut overlay = SessionsOverlay::new(state.sessions.clone());
    overlay.filter_focused = false;
    overlay.handle_key(&key(KeyCode::Char('r'), KeyModifiers::NONE), &mut state);
    overlay.rename_buffer = "New name".into();
    overlay.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
    assert_eq!(overlay.entries[0].label, "New name");
    assert_eq!(state.sessions[0].label, "New name");
    assert!(matches!(
        state.pending_actions.as_slice(),
        [crate::event::Action::RenameSession { session_id, label }]
            if session_id == "a" && label == "New name"
    ));
}

#[test]
fn new_session_on_ctrl_n_does_not_send_an_empty_prompt() {
    let mut state = AppState::new();
    state.session_id = Some("old".into());
    let mut overlay = SessionsOverlay::new(vec![]);
    let result = overlay.handle_key(&key(KeyCode::Char('n'), KeyModifiers::CONTROL), &mut state);
    assert!(matches!(result, Consumed::Yes { dismiss: true }));
    assert!(state.pending_actions.is_empty());
    assert!(state.session_id.is_none());
}

#[test]
fn tab_toggles_filter_focus() {
    let mut state = AppState::new();
    let mut overlay = SessionsOverlay::new(vec![]);
    assert!(overlay.filter_focused);
    let _ = overlay.handle_key(&key(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert!(!overlay.filter_focused);
}
