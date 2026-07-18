use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

use super::*;

#[test]
fn empty_focused_composer_exposes_a_hardware_cursor_after_prompt() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| dispatch(frame, &AppState::new()))
        .expect("draw");
    terminal.backend_mut().assert_cursor_position((2, 21));
}

#[test]
fn compact_width_keeps_command_picker_renderable() {
    for width in [40, 20, 10, 3] {
        let backend = TestBackend::new(width, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut state = AppState::new();
        state.handle_key(&KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));

        terminal
            .draw(|frame| dispatch(frame, &state))
            .unwrap_or_else(|error| panic!("render command picker at {width} columns: {error}"));
    }
}

#[test]
fn chinese_composer_cursor_uses_display_cells_not_scalar_count() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = AppState::new();
    for character in "你好".chars() {
        state.handle_key(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    terminal
        .draw(|frame| dispatch(frame, &state))
        .expect("draw");
    terminal.backend_mut().assert_cursor_position((6, 21));
}

#[test]
fn tool_activity_keeps_one_live_composer_chrome_and_cursor() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = AppState::new();
    state.messages.push(crate::app::ChatMessage::User(
        "inspect the workspace".into(),
    ));
    state.apply(crate::event::DomainEvent::ToolStarted {
        call_id: "tool-1".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "pwd"}),
    });

    terminal
        .draw(|frame| dispatch(frame, &state))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let rule_rows = (0..buffer.area.height)
        .filter(|&y| {
            (0..buffer.area.width)
                .all(|x| buffer.cell((x, y)).is_some_and(|cell| cell.symbol() == "─"))
        })
        .count();

    assert_eq!(rule_rows, 2, "only the live Composer owns chrome");
    terminal.backend_mut().assert_cursor_position((2, 21));
}

#[test]
fn transcript_scroll_uses_the_rendered_top_as_a_hard_limit() {
    let mut state = AppState::new();
    state.welcomed = false;
    for index in 0..40 {
        state.messages.push(crate::app::ChatMessage::Info(format!(
            "history row {index}"
        )));
    }
    let limit = transcript_scroll_limit(ratatui::layout::Rect::new(0, 0, 80, 24), &state);
    assert!(limit > 0);
    state.set_chat_scroll_limit(limit);
    state.scroll_transcript(isize::MAX);
    assert_eq!(state.chat_scroll, limit);
    state.scroll_transcript(-4);
    assert_eq!(state.chat_scroll, limit - 4);
}
