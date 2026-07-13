//! Runtime configuration for the TUI.
//!
//! Environment parsing lives here so rendering, application state, and the
//! transport service never read process configuration independently.

use std::path::PathBuf;
use std::time::Duration;

use crate::input::EditingStyle;
use crate::keymap::KeyMap;
use crate::model::RuntimeMetadata;
use crate::theme::{ColorCapability, ThemeName, ThemeOverrides};

const DEFAULT_SOCKET: &str = "/tmp/sylvander.sock";

#[derive(Debug, Clone)]
pub struct TuiConfig {
    pub socket_path: PathBuf,
    pub history_path: Option<PathBuf>,
    pub theme: ThemeName,
    pub theme_overrides: ThemeOverrides,
    pub color_capability: ColorCapability,
    pub editing_style: EditingStyle,
    pub render_interval: Duration,
    pub animation_interval: Duration,
    pub reconnect_interval: Duration,
    pub mouse_scroll_lines: usize,
    pub reduced_motion: bool,
    pub no_italic: bool,
    pub keymap: KeyMap,
    pub metadata: RuntimeMetadata,
}

impl TuiConfig {
    pub fn from_env_and_args() -> Result<Self, String> {
        let socket_path = std::env::args()
            .nth(1)
            .or_else(|| std::env::var("SYLVANDER_SOCKET").ok())
            .unwrap_or_else(|| DEFAULT_SOCKET.into())
            .into();
        let theme = std::env::var("SYLVANDER_TUI_THEME")
            .unwrap_or_else(|_| "sylvander".into())
            .parse()?;
        let color_capability = resolve_color_capability(
            std::env::var("SYLVANDER_TUI_COLOR").ok().as_deref(),
            std::env::var_os("NO_COLOR").is_some(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var("COLORTERM").ok().as_deref(),
        )?;
        let theme_overrides = ThemeOverrides {
            foreground: env_color("SYLVANDER_TUI_FOREGROUND")?,
            accent: env_color("SYLVANDER_TUI_ACCENT")?,
        };
        crate::theme::validate_palette(
            crate::theme::resolved_palette(theme, color_capability, theme_overrides),
            color_capability,
        )?;
        let editing_style = std::env::var("SYLVANDER_TUI_EDITING")
            .unwrap_or_else(|_| "standard".into())
            .parse()?;
        let render_fps = env_number("SYLVANDER_TUI_RENDER_FPS", 60, 5, 120)?;
        let animation_ms = env_number("SYLVANDER_TUI_ANIMATION_MS", 200, 50, 2_000)?;
        let reconnect_ms = env_number("SYLVANDER_TUI_RECONNECT_MS", 1_500, 250, 30_000)?;
        let mouse_scroll_lines = env_number("SYLVANDER_TUI_MOUSE_SCROLL_LINES", 4, 1, 40)?;
        let reduced_motion = env_bool("SYLVANDER_TUI_REDUCED_MOTION", false)?;
        let no_italic = env_bool("SYLVANDER_TUI_NO_ITALIC", false)?;
        let keymap = KeyMap::from_environment()?;

        Ok(Self {
            socket_path,
            history_path: history_path(),
            theme,
            theme_overrides,
            color_capability,
            editing_style,
            render_interval: Duration::from_millis(1_000 / render_fps as u64),
            animation_interval: Duration::from_millis(animation_ms as u64),
            reconnect_interval: Duration::from_millis(reconnect_ms as u64),
            mouse_scroll_lines,
            reduced_motion,
            no_italic,
            keymap,
            metadata: RuntimeMetadata {
                model: std::env::var("SYLVANDER_MODEL").unwrap_or_else(|_| "—".into()),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
                models: Vec::new(),
                permissions: sylvander_protocol::PermissionProfile::default(),
                workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("~")),
                branch: git_branch(),
                capabilities: 0,
                approval_enabled: false,
                max_attachment_bytes: 512 * 1024,
            },
        })
    }

    pub fn report(&self, metadata: &RuntimeMetadata) -> String {
        format!(
            "theme       {}\nforeground  {}\naccent      {}\ncolors      {}\nediting     {}\nsocket      {}\nhistory     {}\nworkspace   {}\nmodel       {}\nrender      {} ms\nanimation   {}\nitalics     {}\nreconnect   {} ms\nmouse wheel {} lines\nkeys        {}\nattachment  {} bytes",
            self.theme,
            self.theme_overrides.describe_foreground(),
            self.theme_overrides.describe_accent(),
            self.color_capability,
            self.editing_style,
            self.socket_path.display(),
            self.history_path
                .as_deref()
                .map_or_else(|| "disabled".into(), |path| path.display().to_string()),
            metadata.workspace.display(),
            metadata.model_label(),
            self.render_interval.as_millis(),
            if self.reduced_motion {
                "reduced".into()
            } else {
                format!("{} ms", self.animation_interval.as_millis())
            },
            if self.no_italic {
                "disabled"
            } else {
                "enabled"
            },
            self.reconnect_interval.as_millis(),
            self.mouse_scroll_lines,
            self.keymap.summary(),
            metadata.max_attachment_bytes,
        )
    }
}

fn env_color(name: &str) -> Result<Option<ratatui::style::Color>, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| crate::theme::parse_color(&value))
        .transpose()
}

fn resolve_color_capability(
    configured: Option<&str>,
    no_color: bool,
    term: Option<&str>,
    color_term: Option<&str>,
) -> Result<ColorCapability, String> {
    let configured = configured.map(str::trim);
    match configured {
        None | Some("") => Ok(crate::theme::detect_color_capability(
            no_color, term, color_term,
        )),
        Some(value) if value.eq_ignore_ascii_case("auto") => Ok(
            crate::theme::detect_color_capability(no_color, term, color_term),
        ),
        Some(value) => value.parse(),
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool, String> {
    parse_bool(name, std::env::var(name).ok().as_deref(), default)
}

fn parse_bool(name: &str, raw: Option<&str>, default: bool) -> Result<bool, String> {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None => Ok(default),
        Some("1" | "true" | "yes" | "on") => Ok(true),
        Some("0" | "false" | "no" | "off") => Ok(false),
        Some(value) => Err(format!(
            "{name} must be true/false, yes/no, on/off, or 1/0; got {value:?}"
        )),
    }
}

fn git_branch() -> String {
    std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|branch| branch.trim().chars().take(40).collect::<String>())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "—".into())
}

fn env_number(name: &str, default: usize, min: usize, max: usize) -> Result<usize, String> {
    let raw = std::env::var(name).ok();
    parse_number(name, raw.as_deref(), default, min, max)
}

fn parse_number(
    name: &str,
    raw: Option<&str>,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, String> {
    let value = raw
        .map(|raw| {
            raw.parse::<usize>()
                .map_err(|_| format!("{name} must be an integer, got {raw:?}"))
        })
        .transpose()?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(format!(
            "{name} must be between {min} and {max}, got {value}"
        ));
    }
    Ok(value)
}

fn history_path() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("SYLVANDER_HISTORY_PATH") {
        return (!value.is_empty()).then(|| value.into());
    }
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".cache"))
        })?;
    Some(base.join("sylvander-tui").join("history.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_config_rejects_values_outside_bounds() {
        assert_eq!(parse_number("FPS", None, 30, 5, 120).unwrap(), 30);
        assert_eq!(parse_number("FPS", Some("60"), 30, 5, 120).unwrap(), 60);
        assert!(parse_number("FPS", Some("4"), 30, 5, 120).is_err());
        assert!(parse_number("FPS", Some("fast"), 30, 5, 120).is_err());
    }

    #[test]
    fn accessibility_flags_parse_strictly() {
        assert!(parse_bool("MOTION", Some("yes"), false).unwrap());
        assert!(!parse_bool("ITALIC", Some("0"), true).unwrap());
        assert!(parse_bool("MOTION", Some("sometimes"), false).is_err());
    }

    #[test]
    fn default_theme_name_is_parseable() {
        assert_eq!(
            "sylvander".parse::<ThemeName>().unwrap(),
            ThemeName::Sylvander
        );
    }

    #[test]
    fn report_uses_resolved_values_without_reading_environment_again() {
        let config = TuiConfig {
            socket_path: "/tmp/test.sock".into(),
            history_path: None,
            theme: ThemeName::Midnight,
            theme_overrides: ThemeOverrides {
                foreground: Some(ratatui::style::Color::Rgb(0xEE, 0xEE, 0xEE)),
                accent: Some(ratatui::style::Color::Rgb(0xAA, 0x77, 0xFF)),
            },
            color_capability: ColorCapability::TrueColor,
            editing_style: EditingStyle::Vim,
            render_interval: Duration::from_millis(33),
            animation_interval: Duration::from_millis(200),
            reconnect_interval: Duration::from_millis(1_500),
            mouse_scroll_lines: 4,
            reduced_motion: true,
            no_italic: true,
            keymap: KeyMap::default(),
            metadata: RuntimeMetadata::default(),
        };
        let report = config.report(&config.metadata);
        assert!(report.contains("theme       midnight"));
        assert!(report.contains("foreground  #EEEEEE"));
        assert!(report.contains("accent      #AA77FF"));
        assert!(report.contains("colors      truecolor"));
        assert!(report.contains("editing     vim"));
        assert!(report.contains("history     disabled"));
        assert!(report.contains("socket      /tmp/test.sock"));
        assert!(report.contains("sessions=Ctrl+P"));
        assert!(report.contains("animation   reduced"));
        assert!(report.contains("italics     disabled"));
    }

    #[test]
    fn color_capability_prefers_explicit_setting_and_honors_no_color() {
        assert_eq!(
            resolve_color_capability(Some("ansi256"), true, Some("dumb"), None).unwrap(),
            ColorCapability::Ansi256
        );
        assert_eq!(
            resolve_color_capability(None, true, Some("xterm-256color"), None).unwrap(),
            ColorCapability::Monochrome
        );
        assert_eq!(
            resolve_color_capability(Some("AUTO"), false, Some("xterm-256color"), None).unwrap(),
            ColorCapability::Ansi256
        );
        assert!(resolve_color_capability(Some("millions"), false, None, None).is_err());
    }
}
