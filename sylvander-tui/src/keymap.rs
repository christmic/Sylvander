//! Configurable global key bindings.
//!
//! Text editing, decision-modal keys, and emergency interrupt keys stay fixed;
//! this map covers global navigation surfaces that are safe to remap.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Sessions,
    ToolDetails,
    Commands,
    TranscriptPageUp,
    TranscriptPageDown,
    ReturnLive,
}

impl KeyAction {
    const ALL: [Self; 6] = [
        Self::Sessions,
        Self::ToolDetails,
        Self::Commands,
        Self::TranscriptPageUp,
        Self::TranscriptPageDown,
        Self::ReturnLive,
    ];

    fn config_name(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::ToolDetails => "tool_details",
            Self::Commands => "commands",
            Self::TranscriptPageUp => "transcript_page_up",
            Self::TranscriptPageDown => "transcript_page_down",
            Self::ReturnLive => "return_live",
        }
    }

    fn env_name(self) -> String {
        format!("SYLVANDER_TUI_KEY_{}", self.config_name().to_uppercase())
    }

    fn default_chord(self) -> &'static str {
        match self {
            Self::Sessions => "ctrl+p",
            Self::ToolDetails => "ctrl+o",
            Self::Commands => "ctrl+k",
            Self::TranscriptPageUp => "pageup",
            Self::TranscriptPageDown => "pagedown",
            Self::ReturnLive => "ctrl+end",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyChord {
    code: KeyCode,
    modifiers: KeyModifiers,
    label: String,
}

impl KeyChord {
    fn parse(raw: &str) -> Result<Self, String> {
        let parts = raw
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let (key, modifiers) = parts
            .split_last()
            .ok_or_else(|| "key binding cannot be empty".to_string())?;
        let mut parsed_modifiers = KeyModifiers::NONE;
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => parsed_modifiers.insert(KeyModifiers::CONTROL),
                "alt" | "option" => parsed_modifiers.insert(KeyModifiers::ALT),
                "shift" => parsed_modifiers.insert(KeyModifiers::SHIFT),
                other => return Err(format!("unknown key modifier {other:?}")),
            }
        }
        let normalized = key.to_ascii_lowercase();
        let code = match normalized.as_str() {
            "pageup" | "page_up" => KeyCode::PageUp,
            "pagedown" | "page_down" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "tab" => KeyCode::Tab,
            "enter" => KeyCode::Enter,
            value if value.chars().count() == 1 => KeyCode::Char(value.chars().next().unwrap()),
            _ => return Err(format!("unknown key {key:?}")),
        };
        if matches!(code, KeyCode::Enter) {
            return Err("Enter is reserved for Composer submission and newlines".into());
        }
        if matches!(code, KeyCode::Char(_))
            && !parsed_modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Err("global printable bindings require ctrl or alt".into());
        }
        if parsed_modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('c' | 'x' | 'z'))
        {
            return Err("Ctrl+C, Ctrl+X, and Ctrl+Z are reserved safety/editing keys".into());
        }
        Ok(Self {
            label: chord_label(code, parsed_modifiers),
            code,
            modifiers: parsed_modifiers,
        })
    }

    fn matches(&self, event: &KeyEvent) -> bool {
        let same_code = match (&self.code, &event.code) {
            (KeyCode::Char(expected), KeyCode::Char(actual)) => {
                expected.eq_ignore_ascii_case(actual)
            }
            _ => self.code == event.code,
        };
        same_code && self.modifiers == event.modifiers
    }
}

#[derive(Debug, Clone)]
pub struct KeyMap {
    bindings: Vec<(KeyAction, KeyChord)>,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::from_overrides(&[]).expect("default key map must be valid")
    }
}

impl KeyMap {
    pub fn from_environment() -> Result<Self, String> {
        let overrides = KeyAction::ALL
            .into_iter()
            .filter_map(|action| {
                std::env::var(action.env_name())
                    .ok()
                    .map(|value| (action, value))
            })
            .collect::<Vec<_>>();
        Self::from_overrides(&overrides)
    }

    fn from_overrides(overrides: &[(KeyAction, String)]) -> Result<Self, String> {
        let mut bindings: Vec<(KeyAction, KeyChord)> = Vec::with_capacity(KeyAction::ALL.len());
        for action in KeyAction::ALL {
            let raw = overrides
                .iter()
                .find_map(|(candidate, value)| (*candidate == action).then_some(value.as_str()))
                .unwrap_or_else(|| action.default_chord());
            let chord =
                KeyChord::parse(raw).map_err(|error| format!("{}: {error}", action.env_name()))?;
            if let Some((conflict, _)) = bindings.iter().find(|(_, existing)| existing == &chord) {
                return Err(format!(
                    "key binding conflict: {} and {} both use {}",
                    conflict.config_name(),
                    action.config_name(),
                    chord.label
                ));
            }
            bindings.push((action, chord));
        }
        Ok(Self { bindings })
    }

    pub fn matches(&self, action: KeyAction, event: &KeyEvent) -> bool {
        self.bindings
            .iter()
            .find(|(candidate, _)| *candidate == action)
            .is_some_and(|(_, chord)| chord.matches(event))
    }

    pub fn label(&self, action: KeyAction) -> &str {
        self.bindings
            .iter()
            .find(|(candidate, _)| *candidate == action)
            .map_or("unbound", |(_, chord)| chord.label.as_str())
    }

    pub fn summary(&self) -> String {
        self.bindings
            .iter()
            .map(|(action, chord)| format!("{}={}", action.config_name(), chord.label))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn chord_label(code: KeyCode, modifiers: KeyModifiers) -> String {
    let mut parts = Vec::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl".to_string());
    }
    if modifiers.contains(KeyModifiers::ALT) {
        parts.push("Alt".to_string());
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("Shift".to_string());
    }
    parts.push(match code {
        KeyCode::Char(character) => character.to_ascii_uppercase().to_string(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Enter => "Enter".into(),
        _ => "Unknown".into(),
    });
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_are_parsed_and_match_terminal_events() {
        let map = KeyMap::from_overrides(&[(KeyAction::Sessions, "alt+s".into())]).unwrap();
        assert!(map.matches(
            KeyAction::Sessions,
            &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT)
        ));
        assert_eq!(map.label(KeyAction::Sessions), "Alt+S");
    }

    #[test]
    fn conflicts_and_printable_global_keys_fail_closed() {
        let conflict = KeyMap::from_overrides(&[
            (KeyAction::Sessions, "ctrl+k".into()),
            (KeyAction::Commands, "ctrl+k".into()),
        ])
        .unwrap_err();
        assert!(conflict.contains("conflict"));
        assert!(KeyMap::from_overrides(&[(KeyAction::Sessions, "s".into())]).is_err());
        assert!(KeyMap::from_overrides(&[(KeyAction::Sessions, "shift+s".into())]).is_err());
        assert!(KeyMap::from_overrides(&[(KeyAction::Sessions, "ctrl+c".into())]).is_err());
    }
}
