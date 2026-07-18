use super::*;

#[test]
fn overrides_are_parsed_and_match_terminal_events() {
    let map = KeyMap::from_overrides(&[(KeyAction::Sessions, "alt+s".into())]).unwrap();
    assert!(map.matches(
        KeyAction::Sessions,
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT)
    ));
    assert_eq!(map.label(KeyAction::Sessions), "Alt+S");
}

#[test]
fn conflicts_and_printable_global_keys_fail_closed() {
    let conflict = KeyMap::from_overrides(&[
        (KeyAction::Sessions, "ctrl+k".into()),
        (KeyAction::Commands, "ctrl+k".into()),
    ])
    .unwrap_err();
    assert!(conflict.contains("conflict"));
    assert!(KeyMap::from_overrides(&[(KeyAction::Sessions, "s".into())]).is_err());
    assert!(KeyMap::from_overrides(&[(KeyAction::Sessions, "shift+s".into())]).is_err());
    assert!(KeyMap::from_overrides(&[(KeyAction::Sessions, "ctrl+c".into())]).is_err());
}
