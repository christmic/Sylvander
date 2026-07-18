//! Approval Decision Dock — UX §10.1.
//!
//! Two sub-modes:
//! - `Navigate` (default): the user sees the tool list and chooses
//!   approve / reject with optional "all remaining" bulk shortcuts.
//!   `n` enters feedback capture by transitioning to `RejectFeedback`.
//! - `RejectFeedback`: the user is typing free-form text that will be
//!   attached to each rejected approval decision.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppMode, AppState, ToolInfo};
use crate::approval_presenter::{RiskLevel, summarize};
use crate::event::Action;
use crate::modal::{Consumed, Modal, ModalPlacement, surface::decision_dock};
use crate::theme;

/// Per-tool decision. Pending means user has not yet decided on this row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Pending,
    Approve(sylvander_protocol::ApprovalScope),
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Initial state — navigate the batch, decide y/n per tool.
    Navigate,
    /// Capturing free-form rejection feedback.
    RejectFeedback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalChoice {
    Once,
    Reject,
    Session,
    Persistent,
}

/// One batch of tools awaiting user approval.
pub struct ApprovalModal {
    pub batch_id: String,
    pub tools: Vec<ToolInfo>,
    pub current: usize,
    /// Cursor over the plain-language choices for the current request.
    pub choice_index: usize,
    pub decisions: Vec<Decision>,
    pub mode: ApprovalMode,
    /// Typed feedback for the current rejection. Captured in `RejectFeedback`.
    pub feedback: String,
    /// Position of `ModalStack`: 0 = first in stack, 1 = second...
    /// Set by `ModalStack::push` (we accept it via constructor default 0).
    pub stack_position: usize,
    /// Total modal count when this modal was pushed.
    pub queue_total: usize,
    /// Server-advertised scopes. The modal never invents a broader grant.
    pub allowed_scopes: Vec<sylvander_protocol::ApprovalScope>,
}

impl ApprovalModal {
    pub fn new(batch_id: String, tools: Vec<ToolInfo>) -> Self {
        let decisions = vec![Decision::Pending; tools.len()];
        let mut modal = Self {
            batch_id,
            tools,
            current: 0,
            choice_index: 0,
            decisions,
            mode: ApprovalMode::Navigate,
            feedback: String::new(),
            stack_position: 0,
            queue_total: 1,
            allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
        };
        modal.reset_choice();
        modal
    }

    pub fn with_allowed_scopes(
        mut self,
        allowed_scopes: Vec<sylvander_protocol::ApprovalScope>,
    ) -> Self {
        self.allowed_scopes = allowed_scopes;
        if !self
            .allowed_scopes
            .contains(&sylvander_protocol::ApprovalScope::Once)
        {
            self.allowed_scopes
                .insert(0, sylvander_protocol::ApprovalScope::Once);
        }
        self
    }

    fn choices(&self) -> Vec<ApprovalChoice> {
        let critical = self.tools.get(self.current).is_some_and(|tool| {
            summarize(&tool.tool_name, &tool.input).risk == RiskLevel::Critical
        });
        let mut choices = if critical {
            vec![ApprovalChoice::Reject, ApprovalChoice::Once]
        } else {
            vec![ApprovalChoice::Once, ApprovalChoice::Reject]
        };
        if self
            .allowed_scopes
            .contains(&sylvander_protocol::ApprovalScope::Session)
        {
            choices.push(ApprovalChoice::Session);
        }
        if self
            .allowed_scopes
            .contains(&sylvander_protocol::ApprovalScope::Persistent)
        {
            choices.push(ApprovalChoice::Persistent);
        }
        choices
    }

    fn reset_choice(&mut self) {
        // The recommended choice always occupies the first row: Allow once
        // for ordinary requests, Deny for critical requests.
        self.choice_index = 0;
    }
}

impl Modal for ApprovalModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Tool Approval"
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        let body_rows = match self.mode {
            ApprovalMode::Navigate => 6 + self.choices().len() as u16,
            ApprovalMode::RejectFeedback => 5,
        };
        ModalPlacement::BelowComposer {
            rows: body_rows.saturating_add(1),
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        match self.mode {
            ApprovalMode::Navigate => self.render_navigate(frame, parent),
            ApprovalMode::RejectFeedback => self.render_feedback(frame, parent),
        }
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match self.mode {
            ApprovalMode::Navigate => self.handle_navigate_key(key, state),
            ApprovalMode::RejectFeedback => self.handle_feedback_key(key, state),
        }
    }
}

// ===========================================================================
// Navigate sub-mode
// ===========================================================================

impl ApprovalModal {
    fn render_navigate(&self, frame: &mut Frame, parent: Rect) {
        let choices = self.choices();
        let body = decision_dock(frame, parent, 6 + choices.len() as u16);
        let Some(tool) = self.tools.get(self.current) else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "◆ Approval request is no longer available",
                    theme::warning(),
                ))),
                body,
            );
            return;
        };

        let summary = summarize(&tool.tool_name, &tool.input);
        let risk_style = match summary.risk {
            RiskLevel::Low => theme::verified(),
            RiskLevel::Medium => theme::active(),
            RiskLevel::High => theme::warning(),
            RiskLevel::Critical => theme::danger().bold(),
        };
        let header = "◆ Permission needed";
        let progress = format!("{} of {}", self.current + 1, self.tools.len());
        let gap = (body.width as usize)
            .saturating_sub(UnicodeWidthStr::width(header) + UnicodeWidthStr::width(&*progress));
        let mut lines = vec![
            Line::from(vec![
                Span::styled(header, theme::warning().bold()),
                Span::raw(" ".repeat(gap)),
                Span::styled(progress, theme::text_muted()),
            ]),
            Line::from(Span::styled(
                approval_action_label(&tool.tool_name),
                theme::text_muted(),
            )),
            Line::from(Span::styled(
                truncate_for_display(
                    &approval_target(&tool.tool_name, &summary.action),
                    body.width as usize,
                ),
                theme::text().bold(),
            )),
            Line::from(Span::styled(
                truncate_for_display(
                    &format!(
                        "{} · {} · {}",
                        risk_label(summary.risk),
                        risk_explanation(summary.risk),
                        summary.scope
                    ),
                    body.width as usize,
                ),
                risk_style,
            )),
            Line::from(""),
        ];

        for (index, choice) in choices.iter().copied().enumerate() {
            let selected = index == self.choice_index;
            let choice_style = if choice == ApprovalChoice::Reject {
                if selected {
                    theme::danger().bold()
                } else {
                    theme::text()
                }
            } else if selected {
                theme::brand_violet().bold()
            } else {
                theme::text()
            };
            let recommendation =
                if choice == ApprovalChoice::Reject && summary.risk == RiskLevel::Critical {
                    "  recommended for critical operations"
                } else {
                    ""
                };
            lines.push(Line::from(vec![
                Span::styled(if selected { "› " } else { "  " }, choice_style),
                Span::styled(
                    format!("{}. {}", index + 1, approval_choice_label(choice)),
                    choice_style,
                ),
                Span::styled(recommendation, theme::text_muted()),
            ]));
        }

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    }

    fn handle_navigate_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) => self.apply_selected_choice(state),
            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                self.decisions[self.current] =
                    Decision::Approve(sylvander_protocol::ApprovalScope::Once);
                advance(self, state)
            }
            (KeyCode::Char('s'), KeyModifiers::NONE) => {
                self.approve_with_scope(sylvander_protocol::ApprovalScope::Session, state)
            }
            (KeyCode::Char('p'), KeyModifiers::NONE) => {
                self.approve_with_scope(sylvander_protocol::ApprovalScope::Persistent, state)
            }
            (KeyCode::Char('n' | 'r'), KeyModifiers::NONE) => self.begin_rejection(state),
            (KeyCode::Char(number @ '1'..='4'), KeyModifiers::NONE) => {
                let index = number as usize - '1' as usize;
                if index < self.choices().len() {
                    self.choice_index = index;
                    self.apply_selected_choice(state)
                } else {
                    Consumed::Yes { dismiss: false }
                }
            }
            (KeyCode::Char('Y'), KeyModifiers::SHIFT)
            | (KeyCode::Char('a'), KeyModifiers::NONE) => {
                // Approve all remaining including current.
                for d in &mut self.decisions[self.current..] {
                    if *d == Decision::Pending {
                        *d = Decision::Approve(sylvander_protocol::ApprovalScope::Once);
                    }
                }
                finish(self, state)
            }
            (KeyCode::Char('N'), KeyModifiers::SHIFT) => {
                // Reject all remaining — and jump straight to feedback.
                for d in &mut self.decisions[self.current..] {
                    if *d == Decision::Pending {
                        *d = Decision::Reject;
                    }
                }
                self.mode = ApprovalMode::RejectFeedback;
                self.feedback.clear();
                state.mode = AppMode::ApprovalPending;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Up, _) => {
                if self.choice_index > 0 {
                    self.choice_index -= 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Down, _) => {
                if self.choice_index + 1 < self.choices().len() {
                    self.choice_index += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Left | KeyCode::Backspace, _) => {
                if self.current > 0 {
                    self.current -= 1;
                    self.decisions[self.current] = Decision::Pending;
                    self.reset_choice();
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                reject_pending_and_finish(self, state)
            }
            _ => Consumed::Ignored,
        }
    }

    fn approve_with_scope(
        &mut self,
        scope: sylvander_protocol::ApprovalScope,
        state: &mut AppState,
    ) -> Consumed {
        if !self.allowed_scopes.contains(&scope) {
            state.status = format!("{} approval is disabled by the server", scope_label(scope));
            state.dirty.mark();
            return Consumed::Yes { dismiss: false };
        }
        self.decisions[self.current] = Decision::Approve(scope);
        advance(self, state)
    }

    fn apply_selected_choice(&mut self, state: &mut AppState) -> Consumed {
        let Some(choice) = self.choices().get(self.choice_index).copied() else {
            return Consumed::Yes { dismiss: false };
        };
        match choice {
            ApprovalChoice::Once => {
                self.decisions[self.current] =
                    Decision::Approve(sylvander_protocol::ApprovalScope::Once);
                advance(self, state)
            }
            ApprovalChoice::Reject => self.begin_rejection(state),
            ApprovalChoice::Session => {
                self.approve_with_scope(sylvander_protocol::ApprovalScope::Session, state)
            }
            ApprovalChoice::Persistent => {
                self.approve_with_scope(sylvander_protocol::ApprovalScope::Persistent, state)
            }
        }
    }

    fn begin_rejection(&mut self, state: &mut AppState) -> Consumed {
        self.decisions[self.current] = Decision::Reject;
        self.mode = ApprovalMode::RejectFeedback;
        self.feedback.clear();
        state.mode = AppMode::ApprovalPending;
        state.dirty.mark();
        Consumed::Yes { dismiss: false }
    }
}

// ===========================================================================
// RejectFeedback sub-mode
// ===========================================================================

impl ApprovalModal {
    fn render_feedback(&self, frame: &mut Frame, parent: Rect) {
        let body = decision_dock(frame, parent, 5);
        let lines = vec![
            Line::from(Span::styled("◆ Deny this request", theme::danger().bold())),
            Line::from(Span::styled(
                "Add guidance for Sylvander (optional).",
                theme::text_muted(),
            )),
            Line::from(vec![
                Span::styled("> ", theme::danger()),
                Span::styled(&self.feedback, theme::text()),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "↵ reject with guidance   esc back   500 character max",
                theme::text_muted(),
            )),
        ];

        frame.render_widget(Paragraph::new(lines), body);

        // Hardware cursor at end of feedback text.
        let cursor_x = body.x + 2 + UnicodeWidthStr::width(self.feedback.as_str()) as u16;
        let cursor_y = body.y + 2;
        if cursor_x < body.x + body.width && cursor_y < body.y + body.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn handle_feedback_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Enter => finish(self, state),
            KeyCode::Esc => {
                // Cancel feedback capture, drop back to navigate so the
                // user can revise their decision.
                self.mode = ApprovalMode::Navigate;
                self.feedback.clear();
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                if !self.feedback.is_empty() {
                    self.feedback.pop();
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && self.feedback.chars().count() < 500
                {
                    self.feedback.push(c);
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
}

// ===========================================================================
// End-of-batch helpers
// ===========================================================================

/// Move cursor to next pending tool; if all decided, finalize the batch.
fn advance(modal: &mut ApprovalModal, state: &mut AppState) -> Consumed {
    // Move past any decided slots.
    while modal.current + 1 < modal.tools.len()
        && modal.decisions[modal.current + 1] != Decision::Pending
    {
        modal.current += 1;
    }
    if modal.current + 1 < modal.tools.len() {
        modal.current += 1;
        modal.reset_choice();
        return Consumed::Yes { dismiss: false };
    }
    // Last tool or all decided — fill remaining pending as approve (default).
    for d in &mut modal.decisions {
        if *d == Decision::Pending {
            *d = Decision::Approve(sylvander_protocol::ApprovalScope::Once);
        }
    }
    finish(modal, state)
}

/// Drain decisions into `pending_actions` and dismiss the modal.
fn finish(modal: &mut ApprovalModal, state: &mut AppState) -> Consumed {
    let rejection_reason = (!modal.feedback.trim().is_empty())
        .then(|| modal.feedback.trim().chars().take(500).collect::<String>());
    let decisions = std::mem::take(&mut modal.decisions);
    let tools = std::mem::take(&mut modal.tools);
    modal.feedback.clear();
    state.sync_decision_dock_mode();
    let approved_count = decisions
        .iter()
        .filter(|decision| matches!(decision, Decision::Approve(_)))
        .count();
    let rejected_count = decisions.len().saturating_sub(approved_count);
    for (tool, decision) in tools.iter().zip(decisions.iter()) {
        let approved = matches!(decision, Decision::Approve(_));
        let scope = match decision {
            Decision::Approve(scope) => *scope,
            Decision::Pending | Decision::Reject => sylvander_protocol::ApprovalScope::Once,
        };
        state.pending_actions.push(Action::SendApprove {
            session_id: state.session_id.clone().unwrap_or_default(),
            call_id: tool.call_id.clone(),
            approved,
            scope,
            reason: if approved {
                None
            } else {
                rejection_reason.clone()
            },
        });
    }
    state.messages.push(crate::app::ChatMessage::Info(format!(
        "approval · {approved_count} approved · {rejected_count} rejected"
    )));
    Consumed::Yes { dismiss: true }
}

fn scope_label(scope: sylvander_protocol::ApprovalScope) -> &'static str {
    match scope {
        sylvander_protocol::ApprovalScope::Once => "one-shot",
        sylvander_protocol::ApprovalScope::Session => "session",
        sylvander_protocol::ApprovalScope::Persistent => "persistent",
    }
}

fn approval_choice_label(choice: ApprovalChoice) -> &'static str {
    match choice {
        ApprovalChoice::Once => "Allow once",
        ApprovalChoice::Reject => "Deny",
        ApprovalChoice::Session => "Allow this exact request for this session",
        ApprovalChoice::Persistent => "Always allow this exact request",
    }
}

fn approval_action_label(tool_name: &str) -> &'static str {
    match tool_name.to_ascii_lowercase().as_str() {
        "bash" | "shell" | "exec" => "Run command",
        "write" | "write_file" => "Write file",
        "edit" | "edit_file" => "Edit file",
        "read" | "read_file" => "Read file",
        "search" | "grep" | "rg" => "Search workspace",
        _ => "Use tool",
    }
}

fn approval_target(tool_name: &str, target: &str) -> String {
    if target.trim().is_empty()
        || target.eq_ignore_ascii_case(tool_name)
        || target.eq_ignore_ascii_case(approval_action_label(tool_name))
    {
        "Target details were not provided".into()
    } else {
        target.to_string()
    }
}

fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Low => "Low risk",
        RiskLevel::Medium => "Medium risk",
        RiskLevel::High => "High risk",
        RiskLevel::Critical => "Critical",
    }
}

fn risk_explanation(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Low => "read-only operation",
        RiskLevel::Medium => "changes workspace content",
        RiskLevel::High => "runs a process or external capability",
        RiskLevel::Critical => "may destroy data or repository state",
    }
}

fn reject_pending_and_finish(modal: &mut ApprovalModal, state: &mut AppState) -> Consumed {
    for decision in &mut modal.decisions {
        if *decision == Decision::Pending {
            *decision = Decision::Reject;
        }
    }
    finish(modal, state)
}

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "../../tests/unit/modal_approval.rs"]
mod tests;
