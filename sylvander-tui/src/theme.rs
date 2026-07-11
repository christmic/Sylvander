//! Design tokens — verbatim from `docs/design/02-tui-immersive.svg`.
//!
//! The SVG `<style>` block is the ground truth; the prose in
//! `docs/sylvander-tui-ux-design.md` is commentary. Every color used
//! anywhere in the TUI must come through this module so we can do a
//! palette swap without touching rendering code.
//!
//! Palette intent (UX §2.1):
//! - Warm-neutral canvas instead of pure black, soft ivory text.
//! - Coral accent reserved for identity + selection — never decorative.
//! - Teal for verified success; blue for active work; amber for waiting.

use ratatui::style::{Color, Modifier, Style};

/// Warm-neutral dark canvas. Replaces ratatui's pure-black default.
pub const BG: Color = Color::Rgb(0x11, 0x13, 0x15);

/// Soft ivory primary text. Replaces raw `Color::White`.
pub const TEXT: Color = Color::Rgb(0xEC, 0xE7, 0xDE);

/// Secondary metadata — dimmer than primary, lighter than tertiary.
pub const TEXT_DIM: Color = Color::Rgb(0x98, 0x9B, 0x9D);

/// Tertiary muted — help-line, sub-labels, very quiet.
pub const TEXT_MUTED: Color = Color::Rgb(0x66, 0x6C, 0x72);

/// Coral — Sylvander identity, plan-marker `◐`, focus stroke.
/// Per §2.1: "Coral accent for identity and selection, used sparingly."
pub const CORAL: Color = Color::Rgb(0xE8, 0x79, 0x6A);

/// Blue — active work in progress, the live `●` marker.
pub const BLUE: Color = Color::Rgb(0x75, 0xA7, 0xE8);

/// Teal — verified success, the completed `✓` marker.
pub const TEAL: Color = Color::Rgb(0x72, 0xC7, 0xB1);

/// Amber — warning, waiting on user decision, draft preserved.
pub const AMBER: Color = Color::Rgb(0xD9, 0xAF, 0x62);

/// Hairline separator between regions. Slightly warmer than the canvas
/// to be visible without competing with primary text.
pub const RULE: Color = Color::Rgb(0x34, 0x3A, 0x40);

/// Thin vertical guide for grouped operations (tool rhythm step header
/// → child tools). Quiet enough that it disappears when not focused.
pub const GUIDE: Color = Color::Rgb(0x4A, 0x53, 0x5C);

/// Composer focus accent — 8% alpha coral fill + coral stroke.
/// Ratatui `Rgb` is 0..=255; true alpha requires the lower 24 bits to
/// encode the RGB and the upper byte to encode alpha. Since the wire
/// format is `0xAARRGGBB`, an 8% fill (≈0x14 alpha) is `0x14E8796A`.
pub const FOCUS_FILL_FG: Color = CORAL;
pub const FOCUS_STROKE: Color = CORAL;

// ===========================================================================
// Style helpers — typed wrappers so call sites are short and theme changes
// don't ripple through every panel.
// ===========================================================================

/// Body text. Hard ivory.
pub fn text() -> Style {
    Style::default().fg(TEXT)
}

/// Secondary metadata. Dimmer.
pub fn text_dim() -> Style {
    Style::default().fg(TEXT_DIM)
}

/// Tertiary muted. Very quiet — used for help-line, sub-labels.
pub fn text_muted() -> Style {
    Style::default().fg(TEXT_MUTED)
}

/// Bold ivory header (UX §5.1). Used for screen titles, "Proposed plan".
pub fn header() -> Style {
    Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
}

/// Coral brand mark — `◖S◗`, the agent label `ORACULO`, focus stroke.
pub fn coral() -> Style {
    Style::default().fg(CORAL).add_modifier(Modifier::BOLD)
}

/// Active in-progress. Blue. Live `●` marker.
pub fn active() -> Style {
    Style::default().fg(BLUE)
}

/// Verified success. Teal. `✓` marker.
pub fn verified() -> Style {
    Style::default().fg(TEAL)
}

/// Waiting for user / amber warning. `●` waiting for approval marker.
pub fn warning() -> Style {
    Style::default().fg(AMBER)
}

/// Hairline rule between regions. Renders as `─` characters, not widget
/// borders, so the line stays aligned with the canvas.
pub fn rule() -> Style {
    Style::default().fg(RULE)
}

/// Vertical guide for grouped operations. Rendered as `│` between a
/// step header and its children.
pub fn guide() -> Style {
    Style::default().fg(GUIDE)
}

/// Composer focus accent. Style for the bordered Box around the
/// composer when it owns focus.
pub fn focus_box() -> Style {
    Style::default().fg(CORAL)
}

/// Composer placeholder text, shown when the buffer is empty.
pub fn composer_placeholder() -> Style {
    Style::default().fg(TEXT_DIM)
}

/// Composer helper text — line directly below the input rows, the
/// "Type while I work — steer, queue, or interrupt" line per `18`.
pub fn composer_helper() -> Style {
    Style::default().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC)
}
