//! Chat panel — main conversation area. Uses virtual scroll to render
//! only the lines that fit on screen, bottom-aligned. Every color and
//! state-derived glyph routes through `crate::theme` so the design
//! palette + state glyphs stay in a single module.

use ratatui::{
    layout::{Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{AppState, ChatMessage, ToolStatus};
use crate::component::Component;
use crate::theme;

pub struct ChatPanel;

impl Component for ChatPanel {
    fn height(&self) -> Constraint {
        Constraint::Min(0)
    }

    fn render(&self, frame: &mut Frame, area: Rect, state: &AppState) {
        let block = Block::default().borders(Borders::NONE);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let width = inner.width as usize;

        // Welcome lockup (UX §2.2): first launch before any messages
        // and any known sessions.
        if !state.welcomed && state.messages.is_empty() && state.sessions.is_empty() {
            if let Some(welcome_lines) = build_welcome_lockup(width) {
                let total = welcome_lines.len() as u16;
                let top_pad = inner.height.saturating_sub(total) / 2;
                let centered = Rect {
                    x: inner.x,
                    y: inner.y + top_pad,
                    width: inner.width,
                    height: total.min(inner.height),
                };
                frame.render_widget(Paragraph::new(welcome_lines), centered);
                return;
            }
        }

        let mut lines: Vec<Line> = Vec::new();
        for msg in &state.messages {
            push_message_lines(msg, &mut lines, width);
        }

        if !state.streaming_thinking.is_empty() {
            for chunk in char_chunks(&state.streaming_thinking, width) {
                lines.push(Line::from(Span::styled(
                    format!("(thinking) {chunk}"),
                    theme::thinking_text(),
                )));
            }
        }

        if !state.streaming.is_empty() {
            for chunk in char_chunks(&state.streaming, width) {
                lines.push(Line::from(Span::styled(chunk.to_string(), theme::text())));
            }
        }

        let visible = inner.height as usize;
        let total = lines.len();
        let start = if total > visible + state.chat_scroll {
            total - visible - state.chat_scroll
        } else {
            0
        };

        for (row, line) in lines.iter().skip(start).enumerate() {
            if row >= visible {
                break;
            }
            let line_area = Rect {
                x: inner.x,
                y: inner.y + row as u16,
                width: inner.width,
                height: 1,
            };
            frame.render_widget(line.clone(), line_area);
        }
    }
}

fn push_message_lines<'a>(msg: &'a ChatMessage, lines: &mut Vec<Line<'a>>, width: usize) {
    match msg {
        ChatMessage::User(text) => {
            for chunk in char_chunks(text, width.saturating_sub(5)) {
                lines.push(Line::from(vec![
                    Span::styled("You: ", theme::user_speaker()),
                    Span::styled(chunk.to_string(), theme::text()),
                ]));
            }
        }
        ChatMessage::Agent(text) => {
            for chunk in char_chunks(text, width) {
                lines.push(Line::from(Span::styled(chunk.to_string(), theme::text())));
            }
        }
        ChatMessage::ToolCall { name, status, input } => {
            let (icon, st) = theme::tool_status_glyph_and_style(*status);
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), st),
                Span::styled(name, st),
            ]));
            for line in input_kv_lines(input, width) {
                lines.push(line);
            }
        }
        ChatMessage::ToolResult { name, output, ok } => {
            let icon = if *ok { "  ✓" } else { "  ✗" };
            let st = if *ok { theme::verified() } else { theme::warning() };
            let summary = truncate(output, width.saturating_sub(name.len() + 6));
            lines.push(Line::from(vec![
                Span::styled(icon, st),
                Span::styled(format!(" {name}: "), theme::text_dim()),
                Span::styled(summary, st),
            ]));
        }
        ChatMessage::ToolStep { name, started_at_secs, children } => {
            let all_done = children
                .iter()
                .all(|c| c.status != ToolStatus::Pending);
            let (step_glyph, step_style) = if all_done {
                (theme::tool_status_glyph(ToolStatus::Done), theme::verified())
            } else {
                (theme::tool_status_glyph(ToolStatus::Pending), theme::active_bold())
            };
            let elapsed = format_elapsed(*started_at_secs);
            lines.push(Line::from(vec![
                Span::styled(format!("{step_glyph} "), step_style),
                Span::styled(name.clone(), step_style),
                Span::styled(format!("{elapsed:>8}"), theme::text_muted()),
            ]));
            for child in children {
                let (g, gstyle) = theme::tool_status_glyph_and_style(child.status);
                let target = child_target_line(&child.name, &child.input);
                let meta = child
                    .output
                    .as_deref()
                    .map(|o| summarize_output(o, width))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled("│ ", theme::guide()),
                    Span::styled(format!("{g} "), gstyle),
                    Span::styled(target, theme::text_dim()),
                    Span::styled(
                        if meta.is_empty() {
                            String::new()
                        } else {
                            format!("  {meta}")
                        },
                        theme::text_muted(),
                    ),
                ]));
            }
        }
        ChatMessage::Thinking(text) => {
            for chunk in char_chunks(text, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    theme::thinking_text(),
                )));
            }
        }
        ChatMessage::Info(text) => {
            lines.push(Line::from(Span::styled(
                format!("  {text}"),
                theme::text_muted(),
            )));
        }
        ChatMessage::Plan { plan_id: _, steps, current } => {
            lines.push(Line::from(Span::styled(
                "Proposed plan",
                theme::header(),
            )));
            for (i, step) in steps.iter().enumerate() {
                let completed = i < *current;
                let current_step = i == *current;
                let (marker, st) =
                    theme::plan_step_glyph_and_style(completed, current_step);
                lines.push(Line::from(Span::styled(
                    format!("  {marker} {}. {step}", i + 1),
                    st,
                )));
            }
        }
        ChatMessage::TaskList { tasks } => {
            let total = tasks.len();
            let done = tasks
                .iter()
                .filter(|t| t.state == crate::app::TaskState::Done)
                .count();
            let running = tasks
                .iter()
                .filter(|t| t.state == crate::app::TaskState::Running)
                .count();
            lines.push(Line::from(Span::styled(
                format!("▶ tasks {done}/{total} done · {running} running"),
                theme::task_summary_line(),
            )));
        }
    }
}

fn char_chunks(s: &str, width: usize) -> Vec<&str> {
    if width == 0 {
        return vec![s];
    }
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in s.char_indices() {
        if i - start >= width {
            out.push(&s[start..i]);
            start = i;
        }
    }
    out.push(&s[start..]);
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn truncate_first(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn render_json_value(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Null => "-".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let inner: Vec<String> = a.iter().take(4).map(render_json_value).collect();
            if a.len() > 4 {
                format!("[{}, …(+{})]", inner.join(", "), a.len() - 4)
            } else {
                format!("[{}]", inner.join(", "))
            }
        }
        Value::Object(o) => {
            let inner: Vec<String> = o
                .iter()
                .take(4)
                .map(|(k, v)| format!("{k}={}", render_json_value(v)))
                .collect();
            if o.len() > 4 {
                format!("{{{}, …(+{})}}", inner.join(", "), o.len() - 4)
            } else {
                format!("{{{}}}", inner.join(", "))
            }
        }
    }
}

fn input_kv_lines(input: &serde_json::Value, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    match input {
        serde_json::Value::Null => {}
        serde_json::Value::Object(map) if map.is_empty() => {}
        serde_json::Value::Object(map) => {
            let label_w = map
                .keys()
                .map(|k| k.chars().count())
                .max()
                .unwrap_or(0)
                .min(20);
            for (k, v) in map {
                let rendered = render_json_value(v);
                let label = format!("  {:<w$}  ", k, w = label_w);
                let val_str = truncate(&rendered, width.saturating_sub(label_w + 6));
                out.push(Line::from(vec![
                    Span::styled(label, theme::kv_label()),
                    Span::styled(val_str, theme::kv_value()),
                ]));
            }
        }
        _ => {
            let rendered = render_json_value(input);
            out.push(Line::from(Span::styled(
                format!("  → {}", truncate(&rendered, width.saturating_sub(6))),
                theme::text(),
            )));
        }
    }
    out
}

fn child_target_line(tool: &str, input: &serde_json::Value) -> String {
    let fallback = || format!("{tool} {}", render_json_value(input));
    let some_path = |k| input.get(k).and_then(|v| v.as_str());
    match tool {
        "read" | "write" | "edit" => match some_path("path") {
            Some(p) => format!("{tool} {p}"),
            None => fallback(),
        },
        "bash" => match some_path("command") {
            Some(c) => format!("$ {}", truncate_first(c, 60)),
            None => fallback(),
        },
        "search" | "grep" => match (some_path("pattern"), some_path("path")) {
            (Some(p), Some(path)) => format!("search \"{p}\" in {path}"),
            (Some(p), None) => format!("search \"{p}\""),
            _ => fallback(),
        },
        _ => fallback(),
    }
}

fn summarize_output(output: &str, width: usize) -> String {
    let line = output
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        return String::new();
    }
    truncate_first(line, width.saturating_sub(20).min(80))
}

fn format_elapsed(started_at_secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(started_at_secs);
    let s = now.saturating_sub(started_at_secs);
    if s < 60 {
        format!("{:02}s", s)
    } else if s < 3600 {
        format!("{:02}:{:02}", s / 60, s % 60)
    } else {
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

fn build_welcome_lockup(width: usize) -> Option<Vec<Line<'static>>> {
    if width < 40 {
        return None;
    }
    let workspace_label = std::env::current_dir()
        .ok()
        .map(|p| theme::compact_workspace(&p, 50))
        .unwrap_or_else(|| "~".into());

    Some(vec![
        Line::from(vec![
            Span::styled("◖S◗  ", theme::coral()),
            Span::styled("SYLVANDER", theme::header()),
        ]),
        Line::from(Span::styled(
            "      intelligent terminal workspace",
            theme::composer_helper(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("      ", theme::text_dim()),
            Span::styled(workspace_label, theme::text_dim()),
        ]),
        Line::from(Span::styled(
            "      What are we building today?",
            theme::text_dim(),
        )),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppState, ChatMessage, ToolStatus};
    use crate::component::Component;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn terminal(w: u16, h: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(w, h)).expect("terminal")
    }

    fn seeded() -> AppState {
        let mut s = AppState::new();
        s.apply(crate::event::DomainEvent::Connected);
        s.messages.push(ChatMessage::User("Hi".into()));
        s.apply(crate::event::DomainEvent::TextChunk {
            delta: "world".into(),
        });
        s.apply(crate::event::DomainEvent::AgentDone {
            final_text: "world".into(),
        });
        s
    }

    #[test]
    fn user_speaker_uses_dim_color() {
        let s = seeded();
        let mut t = terminal(60, 12);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 12), &s))
            .unwrap();
        let cell = t.backend().buffer().cell((0, 0)).expect("cell");
        assert_eq!(cell.fg, crate::theme::TEXT_DIM);
    }

    #[test]
    fn agent_body_uses_primary_text() {
        let s = seeded();
        let mut t = terminal(60, 12);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 12), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let mut found = false;
        for y in 0..12 {
            for x in 0..60 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.fg == crate::theme::TEXT {
                        found = true;
                        break;
                    }
                }
            }
            if found {
                break;
            }
        }
        assert!(found, "expected a primary-text cell");
    }

    #[test]
    fn welcome_lockup_renders_crab_at_first_launch() {
        let s = AppState::new();
        let mut t = terminal(120, 36);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 120, 36), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let mut found = false;
        for y in 0..36 {
            for x in 0..120 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.symbol() == "◖" {
                        found = true;
                        break;
                    }
                }
            }
            if found {
                break;
            }
        }
        assert!(found, "expected ◖ glyph in welcome lockup");
    }

    #[test]
    fn welcome_lockup_absent_when_welcomed() {
        let mut s = AppState::new();
        s.welcomed = true;
        s.messages.push(ChatMessage::User("x".into()));
        let mut t = terminal(120, 36);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 120, 36), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        for y in 0..36 {
            for x in 0..120 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.symbol() == "◖" {
                        panic!("welcome lockup must not show when welcomed=true");
                    }
                }
            }
        }
    }

    #[test]
    fn input_kv_lines_skips_null_and_empty_object() {
        assert!(input_kv_lines(&serde_json::Value::Null, 80).is_empty());
        assert!(input_kv_lines(&serde_json::json!({}), 80).is_empty());
    }

    #[test]
    fn input_kv_lines_emits_pair_per_object_key() {
        // serde_json::Map defaults to BTreeMap, so keys come back in
        // alphabetical order — assert set membership instead of ordering.
        let lines =
            input_kv_lines(&serde_json::json!({"path": "/tmp", "mode": "r"}), 60);
        assert_eq!(lines.len(), 2);
        let labels: Vec<String> = lines
            .iter()
            .map(|l| l.spans[0].content.to_string())
            .collect();
        assert!(
            labels.iter().any(|l| l.contains("path")),
            "expected `path` label, got: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("mode")),
            "expected `mode` label, got: {labels:?}"
        );
    }

    #[test]
    fn child_target_line_bash_prefixes_dollar() {
        let s = child_target_line("bash", &serde_json::json!({"command": "ls"}));
        assert!(s.starts_with('$'), "expected `$` prefix, got `{s}`");
    }

    #[test]
    fn plan_step_three_glyphs() {
        let (gd, _) = theme::plan_step_glyph_and_style(true, false);
        let (gc, _) = theme::plan_step_glyph_and_style(false, true);
        let (gp, _) = theme::plan_step_glyph_and_style(false, false);
        assert_eq!(gd, "✓");
        assert_eq!(gc, "●");
        assert_eq!(gp, "○");
    }

    #[test]
    fn tool_status_styles_three_distinct_fg() {
        let (_, sp) = theme::tool_status_glyph_and_style(ToolStatus::Pending);
        let (_, sd) = theme::tool_status_glyph_and_style(ToolStatus::Done);
        let (_, se) = theme::tool_status_glyph_and_style(ToolStatus::Error);
        assert_ne!(sp.fg, sd.fg);
        assert_ne!(sp.fg, se.fg);
        assert_ne!(sd.fg, se.fg);
    }

    #[test]
    fn render_order_user_then_agent_then_toolstep() {
        // Contract: messages render in insertion order. User at the
        // top, then the agent's reply, then a grouped tool step.
        let mut s = AppState::new();
        s.apply(crate::event::DomainEvent::Connected);
        s.messages.push(ChatMessage::User("Hi".into()));
        s.apply(crate::event::DomainEvent::TextChunk {
            delta: "Hello back".into(),
        });
        s.apply(crate::event::DomainEvent::AgentDone {
            final_text: "Hello back".into(),
        });
        s.apply(crate::event::DomainEvent::ToolStarted {
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        });
        s.apply(crate::event::DomainEvent::ToolFinished {
            tool_name: "bash".into(),
            output: "a.rs".into(),
            is_error: false,
        });
        let mut t = terminal(60, 20);
        t.draw(|f| ChatPanel.render(f, Rect::new(0, 0, 60, 20), &s))
            .unwrap();
        let buf = t.backend().buffer().clone();
        // Find the first row of each message kind. We do this by
        // searching the first column of each row for "Y" (the
        // "You:" speaker label). Once found, the user row is the
        // smallest such y. The agent body is the row above any tool
        // step (which renders `● Run ...` or `✓ Run ...`).
        let mut you_y = None;
        for y in 0..20 {
            for x in 0..60 {
                if let Some(c) = buf.cell((x, y)) {
                    if c.symbol() == "Y" {
                        you_y = Some(y);
                        break;
                    }
                }
            }
            if you_y.is_some() {
                break;
            }
        }
        let you_y = you_y.expect("expected to find 'Y' from 'You:' label");
        // Tool step row has `●` or `✓` glyph in the step header.
        let mut toolstep_y = None;
        for y in 0..20 {
            for x in 0..60 {
                if let Some(c) = buf.cell((x, y)) {
                    let sym = c.symbol();
                    if sym == "\u{25cf}" || sym == "\u{2713}" {
                        // Step glyphs are tool-step step-header characters.
                        toolstep_y = Some(y);
                        break;
                    }
                }
            }
            if toolstep_y.is_some() {
                break;
            }
        }
        let toolstep_y = toolstep_y.expect("expected to find tool step glyph");
        // Order: user (y=0 by convention but at least smallest) precedes
        // toolstep. They are guaranteed by the way push_message_lines
        // walks the messages vec.
        assert!(you_y < toolstep_y, "user row {you_y} must precede toolstep {toolstep_y}");
    }
}
