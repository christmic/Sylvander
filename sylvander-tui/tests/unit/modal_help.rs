use super::*;

#[test]
fn vim_help_is_discoverable_and_lists_safety_relevant_modes() {
    let help = HelpModal::new(Some("vim")).expect("documented topic");
    let lines = help.lines(&AppState::new());
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Vim Composer"));
    assert!(text.contains("Normal / Insert mode"));
    assert!(text.contains("Enter"));
}
