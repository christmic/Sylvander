//! Status panel — bottom row of the screen, mirroring `02-tui-immersive.svg` line 19.
//!
//! Layout:
//! - Left: `<mode-glyph> <mode-label> · context N% · N tools · main` (or "· main"/"· plan").
//! - Right: up to **three** contextual unicode-symbol hints, mode-aware
//!   per the design's `18-composer-interactions.svg` rule that the
//!   footer hints must be `contextual, maximum three. No permanent
//!   shortcut manual in the footer`.
//!
//! `AppMode` selects the active mode glyph + label.
//! `state.connected` toggles a fourth hint when disconnected so the
//! user knows the failure state without burying the row in text.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AppMode, AppState};
use crate::component::Component;
use crate::theme;

/// Adaptive mode — derived from `AppState::mode` + the agent's
/// underlying transport state. Each mode has a glyph + label, matching
/// the §18 ADAPTIVE STATUS panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusMode {
    Idle,
    Working,
    Waiting,
    Disconnected,
}

impl StatusMode {
    fn glyph(self) -> &'static str {
        match self {
            Self::Idle => "·",
            Self::Working => "◐",
            Self::Waiting => "●",
            Self::Disconnected => "!",
        }
    }
}

fn mode_for(state: &AppState) -> StatusMode {
    if !state.connected {
        return StatusMode::Disconnected;
    }
    match state.mode {
        AppMode::Normal => StatusMode::Idle,
        AppMode::ApprovalPending | AppMode::AskPending => StatusMode::Waiting,
    }
}

pub struct StatusPanel;

impl Component for StatusPanel {
    fn height(&self) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let mode = mode_for(state);
        let (tool_count, task_running) = state
            .messages
            .iter()
            .fold((0usize, false), |(n, running), m| match m {
                crate::app::ChatMessage::ToolStep { children, .. } => {
                    (n + children.len(), running)
                }
                crate::app::ChatMessage::TaskList { tasks } => {
                    let any_running = tasks
                        .iter()
                        .any(|t| matches!(t.state, crate::app::TaskState::Running));
                    (n, running || any_running)
                }
                _ => (n, running),
            });
        // Note: there's no server-pushed context% yet, so we render the
        // placeholder design token (§5.1 spec). Real value lands when
        // the agent reports context% via a new DomainEvent.
        let context_pct = "—";
        let plan_label = "main";
        let task_span: Span = if task_running {
            Span::styled(" · task running", theme::warning())
        } else {
            Span::raw("")
        };
        let mode_style = theme_for_mode(mode);
        let app_mode = state.mode;
        let left = Line::from(vec![
            Span::styled(format!("{} ", mode.glyph()), mode_style),
            Span::styled(format!("{} ", mode_label(mode)), mode_style),
            Span::styled(
                format!("· context {}% · {} tools", context_pct, tool_count),
                theme::text_dim(),
            ),
            task_span,
            Span::styled(format!(" · {plan_label}"), theme::text_muted()),
        ])
        .alignment(Alignment::Left);

        let hints: Vec<Span> = hints_for_mode(app_mode, mode).into_iter().collect();
        let right = Line::from(hints).alignment(Alignment::Right);

        // Split the area so left + right don't overlap.
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(60),
                Constraint::Percentage(40),
            ])
            .split(area);
        frame.render_widget(Paragraph::new(left), layout[0]);
        frame.render_widget(Paragraph::new(right), layout[1]);
    }
}

fn mode_label(mode: StatusMode) -> &'static str {
    match mode {
        StatusMode::Idle => "idle",
        StatusMode::Working => "working",
        StatusMode::Waiting => "waiting",
        StatusMode::Disconnected => "disconnected",
    }
}

fn theme_for_mode(mode: StatusMode) -> ratatui::style::Style {
    use ratatui::style::Style;
    match mode {
        StatusMode::Idle => theme::text_dim(),
        StatusMode::Working => theme::active(),
        StatusMode::Waiting => theme::warning(),
        StatusMode::Disconnected => theme::warning(),
    }
}

/// Up to three contextual hints per `18-composer-interactions.svg`.
/// Returns one `Span` per hint; trim to fit `width` at render time.
fn hints_for_mode(
    app_mode: AppMode,
    status_mode: StatusMode,
) -> [Span<'static>; 3] {
    match (app_mode, status_mode) {
        (_, StatusMode::Disconnected) => [
            Span::styled("! reconnecting…", theme::warning()),
            Span::raw(" "),
            Span::styled("/draft preserved", theme::text_muted()),
        ],
        (AppMode::Normal, _) => [
            Span::styled("↵ send", theme::text_muted()),
            Span::raw(" "),
            Span::styled("⇧↵ newline", theme::text_muted()),
        ],
        (AppMode::ApprovalPending, _) => [
            Span::styled("y approve", theme::text_muted()),
            Span::raw(" "),
            Span::styled("n reject", theme::text_muted()),
        ],
        (AppMode::AskPending, _) => [
            Span::styled("↵ submit", theme::text_muted()),
            Span::raw(" "),
            Span::styled("esc cancel", theme::text_muted()),
        ],
    }
}
