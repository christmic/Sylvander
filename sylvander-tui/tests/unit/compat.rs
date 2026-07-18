use super::*;

#[test]
fn breakpoint_thresholds() {
    assert_eq!(Breakpoint::from_width(200), Breakpoint::Wide);
    assert_eq!(Breakpoint::from_width(100), Breakpoint::Wide);
    assert_eq!(Breakpoint::from_width(99), Breakpoint::Standard);
    assert_eq!(Breakpoint::from_width(80), Breakpoint::Standard);
    assert_eq!(Breakpoint::from_width(79), Breakpoint::Narrow);
    assert_eq!(Breakpoint::from_width(50), Breakpoint::Narrow);
    assert_eq!(Breakpoint::from_width(49), Breakpoint::Compact);
    assert_eq!(Breakpoint::from_width(20), Breakpoint::Compact);
}

#[test]
fn feature_flags_match_design() {
    assert!(Breakpoint::Wide.shows_full_help());
    assert!(!Breakpoint::Standard.shows_full_help());
    assert!(Breakpoint::Wide.shows_secondary_meta());
    assert!(!Breakpoint::Narrow.shows_secondary_meta());
    assert!(Breakpoint::Wide.composer_attachment_strip());
    assert!(!Breakpoint::Narrow.composer_attachment_strip());
}

#[test]
fn compact_help_wide_carries_full_shortcuts_per_mode() {
    // Wide breakpoint must surface full shortcuts so power users can
    // discover them inline. Each mode's line is asserted to mention a
    // mode-specific cue (so we catch a regression where the wide
    // branch collapses too eagerly).
    let normal = compact_help_for(Breakpoint::Wide, "Normal");
    assert!(normal.contains("Shift+Enter"));
    let ap = compact_help_for(Breakpoint::Wide, "ApprovalPending");
    assert!(ap.contains("approve") || ap.contains('Y'));
    let ask = compact_help_for(Breakpoint::Wide, "AskPending");
    assert!(ask.contains("Space"));
}

#[test]
fn compact_help_collapses_at_narrow() {
    // Narrow + below must reduce to `Enter:send  Esc:quit`.
    let narrow = compact_help_for(Breakpoint::Narrow, "Normal");
    let compact = compact_help_for(Breakpoint::Compact, "Normal");
    assert_eq!(narrow, compact);
    assert!(narrow.starts_with("Enter:send"));
}

#[test]
fn compact_help_standard_drops_full_shortcut_block() {
    // Standard widens again compared to Narrow but still drops full
    // multi-shortcut help → caller compresses to a single short line.
    let std_n = compact_help_for(Breakpoint::Standard, "Normal");
    assert!(std_n.starts_with("Enter:send"));
    assert!(!std_n.contains("Shift+Enter"));
}
