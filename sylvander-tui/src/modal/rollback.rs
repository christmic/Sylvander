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

pub struct WorkspaceRollbackModal {
    session_id: String,
    preview: sylvander_protocol::WorkspaceRollbackPreview,
    choice_index: usize,
}

impl WorkspaceRollbackModal {
    pub fn new(session_id: String, preview: sylvander_protocol::WorkspaceRollbackPreview) -> Self {
        Self {
            session_id,
            preview,
            choice_index: 0,
        }
    }

    fn confirm(&self, state: &mut AppState) -> Consumed {
        state
            .pending_actions
            .push(crate::event::Action::ConfirmWorkspaceRollback {
                session_id: self.session_id.clone(),
                expected_turn_id: self.preview.turn_id.clone(),
            });
        state.status = "Rolling back Agent file changes…".into();
        Consumed::Yes { dismiss: true }
    }
}

impl Modal for WorkspaceRollbackModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Rollback files"
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        ModalPlacement::BelowComposer {
            rows: 7u16.saturating_add(self.preview.files.len() as u16),
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let body = decision_dock(frame, parent, 6 + self.preview.files.len() as u16);
        let mut lines = vec![
            Line::from(Span::styled(
                "◆ Restore Agent file changes?",
                theme::danger().bold(),
            )),
            Line::from(Span::styled(
                "Conversation history stays unchanged. External file changes can conflict.",
                theme::text_muted(),
            )),
            Line::default(),
        ];
        lines.extend(self.preview.files.iter().map(|path| {
            Line::from(vec![
                Span::styled("  ↶ ", theme::active()),
                Span::styled(path, theme::text()),
            ])
        }));
        lines.push(Line::default());
        for (index, label) in ["Keep current files", "Restore the files listed above"]
            .iter()
            .enumerate()
        {
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
                state.status = "File rollback cancelled".into();
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Yes { dismiss: false },
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/modal_rollback.rs"]
mod tests;
