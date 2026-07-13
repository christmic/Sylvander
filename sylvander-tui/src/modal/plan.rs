//! Plan interaction surfaces — Decision Dock first, Review View on demand.
//!
//! The plan already lives in transcript history. Receiving a plan therefore
//! opens only a short decision surface. Explicit revision temporarily owns the
//! transcript viewport and returns to the same decision when editing is done.

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
use crate::modal::{
    Consumed, Modal, ModalPlacement,
    surface::{decision_dock, review_view},
};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanMode {
    Decision,
    Review,
    EditStep,
}

pub struct PlanReviewModal {
    plan_id: String,
    steps: Vec<String>,
    original_steps: Vec<String>,
    cursor: usize,
    decision_index: usize,
    edit_step: usize,
    edit_buffer: String,
    mode: PlanMode,
    _session_id: Option<String>,
}

impl PlanReviewModal {
    pub fn new(
        plan_id: String,
        steps: Vec<String>,
        current: usize,
        session_id: Option<String>,
    ) -> Self {
        let cursor = current.min(steps.len().saturating_sub(1));
        Self {
            plan_id,
            original_steps: steps.clone(),
            steps,
            cursor,
            decision_index: 0,
            edit_step: 0,
            edit_buffer: String::new(),
            mode: PlanMode::Decision,
            _session_id: session_id,
        }
    }

    fn render_decision(&self, frame: &mut Frame, parent: Rect) {
        let body = decision_dock(frame, parent, 5);
        let count = format!("{} steps", self.steps.len());
        let title = "◆ Ready to proceed?";
        let gap = (body.width as usize)
            .saturating_sub(UnicodeWidthStr::width(title) + UnicodeWidthStr::width(&*count));
        let first = if self.steps == self.original_steps {
            "Start implementation"
        } else {
            "Use the revised plan"
        };
        let choices = [first, "Revise the plan", "Cancel"];
        let mut lines = vec![
            Line::from(vec![
                Span::styled(title, theme::brand_violet().bold()),
                Span::raw(" ".repeat(gap)),
                Span::styled(count, theme::text_muted()),
            ]),
            Line::from(""),
        ];
        for (index, choice) in choices.iter().enumerate() {
            let selected = self.decision_index == index;
            let style = if selected && index == 2 {
                theme::danger().bold()
            } else if selected {
                theme::brand_violet().bold()
            } else {
                theme::text()
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "{}{}. {choice}",
                    if selected { "› " } else { "  " },
                    index + 1
                ),
                style,
            )));
        }
        frame.render_widget(Paragraph::new(lines), body);
    }

    fn render_editor(&self, frame: &mut Frame, parent: Rect) {
        let footer_rows = u16::from(self.mode == PlanMode::EditStep) + 1;
        let areas = review_view(frame, parent, footer_rows);
        let title = "Plan editor";
        let count = format!("{} steps", self.steps.len());
        let gap = (areas.header.width as usize)
            .saturating_sub(UnicodeWidthStr::width(title) + UnicodeWidthStr::width(&*count));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(title, theme::brand_violet().bold()),
                Span::raw(" ".repeat(gap)),
                Span::styled(count, theme::text_muted()),
            ])),
            areas.header,
        );

        let visible = areas.body.height as usize;
        let start = self
            .cursor
            .saturating_sub(visible.saturating_sub(1).min(visible / 2));
        let lines = self
            .steps
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, step)| {
                let selected = index == self.cursor;
                Line::from(vec![
                    Span::styled(
                        if selected { "› " } else { "  " },
                        if selected {
                            theme::brand_violet().bold()
                        } else {
                            theme::text_muted()
                        },
                    ),
                    Span::styled(
                        format!("{}. {step}", index + 1),
                        if selected {
                            theme::text().bold()
                        } else {
                            theme::text_dim()
                        },
                    ),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), areas.body);

        if self.mode == PlanMode::EditStep {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled("> ", theme::brand_violet()),
                        Span::styled(&self.edit_buffer, theme::text()),
                    ]),
                    Line::from(Span::styled("↵ save   esc discard", theme::text_muted())),
                ]),
                areas.footer,
            );
            let x = areas.footer.x + 2 + UnicodeWidthStr::width(self.edit_buffer.as_str()) as u16;
            if x < areas.footer.x + areas.footer.width {
                frame.set_cursor_position((x, areas.footer.y));
            }
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "e edit step   a add below   d remove",
                    theme::text_muted(),
                )),
                areas.footer,
            );
        }
    }

    fn approve(&mut self, state: &mut AppState) -> Consumed {
        let decision = if self.steps == self.original_steps {
            sylvander_protocol::PlanDecision::Approved
        } else {
            sylvander_protocol::PlanDecision::Revised {
                steps: self.steps.clone(),
            }
        };
        state.pending_actions.push(Action::ResolvePlan {
            session_id: state.session_id.clone().unwrap_or_default(),
            plan_id: self.plan_id.clone(),
            decision,
        });
        state.mode = AppMode::Normal;
        state.dirty.mark();
        Consumed::Yes { dismiss: true }
    }

    fn reject(&mut self, state: &mut AppState) -> Consumed {
        state.pending_actions.push(Action::ResolvePlan {
            session_id: state.session_id.clone().unwrap_or_default(),
            plan_id: self.plan_id.clone(),
            decision: sylvander_protocol::PlanDecision::Rejected {
                reason: "cancelled by user".into(),
            },
        });
        state.mode = AppMode::Normal;
        Consumed::Yes { dismiss: true }
    }

    fn handle_decision_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Up if self.decision_index > 0 => {
                self.decision_index -= 1;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down if self.decision_index < 2 => {
                self.decision_index += 1;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('e') => {
                self.mode = PlanMode::Review;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter if self.decision_index == 1 => {
                self.mode = PlanMode::Review;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter if self.decision_index == 0 => self.approve(state),
            KeyCode::Esc | KeyCode::Enter => self.reject(state),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.reject(state)
            }
            KeyCode::Char(number @ '1'..='3') => {
                self.decision_index = number as usize - '1' as usize;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Yes { dismiss: false },
        }
    }

    fn handle_review_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.mode = PlanMode::Decision;
                self.decision_index = 0;
                state.dirty.mark();
            }
            KeyCode::Up if self.cursor > 0 => self.cursor -= 1,
            KeyCode::Down if self.cursor + 1 < self.steps.len() => self.cursor += 1,
            KeyCode::Char('e') if !self.steps.is_empty() => {
                self.edit_step = self.cursor;
                self.edit_buffer = self.steps[self.cursor].clone();
                self.mode = PlanMode::EditStep;
            }
            KeyCode::Char('a') => {
                self.steps.insert(self.cursor + 1, "(new step)".into());
                self.cursor += 1;
            }
            KeyCode::Char('d') if self.steps.len() > 1 => {
                self.steps.remove(self.cursor);
                self.cursor = self.cursor.min(self.steps.len() - 1);
            }
            _ => return Consumed::Yes { dismiss: false },
        }
        state.dirty.mark();
        Consumed::Yes { dismiss: false }
    }

    fn handle_edit_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc => {
                self.mode = PlanMode::Review;
                self.edit_buffer.clear();
            }
            KeyCode::Enter => {
                if self.edit_step < self.steps.len() && !self.edit_buffer.trim().is_empty() {
                    self.steps[self.edit_step] = std::mem::take(&mut self.edit_buffer);
                }
                self.mode = PlanMode::Review;
            }
            KeyCode::Backspace => {
                self.edit_buffer.pop();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.edit_buffer.push(character);
            }
            _ => return Consumed::Yes { dismiss: false },
        }
        state.dirty.mark();
        Consumed::Yes { dismiss: false }
    }
}

impl Modal for PlanReviewModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        match self.mode {
            PlanMode::Decision => "Plan review",
            PlanMode::Review => "Plan editor",
            PlanMode::EditStep => "Plan · Edit step",
        }
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        match self.mode {
            PlanMode::Decision => ModalPlacement::BelowComposer { rows: 6 },
            PlanMode::Review | PlanMode::EditStep => ModalPlacement::Overlay,
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        match self.mode {
            PlanMode::Decision => self.render_decision(frame, parent),
            PlanMode::Review | PlanMode::EditStep => self.render_editor(frame, parent),
        }
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match self.mode {
            PlanMode::Decision => self.handle_decision_key(key, state),
            PlanMode::Review => self.handle_review_key(key, state),
            PlanMode::EditStep => self.handle_edit_key(key, state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn build_modal(n: usize) -> PlanReviewModal {
        PlanReviewModal::new(
            "p1".into(),
            (1..=n).map(|index| format!("step {index}")).collect(),
            0,
            Some("s1".into()),
        )
    }

    #[test]
    fn enter_approves_through_typed_action() {
        let mut state = AppState::new();
        let mut modal = build_modal(3);
        let consumed = modal.handle_key(&key(KeyCode::Enter), &mut state);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert!(matches!(
            state.pending_actions[0],
            Action::ResolvePlan {
                decision: sylvander_protocol::PlanDecision::Approved,
                ..
            }
        ));
    }

    #[test]
    fn revision_is_explicit_and_returns_to_decision() {
        let mut state = AppState::new();
        let mut modal = build_modal(2);
        modal.handle_key(&key(KeyCode::Char('e')), &mut state);
        assert_eq!(modal.mode, PlanMode::Review);
        modal.handle_key(&key(KeyCode::Char('e')), &mut state);
        assert_eq!(modal.mode, PlanMode::EditStep);
        for _ in 0..6 {
            modal.handle_key(&key(KeyCode::Backspace), &mut state);
        }
        for character in "safer step".chars() {
            modal.handle_key(&key(KeyCode::Char(character)), &mut state);
        }
        modal.handle_key(&key(KeyCode::Enter), &mut state);
        modal.handle_key(&key(KeyCode::Enter), &mut state);
        assert_eq!(modal.mode, PlanMode::Decision);
        modal.handle_key(&key(KeyCode::Enter), &mut state);
        assert!(matches!(
            &state.pending_actions[0],
            Action::ResolvePlan {
                decision: sylvander_protocol::PlanDecision::Revised { steps },
                ..
            } if steps[0] == "safer step"
        ));
    }

    #[test]
    fn escape_rejects_instead_of_abandoning_waiter() {
        let mut state = AppState::new();
        let mut modal = build_modal(2);
        modal.handle_key(&key(KeyCode::Esc), &mut state);
        assert!(matches!(
            &state.pending_actions[0],
            Action::ResolvePlan {
                decision: sylvander_protocol::PlanDecision::Rejected { .. },
                ..
            }
        ));
    }

    #[test]
    fn review_can_add_and_remove_steps_without_resolving_gate() {
        let mut state = AppState::new();
        let mut modal = build_modal(2);
        modal.handle_key(&key(KeyCode::Char('e')), &mut state);
        modal.handle_key(&key(KeyCode::Char('a')), &mut state);
        assert_eq!(modal.steps.len(), 3);
        modal.handle_key(&key(KeyCode::Char('d')), &mut state);
        assert_eq!(modal.steps.len(), 2);
        assert!(state.pending_actions.is_empty());
    }
}
