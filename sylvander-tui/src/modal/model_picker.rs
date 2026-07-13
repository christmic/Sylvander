use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::{AppState, reasoning_label};
use crate::modal::{Consumed, Modal};
use crate::theme;

pub struct ModelPicker {
    cursor: usize,
    effort_index: usize,
}

impl ModelPicker {
    pub fn new(state: &AppState) -> Self {
        let cursor = state
            .metadata
            .models
            .iter()
            .position(|model| model.id == state.metadata.model)
            .unwrap_or(0);
        let effort_index = state
            .metadata
            .models
            .get(cursor)
            .and_then(|model| {
                model
                    .reasoning_efforts
                    .iter()
                    .position(|effort| *effort == state.metadata.reasoning_effort)
            })
            .unwrap_or(0);
        Self {
            cursor,
            effort_index,
        }
    }

    fn selected<'a>(&self, state: &'a AppState) -> Option<&'a sylvander_protocol::ModelDescriptor> {
        state.metadata.models.get(self.cursor)
    }

    fn reset_effort(&mut self, state: &AppState) {
        self.effort_index = self
            .selected(state)
            .and_then(|model| {
                (model.id == state.metadata.model).then(|| {
                    model
                        .reasoning_efforts
                        .iter()
                        .position(|effort| *effort == state.metadata.reasoning_effort)
                        .unwrap_or(0)
                })
            })
            .unwrap_or(0);
    }
}

impl Modal for ModelPicker {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        "Model"
    }

    fn render(&self, frame: &mut Frame, parent: Rect, state: &AppState) {
        let width = parent.width.saturating_sub(4).min(76).max(30);
        let height = (state.metadata.models.len() as u16 + 6)
            .min(parent.height.saturating_sub(4))
            .max(8);
        let area = centered(width, height, parent);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Model · applies next turn ")
                .title_style(theme::modal_title_coral()),
            area,
        );
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(2),
                Constraint::Length(1),
            ])
            .split(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("active  ", theme::text_muted()),
                Span::styled(&state.metadata.model, theme::header()),
                Span::styled("  reasoning ", theme::text_muted()),
                Span::styled(
                    reasoning_label(state.metadata.reasoning_effort),
                    theme::active_bold(),
                ),
            ])),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(rows[1].width as usize),
                theme::rule(),
            ))),
            rows[1],
        );

        let visible = rows[2].height as usize;
        let start = self.cursor.saturating_add(1).saturating_sub(visible);
        let lines = state
            .metadata
            .models
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, model)| {
                let selected = index == self.cursor;
                let active = model.id == state.metadata.model;
                let effort = if selected {
                    model.reasoning_efforts.get(self.effort_index).copied()
                } else if active {
                    Some(state.metadata.reasoning_effort)
                } else {
                    model.reasoning_efforts.first().copied()
                }
                .unwrap_or_default();
                let mut spans = vec![
                    Span::styled(if selected { "› " } else { "  " }, theme::active_bold()),
                    Span::styled(if active { "● " } else { "○ " }, theme::verified()),
                    Span::styled(
                        table_cell(&model.id, 24),
                        if selected {
                            theme::active_bold()
                        } else {
                            theme::text_dim()
                        },
                    ),
                ];
                match &model.lifecycle {
                    sylvander_protocol::ModelLifecycle::Active => {
                        spans.push(Span::styled(
                            table_cell(&model.provider, 22),
                            theme::text_muted(),
                        ));
                        spans.push(Span::styled(
                            reasoning_label(effort),
                            theme::thinking_text(),
                        ));
                    }
                    sylvander_protocol::ModelLifecycle::Deprecated { replacement } => {
                        let label = replacement
                            .as_ref()
                            .map_or_else(|| "deprecated".into(), |id| format!("deprecated → {id}"));
                        spans.push(Span::styled(label, theme::danger()));
                    }
                }
                Line::from(spans)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), rows[2]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "↑↓ model   ←→ reasoning   enter apply   esc close",
                theme::text_muted(),
            ))),
            rows[3],
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc => Consumed::Yes { dismiss: true },
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                self.reset_effort(state);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                if self.cursor + 1 < state.metadata.models.len() {
                    self.cursor += 1;
                    self.reset_effort(state);
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Left => {
                self.effort_index = self.effort_index.saturating_sub(1);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Right => {
                if let Some(model) = self.selected(state)
                    && self.effort_index + 1 < model.reasoning_efforts.len()
                {
                    self.effort_index += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                let Some(model) = self.selected(state) else {
                    return Consumed::Yes { dismiss: true };
                };
                let effort = model
                    .reasoning_efforts
                    .get(self.effort_index)
                    .copied()
                    .unwrap_or_default();
                state
                    .pending_actions
                    .push(crate::event::Action::SelectModel {
                        model: model.id.clone(),
                        reasoning_effort: effort,
                    });
                state.status = "Selecting model…".into();
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn centered(width: u16, height: u16, parent: Rect) -> Rect {
    Rect {
        x: parent.x + parent.width.saturating_sub(width) / 2,
        y: parent.y + parent.height.saturating_sub(height) / 2,
        width: width.min(parent.width),
        height: height.min(parent.height),
    }
}

fn table_cell(value: &str, width: usize) -> String {
    let count = value.chars().count();
    if count <= width {
        return format!("{value:<width$}");
    }
    let mut clipped = value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        let mut state = AppState::new();
        state.metadata.model = "plain".into();
        state.metadata.models = vec![
            sylvander_protocol::ModelDescriptor {
                id: "plain".into(),
                provider: "test".into(),
                capabilities: 0,
                reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
                lifecycle: sylvander_protocol::ModelLifecycle::Active,
            },
            sylvander_protocol::ModelDescriptor {
                id: "thinking".into(),
                provider: "test".into(),
                capabilities: 0,
                reasoning_efforts: vec![
                    sylvander_protocol::ReasoningEffort::Off,
                    sylvander_protocol::ReasoningEffort::Low,
                ],
                lifecycle: sylvander_protocol::ModelLifecycle::Deprecated {
                    replacement: Some("plain".into()),
                },
            },
        ];
        state
    }

    #[test]
    fn keyboard_selects_only_server_advertised_effort() {
        let mut state = state();
        let mut picker = ModelPicker::new(&state);
        picker.handle_key(&KeyEvent::from(KeyCode::Down), &mut state);
        picker.handle_key(&KeyEvent::from(KeyCode::Right), &mut state);
        assert_eq!(
            picker.handle_key(&KeyEvent::from(KeyCode::Enter), &mut state),
            Consumed::Yes { dismiss: true }
        );
        assert!(matches!(
            state.pending_actions.as_slice(),
            [crate::event::Action::SelectModel {
                model,
                reasoning_effort: sylvander_protocol::ReasoningEffort::Low,
            }] if model == "thinking"
        ));
    }
}
