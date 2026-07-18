use super::*;

fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(c, m)
}

#[test]
fn empty_filter_shows_all_commands() {
    let state = AppState::new();
    let p = CommandPalette::new(&state);
    assert_eq!(p.filtered.len(), COMMANDS.len());
}

#[test]
fn filter_substring_matches_command_name() {
    let state = AppState::new();
    let mut p = CommandPalette::new(&state);
    p.filter = "ses".into();
    p.recompute(&state);
    let names: Vec<&'static str> = p
        .filtered
        .iter()
        .map(|entry| COMMANDS[entry.index].name)
        .collect();
    assert!(names.contains(&"sessions"));
    assert!(!names.contains(&"clear"));
}

#[test]
fn filter_no_match_yields_empty_list() {
    let state = AppState::new();
    let mut p = CommandPalette::new(&state);
    p.filter = "zzzzz".into();
    p.recompute(&state);
    assert!(p.filtered.is_empty());
}

#[test]
fn enter_dispatches_quit_command() {
    let mut state = AppState::new();
    let mut p = CommandPalette::new(&state);
    for character in "quit".chars() {
        let _ = p.handle_key(
            &key(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    let consumed = p.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(state.should_quit);
}

#[test]
fn enter_on_clear_command_empties_messages() {
    let mut state = AppState::new();
    use crate::app::ChatMessage;
    state.messages.push(ChatMessage::User("hi".into()));
    let mut p = CommandPalette::new(&state);
    for character in "clear".chars() {
        let _ = p.handle_key(
            &key(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    let consumed = p.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    assert!(state.messages.is_empty());
}

#[test]
fn enter_on_sessions_pushes_sessions_overlay() {
    let mut state = AppState::new();
    let mut p = CommandPalette::new(&state);
    for character in "sessions".chars() {
        let _ = p.handle_key(
            &key(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    let consumed = p.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
    assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
    // Palette itself was popped, but it pushed a sessions overlay.
    assert_eq!(state.modals.len(), 1);
}

#[test]
fn tab_completes_fuzzy_match_and_alias_executes_canonical_command() {
    let mut state = AppState::new();
    let mut palette = CommandPalette::new(&state);
    palette.filter = "sstns".into();
    palette.recompute(&state);
    let _ = palette.handle_key(&key(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(palette.filter, "sessions ");

    palette.filter = "q".into();
    palette.recompute(&state);
    let result = palette.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
    assert_eq!(result, Consumed::Yes { dismiss: true });
    assert!(state.should_quit);
    assert_eq!(
        state.recent_commands.front(),
        Some(&crate::command::CommandId::Quit)
    );
}

#[test]
fn deleting_the_empty_trigger_dismisses_the_palette() {
    let mut state = AppState::new();
    let mut palette = CommandPalette::new(&state);

    assert_eq!(
        palette.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE), &mut state),
        Consumed::Yes { dismiss: true }
    );

    let mut palette = CommandPalette::new(&state);
    assert_eq!(
        palette.handle_key(&key(KeyCode::Delete, KeyModifiers::NONE), &mut state),
        Consumed::Yes { dismiss: true }
    );
}

#[test]
fn app_command_mode_edits_the_persistent_composer() {
    let mut state = AppState::new();
    state.handle_key(&key(KeyCode::Char('/'), KeyModifiers::NONE));

    assert_eq!(state.composer.text(), "/");
    assert!(state.modals.top().is_some_and(Modal::uses_composer_input));

    state.handle_key(&key(KeyCode::Char('s'), KeyModifiers::NONE));
    assert_eq!(state.composer.text(), "/s");

    state.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE));
    state.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(state.composer.is_empty());
    assert!(state.modals.is_empty());
}
