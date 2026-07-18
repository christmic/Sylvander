use super::*;

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

#[test]
fn vim_mode_is_optional_visible_and_does_not_insert_normal_keys() {
    let mut composer = Composer::default();
    composer.set_editing_style(EditingStyle::Vim);
    composer.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
    assert_eq!(composer.text(), "a");
    assert!(composer.handle_escape());
    assert_eq!(composer.mode_label(), Some("NORMAL"));

    composer.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(composer.is_empty());
    composer.handle_key(&key(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('好'), KeyModifiers::NONE));
    assert_eq!(composer.text(), "好");
    assert_eq!(composer.mode_label(), Some("INSERT"));
}

#[test]
fn vim_normal_motions_are_utf8_safe_across_words_and_rows() {
    let mut composer = Composer::default();
    composer.set_editing_style(EditingStyle::Vim);
    composer.replace_text("alpha 世界\nxy");
    assert!(composer.handle_escape());

    composer.handle_key(&key(KeyCode::Char('k'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('0'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('w'), KeyModifiers::NONE));
    assert_eq!(composer.cursor_col_chars(), 6);
    composer.handle_key(&key(KeyCode::Char('j'), KeyModifiers::NONE));
    assert_eq!(composer.cursor_row(), 1);
    assert_eq!(composer.cursor_col_chars(), 2);
    composer.handle_key(&key(KeyCode::Char('k'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(composer.cursor_col_chars(), 0);
}

#[test]
fn vim_open_line_and_enter_submit_follow_composer_contract() {
    let mut composer = Composer::default();
    composer.set_editing_style(EditingStyle::Vim);
    composer.replace_text("first");
    assert!(composer.handle_escape());
    composer.handle_key(&key(KeyCode::Char('o'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('二'), KeyModifiers::NONE));
    assert!(composer.handle_escape());
    assert_eq!(composer.text(), "first\n二");
    assert_eq!(
        composer.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)),
        Some("first\n二".into())
    );
}

#[test]
fn vim_undo_groups_one_insert_or_change_as_one_edit() {
    let mut composer = Composer::default();
    composer.set_editing_style(EditingStyle::Vim);
    for character in "alpha".chars() {
        composer.handle_key(&key(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert!(composer.handle_escape());
    composer.handle_key(&key(KeyCode::Char('u'), KeyModifiers::NONE));
    assert!(composer.is_empty());

    composer.handle_key(&key(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.replace_text("one two");
    assert!(composer.handle_escape());
    composer.handle_key(&key(KeyCode::Char('0'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('c'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('w'), KeyModifiers::NONE));
    for character in "new ".chars() {
        composer.handle_key(&key(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert!(composer.handle_escape());
    assert_eq!(composer.text(), "new two");
    composer.handle_key(&key(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(composer.text(), "one two");
}

#[test]
fn vim_delete_yank_and_paste_use_internal_register() {
    let mut composer = Composer::default();
    composer.set_editing_style(EditingStyle::Vim);
    composer.replace_text("one\ntwo");
    assert!(composer.handle_escape());
    composer.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('y'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('y'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('G'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('p'), KeyModifiers::NONE));
    assert_eq!(composer.text(), "one\ntwo\none");

    composer.handle_key(&key(KeyCode::Char('d'), KeyModifiers::NONE));
    composer.handle_key(&key(KeyCode::Char('d'), KeyModifiers::NONE));
    assert_eq!(composer.text(), "one\ntwo");
    composer.handle_key(&key(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(composer.text(), "one\ntwo\none");
}

#[test]
fn shift_enter_inserts_newline_not_submit() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
    c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(c.text(), "a\nb");
    assert_eq!(c.row_count(), 2);
}

#[test]
fn plain_enter_submits() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Char('i'), KeyModifiers::NONE));
    let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(submitted.as_deref(), Some("hi"));
    assert!(c.is_empty());
}

#[test]
fn plain_enter_on_empty_does_nothing() {
    let mut c = Composer::default();
    let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(submitted, None);
}

#[test]
fn ctrl_enter_inserts_newline_fallback() {
    // Ctrl+Enter is a fallback for terminals that swallow Shift+Enter.
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::CONTROL));
    c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(c.text(), "a\nb");
    assert_eq!(c.row_count(), 2);
}

#[test]
fn multi_line_submit_joined_with_newline() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
    c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
    let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(submitted.as_deref(), Some("a\nb"));
}

#[test]
fn backspace_at_start_of_row_joins_with_previous_row() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
    // Shift+Enter inserts newline; new convention.
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
    // rows = ["ab", ""], cursor at (1, 0)
    c.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE));
    // joins row 1 into row 0 → "ab"
    assert_eq!(c.text(), "ab");
    assert_eq!(c.row_count(), 1);
    assert_eq!(c.cursor_row(), 0);
    assert_eq!(c.cursor_col_chars(), 2);
}

#[test]
fn unicode_safe_round_trip() {
    let mut c = Composer::default();
    for ch in "你好".chars() {
        c.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    c.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(c.text(), "你");
    // Cursor should be on the char boundary, not split a code point.
    assert!(c.rows[0].is_char_boundary(c.cursor_col));
}

#[test]
fn cursor_and_delete_treat_combining_text_as_one_grapheme() {
    let mut composer = Composer::default();
    for character in "e\u{301}好".chars() {
        composer.handle_key(&key(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert_eq!(composer.cursor_col_cells(), 3);

    composer.handle_key(&key(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(composer.cursor_col_cells(), 1);
    composer.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(composer.text(), "好");
    assert_eq!(composer.cursor_col_cells(), 0);
}

#[test]
fn vertical_motion_preserves_terminal_cell_column_for_cjk() {
    let mut composer = Composer::default();
    composer.replace_text("你好世界\nabcdef");
    composer.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(composer.cursor_row(), 0);
    assert_eq!(composer.cursor_col_cells(), 6);
    composer.handle_key(&key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(composer.cursor_col_cells(), 6);
}

#[test]
fn history_up_recalls_previous_submission() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Char('w'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    // Now in fresh empty buffer. Press Up → recall last "w".
    c.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(c.text(), "w");
    // Up again → "h".
    c.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(c.text(), "h");
    // Down once → back to "w".
    c.handle_key(&key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(c.text(), "w");
}

#[test]
fn arrows_move_inside_multiline_draft_before_history() {
    let mut composer = Composer::default();
    composer.history.push_back("older prompt".into());
    composer.replace_text("first\nxy");

    composer.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(composer.text(), "first\nxy");
    assert_eq!(composer.cursor_row(), 0);
    assert_eq!(composer.cursor_col_chars(), 2);
    assert!(composer.history_idx.is_none());
}

#[test]
fn history_dedupes_consecutive_equal_submissions() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(c.history.len(), 1);
}

#[test]
fn history_is_capped_at_history_cap_unique_entries() {
    // Each submission must be distinct to defeat dedup; otherwise the
    // ring would never fill. We append a counter to make every entry unique.
    let mut c = Composer::default();
    for i in 0..(HISTORY_CAP + 5) {
        let s = format!("q{i}");
        for ch in s.chars() {
            c.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let _ = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    }
    assert_eq!(c.history.len(), HISTORY_CAP);
}

#[test]
fn submit_pushes_into_history() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('z'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(c.history.back().map(String::as_str), Some("z"));
}

#[test]
fn take_submit_clears_buffer() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('q'), KeyModifiers::NONE));
    let _ = c.take_submit();
    assert!(c.is_empty());
}

#[test]
fn delete_forward_at_end_of_row_merges_with_next_row() {
    // Construct state with cursor at end of "ab" and a second row containing
    // "cd". Then Delete should join them into "abcd".
    let mut c = Composer {
        rows: vec!["ab".to_string(), "cd".to_string()],
        cursor_row: 0,
        cursor_col: 2,
        anchor: None,
        ..Default::default()
    };
    c.handle_key(&key(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(c.text(), "abcd");
    assert_eq!(c.row_count(), 1);
}

// ----- paste / attachment -----

#[test]
fn paste_under_8_lines_inserts_inline() {
    let mut c = Composer::default();
    let outcome = c.paste("hello\nworld\nfoo");
    assert_eq!(outcome, PasteOutcome::Inlined);
    assert_eq!(c.text(), "hello\nworld\nfoo");
    assert_eq!(c.attachment_count(), 0);
}

#[test]
fn paste_exactly_8_lines_inserts_inline_at_threshold() {
    let mut c = Composer::default();
    let payload = "1\n2\n3\n4\n5\n6\n7\n8"; // 8 lines
    assert_eq!(c.paste(payload), PasteOutcome::Inlined);
}

#[test]
fn paste_over_8_lines_collapses_to_attachment() {
    let mut c = Composer::default();
    let payload = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let outcome = c.paste(&payload);
    assert_eq!(outcome, PasteOutcome::Attached);
    assert_eq!(c.attachment_count(), 1);
    assert_eq!(c.attachments[0].kind, AttachmentKind::Paste);
    assert_eq!(c.attachments[0].line_count, 20);
    assert_eq!(c.attachments[0].content, payload);
    // Draft must remain untouched.
    assert!(c.is_empty());
}

#[test]
fn oversized_paste_and_attachment_flood_are_rejected_without_retention() {
    let mut composer = Composer::default();
    assert_eq!(
        composer.paste(&"x".repeat(MAX_DRAFT_BYTES + 1)),
        PasteOutcome::Rejected
    );
    assert!(composer.is_empty());

    composer.attachments = (0..MAX_COMPOSER_ATTACHMENTS)
        .map(|_| Attachment::new_paste("line 1\nline 2".into()))
        .collect();
    assert_eq!(
        composer.paste("1\n2\n3\n4\n5\n6\n7\n8\n9"),
        PasteOutcome::Rejected
    );
    assert_eq!(composer.attachment_count(), MAX_COMPOSER_ATTACHMENTS);
}

#[test]
fn external_editor_replacement_is_utf8_safe_and_bounded() {
    let mut composer = Composer::default();
    let truncated = composer.replace_text(&"蟹".repeat(MAX_DRAFT_BYTES));
    assert!(truncated);
    let text = composer.text();
    assert!(text.len() <= MAX_DRAFT_BYTES);
    assert!(text.is_char_boundary(text.len()));
}

#[test]
fn empty_paste_is_noop() {
    let mut c = Composer::default();
    assert_eq!(c.paste(""), PasteOutcome::Inlined);
    assert_eq!(c.attachment_count(), 0);
    assert!(c.is_empty());
}

#[test]
fn submit_keeps_attachment_typed_and_out_of_visible_history() {
    let mut c = Composer::default();
    c.paste(
        &(1..=15)
            .map(|i| format!("L{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    c.handle_key(&key(KeyCode::Char('q'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Char('u'), KeyModifiers::NONE));
    c.handle_key(&key(KeyCode::Char('e'), KeyModifiers::NONE));
    // Plain Enter submits in the new convention.
    let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    let submitted = submitted.expect("submit");
    assert_eq!(submitted, "que");
    let attachments = c.take_submitted_attachments();
    assert_eq!(attachments.len(), 1);
    assert!(matches!(
        &attachments[0].content,
        sylvander_protocol::AttachmentContent::Text { text }
            if text.starts_with("L1\n") && text.ends_with("L15")
    ));
    assert_eq!(c.history.back().map(String::as_str), Some("que"));
    // Everything cleared on submit.
    assert!(c.is_empty());
    assert_eq!(c.attachment_count(), 0);
}

#[test]
fn attachment_label_is_human_friendly() {
    let payload = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor";
    let att = Attachment::new_paste(payload.to_string());
    let label = att.label();
    // Label should include kind, line count, and size.
    assert!(label.starts_with("[paste:"));
    assert!(label.contains("lines"));
    assert!(label.contains("lorem"));
    // Preview truncates long content.
    assert!(label.contains('…') || label.chars().count() < 80);
}

#[test]
fn multiple_pastes_become_multiple_attachments() {
    let mut c = Composer::default();
    for _ in 0..3 {
        c.paste(
            &(1..=10)
                .map(|i| format!("L{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    assert_eq!(c.attachment_count(), 3);
}

#[test]
fn workspace_file_attachment_is_scoped_typed_and_reorderable() {
    let root = tempdir();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let mut composer = Composer::default();
    composer
        .attach_file(
            &root,
            std::path::Path::new("src/main.rs"),
            512 * 1024,
            false,
        )
        .unwrap();
    composer
        .attachments
        .push(Attachment::new_paste("one\ntwo".into()));
    assert_eq!(composer.attachments[0].mime_type, "text/x-rust");
    assert!(composer.move_attachment(1, 0));
    assert_eq!(composer.attachments[0].kind, AttachmentKind::Paste);
    assert!(composer.remove_attachment(0));
    assert_eq!(composer.attachments[0].name, "src/main.rs");

    let outside = root.parent().unwrap().join("outside-secret.txt");
    std::fs::write(&outside, "secret").unwrap();
    assert!(
        composer
            .attach_file(&root, &outside, 512 * 1024, false)
            .is_err()
    );
    std::fs::remove_file(outside).ok();
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn image_attachment_is_capability_gated_and_base64_typed() {
    let root = tempdir();
    let image = root.join("crab.png");
    let bytes = b"\x89PNG\r\n\x1a\nsmall-image";
    std::fs::write(&image, bytes).unwrap();
    let mut composer = Composer::default();

    assert!(composer.attach_file(&root, &image, 1024, false).is_err());
    composer.attach_file(&root, &image, 1024, true).unwrap();
    let attachment = composer.attachments.first().unwrap();
    assert_eq!(attachment.kind, AttachmentKind::Image);
    assert_eq!(attachment.mime_type, "image/png");
    assert_eq!(attachment.byte_count, bytes.len());
    assert!(matches!(
        attachment.to_message_attachment(0).content,
        sylvander_protocol::AttachmentContent::Base64 { ref data }
            if base64::engine::general_purpose::STANDARD.decode(data).unwrap() == bytes
    ));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn pasted_attachment_is_checked_against_server_limit_before_submit() {
    let mut composer = Composer::default();
    composer
        .attachments
        .push(Attachment::new_paste("too large".into()));
    assert!(composer.validate_attachments(3, false).is_err());
    assert!(composer.validate_attachments(1024, false).is_ok());
}

#[test]
fn composer_selection_becomes_a_typed_attachment_without_deleting_text() {
    let mut composer = Composer::default();
    for character in "hello".chars() {
        composer.handle_key(&key(KeyCode::Char(character), KeyModifiers::NONE));
    }
    composer.handle_key(&key(KeyCode::Home, KeyModifiers::SHIFT));
    assert_eq!(composer.selected_text().as_deref(), Some("hello"));
    composer
        .attach_text(
            AttachmentKind::Selection,
            "selection",
            "text/plain",
            composer.selected_text().unwrap(),
        )
        .unwrap();
    assert_eq!(composer.text(), "hello");
    assert_eq!(
        composer.attachments[0].to_message_attachment(0).kind,
        sylvander_protocol::AttachmentKind::Selection
    );
}

#[test]
fn history_round_trips_through_disk() {
    let dir = tempdir();
    let path = dir.join("history.json");
    // Pre-populate one entry, then save.
    let mut c1 = Composer::default();
    c1.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
    let _ = c1.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    c1.save_history_to(&path).expect("save");
    // A fresh composer loads from disk; remembered history is there.
    let loaded = Composer::load_history_from(&path);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded.front().map(String::as_str), Some("h"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn history_load_returns_empty_on_missing_file() {
    let dir = tempdir();
    let path = dir.join("nonexistent.json");
    let loaded = Composer::load_history_from(&path);
    assert!(loaded.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn history_save_is_atomic_under_dir() {
    // Save to a path whose parent does not exist yet — save_history_to
    // must create the directory.
    let dir = tempdir();
    let nested = dir.join("nested").join("history.json");
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('q'), KeyModifiers::NONE));
    let _ = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
    c.save_history_to(&nested).expect("save");
    assert!(nested.exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn crash_safe_draft_restores_text_and_typed_attachments_then_clears_on_submit() {
    let dir = tempdir();
    let path = dir.join("draft.json");
    let mut original = Composer::default();
    original.paste("draft text");
    original
        .attachments
        .push(Attachment::new_paste("one\ntwo".into()));
    original.save_draft_to(&path).expect("save draft");

    let mut restored = Composer::default();
    assert!(restored.restore_draft_from(&path).expect("restore"));
    assert_eq!(restored.text(), "draft text");
    assert_eq!(restored.attachments.len(), 1);
    restored.take_submit();
    restored.save_draft_to(&path).expect("clear draft");
    assert!(!path.exists());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn draft_restore_rejects_content_beyond_composer_limits() {
    let dir = tempdir();
    let path = dir.join("draft.json");
    let snapshot = DraftSnapshot {
        text: "x".repeat(MAX_DRAFT_BYTES + 1),
        attachments: Vec::new(),
    };
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let error = Composer::default()
        .restore_draft_from(&path)
        .expect_err("oversized draft");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    std::fs::remove_dir_all(dir).ok();
}

fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sylvander-tui-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

// ----- M-T15.D focus state tests -----

#[test]
fn fresh_composer_is_idle() {
    let c = Composer::default();
    assert!(!c.has_focus_interaction());
}

#[test]
fn typing_a_char_marks_focused() {
    let mut c = Composer::default();
    assert!(!c.has_focus_interaction());
    c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(c.has_focus_interaction());
}

#[test]
fn paste_marks_focused() {
    let mut c = Composer::default();
    assert!(!c.has_focus_interaction());
    let _ = c.paste("hello world");
    assert!(c.has_focus_interaction());
}

#[test]
fn reset_focus_returns_to_idle() {
    let mut c = Composer::default();
    c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(c.has_focus_interaction());
    c.reset_focus();
    assert!(!c.has_focus_interaction());
}
