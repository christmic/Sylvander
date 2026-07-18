use super::*;

#[test]
fn osc52_clipboard_is_bounded_and_round_trips_utf8() {
    let sequence = osc52_sequence("蟹 helper").unwrap();
    let encoded = sequence
        .strip_prefix("\x1b]52;c;")
        .unwrap()
        .strip_suffix('\x07')
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
        "蟹 helper".as_bytes()
    );
    assert!(osc52_sequence(&"x".repeat(100 * 1024 + 1)).is_err());
}

#[test]
fn external_editor_replaces_text_only_after_success() {
    let edited =
        edit_draft_with_command("before", "sh -c 'printf after > \"$1\"' sylvander-editor")
            .unwrap();
    assert_eq!(edited, "after");
    assert!(edit_draft_with_command("before", "sh -c 'exit 7'").is_err());
}

#[test]
fn draft_persistence_waits_for_an_input_pause() {
    let start = Instant::now();
    let mut schedule = DraftSaveSchedule::default();
    schedule.mark_changed(start);
    assert!(!schedule.take_due(start + Duration::from_millis(249)));

    schedule.mark_changed(start + Duration::from_millis(200));
    assert!(!schedule.take_due(start + Duration::from_millis(449)));
    assert!(schedule.take_due(start + Duration::from_millis(450)));
    assert!(!schedule.take_due(start + Duration::from_secs(1)));
}

#[test]
fn only_text_input_schedules_draft_persistence() {
    assert!(affects_draft(&UserIntent::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('中'),
            crossterm::event::KeyModifiers::NONE,
        )
    )));
    assert!(affects_draft(&UserIntent::Paste("中文".into())));
    assert!(!affects_draft(&UserIntent::Redraw));
    assert!(!affects_draft(&UserIntent::ScrollTranscript { lines: 4 }));
}
