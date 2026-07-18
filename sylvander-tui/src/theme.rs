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
pub enum ColorCapability {
    Monochrome,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl FromStr for ColorCapability {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "mono" | "monochrome" => Ok(Self::Monochrome),
            "16" | "ansi" | "ansi16" => Ok(Self::Ansi16),
            "256" | "ansi256" => Ok(Self::Ansi256),
            "truecolor" | "24bit" | "24-bit" => Ok(Self::TrueColor),
            _ => Err(format!(
                "unknown color capability {value:?}; expected auto, none, ansi16, ansi256, or truecolor"
            )),
        }
    }
}

impl std::fmt::Display for ColorCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Monochrome => "monochrome",
            Self::Ansi16 => "ansi16",
            Self::Ansi256 => "ansi256",
            Self::TrueColor => "truecolor",
        })
    }
}

pub fn detect_color_capability(
    no_color: bool,
    term: Option<&str>,
    color_term: Option<&str>,
) -> ColorCapability {
    let term = term.unwrap_or_default().to_ascii_lowercase();
    let color_term = color_term.unwrap_or_default().to_ascii_lowercase();
    if no_color || term == "dumb" {
        ColorCapability::Monochrome
    } else if matches!(color_term.as_str(), "truecolor" | "24bit")
        || term.contains("truecolor")
        || term.contains("24bit")
        || term.contains("direct")
    {
        ColorCapability::TrueColor
    } else if term.contains("256color") {
        ColorCapability::Ansi256
    } else {
        ColorCapability::Ansi16
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThemeOverrides {
    pub foreground: Option<Color>,
    pub accent: Option<Color>,
}

impl ThemeOverrides {
    pub fn describe_foreground(self) -> String {
        self.foreground.map_or_else(|| "theme".into(), color_label)
    }

    pub fn describe_accent(self) -> String {
        self.accent.map_or_else(|| "theme".into(), color_label)
    }
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

const MONOCHROME: Palette = Palette {
    canvas: Color::Black,
    text: Color::White,
    text_dim: Color::Gray,
    text_muted: Color::Gray,
    identity: Color::White,
    brand_warm: Color::White,
    brand_violet: Color::White,
    active: Color::White,
    verified: Color::White,
    waiting: Color::White,
    danger: Color::White,
    rule: Color::DarkGray,
    guide: Color::Gray,
};

static ACTIVE: RwLock<Palette> = RwLock::new(SYLVANDER);
static ACTIVE_NAME: RwLock<ThemeName> = RwLock::new(ThemeName::Sylvander);
static ACTIVE_CAPABILITY: RwLock<ColorCapability> = RwLock::new(ColorCapability::TrueColor);
static ACTIVE_OVERRIDES: RwLock<ThemeOverrides> = RwLock::new(ThemeOverrides {
    foreground: None,
    accent: None,
});
static ACCESSIBILITY: RwLock<Accessibility> = RwLock::new(Accessibility {
    reduced_motion: false,
    no_italic: false,
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Accessibility {
    reduced_motion: bool,
    no_italic: bool,
}

pub fn configure_accessibility(reduced_motion: bool, no_italic: bool) {
    *ACCESSIBILITY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Accessibility {
        reduced_motion,
        no_italic,
    };
}

fn accessibility() -> Accessibility {
    *ACCESSIBILITY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn emphasis(style: Style) -> Style {
    emphasis_for(style, accessibility().no_italic)
}

fn emphasis_for(style: Style, no_italic: bool) -> Style {
    if no_italic {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        style.add_modifier(Modifier::ITALIC)
    }
}

pub fn subtle_emphasis(style: Style) -> Style {
    subtle_emphasis_for(style, accessibility().no_italic)
}

fn subtle_emphasis_for(style: Style, no_italic: bool) -> Style {
    if no_italic {
        style.add_modifier(Modifier::DIM)
    } else {
        style.add_modifier(Modifier::ITALIC)
    }
}

pub fn cursor() -> Style {
    cursor_for(accessibility().reduced_motion)
}

fn cursor_for(reduced_motion: bool) -> Style {
    if reduced_motion {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::SLOW_BLINK)
    }
}

pub fn palette_for(name: ThemeName) -> Palette {
    match name {
        ThemeName::Sylvander => SYLVANDER,
        ThemeName::Midnight => MIDNIGHT,
        ThemeName::HighContrast => HIGH_CONTRAST,
    }
}

pub fn palette_for_capability(name: ThemeName, capability: ColorCapability) -> Palette {
    match capability {
        ColorCapability::Monochrome => MONOCHROME,
        ColorCapability::Ansi16 => HIGH_CONTRAST,
        ColorCapability::Ansi256 => map_palette(palette_for(name), rgb_to_ansi256),
        ColorCapability::TrueColor => palette_for(name),
    }
}

pub fn resolved_palette(
    name: ThemeName,
    capability: ColorCapability,
    overrides: ThemeOverrides,
) -> Palette {
    let mut palette = palette_for_capability(name, capability);
    if let Some(foreground) = overrides.foreground {
        palette.text = map_override(foreground, capability);
    }
    if let Some(accent) = overrides.accent {
        let accent = map_override(accent, capability);
        palette.identity = accent;
        palette.brand_violet = accent;
        palette.active = accent;
    }
    palette
}

pub fn parse_color(value: &str) -> Result<Color, String> {
    let value = value.trim();
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid color {value:?}; expected six-digit RGB such as #9B72FF"
        ));
    }
    let component = |range| {
        u8::from_str_radix(&hex[range], 16)
            .map_err(|_| format!("invalid RGB component in {value:?}"))
    };
    Ok(Color::Rgb(
        component(0..2)?,
        component(2..4)?,
        component(4..6)?,
    ))
}

fn color_label(color: Color) -> String {
    match color {
        Color::Rgb(red, green, blue) => format!("#{red:02X}{green:02X}{blue:02X}"),
        other => format!("{other:?}"),
    }
}

fn map_override(color: Color, capability: ColorCapability) -> Color {
    match capability {
        ColorCapability::Monochrome => Color::White,
        ColorCapability::Ansi16 => rgb_to_ansi16(color),
        ColorCapability::Ansi256 => rgb_to_ansi256(color),
        ColorCapability::TrueColor => color,
    }
}

fn map_palette(palette: Palette, map: impl Fn(Color) -> Color) -> Palette {
    Palette {
        canvas: map(palette.canvas),
        text: map(palette.text),
        text_dim: map(palette.text_dim),
        text_muted: map(palette.text_muted),
        identity: map(palette.identity),
        brand_warm: map(palette.brand_warm),
        brand_violet: map(palette.brand_violet),
        active: map(palette.active),
        verified: map(palette.verified),
        waiting: map(palette.waiting),
        danger: map(palette.danger),
        rule: map(palette.rule),
        guide: map(palette.guide),
    }
}

fn rgb_to_ansi256(color: Color) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    let level = |channel: u8| ((u16::from(channel) * 5 + 127) / 255) as u8;
    Color::Indexed(16 + 36 * level(red) + 6 * level(green) + level(blue))
}

fn rgb_to_ansi16(color: Color) -> Color {
    let Some(rgb) = color_rgb(color) else {
        return color;
    };
    const CANDIDATES: [Color; 16] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    CANDIDATES
        .into_iter()
        .min_by_key(|candidate| {
            let candidate = color_rgb(*candidate).unwrap_or_default();
            let delta = |left: u8, right: u8| i32::from(left) - i32::from(right);
            delta(rgb.0, candidate.0).pow(2)
                + delta(rgb.1, candidate.1).pow(2)
                + delta(rgb.2, candidate.2).pow(2)
        })
        .unwrap_or(color)
}

pub fn validate_palette(palette: Palette, capability: ColorCapability) -> Result<(), String> {
    if palette.text == palette.canvas {
        return Err("theme text and canvas must differ".into());
    }
    if capability == ColorCapability::Monochrome {
        return Ok(());
    }
    for (role, color, minimum) in [
        ("text", palette.text, 7.0),
        ("text_dim", palette.text_dim, 4.0),
        ("text_muted", palette.text_muted, 3.0),
        ("identity", palette.identity, 3.0),
        ("active", palette.active, 3.0),
        ("verified", palette.verified, 3.0),
        ("waiting", palette.waiting, 3.0),
        ("danger", palette.danger, 3.0),
    ] {
        let ratio = contrast_ratio(color, palette.canvas)
            .ok_or_else(|| format!("cannot validate terminal color for semantic role {role}"))?;
        if ratio < minimum {
            return Err(format!(
                "semantic role {role} has contrast {ratio:.2}:1; requires {minimum:.1}:1"
            ));
        }
    }
    Ok(())
}

fn contrast_ratio(foreground: Color, background: Color) -> Option<f64> {
    let foreground = relative_luminance(color_rgb(foreground)?);
    let background = relative_luminance(color_rgb(background)?);
    Some((foreground.max(background) + 0.05) / (foreground.min(background) + 0.05))
}

fn relative_luminance((red, green, blue): (u8, u8, u8)) -> f64 {
    let linear = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
}

fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((128, 0, 0)),
        Color::Green => Some((0, 128, 0)),
        Color::Yellow => Some((128, 128, 0)),
        Color::Blue => Some((0, 0, 128)),
        Color::Magenta => Some((128, 0, 128)),
        Color::Cyan => Some((0, 128, 128)),
        Color::Gray => Some((192, 192, 192)),
        Color::DarkGray => Some((128, 128, 128)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((0, 0, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        Color::Indexed(index) if index >= 16 => {
            let index = index - 16;
            let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            Some((
                channel(index / 36),
                channel(index % 36 / 6),
                channel(index % 6),
            ))
        }
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        _ => None,
    }
}

pub fn configure(name: ThemeName) {
    let capability = active_color_capability();
    let overrides = *ACTIVE_OVERRIDES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *ACTIVE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        resolved_palette(name, capability, overrides);
    *ACTIVE_NAME
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = name;
}

pub fn configure_overrides(overrides: ThemeOverrides) {
    *ACTIVE_OVERRIDES
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = overrides;
}

pub fn configure_color_capability(capability: ColorCapability) {
    *ACTIVE_CAPABILITY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = capability;
}

pub fn active_color_capability() -> ColorCapability {
    *ACTIVE_CAPABILITY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    subtle_emphasis(Style::default().fg(palette().brand_violet))
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
    subtle_emphasis(Style::default().fg(palette().text_muted))
}
pub fn thinking_text() -> Style {
    subtle_emphasis(Style::default().fg(palette().text_muted))
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
#[path = "../tests/unit/theme.rs"]
mod tests;
