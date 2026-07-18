//! Header panel — top of the screen, 2-line identity block per UX §5.1.
//!
//! Uses the approved Seed-Crab terminal presence + session name on line 1
//! (left), `<role> · <mode>` on line 1 (right),
//! workspace + branch + session-id on line 2, hairline rule below.
//!
//! This panel replaces the old one-line status bar (`Sylvander · model
//! · connected`) that lived at the top. The status semantics now live
//! at the BOTTOM (`M-T14.C`), matching the SVG ground truth.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::AppState;
use crate::component::Component;
use crate::theme;

pub struct HeaderPanel;

impl Component for HeaderPanel {
    fn height(&self, _state: &AppState, _viewport_width: u16) -> Constraint {
        Constraint::Length(3) // 2 content rows + 1 hairline
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let session_label = session_label_for(state);
        let workspace = workspace_label_for(state);
        let model = state.metadata.model_label();
        let mode = mode_label(state);
        let session_id = state
            .session_id
            .as_deref()
            .map_or_else(|| "—".into(), |s| truncate(s, 8));

        // Compact Seed-Crab presence from the approved terminal system.
        // Line 1 right: <model> · <mode>  (coral model + dim mode)
        let left = Line::from(vec![
            Span::styled("/\\", theme::coral()),
            Span::raw("  Sylvander  "),
            Span::styled(session_label, theme::header()),
        ]);
        let right = Line::from(vec![
            Span::styled(model, theme::text_dim()),
            Span::styled(" · ", theme::text_muted()),
            Span::styled(mode, theme::text_dim()),
        ]);

        // Compose top row with left/right alignment by drawing into two
        // chunks: 0..(w-30) for the left, (w-30)..w for the right.
        let (left_area, right_area) = split_left_right(area, 30);
        frame.render_widget(Paragraph::new(left), left_area);
        frame.render_widget(
            Paragraph::new(right).alignment(Alignment::Right),
            right_area,
        );

        // Line 2: <workspace> · session <id> (subtitle, dim)
        let subtitle = Line::from(vec![
            Span::styled(workspace, theme::text_dim()),
            Span::styled(" · session ", theme::text_muted()),
            Span::styled(session_id, theme::text_dim()),
        ]);
        let subtitle_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(subtitle), subtitle_area);

        // Line 3: hairline rule (a full-width `─` line in RULE color).
        let rule_line = Line::from("─".repeat(area.width as usize)).style(theme::rule());
        let rule_area = Rect {
            x: area.x,
            y: area.y + 2,
            width: area.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(rule_line), rule_area);
    }
}

fn split_left_right(area: Rect, right_min: u16) -> (Rect, Rect) {
    let right_w = area.width.saturating_sub(right_min).max(8);
    let left_w = area.width - right_w;
    let left = Rect {
        x: area.x,
        y: area.y,
        width: left_w,
        height: area.height.min(1),
    };
    let right = Rect {
        x: area.x + left_w,
        y: area.y,
        width: right_w,
        height: area.height.min(1),
    };
    (left, right)
}

fn session_label_for(state: &AppState) -> String {
    if state.sessions.is_empty() {
        return "new session".into();
    }
    let active = state.session_id.as_deref();
    if let Some(active_id) = active
        && let Some(e) = state.sessions.iter().find(|s| s.id == active_id)
    {
        return e.label.clone();
    }
    state.sessions[0].label.clone()
}

fn workspace_label_for(state: &AppState) -> String {
    if let Some(e) = state
        .sessions
        .iter()
        .find(|s| state.session_id.as_deref().is_some_and(|id| s.id == id))
        && !e.workspace.is_empty()
    {
        return theme::compact_workspace(&std::path::PathBuf::from(&e.workspace), 54);
    }
    theme::compact_workspace(&state.metadata.workspace, 54)
}

fn mode_label(state: &AppState) -> &'static str {
    match state.mode {
        crate::app::AppMode::Normal => "main",
        crate::app::AppMode::ApprovalPending => "approval",
        crate::app::AppMode::AskPending => "ask",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/panel_header.rs"]
mod tests;
