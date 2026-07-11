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

use sylvander_tui::app::{AppMode, AppState, ChatMessage, ToolInfo, ToolStatus};
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
    // Type "ab", Shift+Enter (newline), "cd" — exercises the composer panel.
    // Plain Enter now submits in the new keymap convention.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let k = |c, m| KeyEvent::new(c, m);
    state.handle_key(&k(KeyCode::Char('a'), KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Char('b'), KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Enter, KeyModifiers::SHIFT));
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

#[test]
fn approval_modal_batch_with_three_tools() {
    let mut state = AppState::new();
    state.messages.push(ChatMessage::User("run setup".into()));
    state.apply(DomainEvent::ToolStarted {
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "ls"}),
    });
    state.apply(DomainEvent::ApprovalRequested {
        batch_id: "batch-1".into(),
        tools: vec![
            ToolInfo {
                call_id: "c1".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({"command": "ls -la"}),
            },
            ToolInfo {
                call_id: "c2".into(),
                tool_name: "write".into(),
                input: serde_json::json!({"path": "/tmp/foo"}),
            },
            ToolInfo {
                call_id: "c3".into(),
                tool_name: "read".into(),
                input: serde_json::json!({"path": "/etc/hostname"}),
            },
        ],
    });
    // Approve first, navigate to second, reject → enter feedback capture.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let k = |c, m| KeyEvent::new(c, m);
    state.handle_key(&k(KeyCode::Char('y'), KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Char('n'), KeyModifiers::NONE));
    // type some feedback
    for ch in "use docker".chars() {
        state.handle_key(&k(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    insta::assert_snapshot!(render_buf(&state, 90, 28));
}

#[test]
fn approval_modal_with_queue_header() {
    let mut state = AppState::new();
    // Two batches stack — second one should show "batch 2/2" header.
    state.apply(DomainEvent::ApprovalRequested {
        batch_id: "first".into(),
        tools: vec![ToolInfo {
            call_id: "a".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({}),
        }],
    });
    state.apply(DomainEvent::ApprovalRequested {
        batch_id: "second".into(),
        tools: vec![ToolInfo {
            call_id: "b".into(),
            tool_name: "write".into(),
            input: serde_json::json!({}),
        }],
    });
    insta::assert_snapshot!(render_buf(&state, 90, 22));
}

#[test]
fn ask_user_single_select_open() {
    let mut state = AppState::new();
    state.messages.push(ChatMessage::User("change it".into()));
    state.apply(DomainEvent::AskUserRequested {
        call_id: "q1".into(),
        question: "Which style do you prefer?".into(),
        options: vec![
            "Minimalist".into(),
            "Colorful".into(),
            "Monochrome".into(),
        ],
        multi_select: false,
    });
    assert_eq!(state.mode, AppMode::AskPending);
    insta::assert_snapshot!(render_buf(&state, 90, 24));
}

#[test]
fn ask_user_multi_select_with_toggles() {
    let mut state = AppState::new();
    state.apply(DomainEvent::AskUserRequested {
        call_id: "q2".into(),
        question: "Tags for this issue?".into(),
        options: vec![
            "urgent".into(),
            "bug".into(),
            "feature".into(),
            "needs-review".into(),
        ],
        multi_select: true,
    });
    // Toggle first option with Space, then write some free-text.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let k = |c, m| KeyEvent::new(c, m);
    state.handle_key(&k(KeyCode::Char(' '), KeyModifiers::NONE));
    state.handle_key(&k(KeyCode::Char(' '), KeyModifiers::NONE));
    for ch in "edge case".chars() {
        state.handle_key(&k(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    insta::assert_snapshot!(render_buf(&state, 90, 26));
}

#[test]
fn ask_user_free_text_mode() {
    let mut state = AppState::new();
    state.apply(DomainEvent::AskUserRequested {
        call_id: "q3".into(),
        question: "Describe the bug in your own words:".into(),
        options: vec![],
        multi_select: false,
    });
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let k = |c, m| KeyEvent::new(c, m);
    for ch in "the loader hangs on cold start".chars() {
        state.handle_key(&k(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    insta::assert_snapshot!(render_buf(&state, 90, 22));
}

#[test]
fn sessions_overlay_empty() {
    let mut state = AppState::new();
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    state.handle_key(&key);
    insta::assert_snapshot!(render_buf(&state, 90, 22));
}

#[test]
fn sessions_overlay_with_filter_match() {
    let mut state = AppState::new();
    // Inject three sessions into the local cache directly.
    state.sessions.push(sylvander_tui::modal::SessionEntry {
        id: "a1b2c3d4".into(),
        label: "auth-refactor".into(),
        status: sylvander_tui::modal::SessionStatus::Working,
        workspace: "~/Projects/acme-api".into(),
        last_seen_secs: 120,
    });
    state.sessions.push(sylvander_tui::modal::SessionEntry {
        id: "e5f6g7h8".into(),
        label: "auth-debug".into(),
        status: sylvander_tui::modal::SessionStatus::Waiting,
        workspace: "~/Projects/acme-api".into(),
        last_seen_secs: 3600,
    });
    state.sessions.push(sylvander_tui::modal::SessionEntry {
        id: "i9j0k1l2".into(),
        label: "login-tests".into(),
        status: sylvander_tui::modal::SessionStatus::Complete,
        workspace: "~/Projects/web".into(),
        last_seen_secs: 86_400,
    });
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('p'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    state.handle_key(&key);
    insta::assert_snapshot!(render_buf(&state, 90, 22));
}

#[test]
fn palette_empty_filter_shows_all() {
    let mut state = AppState::new();
    // `/` opens the palette when composer is empty.
    state.handle_key(&crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('/'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(state.modals.len(), 1);
    insta::assert_snapshot!(render_buf(&state, 90, 22));
}

#[test]
fn palette_with_no_match() {
    let mut state = AppState::new();
    state.handle_key(&crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('/'),
        crossterm::event::KeyModifiers::NONE,
    ));
    // Type more letters that no command matches.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let k = |c, m| KeyEvent::new(c, m);
    for ch in "xyz".chars() {
        state.handle_key(&k(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    insta::assert_snapshot!(render_buf(&state, 90, 22));
}

// ---------------------------------------------------------------------------
// Responsive breakpoint snapshots (UX §13).
// ---------------------------------------------------------------------------

fn seed_state() -> AppState {
    let mut s = AppState::new();
    s.apply(DomainEvent::Connected);
    s.messages.push(ChatMessage::User("Add JWT auth.".into()));
    s.apply(DomainEvent::TextChunk {
        delta: "Inspecting router.".into(),
    });
    s.apply(DomainEvent::AgentDone {
        final_text: "Inspecting router.".into(),
    });
    s
}

#[test]
fn layout_wide_breakpoint() {
    let s = seed_state();
    insta::assert_snapshot!(render_buf(&s, 132, 30));
}

#[test]
fn layout_standard_breakpoint() {
    let s = seed_state();
    insta::assert_snapshot!(render_buf(&s, 88, 24));
}

#[test]
fn layout_narrow_breakpoint_drops_meta() {
    let s = seed_state();
    insta::assert_snapshot!(render_buf(&s, 70, 22));
}

#[test]
fn layout_too_small_renders_resize_message() {
    let s = seed_state();
    insta::assert_snapshot!(render_buf(&s, 40, 20));
}

#[test]
fn server_side_tool_rejection_lands_in_transcript() {
    let mut s = AppState::new();
    s.messages
        .push(ChatMessage::User("try `rm -rf /`".into()));
    s.apply(DomainEvent::ToolStarted {
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "rm -rf /"}),
    });
    s.apply(DomainEvent::ToolRejected {
        tool_name: "bash".into(),
        reason: "destructive commands blocked by policy".into(),
    });
    insta::assert_snapshot!(render_buf(&s, 80, 22));
}

#[test]
fn plan_block_renders_with_progress_markers() {
    let mut s = AppState::new();
    s.messages
        .push(ChatMessage::User("set up auth".into()));
    s.apply(DomainEvent::PlanReceived {
        plan_id: "p-1".into(),
        steps: vec![
            "Inspect the current authentication boundary".into(),
            "Define the JWT verification interface".into(),
            "Implement verifier and middleware".into(),
            "Add unit and integration tests".into(),
            "Run workspace verification".into(),
        ],
        current: 1,
    });
    insta::assert_snapshot!(render_buf(&s, 90, 24));
}

#[test]
fn tasks_summary_line_compacts_running_and_done() {
    let mut s = AppState::new();
    s.apply(DomainEvent::TaskStarted {
        task_id: "t1".into(),
        owner: "explorer".into(),
        purpose: "scan auth middleware".into(),
    });
    s.apply(DomainEvent::TaskStarted {
        task_id: "t2".into(),
        owner: "coder".into(),
        purpose: "draft verifier".into(),
    });
    insta::assert_snapshot!(render_buf(&s, 80, 22));
}

#[test]
fn full_panel_at_user_terminal_size_140x40() {
    // Captures the same dimensions the user's screenshot used (~140×40)
    // so the visual output is directly comparable to docs/
    // sylvander-tui-ux-design.md §5 (Canonical Conversation Screen).
    let mut s = AppState::new();
    s.apply(DomainEvent::Connected);
    s.messages
        .push(ChatMessage::User("Add JWT auth middleware".into()));
    s.apply(DomainEvent::TextChunk {
        delta: "Inspecting the existing router to understand the auth surface.".into(),
    });
    s.apply(DomainEvent::ToolStarted {
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "ls src/http"}),
    });
    s.apply(DomainEvent::ToolFinished {
        tool_name: "bash".into(),
        output: "router.rs\nmiddleware.rs".into(),
        is_error: false,
    });
    s.apply(DomainEvent::ToolStarted {
        tool_name: "read".into(),
        input: serde_json::json!({"path": "src/http/middleware.rs"}),
    });
    s.apply(DomainEvent::TextChunk {
        delta: " I see we have a `TokenGuard` already — let me check it covers Bearer + API-key.".into(),
    });
    insta::assert_snapshot!(render_buf(&s, 140, 40));
}

// ---------------------------------------------------------------------------
// M-T14 parity snapshots — captures the design-ground-truth visual states.
// Reference: docs/design/02-tui-immersive.svg + 18-composer-interactions.svg.
// ---------------------------------------------------------------------------

#[test]
fn design_canonical_welcome_lockup_120x36() {
    // 120 columns × 36 rows is §5's reference viewport. Welcome
    // lockup (§2.2) appears once on first launch when the
    // transcript + sessions cache are empty.
    let state = AppState::new();
    insta::assert_snapshot!(render_buf(&state, 120, 36));
}

#[test]
fn design_canonical_with_tool_step_grouped_120x36() {
    // All M-T14 visual primitives exercised together: header bar
    // with crab mark + workspace meta, tool rhythm (UX §6), and
    // bottom status row showing the Working mode glyph + tool count.
    let mut s = AppState::new();
    s.apply(DomainEvent::Connected);
    s.messages
        .push(ChatMessage::User("Review the auth middleware".into()));
    s.apply(DomainEvent::ToolStarted {
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "ls src/http"}),
    });
    s.apply(DomainEvent::ToolFinished {
        tool_name: "bash".into(),
        output: "router.rs\nmiddleware.rs".into(),
        is_error: false,
    });
    s.apply(DomainEvent::ToolStarted {
        tool_name: "read".into(),
        input: serde_json::json!({"path": "src/http/middleware.rs"}),
    });
    insta::assert_snapshot!(render_buf(&s, 120, 36));
}

#[test]
fn design_disconnected_state_120x36() {
    // Status row switches to Disconnected mode (`!` glyph + amber
    // `disconnected` label) — see 18-composer-interactions.svg
    // ADAPTIVE STATUS panel.
    let mut s = AppState::new();
    s.messages.push(ChatMessage::User("any draft here".into()));
    // Do NOT call Connected → AppState.connected stays false.
    insta::assert_snapshot!(render_buf(&s, 120, 36));
}
