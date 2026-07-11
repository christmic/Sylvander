//! Approval modal — UX §8.
//!
//! Two sub-modes:
//! - `Navigate` (default): the user sees the tool list and chooses
//!   approve / reject with optional "all remaining" bulk shortcuts.
//!   `n` enters feedback capture by transitioning to `RejectFeedback`.
//! - `RejectFeedback`: the user is typing free-form text that will be
//!   sent as a `SendFeedback` action after the rejection lands.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{AppMode, AppState, ToolInfo};
use crate::event::Action;
use crate::modal::{Consumed, Modal};

/// Per-tool decision. Pending means user has not yet decided on this row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Pending,
    Approve,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Initial state — navigate the batch, decide y/n per tool.
    Navigate,
    /// Capturing free-form rejection feedback.
    RejectFeedback,
}

/// One batch of tools awaiting user approval.
pub struct ApprovalModal {
    pub batch_id: String,
    pub tools: Vec<ToolInfo>,
    pub current: usize,
    pub decisions: Vec<Decision>,
    pub mode: ApprovalMode,
    /// Typed feedback for the current rejection. Captured in `RejectFeedback`.
    pub feedback: String,
    /// Position of ModalStack: 0 = first in stack, 1 = second...
    /// Set by `ModalStack::push` (we accept it via constructor default 0).
    pub stack_position: usize,
    /// Total modal count when this modal was pushed.
    pub queue_total: usize,
}

impl ApprovalModal {
    pub fn new(batch_id: String, tools: Vec<ToolInfo>) -> Self {
        let decisions = vec![Decision::Pending; tools.len()];
        Self {
            batch_id,
            tools,
            current: 0,
            decisions,
            mode: ApprovalMode::Navigate,
            feedback: String::new(),
            stack_position: 0,
            queue_total: 1,
        }
    }

    /// Per-row decision labels for rendering.
    fn marker(d: Decision, is_current: bool) -> &'static str {
        if is_current {
            match d {
                Decision::Pending => "  >> ",
                Decision::Approve => "  ✓> ",
                Decision::Reject => "  ✗> ",
            }
        } else {
            match d {
                Decision::Pending => "     ",
                Decision::Approve => "  ✓  ",
                Decision::Reject => "  ✗  ",
            }
        }
    }
}

impl Modal for ApprovalModal {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &str {
        if self.mode == ApprovalMode::RejectFeedback {
            "Rejection Feedback"
        } else {
            "Tool Approval"
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
        let height = (12 + self.tools.len() as u16 * 2).min(parent.height.saturating_sub(2));
        let popup_area = centered_rect(60, height, parent);
        frame.render_widget(Clear, popup_area);

        let title = if self.queue_total > 1 {
            format!(
                " Tool Approval (batch {}/{} — {} total) ",
                self.stack_position + 1,
                self.queue_total,
                self.queue_total
            )
        } else {
            " Tool Approval ".to_string()
        };

        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_style(Style::default().fg(Color::Yellow)),
            popup_area,
        );
        let inner = Block::default().borders(Borders::ALL).inner(popup_area);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from("Agent wants to run:".bold()));

        for (i, tool) in self.tools.iter().enumerate() {
            let is_current = i == self.current;
            let marker = Self::marker(self.decisions[i], is_current);
            let marker_color = match (self.decisions[i], is_current) {
                (_, true) => Color::Yellow,
                (Decision::Approve, _) => Color::Green,
                (Decision::Reject, _) => Color::Red,
                (Decision::Pending, _) => Color::Gray,
            };
            let tool_label = format!(
                "{}. {}  {}",
                i + 1,
                tool.tool_name,
                truncate_for_display(&tool.input.to_string(), 40)
            );
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(marker_color)),
                Span::styled(tool_label, Style::default().fg(marker_color)),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "Tool {}/{}  y=approve  n=reject  Y=all  N=reject all  ← back  esc=cancel",
                self.current + 1,
                self.tools.len()
            ),
            Style::default().fg(Color::DarkGray),
        )));

        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn handle_navigate_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match (key.code, key.modifiers) {
            (KeyCode::Char('y'), KeyModifiers::NONE) => {
                self.decisions[self.current] = Decision::Approve;
                advance(self, state)
            }
            (KeyCode::Char('n'), KeyModifiers::NONE) => {
                self.decisions[self.current] = Decision::Reject;
                // Drop into feedback capture for this rejection.
                self.mode = ApprovalMode::RejectFeedback;
                self.feedback.clear();
                state.mode = AppMode::ApprovalPending;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Char('Y'), KeyModifiers::SHIFT) => {
                // Approve all remaining including current.
                for d in &mut self.decisions[self.current..] {
                    if *d == Decision::Pending {
                        *d = Decision::Approve;
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
                if self.current > 0 {
                    self.current -= 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Down, _) => {
                if self.current + 1 < self.tools.len() {
                    self.current += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Left, _) | (KeyCode::Backspace, _) => {
                if self.current > 0 {
                    self.current -= 1;
                    self.decisions[self.current] = Decision::Pending;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            (KeyCode::Esc, _) => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            _ => Consumed::Ignored,
        }
    }
}

// ===========================================================================
// RejectFeedback sub-mode
// ===========================================================================

impl ApprovalModal {
    fn render_feedback(&self, frame: &mut Frame, parent: Rect) {
        let popup_area = centered_rect(60, 7, parent);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Rejection Feedback ")
                .title_style(Style::default().fg(Color::Yellow)),
            popup_area,
        );
        let inner = Block::default().borders(Borders::ALL).inner(popup_area);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(
            "Tell Sylvander what to do instead (or press Enter to send empty):".italic(),
        ));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Yellow)),
            Span::styled(&self.feedback, Style::default()),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "enter=send feedback   esc=back to approval",
            Style::default().fg(Color::DarkGray),
        )));

        frame.render_widget(Paragraph::new(lines), inner);

        // Hardware cursor at end of feedback text.
        let cursor_x = inner.x + 2 + self.feedback.chars().count() as u16;
        let cursor_y = inner.y + 2;
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn handle_feedback_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Enter => {
                // Submit feedback + finish the batch — feedback has already
                // been recorded in `self.feedback`.
                let sid = state.session_id.clone();
                let feedback = self.feedback.trim().to_string();
                if !feedback.is_empty() {
                    state.pending_actions.push(Action::SendFeedback {
                        text: feedback,
                        session_id: sid,
                    });
                }
                finish(self, state)
            }
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
                if !key
                    .modifiers
                    .contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
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
        return Consumed::Yes { dismiss: false };
    }
    // Last tool or all decided — fill remaining pending as approve (default).
    for d in modal.decisions.iter_mut() {
        if *d == Decision::Pending {
            *d = Decision::Approve;
        }
    }
    finish(modal, state)
}

/// Drain decisions into pending_actions and dismiss the modal.
fn finish(modal: &mut ApprovalModal, state: &mut AppState) -> Consumed {
    let decisions = std::mem::take(&mut modal.decisions);
    let tools = std::mem::take(&mut modal.tools);
    state.mode = AppMode::Normal;
    for (tool, decision) in tools.iter().zip(decisions.iter()) {
        let approved = matches!(decision, Decision::Approve);
        state.pending_actions.push(Action::SendApprove {
            call_id: tool.call_id.clone(),
            approved,
        });
    }
    Consumed::Yes { dismiss: true }
}

fn centered_rect(percent_x: u16, height: u16, parent: Rect) -> Rect {
    // `height` is in rows here (not percent) — middle-aligned vertically,
    // percent_x is the horizontal percentage.
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
mod tests {
    use super::*;

    fn tool(call_id: &str) -> ToolInfo {
        ToolInfo {
            call_id: call_id.into(),
            tool_name: "bash".into(),
            input: serde_json::json!({}),
        }
    }

    fn build_modal_with_n_tools(n: usize) -> ApprovalModal {
        let mut m = ApprovalModal::new("b".into(), (0..n).map(|i| tool(&format!("c{i}"))).collect());
        m.queue_total = 1;
        m
    }

    #[test]
    fn decision_tracks_pending_state() {
        let m = build_modal_with_n_tools(3);
        assert!(m.decisions.iter().all(|d| *d == Decision::Pending));
    }

    #[test]
    fn approve_advances_to_next_tool() {
        let mut m = build_modal_with_n_tools(3);
        m.handle_navigate_key(
            &key(KeyCode::Char('y'), KeyModifiers::NONE),
            &mut AppState::new(),
        );
        assert_eq!(m.decisions[0], Decision::Approve);
        assert_eq!(m.current, 1);
    }

    #[test]
    fn approve_y_emits_action_when_last_tool() {
        let mut m = build_modal_with_n_tools(2);
        let mut s = AppState::new();
        // Navigate to the last tool.
        m.handle_navigate_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut s);
        // Decide last.
        let consumed = m.handle_navigate_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        assert_eq!(s.pending_actions.len(), 2);
        assert!(matches!(
            s.pending_actions[0],
            Action::SendApprove { ref call_id, approved: true } if call_id == "c0"
        ));
        assert!(matches!(
            s.pending_actions[1],
            Action::SendApprove { ref call_id, approved: true } if call_id == "c1"
        ));
    }

    #[test]
    fn reject_then_enter_feedback_does_not_yet_emit_action() {
        let mut m = build_modal_with_n_tools(1);
        let mut s = AppState::new();
        // Reject → enter feedback mode, but no SendApprove yet.
        let consumed = m.handle_navigate_key(&key(KeyCode::Char('n'), KeyModifiers::NONE), &mut s);
        assert_eq!(m.mode, ApprovalMode::RejectFeedback);
        assert!(matches!(consumed, Consumed::Yes { dismiss: false }));
        assert!(s.pending_actions.is_empty());
        // Type feedback + Enter.
        for c in "use docker".chars() {
            m.handle_feedback_key(
                &key(KeyCode::Char(c), KeyModifiers::NONE),
                &mut s,
            );
        }
        let consumed = m.handle_feedback_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut s);
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        // SendFeedback lands first, then SendApprove(false) — feedback gets
        // attached to the rejected call as follow-up context.
        assert_eq!(s.pending_actions.len(), 2);
        assert!(matches!(
            s.pending_actions[0],
            Action::SendFeedback { ref text, .. } if text == "use docker"
        ));
        assert!(matches!(
            s.pending_actions[1],
            Action::SendApprove { approved: false, .. }
        ));
    }

    #[test]
    fn shift_y_approves_all_remaining() {
        let mut m = build_modal_with_n_tools(3);
        let mut s = AppState::new();
        let consumed = m.handle_navigate_key(
            &key(KeyCode::Char('Y'), KeyModifiers::SHIFT),
            &mut s,
        );
        assert!(matches!(consumed, Consumed::Yes { dismiss: true }));
        // All decisions are now Approve.
        assert_eq!(
            s.pending_actions.len(),
            3,
            "expected 3 SendApprove actions for 3-tool batch"
        );
    }

    #[test]
    fn backspace_rewinds_and_clears_decision() {
        let mut m = build_modal_with_n_tools(3);
        let mut s = AppState::new();
        m.handle_navigate_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut s);
        // current=1, decisions[0]=Approve, decisions[1]=Pending
        let consumed = m.handle_navigate_key(&key(KeyCode::Backspace, KeyModifiers::NONE), &mut s);
        // cursor moved back, decision[0] should be Pending again.
        assert!(matches!(consumed, Consumed::Yes { dismiss: false }));
        assert_eq!(m.current, 0);
        assert_eq!(m.decisions[0], Decision::Pending);
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
}
