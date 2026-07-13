use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::AppState;
use crate::modal::{Consumed, Modal};
use crate::theme;

pub struct WorkspaceRollbackModal {
    session_id: String,
    preview: sylvander_protocol::WorkspaceRollbackPreview,
}

impl WorkspaceRollbackModal {
    pub fn new(session_id: String, preview: sylvander_protocol::WorkspaceRollbackPreview) -> Self {
        Self {
            session_id,
            preview,
        }
    }
}

impl Modal for WorkspaceRollbackModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        "Rollback files"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let width = parent.width.saturating_sub(4).min(76).max(36);
        let height = (self.preview.files.len() as u16 + 9)
            .min(parent.height.saturating_sub(2))
            .max(10);
        let area = Rect {
            x: parent.x + parent.width.saturating_sub(width) / 2,
            y: parent.y + parent.height.saturating_sub(height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Rollback · latest Agent file turn ")
                .title_style(theme::modal_title_coral()),
            area,
        );
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let mut lines = vec![
            Line::from(Span::styled(
                "Restore these files to their pre-turn contents?",
                theme::header(),
            )),
            Line::from(Span::styled(
                "Only Agent-journaled Write/Edit changes are included.",
                theme::text_muted(),
            )),
            Line::from(Span::styled(
                "External changes cause a conflict; conversation history is unchanged.",
                theme::warning(),
            )),
            Line::default(),
        ];
        lines.extend(
            self.preview.files.iter().map(|path| {
                Line::from(vec![Span::styled("  ↶ ", theme::active()), Span::raw(path)])
            }),
        );
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "enter / y confirm    esc / n cancel",
            theme::text_muted(),
        )));
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => {
                state
                    .pending_actions
                    .push(crate::event::Action::ConfirmWorkspaceRollback {
                        session_id: self.session_id.clone(),
                        expected_turn_id: self.preview.turn_id.clone(),
                    });
                state.status = "Rolling back Agent file changes…".into();
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                state.status = "File rollback cancelled".into();
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Yes { dismiss: false },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_carries_the_preview_turn_id() {
        let mut state = AppState::new();
        let mut modal = WorkspaceRollbackModal::new(
            "s1".into(),
            sylvander_protocol::WorkspaceRollbackPreview {
                turn_id: "turn-7".into(),
                files: vec!["src/lib.rs".into()],
            },
        );
        assert_eq!(
            modal.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
            Consumed::Yes { dismiss: true }
        );
        assert!(matches!(
            state.pending_actions.as_slice(),
            [crate::event::Action::ConfirmWorkspaceRollback {
                expected_turn_id,
                ..
            }] if expected_turn_id == "turn-7"
        ));
    }
}
