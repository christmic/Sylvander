//! Header panel — top of the screen, 2-line identity block per UX §5.1.
//!
//! Mirrors `02-tui-immersive.svg` line 5: a `◖S◗` coral crab mark +
//! session name on line 1 (left), `<model> · <mode>` on line 1 (right),
//! workspace + branch + session-id on line 2, hairline rule below.
//!
//! This panel replaces the old one-line status bar (`Sylvander · model
//! · connected`) that lived at the top. The status semantics now live
//! at the BOTTOM (`M-T14.C`), matching the SVG ground truth.

use std::path::PathBuf;

use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::AppState;
use crate::component::Component;
use crate::theme;

pub struct HeaderPanel;

impl Component for HeaderPanel {
    fn height(&self) -> Constraint {
        Constraint::Length(3) // 2 content rows + 1 hairline
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let session_label = session_label_for(state);
        let workspace = workspace_label_for(state);
        let model = state
            .sessions
            .first()
            .map(|s| format_model_label(s))
            .unwrap_or_else(|| "—".into());
        let mode = mode_label(state);
        let session_id = state
            .session_id
            .as_deref()
            .map(|s| truncate(s, 8))
            .unwrap_or_else(|| "—".into());

        // Line 1 left: ◖S◗ + space + <session-label> (bold ivory)
        // Line 1 right: <model> · <mode>  (coral model + dim mode)
        let left = Line::from(vec![
            Span::styled("◖S◗", theme::coral()),
            Span::raw("  Sylvander  "),
            Span::styled(session_label, theme::header()),
        ]);
        let right = Line::from(vec![
            Span::styled(model.clone(), theme::coral()),
            Span::styled(" · ", theme::text_muted()),
            Span::styled(mode, theme::text_dim()),
        ]);

        // Compose top row with left/right alignment by drawing into two
        // chunks: 0..(w-30) for the left, (w-30)..w for the right.
        let (left_area, right_area) = split_left_right(area, 30);
        frame.render_widget(Paragraph::new(left), left_area);
        frame.render_widget(
            Paragraph::new(right)
                .alignment(Alignment::Right),
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
        let rule_line = Line::from("─".repeat(area.width as usize))
            .style(theme::rule());
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
    if let Some(active_id) = active {
        if let Some(e) = state.sessions.iter().find(|s| s.id == active_id) {
            return e.label.clone();
        }
    }
    state.sessions[0].label.clone()
}

fn workspace_label_for(state: &AppState) -> String {
    if let Some(e) = state
        .sessions
        .iter()
        .find(|s| state.session_id.as_deref().map(|id| s.id == id).unwrap_or(false))
    {
        if !e.workspace.is_empty() {
            return e.workspace.clone();
        }
    }
    std::env::current_dir()
        .map(|p: PathBuf| p.display().to_string())
        .unwrap_or_else(|_| "~/".into())
}

fn format_model_label(session: &crate::modal::sessions::SessionEntry) -> String {
    // Show workspace basename if present, else fall back to a stub.
    if session.workspace.is_empty() {
        session.label.clone()
    } else {
        session.workspace.clone()
    }
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
