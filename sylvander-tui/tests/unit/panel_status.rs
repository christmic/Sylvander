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
            call_id: "call-1".into(),
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
    let palette = CommandPalette::new(&s);
    s.modals.push(Box::new(palette));
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
