use super::*;
use crate::app::{AppState, ChatMessage, ToolStatus};
use crate::component::Component;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn terminal(w: u16, h: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(w, h)).expect("terminal")
}

fn seeded() -> AppState {
    let mut s = AppState::new();
    s.apply(crate::event::DomainEvent::Connected);
    s.messages.push(ChatMessage::User("Hi".into()));
    s.apply(crate::event::DomainEvent::TextChunk {
        delta: "world".into(),
    });
    s.apply(crate::event::DomainEvent::AgentDone {
        final_text: "world".into(),
        feedback_target: None,
    });
    s
}

#[test]
fn user_speaker_uses_dim_color() {
    let s = seeded();
    let mut t = terminal(60, 12);
    t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 12), &s))
        .unwrap();
    let cell = t
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "❯")
        .expect("user speaker cell");
    assert_eq!(cell.fg, crate::theme::TEXT_DIM);
}

#[test]
fn agent_body_uses_primary_text() {
    let s = seeded();
    let mut t = terminal(60, 12);
    t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 12), &s))
        .unwrap();
    let buf = t.backend().buffer().clone();
    let mut found = false;
    for y in 0..12 {
        for x in 0..60 {
            if let Some(c) = buf.cell((x, y))
                && c.fg == crate::theme::TEXT
            {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    assert!(found, "expected a primary-text cell");
}

#[test]
fn welcome_lockup_renders_crab_at_first_launch() {
    let s = AppState::new();
    let mut t = terminal(120, 36);
    t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 120, 36), &s))
        .unwrap();
    let buf = t.backend().buffer().clone();
    let mut found_warm = false;
    let mut found_violet = false;
    for y in 0..36 {
        for x in 0..120 {
            if let Some(c) = buf.cell((x, y)) {
                if c.fg == crate::theme::BRAND_WARM && c.symbol() != " " {
                    found_warm = true;
                }
                if c.fg == crate::theme::BRAND_VIOLET && c.symbol() != " " {
                    found_violet = true;
                }
            }
        }
    }
    assert!(found_warm, "expected warm half of Terminal Seed-Crab");
    assert!(found_violet, "expected violet half of Terminal Seed-Crab");
}

#[test]
fn welcome_uses_complete_canonical_character_and_horizontal_info() {
    let state = AppState::new();
    assert_eq!(TERMINAL_SEED_CRAB.len(), 8);
    assert!(
        TERMINAL_SEED_CRAB[4..]
            .iter()
            .all(|row| !row.trim().is_empty()),
        "lower claws and walking legs must remain in the canonical asset"
    );
    assert!(
        TERMINAL_SEED_CRAB
            .iter()
            .all(|row| row.chars().count() <= SEED_CRAB_CELL_WIDTH),
        "canonical character must stay inside its reserved column"
    );

    let wide = build_welcome_lockup(110, &state);
    assert_eq!(wide.len(), TERMINAL_SEED_CRAB.len() + 1);
    assert!(
        wide.iter()
            .any(|line| line.to_string().contains("Sylvander")),
        "brand information must render beside the character"
    );

    let narrow = build_welcome_lockup(60, &state);
    assert!(
        narrow.len() > TERMINAL_SEED_CRAB.len(),
        "narrow welcome reflows information below the same character"
    );
    for (rendered, canonical) in narrow.iter().zip(TERMINAL_SEED_CRAB.iter()) {
        assert_eq!(rendered.to_string(), *canonical);
    }
}

#[test]
fn readable_column_stays_left_anchored_when_terminal_goes_fullscreen() {
    let normal = readable_area(Rect::new(0, 0, 120, 36));
    let fullscreen = readable_area(Rect::new(0, 0, 240, 60));

    assert_eq!(normal.x, 0);
    assert_eq!(fullscreen.x, normal.x);
    assert_eq!(normal.width, 110);
    assert_eq!(fullscreen.width, normal.width);
}

#[test]
fn welcome_prelude_remains_when_first_turn_is_appended() {
    let mut s = AppState::new();
    s.welcomed = true;
    s.messages.push(ChatMessage::User("x".into()));
    let mut t = terminal(120, 36);
    t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 120, 36), &s))
        .unwrap();
    let buf = t.backend().buffer().clone();
    let mut found_brand = false;
    let mut found_turn = false;
    for y in 0..36 {
        for x in 0..120 {
            if let Some(c) = buf.cell((x, y)) {
                if c.fg == crate::theme::BRAND_WARM && c.symbol() != " " {
                    found_brand = true;
                }
                if c.symbol() == "❯" {
                    found_turn = true;
                }
            }
        }
    }
    assert!(found_brand, "Welcome must remain as the transcript prelude");
    assert!(found_turn, "submitted turn must append below Welcome");
}

#[test]
fn diagnostics_and_temporary_surfaces_never_remove_welcome_prelude() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut state = AppState::new();
    state.messages.push(ChatMessage::Info(
        "Connected to the local Sylvander service".into(),
    ));
    state.handle_key(&KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(!state.modals.is_empty(), "command picker must be open");

    let rendered = transcript_lines(&state, 110)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Sylvander"));
    assert!(rendered.contains("What should we work through?"));
}

#[test]
fn wrapped_user_turn_marks_only_the_first_visual_row() {
    let mut lines = Vec::new();
    let message = ChatMessage::User("a user message that wraps across rows".into());
    push_message_lines(&message, &mut lines, 14, false, &[]);
    let rendered = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(rendered.matches('❯').count(), 1);
    assert!(rendered.lines().skip(1).all(|line| line.starts_with("  ")));
}

#[test]
fn transcript_chunks_measure_cjk_and_mixed_text_in_terminal_cells() {
    assert_eq!(display_chunks("你好世界", 4), ["你好", "世界"]);
    assert_eq!(display_chunks("A你B好C", 3), ["A你", "B好", "C"]);

    for row in display_chunks("A你B好C", 3) {
        assert!(
            UnicodeWidthStr::width(row) <= 3,
            "ordinary graphemes must fit the requested terminal-cell width: {row:?}"
        );
    }
}

#[test]
fn transcript_chunks_never_split_combining_or_emoji_zwj_graphemes() {
    let family = "👨‍👩‍👧‍👦";
    let combined = "e\u{301}";
    let text = format!("a{family}{combined}b");
    let rows = display_chunks(&text, 2);

    assert_eq!(rows.concat(), text);
    assert!(rows.contains(&family));
    assert!(rows.iter().any(|row| row.contains(combined)));
    assert_eq!(
        rows.iter()
            .flat_map(|row| row.graphemes(true))
            .collect::<Vec<_>>(),
        text.graphemes(true).collect::<Vec<_>>()
    );
}

#[test]
fn transcript_chunks_make_progress_for_zero_or_too_narrow_widths() {
    let family = "👨‍👩‍👧‍👦";
    assert_eq!(display_chunks(family, 0), [family]);
    assert_eq!(display_chunks(family, 1), [family]);
    assert_eq!(display_chunks("ab", 0), ["a", "b"]);
    assert_eq!(display_chunks("你好\n\nworld", 8), ["你好", "", "world"]);
}

#[test]
fn queued_and_task_word_wrapping_uses_display_cells_and_keeps_clusters() {
    let family = "👨‍👩‍👧‍👦";
    let text = format!("你好 {family} cafe\u{301} mixed");
    let rows = wrap_words(&text, "", "  ".into(), 8);
    let normalized = rows
        .iter()
        .map(|row| row.trim())
        .collect::<Vec<_>>()
        .join(" ");

    assert_eq!(normalized, text);
    assert!(
        rows.iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) <= 8)
    );
    assert!(rows.iter().any(|row| row.contains(family)));
    assert!(rows.iter().any(|row| row.contains("e\u{301}")));

    assert_eq!(wrap_words("ab", "", "  ".into(), 1), ["a", "b"]);
    assert_eq!(wrap_words(family, "", "  ".into(), 1), [family]);
}

#[test]
fn transcript_truncation_is_cell_bounded_and_grapheme_safe() {
    let combined = "e\u{301}";
    let truncated = truncate_display(&format!("你好{combined}world"), 6);
    let family = "👨‍👩‍👧‍👦";
    let emoji = truncate_display(&format!("{family}abc"), 3);

    assert_eq!(UnicodeWidthStr::width(truncated.as_str()), 6);
    assert!(truncated.contains(combined));
    assert!(truncated.ends_with('…'));
    assert_eq!(emoji, format!("{family}…"));
}

#[test]
fn structured_input_labels_align_by_display_cells() {
    let lines = input_kv_lines(&serde_json::json!({"id": "short", "模型": "中文"}), 40);
    let label_widths = lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.spans[0].content.as_ref()))
        .collect::<Vec<_>>();

    assert_eq!(label_widths, vec![8, 8]);
}

#[test]
fn collapsed_command_shows_only_the_latest_live_progress() {
    let mut lines = Vec::new();
    let message = ChatMessage::ToolStep {
        name: "Run `cargo`".into(),
        started_at_secs: 0,
        children: vec![crate::app::ToolStepChild {
            call_id: "call-1".into(),
            name: "Command".into(),
            status: ToolStatus::Pending,
            input: serde_json::json!({"command": "cargo test"}),
            output: Some("Compiling agent\nCompiling runtime".into()),
            is_error: None,
        }],
    };
    push_message_lines(&message, &mut lines, 100, false, &[]);
    let rendered = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("$ cargo test"));
    assert!(rendered.contains("Compiling runtime"));
    assert!(!rendered.contains("Compiling agent"));
}

#[test]
fn agent_turn_is_clean_word_wrapped_content_with_one_presence_mark() {
    let mut lines = Vec::new();
    push_agent_turn(
        "I have tools:1. **`ask_user`** — Ask for missing information.2. **`Read`** — Read a workspace file.",
        &mut lines,
        42,
    );
    let rendered = lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(rendered.matches('⏺').count(), 1);
    assert!(!rendered.contains("/\\"));
    assert!(!rendered.contains("(••)"));
    assert!(!rendered.contains("<__>"));
    assert!(!rendered.contains("**"));
    assert!(!rendered.contains('`'));
    assert!(rendered.contains("\n  1. ask_user"));
    assert!(rendered.contains("\n  2. Read"));
    assert!(lines.iter().all(|line| line.width() <= 42));
}

#[test]
fn streaming_and_settled_agent_turn_keep_the_same_vertical_origin() {
    let mut state = AppState::new();
    state.welcomed = true;
    state.messages.push(ChatMessage::User("hello".into()));
    state.streaming = "A stable reply".into();

    let marker_y = |state: &AppState| {
        let mut terminal = terminal(120, 36);
        terminal
            .draw(|frame| ChatPanel.render(frame, Rect::new(0, 0, 120, 36), state))
            .expect("render chat");
        let buffer = terminal.backend().buffer();
        (0..36)
            .find(|&y| (0..120).any(|x| buffer.cell((x, y)).is_some_and(|c| c.symbol() == "⏺")))
            .expect("agent presence mark")
    };

    let streaming_y = marker_y(&state);
    state.apply(crate::event::DomainEvent::AgentDone {
        final_text: "A stable reply".into(),
        feedback_target: None,
    });
    let settled_y = marker_y(&state);
    assert_eq!(streaming_y, settled_y);
}

#[test]
fn input_kv_lines_skips_null_and_empty_object() {
    assert!(input_kv_lines(&serde_json::Value::Null, 80).is_empty());
    assert!(input_kv_lines(&serde_json::json!({}), 80).is_empty());
}

#[test]
fn input_kv_lines_emits_pair_per_object_key() {
    // serde_json::Map defaults to BTreeMap, so keys come back in
    // alphabetical order — assert set membership instead of ordering.
    let lines = input_kv_lines(&serde_json::json!({"path": "/tmp", "mode": "r"}), 60);
    assert_eq!(lines.len(), 2);
    let labels: Vec<String> = lines
        .iter()
        .map(|l| l.spans[0].content.to_string())
        .collect();
    assert!(
        labels.iter().any(|l| l.contains("path")),
        "expected `path` label, got: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("mode")),
        "expected `mode` label, got: {labels:?}"
    );
}

#[test]
fn plan_step_three_glyphs() {
    let (gd, _) = theme::plan_step_glyph_and_style(true, false);
    let (gc, _) = theme::plan_step_glyph_and_style(false, true);
    let (gp, _) = theme::plan_step_glyph_and_style(false, false);
    assert_eq!(gd, "✓");
    assert_eq!(gc, "●");
    assert_eq!(gp, "○");
}

#[test]
fn tool_status_styles_three_distinct_fg() {
    let (_, sp) = theme::tool_status_glyph_and_style(ToolStatus::Pending);
    let (_, sd) = theme::tool_status_glyph_and_style(ToolStatus::Done);
    let (_, se) = theme::tool_status_glyph_and_style(ToolStatus::Error);
    assert_ne!(sp.fg, sd.fg);
    assert_ne!(sp.fg, se.fg);
    assert_ne!(sd.fg, se.fg);
}

#[test]
fn render_order_user_then_agent_then_toolstep() {
    // Contract: messages render in insertion order. User at the
    // top, then the agent's reply, then a grouped tool step.
    let mut s = AppState::new();
    s.apply(crate::event::DomainEvent::Connected);
    s.messages.push(ChatMessage::User("Hi".into()));
    s.apply(crate::event::DomainEvent::TextChunk {
        delta: "Hello back".into(),
    });
    s.apply(crate::event::DomainEvent::AgentDone {
        final_text: "Hello back".into(),
        feedback_target: None,
    });
    s.apply(crate::event::DomainEvent::ToolStarted {
        call_id: "call-1".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "ls"}),
    });
    s.apply(crate::event::DomainEvent::ToolFinished {
        call_id: "call-1".into(),
        tool_name: "bash".into(),
        output: "a.rs".into(),
        is_error: false,
    });
    let mut t = terminal(60, 20);
    t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 20), &s))
        .unwrap();
    let buf = t.backend().buffer().clone();
    // Find the first row of each message kind. User turns use the
    // quiet `❯` marker rather than a repeated "You:" heading.
    // The agent body is the row above any tool
    // step (which renders `⏺ Run ...`).
    let mut you_y = None;
    for y in 0..20 {
        for x in 0..60 {
            if let Some(c) = buf.cell((x, y))
                && c.symbol() == "❯"
            {
                you_y = Some(y);
                break;
            }
        }
        if you_y.is_some() {
            break;
        }
    }
    let you_y = you_y.expect("expected to find the user-turn marker");
    // Tool step row uses the Claude-familiar `⏺` activity lead.
    let mut toolstep_y = None;
    for y in 0..20 {
        for x in 0..60 {
            if let Some(c) = buf.cell((x, y)) {
                let sym = c.symbol();
                if sym == "⏺" {
                    // Step glyphs are tool-step step-header characters.
                    toolstep_y = Some(y);
                    break;
                }
            }
        }
        if toolstep_y.is_some() {
            break;
        }
    }
    let toolstep_y = toolstep_y.expect("expected to find tool step glyph");
    // Order: user (y=0 by convention but at least smallest) precedes
    // toolstep. They are guaranteed by the way push_message_lines
    // walks the messages vec.
    assert!(
        you_y < toolstep_y,
        "user row {you_y} must precede toolstep {toolstep_y}"
    );
}
