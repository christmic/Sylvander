//! Command palette — UX §12. Triggered by typing `/` in the composer
//! area (when it is otherwise empty). Provides a fuzzy-filtered command
//! list the user can invoke without leaving the keyboard home row.
//!
//! Scope for M-T6 only covers commands whose underlying action exists:
//! `/new`, `/sessions`, `/clear`, `/help`, `/quit`. The remaining 13
//! commands in the v6.0 initial set will be plugged in as their
//! features land.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::{AppMode, AppState};
use crate::event::Action;
use crate::modal::{Consumed, Modal};

/// One palette entry. `matcher` is the fuzzy needle (lowercase substring).
#[derive(Debug, Clone)]
pub struct Command {
    pub cmd: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
}

impl Command {
    pub fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let n = needle.to_lowercase();
        // Match against command name first; fall back to label.
        self.cmd.to_lowercase().contains(&n) || self.label.to_lowercase().contains(&n)
    }
}

/// Curated list for M-T6.
pub const COMMANDS: &[Command] = &[
    Command {
        cmd: "/new",
        label: "Start a new session",
        hint: "/ new-session",
    },
    Command {
        cmd: "/sessions",
        label: "Switch sessions",
        hint: "ctrl+p",
    },
    Command {
        cmd: "/clear",
        label: "Clear the transcript",
        hint: "local",
    },
    Command {
        cmd: "/help",
        label: "Show help",
        hint: "ui-only",
    },
    Command {
        cmd: "/quit",
        label: "Quit sylvander-tui",
        hint: "ctrl+c",
    },
];

pub struct CommandPalette {
    pub filter: String,
    pub cursor: usize,
    pub filtered: Vec<usize>,
}

impl CommandPalette {
    pub fn new() -> Self {
        let mut s = Self {
            filter: String::new(),
            cursor: 0,
            filtered: Vec::new(),
        };
        s.recompute();
        s
    }

    pub fn recompute(&mut self) {
        self.filtered = COMMANDS
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                if c.matches(&self.filter) {
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
    fn invoke(&self, state: &mut AppState) -> Consumed {
        if let Some(&cmd_idx) = self.filtered.get(self.cursor) {
            let cmd = &COMMANDS[cmd_idx];
            match cmd.cmd {
                "/new" => {
                    state.pending_actions.push(Action::SendChat {
                        text: String::new(),
                        session_id: None,
                    });
                }
                "/sessions" => {
                    // Push the sessions overlay onto the stack so the user
                    // lands directly in it.
                    let snapshot = state.sessions.clone();
                    state
                        .modals
                        .push(Box::new(crate::modal::sessions::SessionsOverlay::new(
                            snapshot,
                        )));
                }
                "/clear" => {
                    state.messages.clear();
                    state.streaming.clear();
                    state.streaming_thinking.clear();
                    state.dirty.mark();
                    state.status = "Cleared transcript".into();
                }
                "/help" => {
                    state.status = "Help: Type /command · ctrl+p sessions · ctrl+k palette".into();
                    state.dirty.mark();
                }
                "/quit" => {
                    state.should_quit = true;
                }
                _ => {
                    // Unknown — no-op for M-T6.
                }
            }
            return Consumed::Yes { dismiss: true };
        }
        Consumed::Ignored
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
        let popup_area = centered_rect(55, 12, parent);
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
                Constraint::Length(1), // divider
                Constraint::Min(7),    // list
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

        // 2. Spacer divider
        frame.render_widget(
            Paragraph::new("─".repeat(layout[1].width as usize)),
            layout[1],
        );

        // 3. List
        let mut lines: Vec<Line> = Vec::new();
        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no commands match)",
                theme::text_muted().italic(),
            )));
        } else {
            for (row_i, &cmd_idx) in self.filtered.iter().enumerate() {
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
                        format!("{:<14}", cmd.cmd),
                        Style::default().fg(if is_cursor {
                            theme::palette().active
                        } else {
                            theme::palette().brand_violet
                        }),
                    ),
                    Span::styled(cmd.label, Style::default().fg(color)),
                    Span::styled(format!("  ({})", cmd.hint), theme::text_muted()),
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
        let names: Vec<&'static str> = p.filtered.iter().map(|&i| COMMANDS[i].cmd).collect();
        assert!(names.contains(&"/sessions"));
        assert!(!names.contains(&"/clear"));
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
        // Cursor lands on /new (first command). Move down to /quit.
        let _ = p.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut state);
        let _ = p.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut state);
        let _ = p.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut state);
        let _ = p.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut state);
        // Now cursor at /quit (index 4).
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
