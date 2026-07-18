use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::AppState;
use crate::modal::{Consumed, Modal, ModalPlacement, surface::focus_picker};
use crate::theme;

/// Compact Agent selection for the single-session TUI.
pub struct AgentPicker {
    cursor: usize,
}

impl AgentPicker {
    pub fn new(state: &AppState) -> Self {
        let cursor = state
            .selected_agent_id
            .as_ref()
            .and_then(|selected| state.agents.iter().position(|agent| &agent.id == selected))
            .unwrap_or(0);
        Self { cursor }
    }
}

impl Modal for AgentPicker {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Agent"
    }

    fn placement(&self, state: &AppState, _viewport_width: u16) -> ModalPlacement {
        ModalPlacement::BelowComposer {
            rows: state.agents.len().clamp(1, 8) as u16 + 4,
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, state: &AppState) {
        let areas = focus_picker(frame, parent, state.agents.len().clamp(1, 8) as u16 + 1);
        let visible = areas.results.height.saturating_sub(1) as usize;
        let start = self.cursor.saturating_add(1).saturating_sub(visible);
        let lines = state
            .agents
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, agent)| {
                let selected = index == self.cursor;
                let active = state.selected_agent_id.as_ref() == Some(&agent.id);
                Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, theme::active_bold()),
                    Span::styled(if active { "● " } else { "○ " }, theme::verified()),
                    Span::styled(
                        format!("{:<20}", agent.name),
                        if selected {
                            theme::active_bold()
                        } else {
                            theme::text()
                        },
                    ),
                    Span::styled(
                        format!("{}/{}", agent.provider_id, agent.default_model_id),
                        theme::text_muted(),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), areas.results);
        let query = state
            .agents
            .get(self.cursor)
            .map_or_else(|| "/agent".into(), |agent| format!("/agent {}", agent.id));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(query, theme::text()))),
            areas.query,
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc => Consumed::Yes { dismiss: true },
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                if self.cursor + 1 < state.agents.len() {
                    self.cursor += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                let Some(agent) = state.agents.get(self.cursor) else {
                    return Consumed::Yes { dismiss: true };
                };
                let changed = state.selected_agent_id.as_ref() != Some(&agent.id);
                state.selected_agent_id = Some(agent.id.clone());
                state.metadata.models.clone_from(&agent.models);
                state.metadata.model.clone_from(&agent.default_model_id);
                state.session_model_override = None;
                if changed && state.session_id.is_some() {
                    state.session_id = None;
                    state.session_config = None;
                    state.session_creation_pending = false;
                    state.messages.clear();
                    state.welcomed = false;
                    state.status =
                        format!("{} selected · next prompt starts a new session", agent.name);
                } else {
                    state.status = format!("{} selected", agent.name);
                }
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Ignored,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/modal_agent_picker.rs"]
mod tests;
