//! Runtime configuration for the TUI.
//!
//! Environment parsing lives here so rendering, application state, and the
//! transport service never read process configuration independently.

use std::path::PathBuf;
use std::time::Duration;

use crate::model::RuntimeMetadata;
use crate::theme::ThemeName;

const DEFAULT_SOCKET: &str = "/tmp/sylvander.sock";

#[derive(Debug, Clone)]
pub struct TuiConfig {
    pub socket_path: PathBuf,
    pub history_path: Option<PathBuf>,
    pub theme: ThemeName,
    pub render_interval: Duration,
    pub animation_interval: Duration,
    pub reconnect_interval: Duration,
    pub mouse_scroll_lines: usize,
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
        let render_fps = env_number("SYLVANDER_TUI_RENDER_FPS", 30, 5, 120)?;
        let animation_ms = env_number("SYLVANDER_TUI_ANIMATION_MS", 200, 50, 2_000)?;
        let reconnect_ms = env_number("SYLVANDER_TUI_RECONNECT_MS", 1_500, 250, 30_000)?;
        let mouse_scroll_lines = env_number("SYLVANDER_TUI_MOUSE_SCROLL_LINES", 4, 1, 40)?;

        Ok(Self {
            socket_path,
            history_path: history_path(),
            theme,
            render_interval: Duration::from_millis(1_000 / render_fps as u64),
            animation_interval: Duration::from_millis(animation_ms as u64),
            reconnect_interval: Duration::from_millis(reconnect_ms as u64),
            mouse_scroll_lines,
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
    fn default_theme_name_is_parseable() {
        assert_eq!(
            "sylvander".parse::<ThemeName>().unwrap(),
            ThemeName::Sylvander
        );
    }
}
