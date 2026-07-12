//! Contextual help overlay for commands, approvals, and tool activity.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::AppState;
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

    fn lines(&self) -> Vec<Line<'static>> {
        let rows: &[(&str, &str)] = match self.topic {
            HelpTopic::Overview => &[
                ("Enter", "send prompt"),
                ("Shift+Enter", "insert newline"),
                ("↑ / ↓", "draft cursor or submitted prompt history"),
                ("mouse wheel", "review transcript only"),
                ("PageUp / PageDown", "review transcript by page"),
                ("Ctrl+End", "return to live output"),
                ("Ctrl+O", "expand or collapse tool details"),
                ("Ctrl+P", "sessions"),
                ("/ or Ctrl+K", "command line"),
            ],
            HelpTopic::Commands => &[
                ("/new", "prepare a clean session"),
                ("/sessions", "browse known sessions"),
                ("/clear", "clear local transcript"),
                ("/theme <name>", "switch semantic color palette"),
                ("/tools expand", "show full tool inputs and results"),
                ("/status", "append runtime and token details"),
                ("/help <topic>", "overview, commands, approval, tools"),
            ],
            HelpTopic::Approval => &[
                ("↑ / ↓", "select tool request"),
                ("y or Enter", "approve selected request"),
                ("n", "reject and optionally explain why"),
                ("Y", "approve all remaining requests"),
                ("N", "reject all remaining requests"),
                ("Esc", "reject pending requests and unblock Agent"),
            ],
            HelpTopic::Tools => &[
                ("compact", "one semantic target and result summary"),
                ("expanded", "structured input plus up to 12 result rows"),
                ("Ctrl+O", "toggle compact and expanded modes"),
                ("call ID", "pairs concurrent results with the correct call"),
                ("amber result", "tool returned an error"),
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
        lines.extend(rows.iter().map(|(key, description)| {
            Line::from(vec![
                Span::styled(format!("{key:<20}"), theme::brand_violet()),
                Span::styled(*description, theme::text()),
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

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
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
            Paragraph::new(self.lines()).wrap(Wrap { trim: false }),
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
