//! AskUser Decision Dock — model asks the user a clarifying question.
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
    Frame,
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppMode, AppState};
use crate::event::Action;
use crate::modal::{Consumed, Modal, surface::decision_dock};
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
    selection: Selection,
    /// Cursor for single-select / multi-select navigation.
    pub cursor: usize,
    /// Free-text answer the user has typed (always available).
    pub answer: String,
    /// Whether the synthetic `Other…` row owns text input.
    pub editing_other: bool,
    pub validation_error: Option<String>,
}

impl AskUserModal {
    pub fn new(
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    ) -> Self {
        let free_text = options.is_empty();
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
            editing_other: free_text,
            validation_error: None,
        }
    }

    fn other_index(&self) -> usize {
        self.options.len()
    }

    fn on_other_row(&self) -> bool {
        self.options.is_empty() || self.cursor == self.other_index()
    }

    /// Compose the answer string. Format chosen for easy parsing by the
    /// agent's `ask_user` handler: joined options first, free text after `; `.
    fn submit_answer(&self) -> String {
        match &self.selection {
            Selection::None if !self.options.is_empty() && !self.multi_select => {
                let Some(option) = self.options.get(self.cursor).cloned() else {
                    return self.answer.trim().to_string();
                };
                if self.answer.trim().is_empty() {
                    option
                } else {
                    format!("{option}; {}", self.answer.trim())
                }
            }
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
        let choice_rows = self.options.len().saturating_add(1) as u16;
        let error_rows = u16::from(self.validation_error.is_some());
        let body = decision_dock(frame, parent, 4 + choice_rows + error_rows);
        let heading = if self.options.is_empty() {
            "◆ Your input is needed"
        } else if self.multi_select {
            "◆ Choose any that apply"
        } else {
            "◆ One choice needed"
        };
        let mut lines = vec![
            Line::from(Span::styled(heading, theme::brand_violet().bold())),
            Line::from(Span::styled(
                wrap_text(&self.question, body.width as usize),
                theme::text().bold(),
            )),
            Line::from(""),
        ];

        for (index, option) in self.options.iter().enumerate() {
            let cursor = self.cursor == index && !self.editing_other;
            let selected = match &self.selection {
                Selection::Single(selected) => *selected == index,
                Selection::Multi(mask) => mask.get(index).copied().unwrap_or(false),
                Selection::None => false,
            };
            let style = if cursor {
                theme::brand_violet().bold()
            } else if selected {
                theme::verified()
            } else {
                theme::text()
            };
            let marker = if self.multi_select {
                if selected { "[✓]" } else { "[ ]" }
            } else {
                ""
            };
            let label = if marker.is_empty() {
                format!(
                    "{}{}. {}",
                    if cursor { "› " } else { "  " },
                    index + 1,
                    option
                )
            } else {
                format!(
                    "{}{} {}. {}",
                    if cursor { "› " } else { "  " },
                    marker,
                    index + 1,
                    option
                )
            };
            lines.push(Line::from(Span::styled(
                truncate_for_display(&label, body.width as usize),
                style,
            )));
        }

        let other_row = lines.len() as u16;
        let other_cursor = self.on_other_row();
        let other_style = if other_cursor {
            theme::brand_violet().bold()
        } else {
            theme::text()
        };
        let other_prefix = if self.options.is_empty() {
            "> ".to_string()
        } else {
            format!(
                "{}{}. Other… ",
                if other_cursor { "› " } else { "  " },
                self.other_index() + 1
            )
        };
        lines.push(Line::from(vec![
            Span::styled(&other_prefix, other_style),
            Span::styled(&self.answer, theme::text()),
        ]));

        if let Some(error) = &self.validation_error {
            lines.push(Line::from(Span::styled(
                format!("! {error}"),
                theme::warning(),
            )));
        }
        lines.push(Line::from(Span::styled(
            if self.editing_other {
                "Type your answer · ↵ submit · esc return"
            } else if self.multi_select {
                "Space toggles · type for Other…"
            } else {
                "Type to answer with Other…"
            },
            theme::text_muted(),
        )));

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

        if self.editing_other {
            let cursor_x = body.x
                + UnicodeWidthStr::width(other_prefix.as_str()) as u16
                + UnicodeWidthStr::width(self.answer.as_str()) as u16;
            let cursor_y = body.y + other_row;
            if cursor_x < body.x + body.width && cursor_y < body.y + body.height {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc if self.editing_other && !self.options.is_empty() => {
                self.editing_other = false;
                self.validation_error = None;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Esc => self.cancel(state),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel(state)
            }
            KeyCode::Up => {
                if self.editing_other && !self.options.is_empty() {
                    self.editing_other = false;
                    self.cursor = self.options.len().saturating_sub(1);
                    state.dirty.mark();
                } else if self.cursor > 0 {
                    self.cursor -= 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                if !self.editing_other && self.cursor < self.other_index() {
                    self.cursor += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char(' ') if self.multi_select && !self.editing_other => {
                if self.on_other_row() {
                    self.editing_other = true;
                } else if let Selection::Multi(ref mut mask) = self.selection {
                    if self.cursor < mask.len() {
                        mask[self.cursor] = !mask[self.cursor];
                    }
                }
                self.validation_error = None;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                if self.on_other_row() && !self.editing_other && !self.options.is_empty() {
                    self.editing_other = true;
                    state.dirty.mark();
                    return Consumed::Yes { dismiss: false };
                }
                if !self.multi_select && !self.on_other_row() {
                    self.selection = Selection::Single(self.cursor);
                }
                self.submit(state)
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT)
                {
                    return Consumed::Ignored;
                }
                // Numeric jump for both single and multi.
                if !self.editing_other
                    && let Some(d) = c.to_digit(10)
                {
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
                        self.validation_error = None;
                        return Consumed::Yes { dismiss: false };
                    }
                }
                // Typing switches directly to the inline Other editor while
                // preserving any already selected option(s).
                self.cursor = self.other_index();
                self.editing_other = true;
                self.answer.push(c);
                self.validation_error = None;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                if !self.answer.is_empty() {
                    self.answer.pop();
                } else if !self.options.is_empty() {
                    self.editing_other = false;
                }
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
}

impl AskUserModal {
    fn submit(&mut self, state: &mut AppState) -> Consumed {
        let answer = self.submit_answer();
        if answer.is_empty() {
            self.validation_error =
                Some("Choose an option, type an answer, or press Esc to skip".into());
            state.dirty.mark();
            return Consumed::Yes { dismiss: false };
        }
        let call_id = std::mem::take(&mut self.call_id);
        state.mode = AppMode::Normal;
        state.pending_actions.push(Action::SendAnswer {
            session_id: state.session_id.clone().unwrap_or_default(),
            call_id,
            answer: answer.clone(),
        });
        state.messages.push(crate::app::ChatMessage::Info(format!(
            "answered · {answer}"
        )));
        Consumed::Yes { dismiss: true }
    }

    fn cancel(&mut self, state: &mut AppState) -> Consumed {
        let call_id = std::mem::take(&mut self.call_id);
        state.pending_actions.push(Action::SendAnswer {
            session_id: state.session_id.clone().unwrap_or_default(),
            call_id,
            answer: String::new(),
        });
        state
            .messages
            .push(crate::app::ChatMessage::Info("question cancelled".into()));
        state.mode = AppMode::Normal;
        Consumed::Yes { dismiss: true }
    }
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

fn truncate_for_display(s: &str, max: usize) -> String {
    wrap_text(s, max)
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
            Action::SendAnswer { ref call_id, ref answer, .. } if call_id == "c" && answer == "make it blue"
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
    fn esc_cancels_and_unblocks_the_agent() {
        let mut m = AskUserModal::new(
            "c".into(),
            "?".into(),
            vec!["yes".into(), "no".into()],
            false,
        );
        let mut s = AppState::new();
        let consumed = m.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(matches!(
            s.pending_actions.as_slice(),
            [Action::SendAnswer { call_id, answer, .. }] if call_id == "c" && answer.is_empty()
        ));
        assert_eq!(s.mode, AppMode::Normal);
    }
}
