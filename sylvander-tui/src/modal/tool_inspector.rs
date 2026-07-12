//! Focused long-output inspection with search and explicit clipboard copy.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::AppState;
use crate::event::Action;
use crate::modal::{Consumed, Modal};
use crate::theme;

pub struct ToolInspector {
    call_id: String,
    tool_name: String,
    output: String,
    query: String,
    searching: bool,
    cursor: usize,
}

impl ToolInspector {
    pub fn new(call_id: String, tool_name: String, output: String) -> Self {
        Self {
            call_id,
            tool_name,
            output,
            query: String::new(),
            searching: false,
            cursor: 0,
        }
    }

    fn lines(&self) -> Vec<String> {
        crate::markdown::sanitize_terminal_text(&self.output)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn matches(&self, lines: &[String]) -> Vec<usize> {
        if self.query.is_empty() {
            return Vec::new();
        }
        let query = self.query.to_lowercase();
        lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| line.to_lowercase().contains(&query).then_some(index))
            .collect()
    }
}

impl Modal for ToolInspector {
    fn active(&self) -> bool {
        true
    }
    fn title(&self) -> &str {
        "Tool output"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let area = inset(parent, 4, 2);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " {} · {} ",
                    self.tool_name,
                    short_id(&self.call_id)
                ))
                .title_style(theme::active_bold()),
            area,
        );
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(inner);
        let search = if self.searching {
            format!("/{}▏", self.query)
        } else if self.query.is_empty() {
            "search inactive".into()
        } else {
            format!("/{}", self.query)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(search, theme::text_muted())),
            rows[0],
        );

        let lines = self.lines();
        let matches = self.matches(&lines);
        let height = rows[1].height as usize;
        let max_start = lines.len().saturating_sub(height);
        let start = self
            .cursor
            .saturating_sub(height.saturating_sub(1))
            .min(max_start);
        let visible = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(height)
            .map(|(index, line)| {
                let matched = matches.contains(&index);
                Line::from(vec![
                    Span::styled(format!("{:>5}  ", index + 1), theme::text_muted()),
                    Span::styled(
                        line.clone(),
                        if matched {
                            theme::active()
                        } else {
                            theme::text()
                        },
                    ),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), rows[1]);
        let match_label = if self.query.is_empty() {
            String::new()
        } else {
            format!(" · {} matches", matches.len())
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("↑↓ scroll · / search · n next · c copy · esc close{match_label}"),
                theme::text_muted(),
            )),
            rows[2],
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        if self.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.query.push(character);
                }
                _ => {}
            }
            state.dirty.mark();
            return Consumed::Yes { dismiss: false };
        }
        match key.code {
            KeyCode::Esc => Consumed::Yes { dismiss: true },
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(self.lines().len().saturating_sub(1));
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('/') => {
                self.searching = true;
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('n') => {
                let lines = self.lines();
                if let Some(next) = self
                    .matches(&lines)
                    .into_iter()
                    .find(|index| *index > self.cursor)
                {
                    self.cursor = next;
                } else if let Some(first) = self.matches(&lines).first() {
                    self.cursor = *first;
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('c') => {
                state.pending_actions.push(Action::CopyText {
                    text: self.output.clone(),
                });
                state.status = "Copying tool output…".into();
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn inset(parent: Rect, x: u16, y: u16) -> Rect {
    Rect {
        x: parent.x.saturating_add(x),
        y: parent.y.saturating_add(y),
        width: parent.width.saturating_sub(x.saturating_mul(2)).max(1),
        height: parent.height.saturating_sub(y.saturating_mul(2)).max(1),
    }
}

fn short_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_wraps_and_copy_emits_a_local_effect() {
        let mut inspector = ToolInspector::new(
            "call-123456".into(),
            "bash".into(),
            "first\nneedle one\nlast needle".into(),
        );
        inspector.query = "needle".into();
        let lines = inspector.lines();
        assert_eq!(inspector.matches(&lines), [1, 2]);
        inspector.cursor = 2;
        let mut state = AppState::new();
        inspector.handle_key(
            &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            &mut state,
        );
        assert_eq!(inspector.cursor, 1);
        inspector.handle_key(
            &KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
            &mut state,
        );
        assert!(matches!(
            state.pending_actions.as_slice(),
            [Action::CopyText { text }] if text.contains("needle one")
        ));
    }
}
