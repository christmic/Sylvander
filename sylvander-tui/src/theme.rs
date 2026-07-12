//! Configurable semantic theme system.
//!
//! Rendering code requests semantic roles instead of concrete terminal colors.
//! A palette is selected once from `TuiConfig` during process startup.

use ratatui::style::{Color, Modifier, Style};
use std::str::FromStr;
use std::sync::RwLock;

// Public defaults remain available to snapshot/unit tests and downstream code.
pub const BG: Color = Color::Rgb(0x00, 0x00, 0x00);
pub const TEXT: Color = Color::Rgb(0xEC, 0xE7, 0xDE);
pub const TEXT_DIM: Color = Color::Rgb(0x98, 0x9B, 0x9D);
pub const TEXT_MUTED: Color = Color::Rgb(0x66, 0x6C, 0x72);
pub const CORAL: Color = Color::Rgb(0xE8, 0x79, 0x6A);
pub const BRAND_WARM: Color = Color::Rgb(0xF0, 0xBE, 0x72);
pub const BRAND_VIOLET: Color = Color::Rgb(0x9B, 0x72, 0xFF);
pub const BLUE: Color = Color::Rgb(0x75, 0xA7, 0xE8);
pub const TEAL: Color = Color::Rgb(0x72, 0xC7, 0xB1);
pub const AMBER: Color = Color::Rgb(0xD9, 0xAF, 0x62);
pub const RULE: Color = Color::Rgb(0x34, 0x3A, 0x40);
pub const GUIDE: Color = Color::Rgb(0x4A, 0x53, 0x5C);
pub const DANGER: Color = Color::Rgb(0xE0, 0x6C, 0x75);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Sylvander,
    Midnight,
    HighContrast,
}

impl FromStr for ThemeName {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sylvander" | "default" => Ok(Self::Sylvander),
            "midnight" => Ok(Self::Midnight),
            "high-contrast" | "high_contrast" | "contrast" => Ok(Self::HighContrast),
            _ => Err(format!(
                "unknown theme {value:?}; expected sylvander, midnight, or high-contrast"
            )),
        }
    }
}

impl std::fmt::Display for ThemeName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Sylvander => "sylvander",
            Self::Midnight => "midnight",
            Self::HighContrast => "high-contrast",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub canvas: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_muted: Color,
    pub identity: Color,
    pub brand_warm: Color,
    pub brand_violet: Color,
    pub active: Color,
    pub verified: Color,
    pub waiting: Color,
    pub danger: Color,
    pub rule: Color,
    pub guide: Color,
}

pub const SYLVANDER: Palette = Palette {
    canvas: BG,
    text: TEXT,
    text_dim: TEXT_DIM,
    text_muted: TEXT_MUTED,
    identity: CORAL,
    brand_warm: BRAND_WARM,
    brand_violet: BRAND_VIOLET,
    active: BLUE,
    verified: TEAL,
    waiting: AMBER,
    danger: DANGER,
    rule: RULE,
    guide: GUIDE,
};

pub const MIDNIGHT: Palette = Palette {
    canvas: Color::Rgb(0x03, 0x05, 0x08),
    text: Color::Rgb(0xD9, 0xE2, 0xEC),
    text_dim: Color::Rgb(0x8A, 0x99, 0xA8),
    text_muted: Color::Rgb(0x55, 0x64, 0x73),
    identity: Color::Rgb(0xD9, 0x8B, 0x73),
    brand_warm: Color::Rgb(0xE7, 0xB8, 0x6A),
    brand_violet: Color::Rgb(0x86, 0x8B, 0xFF),
    active: Color::Rgb(0x64, 0xB5, 0xF6),
    verified: Color::Rgb(0x67, 0xC5, 0xA0),
    waiting: Color::Rgb(0xD7, 0xA9, 0x55),
    danger: Color::Rgb(0xEF, 0x6B, 0x73),
    rule: Color::Rgb(0x24, 0x2C, 0x35),
    guide: Color::Rgb(0x3B, 0x47, 0x54),
};

pub const HIGH_CONTRAST: Palette = Palette {
    canvas: Color::Black,
    text: Color::White,
    text_dim: Color::Gray,
    text_muted: Color::DarkGray,
    identity: Color::LightRed,
    brand_warm: Color::LightYellow,
    brand_violet: Color::LightMagenta,
    active: Color::LightCyan,
    verified: Color::LightGreen,
    waiting: Color::Yellow,
    danger: Color::LightRed,
    rule: Color::Gray,
    guide: Color::DarkGray,
};

static ACTIVE: RwLock<Palette> = RwLock::new(SYLVANDER);
static ACTIVE_NAME: RwLock<ThemeName> = RwLock::new(ThemeName::Sylvander);

pub fn palette_for(name: ThemeName) -> Palette {
    match name {
        ThemeName::Sylvander => SYLVANDER,
        ThemeName::Midnight => MIDNIGHT,
        ThemeName::HighContrast => HIGH_CONTRAST,
    }
}

pub fn configure(name: ThemeName) {
    *ACTIVE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = palette_for(name);
    *ACTIVE_NAME
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = name;
}

pub fn active_name() -> ThemeName {
    *ACTIVE_NAME
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn palette() -> Palette {
    *ACTIVE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn text() -> Style {
    Style::default().fg(palette().text)
}
pub fn text_on_canvas() -> Style {
    let palette = palette();
    Style::default().fg(palette.text).bg(palette.canvas)
}
pub fn text_dim() -> Style {
    Style::default().fg(palette().text_dim)
}
pub fn text_muted() -> Style {
    Style::default().fg(palette().text_muted)
}
pub fn header() -> Style {
    Style::default()
        .fg(palette().text)
        .add_modifier(Modifier::BOLD)
}
pub fn coral() -> Style {
    Style::default()
        .fg(palette().identity)
        .add_modifier(Modifier::BOLD)
}
pub fn brand_warm() -> Style {
    Style::default().fg(palette().brand_warm)
}
pub fn brand_violet() -> Style {
    Style::default().fg(palette().brand_violet)
}
pub fn brand_wordmark() -> Style {
    Style::default()
        .fg(palette().text)
        .add_modifier(Modifier::BOLD)
}
pub fn brand_tagline() -> Style {
    Style::default()
        .fg(palette().brand_violet)
        .add_modifier(Modifier::ITALIC)
}
pub fn active() -> Style {
    Style::default().fg(palette().active)
}
pub fn active_bold() -> Style {
    Style::default()
        .fg(palette().active)
        .add_modifier(Modifier::BOLD)
}
pub fn verified() -> Style {
    Style::default().fg(palette().verified)
}
pub fn warning() -> Style {
    Style::default().fg(palette().waiting)
}
pub fn danger() -> Style {
    Style::default().fg(palette().danger)
}
pub fn rule() -> Style {
    Style::default().fg(palette().rule)
}
pub fn guide() -> Style {
    Style::default().fg(palette().guide)
}
pub fn focus_box() -> Style {
    Style::default().fg(palette().identity)
}
pub fn composer_idle_border() -> Style {
    Style::default().fg(palette().text_muted)
}
pub fn composer_placeholder() -> Style {
    Style::default().fg(palette().text_dim)
}
pub fn composer_helper() -> Style {
    Style::default()
        .fg(palette().text_muted)
        .add_modifier(Modifier::ITALIC)
}
pub fn thinking_text() -> Style {
    Style::default()
        .fg(palette().text_muted)
        .add_modifier(Modifier::ITALIC)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMode {
    Connecting,
    Idle,
    Working,
    WaitingApproval,
    Asking,
    Disconnected,
}

impl StatusMode {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Connecting | Self::Working => "◌",
            Self::Idle => "\u{00b7}",
            Self::WaitingApproval | Self::Asking => "\u{25cf}",
            Self::Disconnected => "!",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::WaitingApproval => "approval",
            Self::Asking => "asking",
            Self::Disconnected => "disconnected",
        }
    }
    pub fn style(self) -> Style {
        match self {
            Self::Connecting | Self::Working => active(),
            Self::Idle | Self::Asking => text_dim(),
            Self::WaitingApproval | Self::Disconnected => warning(),
        }
    }
}

pub fn tool_status_glyph_and_style(status: crate::app::ToolStatus) -> (&'static str, Style) {
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

pub fn plan_step_glyph_and_style(completed: bool, current: bool) -> (&'static str, Style) {
    if completed {
        ("\u{2713}", verified())
    } else if current {
        ("\u{25cf}", active())
    } else {
        ("\u{25cb}", rule())
    }
}

pub fn user_speaker() -> Style {
    Style::default()
        .fg(palette().text_dim)
        .add_modifier(Modifier::BOLD)
}
pub fn agent_speaker() -> Style {
    Style::default()
        .fg(palette().brand_violet)
        .add_modifier(Modifier::BOLD)
}

pub fn modal_title_coral() -> Style {
    Style::default()
        .fg(palette().identity)
        .add_modifier(Modifier::BOLD)
}
pub fn task_summary_line() -> Style {
    Style::default().fg(palette().identity)
}
pub fn selected() -> Style {
    Style::default().fg(palette().identity)
}
pub fn dimmed() -> Style {
    Style::default().fg(palette().text_muted)
}

pub fn kv_label() -> Style {
    Style::default().fg(palette().text_muted)
}
pub fn kv_value() -> Style {
    Style::default().fg(palette().text)
}

pub fn compact_workspace(path: &std::path::Path, max_chars: usize) -> String {
    let s = path.display().to_string();
    if s.chars().count() <= max_chars {
        return s;
    }
    let basename = path.components().next_back().map_or_else(
        || "~".into(),
        |c| c.as_os_str().to_string_lossy().into_owned(),
    );
    format!(".../{basename}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ToolStatus;

    #[test]
    fn status_mode_glyph_label_style_triple() {
        for m in [
            StatusMode::Connecting,
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
        let p = std::path::PathBuf::from("/Users/christmix/OraculoSpace/Sylvander/sylvander-tui");
        let s = compact_workspace(&p, 25);
        assert!(
            s.starts_with("~/.../") || s.starts_with(".../"),
            "expected abbreviation, got: {s}",
        );
        assert!(s.contains("sylvander-tui"));
        assert!(s.chars().count() < p.display().to_string().chars().count());
    }

    #[test]
    fn built_in_themes_have_distinct_semantic_palettes() {
        assert_ne!(
            palette_for(ThemeName::Sylvander),
            palette_for(ThemeName::Midnight)
        );
        assert_ne!(
            palette_for(ThemeName::Sylvander),
            palette_for(ThemeName::HighContrast)
        );
        assert_eq!(palette_for(ThemeName::Sylvander).canvas, BG);
    }

    #[test]
    fn theme_names_accept_documented_aliases() {
        assert_eq!(
            "default".parse::<ThemeName>().unwrap(),
            ThemeName::Sylvander
        );
        assert_eq!(
            "midnight".parse::<ThemeName>().unwrap(),
            ThemeName::Midnight
        );
        assert_eq!(
            "high-contrast".parse::<ThemeName>().unwrap(),
            ThemeName::HighContrast
        );
        assert!("unknown".parse::<ThemeName>().is_err());
    }
}
