//! Status panel — bottom row of the screen, mirroring `02-tui-immersive.svg` line 19.
//!
//! Layout (UX §5.1):
//! - Left: `<glyph> <label> · context —% · N tools · <main|plan>`
//! - Right: up to **three** contextual unicode-symbol hints, mode-aware.
//!
//! Status modes are owned by `theme::StatusMode` (5-mode enum). This
//! panel just derives which one is current based on AppState.
//!
//! **Status contract** (M-T15.C):
//! - `Disconnected`         — Unix socket is closed (`!` glyph + amber).
//! - `Working`              — agent is iterating (`◐` glyph + blue).
//!   Detected observationally: streaming buffer is non-empty, or a
//!   ToolStep has any Pending child. (When the server starts emitting
//!   `WorkingStarted`/`WorkingEnded` events, AppState.working_active
//!   will override this.)
//! - `WaitingApproval`     — Approval modal is open (`●` glyph + amber).
//! - `Asking`               — AskUser modal is open (`●` glyph + dim).
//! - `Idle`                 — everything else (`·` glyph + dim).

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{AppMode, AppState};
use crate::component::Component;
use crate::theme::{self, StatusMode};

/// Single source of truth for which status mode is current.
/// Pure function — no side effects, easy to unit-test.
pub fn status_mode_for(state: &AppState) -> StatusMode {
    if !state.connected {
        if state.status == "Connecting..." {
            return StatusMode::Connecting;
        }
        return StatusMode::Disconnected;
    }

    // Priority order: an open modal always wins over the agent loop.
    if let Some(top) = state.modals.top() {
        let t = top.title();
        if t == "Tool Approval" {
            return StatusMode::WaitingApproval;
        }
        if t == "Plan review" || t == "Agent asks" || t == "Commands" {
            // Asking covers AskUser + Palette (palette is morally an
            // interactive decision the user must make).
            return StatusMode::Asking;
        }
    }

    // Working is detected observationally since the agent loop doesn't
    // currently push WorkingStarted/Ended events.
    let working = !state.streaming.is_empty()
        || !state.streaming_thinking.is_empty()
        || state.messages.iter().any(|m| match m {
            crate::app::ChatMessage::ToolStep { children, .. } => children
                .iter()
                .any(|c| c.status == crate::app::ToolStatus::Pending),
            _ => false,
        });
    if working {
        return StatusMode::Working;
    }

    StatusMode::Idle
}

pub struct StatusPanel;

impl Component for StatusPanel {
    fn height(&self, _state: &AppState, _viewport_width: u16) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let mode = status_mode_for(state);

        let tool_count = state.messages.iter().fold(0usize, |n, m| match m {
            crate::app::ChatMessage::ToolStep { children, .. } => n + children.len(),
            _ => n,
        });
        let task_running = state.messages.iter().any(|m| match m {
            crate::app::ChatMessage::TaskList { tasks } => tasks
                .iter()
                .any(|t| matches!(t.state, crate::app::TaskState::Running)),
            _ => false,
        });

        let task_span: Span = if task_running {
            Span::styled(" · task running", theme::warning())
        } else {
            Span::raw("")
        };

        let session = state
            .session_id
            .as_deref()
            .map(|id| id.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "—".into());
        let model = &state.metadata.model;
        let branch = &state.metadata.branch;
        if area.width < 80 {
            let compact = Line::from(vec![
                Span::styled(format!("{} {}", mode.glyph(), mode.label()), mode.style()),
                Span::styled(
                    format!(" · model {model} · branch {branch} · session {session}"),
                    theme::text_dim(),
                ),
            ]);
            frame.render_widget(Paragraph::new(compact), area);
            return;
        }
        let left = Line::from(vec![
            Span::styled(format!("{} ", mode.glyph()), mode.style()),
            Span::styled(mode.label(), mode.style()),
            Span::styled(
                format!(" · model {model} · branch {branch} · session {session} · {tool_count}t"),
                theme::text_dim(),
            ),
            task_span,
        ])
        .alignment(Alignment::Left);

        let hints: Vec<Span> = hints_for_mode(state.mode, mode).into_iter().collect();
        let right = Line::from(hints).alignment(Alignment::Right);

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
            .split(area);
        frame.render_widget(Paragraph::new(left), layout[0]);
        frame.render_widget(Paragraph::new(right), layout[1]);
    }
}

/// Up to three contextual hints per `18-composer-interactions.svg`.
/// Compact, mode-aware, ≤ 3 entries. No permanent shortcut manual.
fn hints_for_mode(app_mode: AppMode, status_mode: StatusMode) -> [Span<'static>; 3] {
    match (app_mode, status_mode) {
        (_, StatusMode::Connecting) => [
            Span::styled("connecting…", theme::active()),
            Span::raw("   "),
            Span::styled("draft local", theme::text_muted()),
        ],
        (_, StatusMode::Disconnected) => [
            Span::styled("! reconnecting…", theme::warning()),
            Span::raw("   "),
            Span::styled("/draft preserved", theme::text_muted()),
        ],
        (_, StatusMode::WaitingApproval) => [
            Span::styled("y approve", theme::text_muted()),
            Span::raw("   "),
            Span::styled("n reject", theme::text_muted()),
        ],
        (_, StatusMode::Asking) => [
            Span::styled("↵ submit", theme::text_muted()),
            Span::raw("   "),
            Span::styled("esc cancel", theme::text_muted()),
        ],
        (_, StatusMode::Working) => [
            Span::styled("esc interrupt", theme::text_muted()),
            Span::raw("   "),
            Span::styled("/draft", theme::text_muted()),
        ],
        (AppMode::Normal, StatusMode::Idle) => [
            Span::styled("↵ send", theme::text_muted()),
            Span::raw("   "),
            Span::styled("⇧↵ newline", theme::text_muted()),
        ],
        (AppMode::ApprovalPending, StatusMode::Idle) => [
            Span::styled("y approve", theme::text_muted()),
            Span::raw("   "),
            Span::styled("esc cancel", theme::text_muted()),
        ],
        (AppMode::AskPending, StatusMode::Idle) => [
            Span::styled("↵ submit", theme::text_muted()),
            Span::raw("   "),
            Span::styled("esc cancel", theme::text_muted()),
        ],
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppMode, AppState, ChatMessage, ToolInfo, ToolStatus};
    use crate::event::DomainEvent;
    use crate::modal::{ApprovalModal, AskUserModal, CommandPalette, PlanReviewModal};

    fn fresh_state() -> AppState {
        let mut s = AppState::new();
        s.apply(DomainEvent::Connected);
        s
    }

    #[test]
    fn disconnected_state_overrides_everything_else() {
        let mut s = AppState::new();
        s.apply(DomainEvent::Disconnected {
            reason: "offline".into(),
        });
        s.messages.push(ChatMessage::ToolStep {
            name: "x".into(),
            started_at_secs: 0,
            children: vec![],
        });
        assert_eq!(status_mode_for(&s), StatusMode::Disconnected);
    }

    #[test]
    fn idle_when_connected_no_streaming_no_modal() {
        let s = fresh_state();
        assert_eq!(status_mode_for(&s), StatusMode::Idle);
    }

    #[test]
    fn working_when_streaming_text_open() {
        let mut s = fresh_state();
        s.streaming.push_str("partial");
        assert_eq!(status_mode_for(&s), StatusMode::Working);
    }

    #[test]
    fn working_when_thinking_streaming() {
        let mut s = fresh_state();
        s.streaming_thinking.push_str("mulling");
        assert_eq!(status_mode_for(&s), StatusMode::Working);
    }

    #[test]
    fn working_when_a_tool_step_has_pending_child() {
        let mut s = fresh_state();
        s.messages.push(ChatMessage::ToolStep {
            name: "step".into(),
            started_at_secs: 0,
            children: vec![crate::app::ToolStepChild {
                name: "bash".into(),
                status: ToolStatus::Pending,
                input: serde_json::json!({}),
                output: None,
                is_error: None,
            }],
        });
        assert_eq!(status_mode_for(&s), StatusMode::Working);
    }

    #[test]
    fn waiting_approval_when_approval_modal_open() {
        let mut s = fresh_state();
        s.modals.push(Box::new(ApprovalModal::new(
            "b1".into(),
            vec![ToolInfo {
                call_id: "c".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({}),
            }],
        )));
        // AppMode is still Normal (modal hasn't committed yet), but the
        // status function looks at the top modal title.
        assert_eq!(status_mode_for(&s), StatusMode::WaitingApproval);
    }

    #[test]
    fn asking_when_askuser_or_plan_or_palette_modal_open() {
        let mut s = fresh_state();
        s.modals.push(Box::new(AskUserModal::new(
            "c".into(),
            "q".into(),
            vec![],
            false,
        )));
        assert_eq!(status_mode_for(&s), StatusMode::Asking);
        s.modals.pop();
        s.modals.push(Box::new(PlanReviewModal::new(
            "p1".into(),
            vec!["step".into()],
            0,
            None,
        )));
        assert_eq!(status_mode_for(&s), StatusMode::Asking);
        s.modals.pop();
        s.modals.push(Box::new(CommandPalette::new()));
        assert_eq!(status_mode_for(&s), StatusMode::Asking);
    }

    #[test]
    fn asking_modal_wins_over_streaming_observation() {
        // When an AskUser modal is open, the agent loop is paused and
        // waiting on the user — the status row should reflect `Asking`,
        // not a stale `Working` observed from residual streaming.
        let mut s = fresh_state();
        s.streaming.push_str("partial");
        s.modals.push(Box::new(AskUserModal::new(
            "c".into(),
            "?".into(),
            vec![],
            false,
        )));
        assert_eq!(status_mode_for(&s), StatusMode::Asking);
    }

    #[test]
    fn app_mode_alone_does_not_imply_waiting() {
        let mut s = fresh_state();
        s.mode = AppMode::ApprovalPending;
        // No streaming, no modal pushed: still Idle.
        // (Approval modal is what flips the status to WaitingApproval.)
        assert_eq!(status_mode_for(&s), StatusMode::Idle);
    }
}
