//! Focused long-output inspection with search and explicit clipboard copy.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::app::AppState;
use crate::event::Action;
use crate::modal::{Consumed, Modal, surface::review_view};
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
    fn title(&self) -> &'static str {
        "Tool output"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let lines = self.lines();
        let matches = self.matches(&lines);
        let areas = review_view(frame, parent, 1);
        let title = format!("{} output · {}", self.tool_name, short_id(&self.call_id));
        let detail = if self.query.is_empty() {
            format!("{} lines", lines.len())
        } else {
            format!("{} matches", matches.len())
        };
        let gap = (areas.header.width as usize)
            .saturating_sub(UnicodeWidthStr::width(&*title) + UnicodeWidthStr::width(&*detail));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(title, theme::brand_violet()),
                Span::raw(" ".repeat(gap)),
                Span::styled(detail, theme::text_muted()),
            ])),
            areas.header,
        );

        let content = Rect {
            width: parent.width.saturating_sub(4),
            ..areas.body
        };
        let height = content.height as usize;
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
                let is_match = matches.contains(&index);
                Line::from(vec![
                    Span::styled(format!("{:>5}  ", index + 1), theme::text_muted()),
                    Span::styled(
                        line.clone(),
                        if is_match {
                            theme::active()
                        } else {
                            theme::text()
                        },
                    ),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible), content);
        let footer = if self.searching {
            Line::from(vec![
                Span::styled("/", theme::brand_violet()),
                Span::styled(&self.query, theme::text()),
            ])
        } else {
            Line::from(Span::styled(
                "n next match · c copy full output",
                theme::text_muted(),
            ))
        };
        frame.render_widget(Paragraph::new(footer), areas.footer);
        if self.searching {
            let x = areas.footer.x + 1 + UnicodeWidthStr::width(self.query.as_str()) as u16;
            if x < areas.footer.x + areas.footer.width {
                frame.set_cursor_position((x, areas.footer.y));
            }
        }
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

fn short_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

#[cfg(test)]
#[path = "../../tests/unit/modal_tool_inspector.rs"]
mod tests;
