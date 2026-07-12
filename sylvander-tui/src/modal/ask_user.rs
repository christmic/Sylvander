//! AskUser modal — model asks the user a clarifying question.
//!
//! Three content modes (UX §12.1):
//! - **Single select**: options != empty, multi_select=false.
//!   Arrow keys choose, number keys jump, Enter confirms, Esc cancels.
//! - **Multi select**: options != empty, multi_select=true.
//!   Space toggles the current option (or number jumps), Enter confirms.
//! - **Free text**: options empty. Composer behavior is reused; user types
//!   freely and submits with Enter.
//!
//! In every mode the user can also type free text alongside — that text
//! becomes the answer if no option is selected, or gets appended after the
//! selected options separated by `; `.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::event::Action;
use crate::modal::{Consumed, Modal};
use crate::theme;

/// Selection state — `None` means single-select with no choice yet, OR
/// free-text only. For multi-select, the bit-mask aligns with `options`.
#[derive(Debug, Clone)]
enum Selection {
    /// No choice yet — answer box is empty / user is typing free text.
    None,
    /// Single-select: index into `options`.
    Single(usize),
    /// Multi-select: bit-mask, `true` at index `i` means option `i` chosen.
    Multi(Vec<bool>),
}

pub struct AskUserModal {
    pub call_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub multi_select: bool,
    pub selection: Selection,
    /// Cursor for single-select / multi-select navigation.
    pub cursor: usize,
    /// Free-text answer the user has typed (always available).
    pub answer: String,
}

impl AskUserModal {
    pub fn new(
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    ) -> Self {
        let selection = if multi_select && !options.is_empty() {
            Selection::Multi(vec![false; options.len()])
        } else {
            Selection::None
        };
        Self {
            call_id,
            question,
            options,
            multi_select,
            selection,
            cursor: 0,
            answer: String::new(),
        }
    }

    fn kind(&self) -> &'static str {
        if self.options.is_empty() {
            "free text"
        } else if self.multi_select {
            "multi-select"
        } else {
            "single-select"
        }
    }

    /// Compose the answer string. Format chosen for easy parsing by the
    /// agent's `ask_user` handler: joined options first, free text after `; `.
    fn submit_answer(&self) -> String {
        match &self.selection {
            Selection::None => self.answer.trim().to_string(),
            Selection::Single(i) => {
                let opt = self.options[*i].clone();
                if self.answer.trim().is_empty() {
                    opt
                } else {
                    format!("{opt}; {}", self.answer.trim())
                }
            }
            Selection::Multi(mask) => {
                let chosen: Vec<String> = self
                    .options
                    .iter()
                    .zip(mask.iter())
                    .filter_map(|(o, b)| if *b { Some(o.clone()) } else { None })
                    .collect();
                let joined = chosen.join(", ");
                if self.answer.trim().is_empty() {
                    joined
                } else if joined.is_empty() {
                    self.answer.trim().to_string()
                } else {
                    format!("{joined}; {}", self.answer.trim())
                }
            }
        }
    }
}

impl Modal for AskUserModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        "Agent asks"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        // Height adapts to option count + free-text input.
        let options_lines = self.options.len().max(1) as u16;
        let height = (8 + options_lines).min(parent.height.saturating_sub(2));
        let popup_area = centered_rect(60, height, parent);
        frame.render_widget(Clear, popup_area);

        let title = format!(" Agent asks ({}) ", self.kind());
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_style(theme::modal_title_coral()),
            popup_area,
        );

        let inner = Block::default().borders(Borders::ALL).inner(popup_area);
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            wrap_text(&self.question, (inner.width as usize).saturating_sub(2)),
            theme::text().bold(),
        )));
        lines.push(Line::from(""));

        if !self.options.is_empty() {
            for (i, opt) in self.options.iter().enumerate() {
                let is_cursor = i == self.cursor;
                let (marker, marker_color) = match &self.selection {
                    Selection::Multi(mask) => {
                        let check = if mask[i] { "☑" } else { "☐" };
                        (
                            format!("{} [{}] ", check, i + 1),
                            if mask[i] {
                                ratatui::style::Color::Green
                            } else if is_cursor {
                                ratatui::style::Color::Cyan
                            } else {
                                ratatui::style::Color::Gray
                            },
                        )
                    }
                    _ => (
                        format!("  [{}] ", i + 1),
                        if is_cursor { ratatui::style::Color::Cyan } else { ratatui::style::Color::Gray },
                    ),
                };

                let prefix = if self.multi_select {
                    marker
                } else if is_cursor {
                    " ›  ".to_string()
                } else {
                    "    ".to_string()
                };

                let label = wrap_text(opt, (inner.width as usize).saturating_sub(8));
                if self.multi_select {
                    lines.push(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(marker_color)),
                        Span::styled(label, Style::default().fg(marker_color)),
                    ]));
                } else {
                    let color = if is_cursor { ratatui::style::Color::Cyan } else { ratatui::style::Color::Gray };
                    lines.push(Line::from(Span::styled(
                        format!("{prefix}{label}"),
                        Style::default().fg(color),
                    )));
                }
            }
            lines.push(Line::from(""));
        }

        // Free-text answer field.
        let answer_label = if self.options.is_empty() {
            "Type your answer:"
        } else {
            "Or type a free-text reply:"
        };
        lines.push(Line::from(Span::styled(
            answer_label,
            theme::text_muted(),
        )));
        lines.push(Line::from(vec![
            Span::styled("> ", theme::verified()),
            Span::styled(&self.answer, Style::default()),
        ]));
        lines.push(Line::from(""));
        let hint = match self.kind() {
            "free text" => "Enter: submit   Esc: cancel",
            "multi-select" => "Space: toggle   Enter: submit   Esc: cancel",
            _ => "Enter: submit option   Esc: cancel",
        };
        lines.push(Line::from(Span::styled(
            hint,
            theme::text_muted(),
        )));

        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            inner,
        );

        // Hardware cursor at end of free-text answer.
        let cursor_x = inner.x + 2 + self.answer.chars().count() as u16;
        let cursor_y = inner.y + inner.height.saturating_sub(4);
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
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
                if !self.options.is_empty() && self.cursor + 1 < self.options.len() {
                    self.cursor += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char(' ') if self.multi_select => {
                if let Selection::Multi(ref mut mask) = self.selection {
                    if self.cursor < mask.len() {
                        mask[self.cursor] = !mask[self.cursor];
                        state.dirty.mark();
                    }
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                let call_id = std::mem::take(&mut self.call_id);
                let answer = self.submit_answer();
                state.mode = AppMode::Normal;
                if !answer.is_empty() {
                    state.pending_actions.push(Action::SendAnswer { call_id, answer });
                }
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char(c) => {
                if key
                    .modifiers
                    .contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT)
                {
                    return Consumed::Ignored;
                }
                // Numeric jump for both single and multi.
                if let Some(d) = c.to_digit(10) {
                    let idx = (d as usize).saturating_sub(1);
                    if idx < self.options.len() {
                        self.cursor = idx;
                        match &mut self.selection {
                            Selection::None => {
                                self.selection = Selection::Single(idx);
                            }
                            Selection::Multi(mask) => {
                                if idx < mask.len() {
                                    mask[idx] = !mask[idx];
                                }
                            }
                            Selection::Single(_) => {
                                self.selection = Selection::Single(idx);
                            }
                        }
                        state.dirty.mark();
                        return Consumed::Yes { dismiss: false };
                    }
                }
                // Free-text input (always available as a fallback).
                self.answer.push(c);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                // Prefer to delete from free-text, not from selected option.
                if !self.answer.is_empty() {
                    self.answer.pop();
                } else {
                    // Clearing selection back to "None" if user backspaces
                    // after a single-select — keeps the modal recoverable.
                    if matches!(self.selection, Selection::Single(_)) {
                        self.selection = Selection::None;
                    }
                }
                state.dirty.mark();
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

fn wrap_text(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max < 4 {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

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
    fn free_text_mode_returns_typed_answer() {
        let mut m = AskUserModal::new("c".into(), "why?".into(), vec![], false);
        let mut s = AppState::new();
        for ch in "make it blue".chars() {
            m.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE), &mut s);
        }
        let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert_eq!(s.pending_actions.len(), 1);
        assert!(matches!(
            s.pending_actions[0],
            Action::SendAnswer { ref call_id, ref answer } if call_id == "c" && answer == "make it blue"
        ));
    }

    #[test]
    fn single_select_with_numeric() {
        let mut m = AskUserModal::new(
            "c".into(),
            "color?".into(),
            vec!["red".into(), "green".into(), "blue".into()],
            false,
        );
        let mut s = AppState::new();
        m.handle_key(&key(KeyCode::Char('2'), KeyModifiers::NONE), &mut s);
        let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(matches!(
            s.pending_actions[0],
            Action::SendAnswer { ref answer, .. } if answer == "green"
        ));
    }

    #[test]
    fn multi_select_toggle_with_space() {
        let mut m = AskUserModal::new(
            "c".into(),
            "tags?".into(),
            vec!["urgent".into(), "bug".into(), "feature".into()],
            true,
        );
        let mut s = AppState::new();
        // Cursor on row 0; Space → toggle.
        m.handle_key(&key(KeyCode::Char(' '), KeyModifiers::NONE), &mut s);
        // Down to row 2.
        m.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut s);
        m.handle_key(&key(KeyCode::Down, KeyModifiers::NONE), &mut s);
        // Toggle row 2.
        m.handle_key(&key(KeyCode::Char(' '), KeyModifiers::NONE), &mut s);
        let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(matches!(
            s.pending_actions[0],
            Action::SendAnswer { ref answer, .. } if answer == "urgent, feature"
        ));
    }

    #[test]
    fn option_plus_free_text_concatenates_with_semicolon() {
        let mut m = AskUserModal::new(
            "c".into(),
            "?".into(),
            vec!["red".into(), "green".into()],
            false,
        );
        let mut s = AppState::new();
        m.handle_key(&key(KeyCode::Char('1'), KeyModifiers::NONE), &mut s);
        for ch in " but smaller".chars() {
            m.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE), &mut s);
        }
        let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(matches!(
            s.pending_actions[0],
            Action::SendAnswer { ref answer, .. } if answer == "red; but smaller"
        ));
    }

    #[test]
    fn esc_cancels_without_emitting_answer() {
        let mut m = AskUserModal::new(
            "c".into(),
            "?".into(),
            vec!["yes".into(), "no".into()],
            false,
        );
        let mut s = AppState::new();
        let consumed = m.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(s.pending_actions.is_empty());
        assert_eq!(s.mode, AppMode::Normal);
    }
}
