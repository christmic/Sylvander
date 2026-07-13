//! Terminal compatibility + responsive breakpoints (UX §13 + §32).
//!
//! Each panel/modal consults `Breakpoint::from_width(cols)` at render
//! time to decide what to drop or collapse. Per design:
//!
//! - **Wide**     ≥ 100 cols : full metadata + descriptions
//! - **Standard** 80–99 cols  : compact metadata + tool summaries
//! - **Narrow**   50–79 cols  : single-column regions, minimal status
//! - **Compact**  <  50 cols  : the same core UI, aggressively reflowed

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    Wide,
    Standard,
    Narrow,
    Compact,
}

impl Breakpoint {
    pub const fn from_width(cols: u16) -> Self {
        if cols >= 100 {
            Self::Wide
        } else if cols >= 80 {
            Self::Standard
        } else if cols >= 50 {
            Self::Narrow
        } else {
            Self::Compact
        }
    }

    /// Minimum height the layout requires to render its primary surface.
    pub const fn min_rows(&self) -> u16 {
        match self {
            Self::Wide | Self::Standard => 12,
            Self::Narrow => 14,
            Self::Compact => 1,
        }
    }

    /// Whether secondary metadata (e.g. "model · connected" in the
    /// status panel) should be rendered. Dropped at Narrow + below.
    pub const fn shows_secondary_meta(&self) -> bool {
        matches!(self, Self::Wide | Self::Standard)
    }

    /// Whether the help-bar can show full shortcuts. At Narrow + below
    /// the help is condensed to "esc cancel · enter send".
    pub const fn shows_full_help(&self) -> bool {
        matches!(self, Self::Wide)
    }

    /// Whether the modal popup should center with a 60% width vs 80%.
    pub const fn modal_width_pct(&self) -> u16 {
        match self {
            Self::Wide => 60,
            Self::Standard => 75,
            Self::Narrow => 90,
            Self::Compact => 100,
        }
    }

    /// Whether the composer panel can afford the attachment-token row
    /// above the input. Drops at Narrow + below to save vertical lines.
    pub const fn composer_attachment_strip(&self) -> bool {
        matches!(self, Self::Wide | Self::Standard)
    }
}

/// Small help text per breakpoint — used by the Help panel when the
/// standard hint line would overflow.
pub fn compact_help_for(breakpoint: Breakpoint, mode_label: &str) -> &'static str {
    if breakpoint.shows_full_help() {
        match mode_label {
            "Normal" => {
                "Enter:send  Shift+Enter:newline  Esc:quit  Ctrl+C:quit  Ctrl+P:sessions  /:command"
            }
            "ApprovalPending" => "y:approve  n:reject  Y:all  N:reject-all  esc:cancel",
            "AskPending" => "Enter:submit  Space:toggle  Esc:cancel",
            _ => "Enter:send  Esc:quit",
        }
    } else {
        "Enter:send  Esc:quit"
    }
}

#[cfg(test)]
mod tests {
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
}
