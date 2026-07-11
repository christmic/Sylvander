//! Snapshot tests for `sylvander-tui` rendering.
//!
//! Each test instantiates an `AppState`, drives it through a few `DomainEvent`s
//! to set up the scene, then renders via `ui::dispatch` into a `TestBackend`
//! and asserts the resulting buffer against an insta YAML snapshot.
//!
//! Snapshot files live in `tests/snapshots/` and are checked in so reviewers
//! can diff visual changes via `cargo insta review`.

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use sylvander_tui::app::{AppMode, AppState, ChatMessage, ToolStatus};
use sylvander_tui::event::DomainEvent;

/// Render `state` into a `(width, height)` TestBackend and return the
/// resulting buffer as a human-friendly string (one cell per char, joined
/// with newlines per row).
fn render_buf(state: &AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            sylvander_tui::ui::dispatch(frame, state);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            out.push_str(cell.symbol());
        }
        if y + 1 < buffer.area.height {
            out.push('\n');
        }
    }
    out
}

#[test]
fn empty_terminal_at_startup() {
    let state = AppState::new();
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn one_user_message_visible() {
    let mut state = AppState::new();
    state.apply(DomainEvent::TextChunk {
        delta: "hi there".into(),
    });
    state.apply(DomainEvent::AgentDone {
        final_text: "hi there".into(),
    });
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn streaming_agent_with_partial_text() {
    let mut state = AppState::new();
    // User asked something, agent is mid-stream.
    state.messages.push(ChatMessage::User("hello".into()));
    state.apply(DomainEvent::TextChunk {
        delta: "Thinking about it.".into(),
    });
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn tool_call_in_progress() {
    let mut state = AppState::new();
    state.messages.push(ChatMessage::User("list src".into()));
    state.apply(DomainEvent::ToolStarted {
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "ls src"}),
    });
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn tool_call_done_with_output() {
    let mut state = AppState::new();
    state.messages.push(ChatMessage::User("list src".into()));
    state.apply(DomainEvent::ToolStarted {
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "ls src"}),
    });
    state.apply(DomainEvent::ToolFinished {
        tool_name: "bash".into(),
        output: "main.rs\nlib.rs".into(),
        is_error: false,
    });
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn approval_modal_overlays_chat() {
    let mut state = AppState::new();
    state.messages.push(ChatMessage::User("rm -rf /".into()));
    state.apply(DomainEvent::ToolStarted {
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "rm -rf /"}),
    });
    state.apply(DomainEvent::ApprovalRequested {
        batch_id: "batch-1".into(),
        tools: vec![sylvander_tui::app::ToolInfo {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "rm -rf /"}),
        }],
    });
    assert_eq!(state.mode, AppMode::ApprovalPending);
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn multiline_composer_renders_two_rows() {
    let mut state = AppState::new();
    // Type "ab", Enter, "cd" — exercises the composer panel.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let k = |c, m| KeyEvent::new(c, m);
    state.handle_key(&k(KeyCode::Char('a'), KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Char('b'), KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Enter, KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Char('c'), KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Char('d'), KeyModifiers::NONE));
    // Sanity check: composer should be 2 rows.
    assert_eq!(state.composer.row_count(), 2);
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn paste_inline_under_8_lines() {
    let mut state = AppState::new();
    // Short paste (≤ 8 lines) should land in the draft directly.
    state.handle_paste("alpha\nbeta\ngamma");
    assert_eq!(state.composer.row_count(), 3);
    assert_eq!(state.composer.attachment_count(), 0);
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn paste_over_8_lines_collapses_to_attachment_token() {
    let mut state = AppState::new();
    // 20-line paste — should become a single attachment token above the draft.
    let payload = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    state.handle_paste(&payload);
    assert_eq!(state.composer.attachment_count(), 1);
    assert_eq!(state.composer.row_count(), 1); // draft still empty
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}

#[test]
fn many_attachments_collapses_with_more_indicator() {
    let mut state = AppState::new();
    // Six over-limit pastes — only 4 render as token, the rest get a
    // "… (+2 more attachments)" indicator.
    for _ in 0..6 {
        let payload = (1..=10).map(|i| format!("L{i}")).collect::<Vec<_>>().join("\n");
        state.handle_paste(&payload);
    }
    assert_eq!(state.composer.attachment_count(), 6);
    insta::assert_snapshot!(render_buf(&state, 80, 24));
}
