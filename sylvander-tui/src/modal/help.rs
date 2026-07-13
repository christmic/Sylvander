//! Contextual help overlay for commands, approvals, and tool activity.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::app::AppState;
use crate::keymap::KeyAction;
use crate::modal::{Consumed, Modal, surface::review_view};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Overview,
    Commands,
    Approval,
    Tools,
    Vim,
}

pub struct HelpModal {
    topic: HelpTopic,
}

impl HelpModal {
    pub fn new(topic: Option<&str>) -> Result<Self, String> {
        let topic = match topic.unwrap_or("overview") {
            "overview" | "keys" => HelpTopic::Overview,
            "commands" | "command" => HelpTopic::Commands,
            "approval" | "approvals" | "permissions" => HelpTopic::Approval,
            "tool" | "tools" => HelpTopic::Tools,
            "vim" | "editing" | "composer" => HelpTopic::Vim,
            other => return Err(format!("Unknown help topic {other:?}")),
        };
        Ok(Self { topic })
    }

    fn lines(&self, state: &AppState) -> Vec<Line<'static>> {
        let rows: Vec<(String, String)> = match self.topic {
            HelpTopic::Overview => vec![
                ("Enter".into(), "send prompt".into()),
                ("Shift+Enter".into(), "insert newline".into()),
                (
                    "↑ / ↓".into(),
                    "draft cursor or submitted prompt history".into(),
                ),
                ("mouse wheel".into(), "review transcript only".into()),
                (
                    format!(
                        "{} / {}",
                        state.keymap.label(KeyAction::TranscriptPageUp),
                        state.keymap.label(KeyAction::TranscriptPageDown)
                    ),
                    "review transcript by page".into(),
                ),
                (
                    state.keymap.label(KeyAction::ReturnLive).into(),
                    "return to live output".into(),
                ),
                (
                    state.keymap.label(KeyAction::ToolDetails).into(),
                    "expand or collapse tool details".into(),
                ),
                (
                    state.keymap.label(KeyAction::Sessions).into(),
                    "sessions".into(),
                ),
                (
                    format!("/ or {}", state.keymap.label(KeyAction::Commands)),
                    "command line".into(),
                ),
            ],
            HelpTopic::Commands => vec![
                ("/new".into(), "prepare a clean session".into()),
                ("/sessions".into(), "browse known sessions".into()),
                ("/clear".into(), "clear local transcript".into()),
                (
                    "/theme <name>".into(),
                    "switch semantic color palette".into(),
                ),
                (
                    "/tools expand".into(),
                    "show full tool inputs and results".into(),
                ),
                ("/status".into(), "append runtime and token details".into()),
                (
                    "/help <topic>".into(),
                    "overview, commands, approval, tools, vim".into(),
                ),
            ],
            HelpTopic::Approval => vec![
                ("↑ / ↓".into(), "select tool request".into()),
                ("y or Enter".into(), "approve selected request".into()),
                ("n".into(), "reject and optionally explain why".into()),
                ("Y".into(), "approve all remaining requests".into()),
                ("N".into(), "reject all remaining requests".into()),
                (
                    "Esc".into(),
                    "reject pending requests and unblock Agent".into(),
                ),
            ],
            HelpTopic::Tools => vec![
                (
                    "compact".into(),
                    "one semantic target and result summary".into(),
                ),
                (
                    "expanded".into(),
                    "structured input plus up to 12 result rows".into(),
                ),
                (
                    state.keymap.label(KeyAction::ToolDetails).into(),
                    "toggle compact and expanded modes".into(),
                ),
                (
                    "call ID".into(),
                    "pairs concurrent results with the correct call".into(),
                ),
                ("amber result".into(), "tool returned an error".into()),
            ],
            HelpTopic::Vim => vec![
                ("Esc / i a I A".into(), "Normal / Insert mode".into()),
                (
                    "h j k l / arrows".into(),
                    "move by character or line".into(),
                ),
                ("w b / 0 $".into(), "move by word or line edge".into()),
                ("o O".into(), "open line below or above".into()),
                ("x / D / dd dw d$".into(), "delete into register".into()),
                ("C / cc cw c$".into(), "change and enter Insert".into()),
                ("yy yw / p P".into(), "yank and paste register".into()),
                ("u / gg G".into(), "undo / first or last line".into()),
                ("Enter".into(), "send prompt from Normal mode".into()),
            ],
        };
        let mut lines = vec![
            Line::from(Span::styled(
                match self.topic {
                    HelpTopic::Overview => "Essential interaction",
                    HelpTopic::Commands => "Command line",
                    HelpTopic::Approval => "Tool approval",
                    HelpTopic::Tools => "Tool activity",
                    HelpTopic::Vim => "Vim Composer",
                },
                theme::header(),
            )),
            Line::from(""),
        ];
        lines.extend(rows.into_iter().map(|(key, description)| {
            Line::from(vec![
                Span::styled(format!("{key:<20}"), theme::brand_violet()),
                Span::styled(description, theme::text()),
            ])
        }));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vim_help_is_discoverable_and_lists_safety_relevant_modes() {
        let help = HelpModal::new(Some("vim")).expect("documented topic");
        let lines = help.lines(&AppState::new());
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Vim Composer"));
        assert!(text.contains("Normal / Insert mode"));
        assert!(text.contains("Enter"));
    }
}

impl Modal for HelpModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        "Help"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, state: &AppState) {
        let areas = review_view(frame, parent, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Sylvander help", theme::brand_violet()),
                Span::styled(" · reference", theme::text_muted()),
            ])),
            areas.header,
        );
        frame.render_widget(
            Paragraph::new(self.lines(state)).wrap(Wrap { trim: false }),
            areas.body,
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Reference for the current interaction mode",
                theme::text_muted(),
            )),
            areas.footer,
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, _state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Consumed::Yes { dismiss: true },
            _ => Consumed::Yes { dismiss: false },
        }
    }
}
