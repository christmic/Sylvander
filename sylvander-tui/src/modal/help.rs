//! Contextual help overlay for commands, approvals, and tool activity.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::AppState;
use crate::keymap::KeyAction;
use crate::modal::{Consumed, Modal};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Overview,
    Commands,
    Approval,
    Tools,
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
                    "overview, commands, approval, tools".into(),
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
        };
        let mut lines = vec![
            Line::from(Span::styled(
                match self.topic {
                    HelpTopic::Overview => "Essential interaction",
                    HelpTopic::Commands => "Command line",
                    HelpTopic::Approval => "Tool approval",
                    HelpTopic::Tools => "Tool activity",
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
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Esc close", theme::text_muted())));
        lines
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
        let area = centered_rect(68, 16, parent);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Sylvander help ")
                .title_style(theme::modal_title_coral()),
            area,
        );
        let inner = Block::default().borders(Borders::ALL).inner(area);
        frame.render_widget(
            Paragraph::new(self.lines(state)).wrap(Wrap { trim: false }),
            inner,
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, _state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Consumed::Yes { dismiss: true },
            _ => Consumed::Yes { dismiss: false },
        }
    }
}

fn centered_rect(percent_x: u16, height: u16, parent: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(parent.height.saturating_sub(height) / 2),
            Constraint::Length(height.min(parent.height)),
            Constraint::Min(0),
        ])
        .split(parent);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}
