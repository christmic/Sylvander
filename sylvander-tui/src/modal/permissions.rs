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

pub struct PermissionsPicker {
    row: usize,
    profile: sylvander_protocol::PermissionProfile,
}

impl PermissionsPicker {
    pub fn new(state: &AppState) -> Self {
        Self {
            row: 0,
            profile: state.metadata.permissions.clone(),
        }
    }

    fn cycle(&mut self, forward: bool, approval_available: bool) {
        match self.row {
            0 => {
                const VALUES: [sylvander_protocol::FileAccess; 3] = [
                    sylvander_protocol::FileAccess::None,
                    sylvander_protocol::FileAccess::ReadOnly,
                    sylvander_protocol::FileAccess::WorkspaceWrite,
                ];
                self.profile.file_access = next(&VALUES, self.profile.file_access, forward);
            }
            1 => {
                const VALUES: [sylvander_protocol::NetworkAccess; 2] = [
                    sylvander_protocol::NetworkAccess::Denied,
                    sylvander_protocol::NetworkAccess::Allowed,
                ];
                self.profile.network_access = next(&VALUES, self.profile.network_access, forward);
            }
            _ => {
                let values = if approval_available {
                    vec![
                        sylvander_protocol::ApprovalPolicy::Ask,
                        sylvander_protocol::ApprovalPolicy::Allow,
                        sylvander_protocol::ApprovalPolicy::Deny,
                    ]
                } else {
                    vec![
                        sylvander_protocol::ApprovalPolicy::Allow,
                        sylvander_protocol::ApprovalPolicy::Deny,
                    ]
                };
                self.profile.approval_policy = next(&values, self.profile.approval_policy, forward);
            }
        }
    }
}

fn next<T: Copy + PartialEq>(values: &[T], current: T, forward: bool) -> T {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    if forward {
        values[(index + 1) % values.len()]
    } else {
        values[(index + values.len() - 1) % values.len()]
    }
}

impl Modal for PermissionsPicker {
    fn active(&self) -> bool {
        true
    }
    fn title(&self) -> &'static str {
        "Permissions"
    }

    fn placement(&self, state: &AppState, _viewport_width: u16) -> ModalPlacement {
        ModalPlacement::BelowComposer {
            rows: if state.metadata.approval_enabled {
                9
            } else {
                10
            },
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, state: &AppState) {
        let result_rows = if state.metadata.approval_enabled {
            6
        } else {
            7
        };
        let areas = focus_picker(frame, parent, result_rows);
        let workspace = state.metadata.workspace.display().to_string();
        let rows = [
            ("filesystem", file_label(self.profile.file_access)),
            ("network", network_label(self.profile.network_access)),
            ("approval", approval_label(self.profile.approval_policy)),
        ];
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Permissions", theme::brand_violet()),
                Span::styled(
                    " · workspace scoped · applies next turn",
                    theme::text_muted(),
                ),
            ]),
            Line::from(vec![
                Span::styled("root  ", theme::text_muted()),
                Span::styled(workspace, theme::text_dim()),
            ]),
        ];
        for (index, (label, value)) in rows.into_iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    if index == self.row { "› " } else { "  " },
                    theme::active_bold(),
                ),
                Span::styled(format!("{label:<12}"), theme::text_muted()),
                Span::styled(
                    value,
                    if index == self.row {
                        theme::active_bold()
                    } else {
                        theme::text_dim()
                    },
                ),
            ]));
        }
        if !state.metadata.approval_enabled {
            lines.push(Line::from(Span::styled(
                "approval ask unavailable: server operator disabled prompts",
                theme::warning(),
            )));
        }
        frame.render_widget(Paragraph::new(lines), areas.results);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/permissions", theme::brand_violet()),
                Span::styled("  ←→ change selected value", theme::text_muted()),
            ])),
            areas.query,
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc => Consumed::Yes { dismiss: true },
            KeyCode::Up => {
                self.row = self.row.saturating_sub(1);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                self.row = (self.row + 1).min(2);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Left | KeyCode::Right => {
                self.cycle(key.code == KeyCode::Right, state.metadata.approval_enabled);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                let Some(session_id) = state.session_id.clone() else {
                    state.status = "Start a session before changing permissions".into();
                    return Consumed::Yes { dismiss: true };
                };
                state
                    .pending_actions
                    .push(crate::event::Action::SelectPermissions {
                        session_id,
                        profile: self.profile.clone(),
                    });
                state.status = "Updating permissions…".into();
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn file_label(value: sylvander_protocol::FileAccess) -> &'static str {
    match value {
        sylvander_protocol::FileAccess::None => "none",
        sylvander_protocol::FileAccess::ReadOnly => "read only",
        sylvander_protocol::FileAccess::WorkspaceWrite => "workspace write",
    }
}
fn network_label(value: sylvander_protocol::NetworkAccess) -> &'static str {
    match value {
        sylvander_protocol::NetworkAccess::Denied => "denied",
        sylvander_protocol::NetworkAccess::Allowed => "allowed",
    }
}
fn approval_label(value: sylvander_protocol::ApprovalPolicy) -> &'static str {
    match value {
        sylvander_protocol::ApprovalPolicy::Ask => "ask",
        sylvander_protocol::ApprovalPolicy::Allow => "allow",
        sylvander_protocol::ApprovalPolicy::Deny => "deny",
    }
}

#[cfg(test)]
#[path = "../../tests/unit/modal_permissions.rs"]
mod tests;
