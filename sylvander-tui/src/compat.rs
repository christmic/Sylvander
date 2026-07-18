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
#[path = "../tests/unit/compat.rs"]
mod tests;
