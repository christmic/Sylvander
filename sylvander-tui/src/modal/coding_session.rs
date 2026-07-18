use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::AppState;
use crate::modal::{Consumed, Modal, ModalPlacement, surface::decision_dock};
use crate::theme;

#[derive(Clone, Copy)]
enum Decision {
    Accept,
    Discard,
}

pub struct CodingSessionConfirmationModal {
    session_id: String,
    decision: Decision,
    choice_index: usize,
}

impl CodingSessionConfirmationModal {
    pub fn accept(session_id: String) -> Self {
        Self {
            session_id,
            decision: Decision::Accept,
            choice_index: 0,
        }
    }

    pub fn discard(session_id: String) -> Self {
        Self {
            session_id,
            decision: Decision::Discard,
            choice_index: 0,
        }
    }

    fn confirm(&self, state: &mut AppState) -> Consumed {
        let action = match self.decision {
            Decision::Accept => crate::event::Action::AcceptCodingSession {
                session_id: self.session_id.clone(),
            },
            Decision::Discard => crate::event::Action::DiscardCodingSession {
                session_id: self.session_id.clone(),
            },
        };
        state.pending_actions.push(action);
        state.status = match self.decision {
            Decision::Accept => "Merging reviewed coding changes…".into(),
            Decision::Discard => "Discarding coding session…".into(),
        };
        Consumed::Yes { dismiss: true }
    }
}

impl Modal for CodingSessionConfirmationModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        match self.decision {
            Decision::Accept => "Accept changes",
            Decision::Discard => "Discard session",
        }
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        ModalPlacement::BelowComposer { rows: 7 }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let body = decision_dock(frame, parent, 6);
        let (question, warning, confirm) = match self.decision {
            Decision::Accept => (
                "◆ Merge reviewed coding changes?",
                "The worktree is committed and merged into its source branch.",
                "Merge changes",
            ),
            Decision::Discard => (
                "◆ Delete this coding session?",
                "The isolated worktree and its unmerged changes are permanently removed.",
                "Discard session",
            ),
        };
        let mut lines = vec![
            Line::from(Span::styled(question, theme::danger().bold())),
            Line::from(Span::styled(warning, theme::text_muted())),
            Line::default(),
        ];
        for (index, label) in ["Cancel", confirm].iter().enumerate() {
            let selected = index == self.choice_index;
            let style = if selected && index == 0 {
                theme::brand_violet().bold()
            } else if selected {
                theme::danger().bold()
            } else {
                theme::text()
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "{}{}. {label}",
                    if selected { "› " } else { "  " },
                    index + 1
                ),
                style,
            )));
        }
        frame.render_widget(Paragraph::new(lines), body);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Up | KeyCode::Char('1') => {
                self.choice_index = 0;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down | KeyCode::Char('2') => {
                self.choice_index = 1;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter if self.choice_index == 1 => self.confirm(state),
            KeyCode::Char('y') => self.confirm(state),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n') => {
                state.status = "Coding session operation cancelled".into();
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Yes { dismiss: false },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/modal_coding_session.rs"]
mod tests;
