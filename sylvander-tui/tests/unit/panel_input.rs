use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn composer_starts_one_row_and_grows_when_text_wraps() {
    let mut state = AppState::new();
    assert_eq!(visual_row_count(&state, 12), 1);
    for _ in 0..24 {
        state.handle_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    }
    assert_eq!(visual_row_count(&state, 12), 3);
}

#[test]
fn chinese_text_wraps_and_positions_cursor_in_terminal_cells() {
    let mut state = AppState::new();
    for character in "你好世界中".chars() {
        state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }

    assert_eq!(state.composer.cursor_col_chars(), 5);
    assert_eq!(state.composer.cursor_col_cells(), 10);
    assert_eq!(
        wrap_composer_row("你好世界中", "❯ ", 8),
        ["❯ 你好世界", "  中"]
    );
    assert_eq!(cursor_position("你好世界中", 10, 8), (1, 2));
    assert_eq!(visual_row_count(&state, 10), 2);
}

#[test]
fn exact_width_draft_allocates_a_cursor_continuation_row() {
    let mut state = AppState::new();
    for character in "你好世界".chars() {
        state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert_eq!(cursor_position("你好世界", 8, 8), (1, 0));
    assert_eq!(visual_row_count(&state, 10), 2);
}
