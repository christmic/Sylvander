//! Status panel — bottom row of the screen, mirroring `02-tui-immersive.svg` line 19.
//!
//! Layout (UX §5.1):
//! - Left: `<glyph> <label> · context —% · N tools · <main|plan>`
//! - Right: up to **three** contextual unicode-symbol hints, mode-aware.
//!
//! Status modes are owned by `theme::StatusMode` (5-mode enum). This
//! panel just derives which one is current based on `AppState`.
//!
//! **Status contract** (M-T15.C):
//! - `Disconnected`         — Unix socket is closed (`!` glyph + amber).
//! - `Working`              — agent is iterating (`◐` glyph + blue).
//!   Detected observationally: streaming buffer is non-empty, or a
//!   `ToolStep` has any Pending child. (When the server starts emitting
//!   `WorkingStarted`/`WorkingEnded` events, `AppState.working_active`
//!   will override this.)
//! - `WaitingApproval`     — Approval modal is open (`●` glyph + amber).
//! - `Asking`               — `AskUser` modal is open (`●` glyph + dim).
//! - `Idle`                 — everything else (`·` glyph + dim).

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

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
        if matches!(
            t,
            "Plan review"
                | "Plan editor"
                | "Plan · Edit step"
                | "Agent asks"
                | "Commands"
                | "Model"
                | "Permissions"
                | "Mention file"
                | "Sessions"
                | "Rollback files"
                | "Tool output"
                | "Help"
        ) {
            // Asking covers AskUser + Palette (palette is morally an
            // interactive decision the user must make).
            return StatusMode::Asking;
        }
    }

    // Local submission marks the turn active immediately; streamed content
    // then keeps the same state until Done/Error/Interrupted settles it.
    let working = state.turn_active
        || !state.streaming.is_empty()
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
        let queue_span: Span = if state.queued_prompts.is_empty() {
            Span::raw("")
        } else {
            Span::styled(
                format!(" · {} queued", state.queued_prompts.len()),
                theme::active(),
            )
        };
        let editing_span = state.composer.mode_label().map_or_else(
            || Span::raw(""),
            |mode| Span::styled(format!(" · {mode}"), theme::brand_violet()),
        );

        let session = state
            .session_id
            .as_deref()
            .map_or_else(|| "—".into(), |id| id.chars().take(8).collect::<String>());
        let model = state.metadata.model_label();
        let branch = &state.metadata.branch;
        let surface = state.modals.top().map(super::super::modal::Modal::title);
        let mode_label = surface_status_label(surface, mode);
        if area.width < 100 {
            let compact = Line::from(vec![
                Span::styled(format!("{} {mode_label}", mode.glyph()), mode.style()),
                Span::styled(
                    format!(" · model {model} · branch {branch} · session {session}"),
                    theme::text_dim(),
                ),
                editing_span,
            ]);
            frame.render_widget(Paragraph::new(compact), area);
            return;
        }
        let tool_summary = if area.width >= 140 {
            format!(" · {tool_count} tools")
        } else {
            String::new()
        };
        let cost_summary = if area.width >= 140 {
            state.cost_nano_usd.map_or_else(
                || " · cost —".into(),
                |cost| format!(" · {}", crate::app::format_cost(cost)),
            )
        } else {
            String::new()
        };
        let metadata_summary = if area.width < 120 {
            format!(" · model {model} · branch {branch}")
        } else {
            format!(
                " · model {model} · branch {branch} · session {session} · {} tok{cost_summary}{tool_summary}",
                state.input_tokens.saturating_add(state.output_tokens),
            )
        };
        let left = Line::from(vec![
            Span::styled(format!("{} ", mode.glyph()), mode.style()),
            Span::styled(mode_label, mode.style()),
            Span::styled(metadata_summary, theme::text_dim()),
            task_span,
            queue_span,
            editing_span,
        ])
        .alignment(Alignment::Left);

        let hints: Vec<Span> = hints_for_surface(surface, state.mode, mode)
            .into_iter()
            .collect();
        let hints_width = hints
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>()
            .saturating_add(2)
            .min(area.width as usize) as u16;
        let right = Line::from(hints).alignment(Alignment::Right);

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(hints_width)])
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
            Span::styled("↑↓ select", theme::text_muted()),
            Span::raw("   "),
            Span::styled("↵ confirm · esc deny", theme::text_muted()),
        ],
        (_, StatusMode::Asking) => [
            Span::styled("↑↓ select", theme::text_muted()),
            Span::raw("   "),
            Span::styled("↵ choose · esc skip", theme::text_muted()),
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

fn surface_status_label(surface: Option<&str>, fallback: StatusMode) -> &str {
    if matches!(fallback, StatusMode::Connecting | StatusMode::Disconnected) {
        return fallback.label();
    }
    match surface {
        Some("Commands") => "choosing command",
        Some("Model") => "choosing model",
        Some("Permissions") => "choosing permissions",
        Some("Mention file") => "choosing file",
        Some("Sessions") => "choosing session",
        Some("Plan review") => "plan review",
        Some("Plan editor" | "Plan · Edit step") => "editing plan",
        Some("Agent asks") => "answering",
        Some("Rollback files") => "rollback confirmation",
        Some("Tool output") => "reviewing tool output",
        Some("Help") => "help",
        _ => fallback.label(),
    }
}

fn hints_for_surface(
    surface: Option<&str>,
    app_mode: AppMode,
    status_mode: StatusMode,
) -> [Span<'static>; 3] {
    if matches!(
        status_mode,
        StatusMode::Connecting | StatusMode::Disconnected
    ) {
        return hints_for_mode(app_mode, status_mode);
    }
    match surface {
        Some("Commands") => picker_hints("tab complete"),
        Some("Model") => picker_hints("←→ effort"),
        Some("Permissions") => picker_hints("↵ apply"),
        Some("Mention file") => picker_hints("↵ insert"),
        Some("Sessions") => picker_hints("↵ load"),
        Some("Plan review" | "Rollback files") => decision_hints("↵ confirm"),
        Some("Plan editor") => [
            Span::styled("↑↓ step", theme::text_muted()),
            Span::raw("   "),
            Span::styled("↵ done · esc back", theme::text_muted()),
        ],
        Some("Plan · Edit step") => picker_hints("↵ save"),
        Some("Tool output") => [
            Span::styled("↑↓ scroll", theme::text_muted()),
            Span::raw("   "),
            Span::styled("/ search · esc close", theme::text_muted()),
        ],
        Some("Help") => [
            Span::styled("reference", theme::text_muted()),
            Span::raw("   "),
            Span::styled("esc close", theme::text_muted()),
        ],
        _ => hints_for_mode(app_mode, status_mode),
    }
}

fn picker_hints(action: &'static str) -> [Span<'static>; 3] {
    [
        Span::styled("↑↓ select", theme::text_muted()),
        Span::raw("   "),
        Span::styled(format!("{action} · esc close"), theme::text_muted()),
    ]
}

fn decision_hints(action: &'static str) -> [Span<'static>; 3] {
    [
        Span::styled("↑↓ select", theme::text_muted()),
        Span::raw("   "),
        Span::styled(format!("{action} · esc cancel"), theme::text_muted()),
    ]
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "../../tests/unit/panel_status.rs"]
mod tests;
