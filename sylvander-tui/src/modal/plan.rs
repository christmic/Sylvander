//! Plan review modal — UX §9 + §12.2.
//!
//! Triggered by `DomainEvent::PlanReceived`. Two sub-modes:
//! - **Navigate** (default): user can approve, edit the focused step,
//!   add a step after the cursor, remove a step, or cancel.
//! - **Edit**: user types replacement text for the focused step. Enter
//!   commits the edit and returns to Navigate; Esc cancels the edit.
//!
//! Approve emits `Action::SendFeedback` carrying the (possibly edited)
//! plan steps, so the agent receives the revised plan as a follow-up
//! user message. This avoids touching the wire protocol just for plan
//! signaling (the agent loop is responsible for plan semantics).

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanMode {
    Navigate,
    Edit,
}

pub struct PlanReviewModal {
    plan_id: String,
    steps: Vec<String>,
    cursor: usize,
    /// Step currently being edited (only when `mode == Edit`).
    edit_step: usize,
    /// Edit buffer for the focused step.
    edit_buffer: String,
    mode: PlanMode,
    session_id: Option<String>,
}

impl PlanReviewModal {
    pub fn new(
        plan_id: String,
        steps: Vec<String>,
        current: usize,
        session_id: Option<String>,
    ) -> Self {
        Self {
            plan_id,
            steps,
            cursor: current.min(0),
            edit_step: 0,
            edit_buffer: String::new(),
            mode: PlanMode::Navigate,
            session_id,
        }
    }

    fn render_steps(&self, area: Rect, frame: &mut Frame) {
        let lines: Vec<Line> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                let is_cursor = i == self.cursor;
                let (marker, color) = if is_cursor {
                    ("● ", Color::Cyan)
                } else {
                    ("  ", Color::DarkGray)
                };
                Line::from(vec![
                    Span::styled(marker, Style::default().fg(color).bold()),
                    Span::styled(
                        format!("{}. {}", i + 1, step),
                        Style::default().fg(if is_cursor { Color::White } else { Color::Gray }),
                    ),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }
}

impl Modal for PlanReviewModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        if self.mode == PlanMode::Edit {
            "Plan · Edit step"
        } else {
            "Plan review"
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let popup_area = centered_rect(70, 16, parent);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Plan review ({} steps) ", self.steps.len()))
                .title_style(Style::default().fg(Color::Yellow)),
            popup_area,
        );

        let inner = Block::default().borders(Borders::ALL).inner(popup_area);
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Min(5),    // steps
                Constraint::Length(2), // edit buffer (when in edit mode)
                Constraint::Length(1), // footer
            ])
            .split(inner);

        // 1. Header
        let intro = Line::from(match self.mode {
            PlanMode::Navigate => {
                "Review the plan — enter approves, e edits the focused step, a adds, d removes, esc cancels."
            }
            PlanMode::Edit => {
                "Editing a step. Enter commits; Esc discards the edit and returns to review."
            }
        });
        frame.render_widget(
            Paragraph::new(intro).wrap(Wrap { trim: false }),
            layout[0],
        );

        // 2. Steps
        self.render_steps(layout[1], frame);

        // 3. Edit buffer (only when editing)
        if self.mode == PlanMode::Edit {
            let prompt = Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Green)),
                Span::styled(&self.edit_buffer, Style::default()),
                Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ]);
            frame.render_widget(Paragraph::new(prompt), layout[2]);
            // Hardware cursor at end of edit buffer.
            let cursor_x = inner.x + 2 + self.edit_buffer.chars().count() as u16;
            let cursor_y = inner.y + layout[2].y - inner.y + (layout[2].height / 2);
            // Best-effort cursor positioning — modal is centered so the
            // exact row depends on layout; we just put it on layout[2]'s
            // first row.
            let y_abs = inner.y
                + layout[0].height
                + layout[1].height
                + (layout[2].height / 2).min(0);
            let _ = cursor_y;
            if cursor_x < inner.x + inner.width
                && y_abs < inner.y + inner.height
            {
                frame.set_cursor_position((cursor_x, y_abs));
            }
        }

        // 4. Footer
        let footer = match self.mode {
            PlanMode::Navigate => "enter=approve  e=edit  a=add  d=remove  esc=cancel",
            PlanMode::Edit => "enter=commit  esc=cancel edit",
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                footer,
                Style::default().fg(Color::DarkGray),
            )),
            layout[3],
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match self.mode {
            PlanMode::Navigate => self.handle_navigate_key(key, state),
            PlanMode::Edit => self.handle_edit_key(key, state),
        }
    }
}

use ratatui::style::Modifier;

impl PlanReviewModal {
    fn handle_navigate_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
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
                if self.cursor + 1 < self.steps.len() {
                    self.cursor += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                // Approve: send the (possibly edited) plan back as feedback.
                let body = self.steps.join("\n  - ");
                let text = format!("[plan-approve]\nPlan approved:\n  - {body}");
                state.pending_actions.push(Action::SendFeedback {
                    text,
                    session_id: self.session_id.clone(),
                });
                state.mode = AppMode::Normal;
                state.dirty.mark();
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('e') => {
                // Enter edit mode for the focused step.
                self.edit_step = self.cursor;
                self.edit_buffer = self.steps[self.cursor].clone();
                self.mode = PlanMode::Edit;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('a') => {
                // Add a step after the cursor.
                self.steps.insert(self.cursor + 1, String::from("(new step)"));
                self.cursor += 1;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('d') => {
                if self.steps.len() > 1 {
                    self.steps.remove(self.cursor);
                    if self.cursor >= self.steps.len() {
                        self.cursor = self.steps.len() - 1;
                    }
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }

    fn handle_edit_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc => {
                // Discard the edit, go back to navigate.
                self.mode = PlanMode::Navigate;
                self.edit_buffer.clear();
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                // Commit the edit to the focused step.
                if self.edit_step < self.steps.len() {
                    self.steps[self.edit_step] = std::mem::take(&mut self.edit_buffer);
                }
                self.mode = PlanMode::Navigate;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                if !self.edit_buffer.is_empty() {
                    self.edit_buffer.pop();
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.edit_buffer.push(c);
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(c, m)
    }

    fn build_modal(n: usize) -> PlanReviewModal {
        let steps: Vec<String> = (1..=n).map(|i| format!("step {i}")).collect();
        PlanReviewModal::new("p1".into(), steps, 0, Some("s1".into()))
    }

    #[test]
    fn enter_sends_feedback_with_bracketed_prefix() {
        let mut state = AppState::new();
        let mut m = build_modal(3);
        // Cursor on step 0 (default). Approve.
        let consumed = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert_eq!(state.pending_actions.len(), 1);
        assert!(matches!(
            state.pending_actions[0],
            Action::SendFeedback { ref text, .. } if text.starts_with("[plan-approve]")
        ));
    }

    #[test]
    fn edit_mode_round_trip_through_enter() {
        let mut state = AppState::new();
        let mut m = build_modal(2);
        // Press 'e' to edit step 0 — buffer is initialized with the existing
        // step text so the user can refine rather than retype.
        let _ = m.handle_key(&key(KeyCode::Char('e'), KeyModifiers::NONE), &mut state);
        assert_eq!(m.mode, PlanMode::Edit);
        assert_eq!(m.edit_step, 0);
        assert_eq!(m.edit_buffer, "step 1");
        // Backspace the whole "step 1" to clear the buffer (6 chars).
        for _ in 0..6 {
            let _ = m.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE), &mut state);
        }
        // Now type the replacement.
        for ch in "rewritten step".chars() {
            let _ = m.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE), &mut state);
        }
        // Commit.
        let _ = m.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert_eq!(m.mode, PlanMode::Navigate);
        assert_eq!(m.steps[0], "rewritten step");
    }

    #[test]
    fn edit_mode_esc_cancels_without_commit() {
        let mut state = AppState::new();
        let mut m = build_modal(2);
        let _ = m.handle_key(&key(KeyCode::Char('e'), KeyModifiers::NONE), &mut state);
        for ch in "thrown away".chars() {
            let _ = m.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE), &mut state);
        }
        let _ = m.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE), &mut state);
        assert_eq!(m.mode, PlanMode::Navigate);
        // Original step text untouched.
        assert_eq!(m.steps[0], "step 1");
    }

    #[test]
    fn a_adds_after_cursor_d_removes_at_cursor() {
        let mut state = AppState::new();
        let mut m = build_modal(2);
        let _ = m.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE), &mut state);
        assert_eq!(m.steps.len(), 3);
        assert_eq!(m.cursor, 1);
        // Cursor on the new (empty-named) step; remove it.
        let _ = m.handle_key(&key(KeyCode::Char('d'), KeyModifiers::NONE), &mut state);
        assert_eq!(m.steps.len(), 2);
    }

    #[test]
    fn esc_cancels_review() {
        let mut state = AppState::new();
        let mut m = build_modal(2);
        let consumed = m.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE), &mut state);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert_eq!(state.mode, AppMode::Normal);
        assert!(state.pending_actions.is_empty());
    }
}
