//! Chat panel — the main conversation area. Uses virtual scroll: only
//! renders the lines that fit on screen, bottom-aligned.

use ratatui::{
    layout::{Constraint, Rect},
    prelude::Stylize,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{AppMode, AppState, ChatMessage, ToolStatus};
use crate::component::Component;

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

        // Welcome lockup (UX §2.2): show once when no messages have
        // ever been created and no sessions are known. The lockup is
        // capped at four rows (per design §2.2 — "Maximum welcome
        // lockup height is five terminal rows"), centered horizontally
        // in the chat panel.
        if !state.welcomed
            && state.messages.is_empty()
            && state.sessions.is_empty()
        {
            if let Some(welcome_lines) = build_welcome_lockup(width) {
                let total = welcome_lines.len() as u16;
                let top_pad = inner.height.saturating_sub(total) / 2;
                let centered = Rect {
                    x: inner.x,
                    y: inner.y + top_pad,
                    width: inner.width,
                    height: total.min(inner.height),
                };
                frame.render_widget(
                    Paragraph::new(welcome_lines),
                    centered,
                );
                return;
            }
        }

        // Build all lines first (cheap, just string formatting).
        let mut lines: Vec<Line> = Vec::new();

        for msg in &state.messages {
            push_message_lines(msg, &mut lines, width);
        }

        // Streaming thinking (italic, gray) — render above streaming text.
        if !state.streaming_thinking.is_empty() {
            for chunk in char_chunks(&state.streaming_thinking, width) {
                lines.push(Line::from(Span::styled(
                    format!("(thinking) {chunk}"),
                    Style::default().fg(Color::DarkGray).italic(),
                )));
            }
        }

        // Streaming text (white).
        if !state.streaming.is_empty() {
            for chunk in char_chunks(&state.streaming, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        }

        // Virtual scroll: show last N lines that fit, plus user scroll offset.
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

        // If we're in AskPending mode with an empty composer, render a hint.
        // Currently the AskUser popup overlays this region, so we skip.
        if matches!(state.mode, AppMode::AskPending) && state.composer.is_empty() {
            // placeholder for future inline hint
        }
    }
}

fn push_message_lines<'a>(msg: &'a ChatMessage, lines: &mut Vec<Line<'a>>, width: usize) {
    match msg {
        ChatMessage::User(text) => {
            for chunk in char_chunks(text, width.saturating_sub(5)) {
                lines.push(Line::from(Span::styled(
                    format!("You: {chunk}"),
                    Style::default().fg(Color::Cyan),
                )));
            }
        }
        ChatMessage::Agent(text) => {
            for chunk in char_chunks(text, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        }
        ChatMessage::ToolCall {
            name,
            status,
            input,
        } => {
            // Legacy flat tool — render as a single one-line entry
            // (kept so tests that pre-date ToolStep grouping still
            // produce a coherent trace).
            let (icon, color) = match status {
                ToolStatus::Pending => ("◐", Color::Yellow),
                ToolStatus::Done => ("✓", Color::Green),
                ToolStatus::Error => ("✗", Color::Red),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color).bold()),
                Span::styled(name, Style::default().fg(color).bold()),
            ]));
            for line in input_kv_lines(input, width) {
                lines.push(line);
            }
        }
        ChatMessage::ToolResult {
            name,
            output,
            ok,
        } => {
            let icon = if *ok { "  +" } else { "  x" };
            let color = if *ok { Color::Green } else { Color::Red };
            let summary = crate::panel::chat::truncate(output, width.saturating_sub(name.len() + 6));
            lines.push(Line::from(Span::styled(
                format!("{icon} {name}: {summary}"),
                Style::default().fg(color),
            )));
        }
        ChatMessage::ToolStep {
            name,
            started_at_secs,
            children,
        } => {
            // UX §6 / 02-tui-immersive.svg lines 9-14: a step header
            // (`● <name>` in blue) followed by indented child rows
            // each starting with a `✓` (teal) / `◐` (blue) glyph.
            // The vertical guide `│` column signals the parent→child
            // relationship. Live children use `◐`; completed ones use
            // `✓`; errored ones use `✗` and dim red.
            let all_done = children
                .iter()
                .all(|c| c.status != ToolStatus::Pending);
            let (step_glyph, step_color) = if all_done {
                ("✓", Color::Green)
            } else {
                ("●", Color::Blue)
            };
            let elapsed = format_elapsed(*started_at_secs);
            lines.push(Line::from(vec![
                Span::styled(format!("{step_glyph} "), Style::default().fg(step_color).bold()),
                Span::styled(name.clone(), Style::default().fg(step_color).bold()),
                Span::styled(
                    format!("{elapsed:>8}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            // Vertical guide on row(s) where children live: rendered as
            // `│` in GUIDE color aligned to the `step_glyph` column.
            for child in children {
                let (g, color) = match child.status {
                    ToolStatus::Pending => ("◐", Color::Blue),
                    ToolStatus::Done => ("✓", Color::Green),
                    ToolStatus::Error => ("✗", Color::Red),
                };
                let target = child_target_line(&child.name, &child.input);
                let meta = if let Some(out) = &child.output {
                    summarize_output(out, width)
                } else {
                    String::new()
                };
                lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{g} "), Style::default().fg(color)),
                    Span::styled(target, Style::default().fg(Color::Gray)),
                    Span::styled(
                        if meta.is_empty() {
                            String::new()
                        } else {
                            format!("  {meta}")
                        },
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        ChatMessage::Thinking(text) => {
            for chunk in char_chunks(text, width) {
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(Color::DarkGray).italic(),
                )));
            }
        }
        ChatMessage::Info(text) => {
            lines.push(Line::from(Span::styled(
                format!("  {text}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        ChatMessage::Plan {
            plan_id: _,
            steps,
            current,
        } => {
            // UX §9: plan renders as a numbered list with ✓ ● ○ markers.
            // Header says "Proposed plan" so the user can scan the
            // transcript and find it later.
            lines.push(Line::from(Span::styled(
                "Proposed plan",
                Style::default().fg(Color::Yellow).bold(),
            )));
            for (i, step) in steps.iter().enumerate() {
                let (marker, color) = if i < *current {
                    ("✓", Color::Green)
                } else if i == *current {
                    ("●", Color::Cyan)
                } else {
                    ("○", Color::DarkGray)
                };
                let label = format!("{marker} {}. {}", i + 1, step);
                lines.push(Line::from(Span::styled(
                    format!("  {label}"),
                    Style::default().fg(color),
                )));
            }
        }
        ChatMessage::TaskList { tasks } => {
            // Compact one-liner per UX §11: `▶ n/m done · <owner> <purpose>`.
            // Drops per-task lines (the agent loop verbose form) — too noisy
            // for the immersive transcript.
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
                Style::default().fg(Color::Magenta),
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

/// Render a `serde_json::Value` as a compact single-line string.
/// Strings are shown unquoted (their content only), numbers / bools as
/// themselves, null as `-`, arrays collapsed to `[…]`, objects to
/// `{k=v, k2=v2}`.
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

/// Render the input of a tool as one or more `key  value` lines. Used
/// both for legacy flat `ToolCall` rendering and for `ToolStep` rows.
fn input_kv_lines(input: &serde_json::Value, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    match input {
        serde_json::Value::Null => {
            // No payload — header alone is enough.
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                // Empty object — keep the line clean: don't dump "{}".
            } else {
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
                        Span::styled(label, Style::default().fg(Color::DarkGray)),
                        Span::styled(val_str, Style::default().fg(Color::White)),
                    ]));
                }
            }
        }
        _ => {
            let rendered = render_json_value(input);
            out.push(Line::from(Span::styled(
                format!("  → {}", truncate(&rendered, width.saturating_sub(6))),
                Style::default().fg(Color::White),
            )));
        }
    }
    out
}

/// Compact one-line representation of a tool child: `<name>  <target>`.
/// For bash tools we render the command; for read/write/edit we render
/// the path; for search we render the pattern + path.
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

fn truncate_first(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Compact one-line summary of a tool's output. Picks the most useful
/// preview metric: file count, search match count, or first non-empty line.
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

/// Format elapsed seconds since `started_at_secs` as `00:00` or `00:00:00`.
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

/// The first-launch welcome lockup (UX §2.2 + `02-tui-immersive.svg`
/// reference).  Four terminal rows:
///
/// ```text
///   ◖S◗  SYLVANDER
///        intelligent terminal workspace
///
///        ~/Projects/<cwd>
///        What are we building today?
/// ```
///
/// The crab mark + `SYLVANDER` wordmark uses coral; the tagline,
/// workspace, and prompt line use the soft ivory / muted palette.
///
/// Returns `None` if `width` is too narrow (<40 cols) so the lockup
/// stays out of the way on minimal terminals (§13 responsive rules).
fn build_welcome_lockup(width: usize) -> Option<Vec<Line<'static>>> {
    if width < 40 {
        return None;
    }
    let workspace_label = std::env::current_dir()
        .ok()
        .map(short_workspace_label)
        .unwrap_or_else(|| "~".into());

    Some(vec![
        // Line 1: ◖S◗ + SYLVANDER
        Line::from(vec![
            Span::styled("◖S◗  ", crate::theme::coral()),
            Span::styled("SYLVANDER", crate::theme::header()),
        ]),
        // Line 2: tagline (italic, dim)
        Line::from(Span::styled(
            "      intelligent terminal workspace",
            crate::theme::composer_helper(),
        )),
        // Line 3: blank spacer
        Line::from(""),
        // Line 4: workspace path
        Line::from(vec![
            Span::styled("      ", crate::theme::text_dim()),
            Span::styled(workspace_label, crate::theme::text_dim()),
        ]),
        // Line 5: prompt line
        Line::from(Span::styled(
            "      What are we building today?",
            crate::theme::text_dim(),
        )),
    ])
}

/// Shorten a workspace path: when the full path is too long, show the
/// basename (e.g. `.../acme-api`), prefixed by `…/` so the truncation
/// is visible. Below the threshold, return the path verbatim.
fn short_workspace_label(p: std::path::PathBuf) -> String {
    let s = p.display().to_string();
    if s.chars().count() <= 60 {
        return s;
    }
    let basename = p
        .components()
        .next_back()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "~".into());
    format!(".../{basename}")
}