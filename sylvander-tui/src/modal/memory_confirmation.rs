//! Guardian memory confirmation Decision Dock.
//!
//! This surface displays only Runtime-sanitized prompt data. It emits a typed
//! decision and never receives or constructs owner identity.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::AppState;
use crate::event::Action;
use crate::modal::{Consumed, Modal, ModalPlacement, surface::decision_dock};
use crate::theme;

const DOCK_BODY_ROWS: u16 = 7;
const SUMMARY_ROWS: u16 = 2;
const ACTION_ROWS: u16 = 3;

pub struct MemoryConfirmationModal {
    session_id: String,
    confirmation: sylvander_protocol::PendingMemoryConfirmation,
    confirm_selected: bool,
}

impl MemoryConfirmationModal {
    pub fn new(
        session_id: String,
        confirmation: sylvander_protocol::PendingMemoryConfirmation,
    ) -> Self {
        Self {
            session_id,
            confirmation,
            confirm_selected: true,
        }
    }

    fn decide(
        &mut self,
        state: &mut AppState,
        decision: sylvander_protocol::MemoryConfirmationDecision,
    ) -> Consumed {
        state
            .pending_actions
            .push(Action::ResolveMemoryConfirmation {
                session_id: std::mem::take(&mut self.session_id),
                candidate_id: std::mem::take(&mut self.confirmation.candidate_id),
                expected_revision: self.confirmation.expected_revision,
                decision,
            });
        state.sync_decision_dock_mode();
        state.status = "Recording memory decision…".into();
        Consumed::Yes { dismiss: true }
    }
}

impl Modal for MemoryConfirmationModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Memory confirmation"
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        ModalPlacement::BelowComposer {
            rows: DOCK_BODY_ROWS + 1,
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let body = decision_dock(frame, parent, DOCK_BODY_ROWS);
        let scope = match self.confirmation.scope {
            sylvander_protocol::MemoryConfirmationScope::Relationship => "our relationship",
            sylvander_protocol::MemoryConfirmationScope::UserProfile => "your profile",
            sylvander_protocol::MemoryConfirmationScope::AgentCanonical => "Agent knowledge",
            sylvander_protocol::MemoryConfirmationScope::WorkspaceKnowledge => {
                "workspace knowledge"
            }
        };
        let allow_style = if self.confirm_selected {
            theme::brand_violet().bold()
        } else {
            theme::text()
        };
        let reject_style = if self.confirm_selected {
            theme::text()
        } else {
            theme::brand_violet().bold()
        };

        let detail_rows = body.height.saturating_sub(ACTION_ROWS);
        let header = row(body, 0, 1);
        let summary_area = row(body, 1, SUMMARY_ROWS.min(detail_rows.saturating_sub(2)));
        let destination = row(body, detail_rows.saturating_sub(1), 1);
        let actions_start = body.height.saturating_sub(ACTION_ROWS);
        let allow = row(body, actions_start, 1);
        let reject = row(body, actions_start.saturating_add(1), 1);
        let help = row(body, actions_start.saturating_add(2), 1);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "◆ Save this for future conversations?",
                theme::brand_violet().bold(),
            ))),
            header,
        );
        let summary = compact_summary(
            self.confirmation.summary.as_str(),
            summary_area.width as usize,
            summary_area.height as usize,
        )
        .into_iter()
        .map(|line| Line::from(Span::styled(line, theme::text().bold())))
        .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(summary), summary_area);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("destination · {scope}"),
                theme::text_muted(),
            ))),
            destination,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if self.confirm_selected {
                    "› 1. Save memory"
                } else {
                    "  1. Save memory"
                },
                allow_style,
            ))),
            allow,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if self.confirm_selected {
                    "  2. Don't save"
                } else {
                    "› 2. Don't save"
                },
                reject_style,
            ))),
            reject,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "↑↓ choose · ↵ confirm · esc reject",
                theme::text_muted(),
            ))),
            help,
        );
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.confirm_selected = !self.confirm_selected;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('1' | 'y' | 'Y') => self.decide(
                state,
                sylvander_protocol::MemoryConfirmationDecision::Confirm,
            ),
            KeyCode::Char('2' | 'n' | 'N') | KeyCode::Esc => self.decide(
                state,
                sylvander_protocol::MemoryConfirmationDecision::Reject,
            ),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.decide(
                state,
                sylvander_protocol::MemoryConfirmationDecision::Reject,
            ),
            KeyCode::Enter => self.decide(
                state,
                if self.confirm_selected {
                    sylvander_protocol::MemoryConfirmationDecision::Confirm
                } else {
                    sylvander_protocol::MemoryConfirmationDecision::Reject
                },
            ),
            _ => Consumed::Ignored,
        }
    }
}

fn row(area: Rect, offset: u16, requested_height: u16) -> Rect {
    let offset = offset.min(area.height);
    Rect {
        x: area.x,
        y: area.y.saturating_add(offset),
        width: area.width,
        height: requested_height.min(area.height.saturating_sub(offset)),
    }
}

/// Wrap a Runtime-sanitized summary into a small, deterministic preview.
///
/// Decision rows are rendered in separate rectangles, so even a long CJK or
/// emoji-rich summary can never displace the confirm/reject controls.
fn compact_summary(summary: &str, width: usize, max_rows: usize) -> Vec<String> {
    if width == 0 || max_rows == 0 {
        return Vec::new();
    }

    let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for grapheme in summary.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if !current.is_empty() && current_width.saturating_add(grapheme_width) > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if current.is_empty() && grapheme.chars().all(char::is_whitespace) {
            continue;
        }
        current.push_str(grapheme);
        current_width = current_width.saturating_add(grapheme_width);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }

    if lines.len() > max_rows {
        lines.truncate(max_rows);
        let last = lines.last_mut().expect("max_rows is non-zero");
        while UnicodeWidthStr::width(last.as_str()).saturating_add(1) > width {
            let Some((index, _)) = last.grapheme_indices(true).next_back() else {
                break;
            };
            last.truncate(index);
        }
        last.push('…');
    }

    lines
}

#[cfg(test)]
#[path = "../../tests/unit/modal_memory_confirmation.rs"]
mod tests;
