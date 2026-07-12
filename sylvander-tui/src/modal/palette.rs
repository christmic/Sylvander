//! Command palette — UX §12. Triggered by typing `/` in the composer
//! area (when it is otherwise empty). Provides a fuzzy-filtered command
//! list the user can invoke without leaving the keyboard home row.
//!
//! The input is a real command line: entries can be selected, or commands with
//! arguments such as `theme midnight` can be typed and submitted directly.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::{AppMode, AppState};
pub use crate::command::{COMMANDS, CommandSpec as Command};
use crate::modal::{Consumed, Modal};

pub struct CommandPalette {
    pub filter: String,
    pub cursor: usize,
    pub filtered: Vec<usize>,
    pub error: Option<String>,
}

impl CommandPalette {
    pub fn new() -> Self {
        let mut s = Self {
            filter: String::new(),
            cursor: 0,
            filtered: Vec::new(),
            error: None,
        };
        s.recompute();
        s
    }

    pub fn recompute(&mut self) {
        let needle = self.filter.split_whitespace().next().unwrap_or("");
        self.filtered = COMMANDS
            .iter()
            .enumerate()
            .filter_map(|(i, command)| {
                if needle.is_empty()
                    || command.name.contains(&needle.to_ascii_lowercase())
                    || command
                        .description
                        .to_ascii_lowercase()
                        .contains(&needle.to_ascii_lowercase())
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();
        if self.cursor >= self.filtered.len() {
            self.cursor = 0;
        }
    }

    /// Run the currently-selected command, pushing the appropriate
    /// side-effect onto AppState's pending_actions.
    fn invoke(&mut self, state: &mut AppState) -> Consumed {
        let typed_name = self.filter.split_whitespace().next().unwrap_or("");
        let exact_typed = COMMANDS
            .iter()
            .any(|command| command.name.eq_ignore_ascii_case(typed_name));
        let line = if exact_typed {
            self.filter.clone()
        } else if let Some(&command_index) = self.filtered.get(self.cursor) {
            COMMANDS[command_index].name.to_string()
        } else {
            self.error = Some("No matching command".into());
            return Consumed::Yes { dismiss: false };
        };
        match crate::command::parse(&line)
            .and_then(|invocation| crate::command::execute(invocation, state))
        {
            Ok(()) => Consumed::Yes { dismiss: true },
            Err(error) => {
                self.error = Some(error);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
        }
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for CommandPalette {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        "Commands"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let desired_height = (self.filtered.len() as u16).saturating_add(4).clamp(8, 18);
        let popup_area = centered_rect(55, desired_height, parent);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Commands ")
                .title_style(theme::modal_title_coral()),
            popup_area,
        );

        let inner = Block::default().borders(Borders::ALL).inner(popup_area);
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // input
                Constraint::Length(1), // error or divider
                Constraint::Min(7),    // command list
            ])
            .split(inner);

        // 1. Filter input
        let prompt = Line::from(vec![
            Span::styled("/", theme::modal_title_coral()),
            Span::styled(&self.filter, Style::default()),
            Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ]);
        frame.render_widget(Paragraph::new(prompt), layout[0]);
        let cursor_x = inner.x + 1 + self.filter.chars().count() as u16;
        let cursor_y = inner.y;
        if cursor_x < inner.x + inner.width {
            frame.set_cursor_position((cursor_x, cursor_y));
        }

        let feedback = self.error.as_deref().map_or_else(
            || {
                Line::from(Span::styled(
                    "─".repeat(layout[1].width as usize),
                    theme::rule(),
                ))
            },
            |error| Line::from(Span::styled(format!("! {error}"), theme::warning())),
        );
        frame.render_widget(Paragraph::new(feedback), layout[1]);

        // 3. List
        let mut lines: Vec<Line> = Vec::new();
        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no commands match)",
                theme::text_muted().italic(),
            )));
        } else {
            let visible_rows = layout[2].height.max(1) as usize;
            let start = self.cursor.saturating_add(1).saturating_sub(visible_rows);
            for (row_i, &cmd_idx) in self
                .filtered
                .iter()
                .enumerate()
                .skip(start)
                .take(visible_rows)
            {
                let cmd = &COMMANDS[cmd_idx];
                let is_cursor = row_i == self.cursor;
                let prefix = if is_cursor { "  › " } else { "    " };
                let color = if is_cursor {
                    theme::palette().active
                } else {
                    theme::palette().text
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(color)),
                    Span::styled(
                        format!("/{:<13}", cmd.name),
                        Style::default().fg(if is_cursor {
                            theme::palette().active
                        } else {
                            theme::palette().brand_violet
                        }),
                    ),
                    Span::styled(cmd.description, Style::default().fg(color)),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(lines), layout[2]);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                if self.cursor + 1 < self.filtered.len() {
                    self.cursor += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                if !self.filter.is_empty() {
                    self.filter.pop();
                    self.error = None;
                    self.recompute();
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => self.invoke(state),
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.filter.push(c);
                    self.error = None;
                    self.recompute();
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn centered_rect(percent_x: u16, height: u16, parent: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(parent.height.saturating_sub(height) / 2),
            Constraint::Length(height.min(parent.height)),
            Constraint::Length(parent.height.saturating_sub(height) / 2),
        ])
        .split(parent);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x.min(95)) / 2),
            Constraint::Percentage(percent_x.min(95)),
            Constraint::Percentage((100 - percent_x.min(95)) / 2),
        ])
        .split(v[1]);
    h[1]
}

use crate::theme;
use ratatui::style::Modifier;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(c, m)
    }

    #[test]
    fn empty_filter_shows_all_commands() {
        let p = CommandPalette::new();
        assert_eq!(p.filtered.len(), COMMANDS.len());
    }

    #[test]
    fn filter_substring_matches_command_name() {
        let mut p = CommandPalette::new();
        p.filter = "ses".into();
        p.recompute();
        let names: Vec<&'static str> = p.filtered.iter().map(|&i| COMMANDS[i].name).collect();
        assert!(names.contains(&"sessions"));
        assert!(!names.contains(&"clear"));
    }

    #[test]
    fn filter_no_match_yields_empty_list() {
        let mut p = CommandPalette::new();
        p.filter = "zzzzz".into();
        p.recompute();
        assert!(p.filtered.is_empty());
    }

    #[test]
    fn enter_dispatches_quit_command() {
        let mut state = AppState::new();
        let mut p = CommandPalette::new();
        for character in "quit".chars() {
            let _ = p.handle_key(
                &key(KeyCode::Char(character), KeyModifiers::NONE),
                &mut state,
            );
        }
        let consumed = p.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(state.should_quit);
    }

    #[test]
    fn enter_on_clear_command_empties_messages() {
        let mut state = AppState::new();
        use crate::app::ChatMessage;
        state.messages.push(ChatMessage::User("hi".into()));
        let mut p = CommandPalette::new();
        // Move to /clear (index 2).
        let _ = p.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut state);
        let _ = p.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut state);
        let consumed = p.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(state.messages.is_empty());
    }

    #[test]
    fn enter_on_sessions_pushes_sessions_overlay() {
        let mut state = AppState::new();
        let mut p = CommandPalette::new();
        // /sessions is at index 1.
        let _ = p.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut state);
        let consumed = p.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        // Palette itself was popped, but it pushed a sessions overlay.
        assert_eq!(state.modals.len(), 1);
    }
}
