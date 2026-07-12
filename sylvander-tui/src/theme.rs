//! Design tokens — verbatim from `docs/design/02-tui-immersive.svg`.
//!
//! Every color and state-derived glyph / colour pair used by the TUI
//! lives here. Panels and modals import from this module — they do
//! not reach into ratatui's `Color::*` enum directly, so a palette
//! swap requires editing only this file.

use ratatui::style::{Color, Modifier, Style};

// Palette constants — derived from the 02-tui-immersive.svg <style>.

pub const BG: Color = Color::Rgb(0x11, 0x13, 0x15);
pub const TEXT: Color = Color::Rgb(0xEC, 0xE7, 0xDE);
pub const TEXT_DIM: Color = Color::Rgb(0x98, 0x9B, 0x9D);
pub const TEXT_MUTED: Color = Color::Rgb(0x66, 0x6C, 0x72);
pub const CORAL: Color = Color::Rgb(0xE8, 0x79, 0x6A);
pub const BLUE: Color = Color::Rgb(0x75, 0xA7, 0xE8);
pub const TEAL: Color = Color::Rgb(0x72, 0xC7, 0xB1);
pub const AMBER: Color = Color::Rgb(0xD9, 0xAF, 0x62);
pub const RULE: Color = Color::Rgb(0x34, 0x3A, 0x40);
pub const GUIDE: Color = Color::Rgb(0x4A, 0x53, 0x5C);

pub fn text() -> Style { Style::default().fg(TEXT) }
pub fn text_on_canvas() -> Style { Style::default().fg(TEXT).bg(BG) }
pub fn text_dim() -> Style { Style::default().fg(TEXT_DIM) }
pub fn text_muted() -> Style { Style::default().fg(TEXT_MUTED) }
pub fn header() -> Style { Style::default().fg(TEXT).add_modifier(Modifier::BOLD) }
pub fn coral() -> Style { Style::default().fg(CORAL).add_modifier(Modifier::BOLD) }
pub fn active() -> Style { Style::default().fg(BLUE) }
pub fn active_bold() -> Style { Style::default().fg(BLUE).add_modifier(Modifier::BOLD) }
pub fn verified() -> Style { Style::default().fg(TEAL) }
pub fn warning() -> Style { Style::default().fg(AMBER) }
pub fn rule() -> Style { Style::default().fg(RULE) }
pub fn guide() -> Style { Style::default().fg(GUIDE) }
pub fn focus_box() -> Style { Style::default().fg(CORAL) }
pub fn composer_idle_border() -> Style { Style::default().fg(TEXT_MUTED) }
pub fn composer_placeholder() -> Style { Style::default().fg(TEXT_DIM) }
pub fn composer_helper() -> Style { Style::default().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC) }
pub fn thinking_text() -> Style { Style::default().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC) }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMode {
    Idle,
    Working,
    WaitingApproval,
    Asking,
    Disconnected,
}

impl StatusMode {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Idle => "\u{00b7}",
            Self::Working => "\u{25cc}",
            Self::WaitingApproval => "\u{25cf}",
            Self::Asking => "\u{25cf}",
            Self::Disconnected => "!",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::WaitingApproval => "approval",
            Self::Asking => "asking",
            Self::Disconnected => "disconnected",
        }
    }
    pub fn style(self) -> Style {
        match self {
            Self::Idle | Self::Asking => text_dim(),
            Self::Working => active(),
            Self::WaitingApproval | Self::Disconnected => warning(),
        }
    }
}

pub fn tool_status_glyph_and_style(
    status: crate::app::ToolStatus,
) -> (&'static str, Style) {
    use crate::app::ToolStatus;
    match status {
        ToolStatus::Pending => ("\u{25cc}", active()),
        ToolStatus::Done => ("\u{2713}", verified()),
        ToolStatus::Error => ("\u{2717}", warning()),
    }
}

pub fn tool_status_glyph(status: crate::app::ToolStatus) -> &'static str {
    tool_status_glyph_and_style(status).0
}

pub fn tool_status_style(status: crate::app::ToolStatus) -> Style {
    tool_status_glyph_and_style(status).1
}

pub fn plan_step_glyph_and_style(
    completed: bool,
    current: bool,
) -> (&'static str, Style) {
    if completed { ("\u{2713}", verified()) }
    else if current { ("\u{25cf}", active()) }
    else { ("\u{25cb}", rule()) }
}

pub fn user_speaker() -> Style {
    Style::default().fg(TEXT_DIM).add_modifier(Modifier::BOLD)
}
pub fn agent_speaker() -> Style {
    Style::default().fg(CORAL).add_modifier(Modifier::BOLD)
}

pub fn modal_title_coral() -> Style {
    Style::default().fg(CORAL).add_modifier(Modifier::BOLD)
}
pub fn task_summary_line() -> Style { Style::default().fg(CORAL) }
pub fn selected() -> Style { Style::default().fg(CORAL) }
pub fn dimmed() -> Style { Style::default().fg(TEXT_MUTED) }

pub fn kv_label() -> Style { Style::default().fg(TEXT_MUTED) }
pub fn kv_value() -> Style { Style::default().fg(TEXT) }

pub fn compact_workspace(path: &std::path::Path, max_chars: usize) -> String {
    let s = path.display().to_string();
    if s.chars().count() <= max_chars {
        return s;
    }
    let basename = path
        .components()
        .next_back()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "~".into());
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        if let Ok(rest) = path.strip_prefix(&home) {
            let rest_str = rest.display().to_string();
            if !rest_str.is_empty() && rest_str != "." {
                return format!("~/.../{basename}");
            }
        }
    }
    format!(".../{basename}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ToolStatus;

    #[test]
    fn status_mode_glyph_label_style_triple() {
        for m in [
            StatusMode::Idle,
            StatusMode::Working,
            StatusMode::WaitingApproval,
            StatusMode::Asking,
            StatusMode::Disconnected,
        ] {
            let _ = m.glyph();
            let _ = m.label();
            let _ = m.style();
        }
        assert_eq!(StatusMode::Working.glyph(), "\u{25cc}");
        assert_eq!(StatusMode::Working.label(), "working");
        assert_eq!(StatusMode::Disconnected.glyph(), "!");
        assert_eq!(StatusMode::WaitingApproval.glyph(), "\u{25cf}");
        assert_eq!(StatusMode::Asking.label(), "asking");
    }

    #[test]
    fn tool_status_glyph_three_distinct_styles() {
        let (g_pending, s_pending) = tool_status_glyph_and_style(ToolStatus::Pending);
        let (g_done, s_done) = tool_status_glyph_and_style(ToolStatus::Done);
        let (g_err, s_err) = tool_status_glyph_and_style(ToolStatus::Error);
        assert_eq!(g_pending, "\u{25cc}");
        assert_eq!(g_done, "\u{2713}");
        assert_eq!(g_err, "\u{2717}");
        assert_ne!(s_pending.fg, s_done.fg);
        assert_ne!(s_pending.fg, s_err.fg);
        assert_ne!(s_done.fg, s_err.fg);
    }

    #[test]
    fn plan_step_three_states_distinct_glyphs() {
        let (g, s) = plan_step_glyph_and_style(true, false);
        assert_eq!(g, "\u{2713}");
        assert_eq!(s.fg, Some(TEAL));
        let (g, s) = plan_step_glyph_and_style(false, true);
        assert_eq!(g, "\u{25cf}");
        assert_eq!(s.fg, Some(BLUE));
        let (g, s) = plan_step_glyph_and_style(false, false);
        assert_eq!(g, "\u{25cb}");
        assert_eq!(s.fg, Some(RULE));
    }

    #[test]
    fn compact_workspace_short_path_passes_through() {
        let p = std::path::PathBuf::from("/Users/christmix");
        assert_eq!(compact_workspace(&p, 60), "/Users/christmix");
    }

    #[test]
    fn compact_workspace_long_path_collapses() {
        let p = std::path::PathBuf::from(
            "/Users/christmix/OraculoSpace/Sylvander/sylvander-tui",
        );
        let s = compact_workspace(&p, 25);
        assert!(
            s.starts_with("~/.../") || s.starts_with(".../"),
            "expected abbreviation, got: {s}",
        );
        assert!(s.contains("sylvander-tui"));
        assert!(s.chars().count() < p.display().to_string().chars().count());
    }
}
