//! Fuzzy workspace-file picker opened by `@` in the composer.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::app::AppState;
use crate::modal::{Consumed, Modal};
use crate::theme;

pub struct FileMentionModal {
    workspace: PathBuf,
    files: Vec<String>,
    query: String,
    cursor: usize,
    error: Option<String>,
    max_attachment_bytes: usize,
    allow_images: bool,
}

impl FileMentionModal {
    pub fn new(workspace: PathBuf, max_attachment_bytes: usize, allow_images: bool) -> Self {
        let files = discover_files(&workspace, 5_000);
        Self { workspace, files, query: String::new(), cursor: 0, error: None, max_attachment_bytes, allow_images }
    }

    fn matches(&self) -> Vec<&str> {
        let mut matches = self.files.iter().filter_map(|path| {
            fuzzy_score(path, &self.query).map(|score| (score, path.as_str()))
        }).collect::<Vec<_>>();
        matches.sort_by_key(|(score, path)| (*score, path.len()));
        matches.into_iter().map(|(_, path)| path).take(10).collect()
    }
}

impl Modal for FileMentionModal {
    fn active(&self) -> bool { true }
    fn title(&self) -> &str { "Mention file" }

    fn render(&self, frame: &mut Frame, parent: Rect, _state: &AppState) {
        let area = centered_rect(72, 15, parent);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default().borders(Borders::ALL).title(" @ Add workspace file ")
                .title_style(theme::active_bold()),
            area,
        );
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let rows = Layout::default().direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(8), Constraint::Length(1)])
            .split(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("@ ", theme::brand_violet()),
                Span::styled(&self.query, theme::text()),
                Span::styled("_", theme::active()),
            ])),
            rows[0],
        );
        let matches = self.matches();
        let lines = if matches.is_empty() {
            vec![Line::from(Span::styled("No matching workspace files", theme::text_muted()))]
        } else {
            matches.iter().enumerate().map(|(index, path)| {
                Line::from(vec![
                    Span::styled(if index == self.cursor { "› " } else { "  " },
                        if index == self.cursor { theme::active() } else { theme::text_muted() }),
                    Span::styled((*path).to_string(),
                        if index == self.cursor { theme::text() } else { theme::text_dim() }),
                ])
            }).collect()
        };
        frame.render_widget(Paragraph::new(lines), rows[1]);
        let footer = self.error.as_deref().unwrap_or("enter attach · ↑↓ select · esc close");
        frame.render_widget(Paragraph::new(Span::styled(footer,
            if self.error.is_some() { theme::danger() } else { theme::text_muted() })), rows[2]);
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        self.error = None;
        match key.code {
            KeyCode::Esc => Consumed::Yes { dismiss: true },
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                let len = self.matches().len();
                if self.cursor + 1 < len { self.cursor += 1; }
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                let selected = self.matches().get(self.cursor).map(|path| (*path).to_string());
                let Some(path) = selected else {
                    self.error = Some("Choose a file".into());
                    return Consumed::Yes { dismiss: false };
                };
                match state.composer.attach_file(
                    &self.workspace,
                    Path::new(&path),
                    self.max_attachment_bytes,
                    self.allow_images,
                ) {
                    Ok(()) => Consumed::Yes { dismiss: true },
                    Err(error) => {
                        self.error = Some(error);
                        Consumed::Yes { dismiss: false }
                    }
                }
            }
            KeyCode::Char(ch) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.query.push(ch);
                self.cursor = 0;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn discover_files(root: &Path, limit: usize) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, files: &mut Vec<String>, limit: usize) {
        if files.len() >= limit { return; }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if files.len() >= limit { break; }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".idea" | ".next") {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() { continue; }
            if kind.is_dir() {
                walk(root, &path, files, limit);
            } else if kind.is_file() {
                if let Ok(relative) = path.strip_prefix(root) {
                    files.push(relative.display().to_string());
                }
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files, limit);
    files
}

fn fuzzy_score(candidate: &str, query: &str) -> Option<usize> {
    if query.is_empty() { return Some(candidate.len()); }
    let candidate_lower = candidate.to_ascii_lowercase();
    let query_lower = query.to_ascii_lowercase();
    if let Some(index) = candidate_lower.find(&query_lower) { return Some(index); }
    let mut offset = 0usize;
    let mut score = 0usize;
    for wanted in query_lower.chars() {
        let found = candidate_lower[offset..].find(wanted)?;
        offset += found + wanted.len_utf8();
        score += found + 4;
    }
    Some(score)
}

fn centered_rect(percent_x: u16, height: u16, parent: Rect) -> Rect {
    let vertical = Layout::default().direction(Direction::Vertical).constraints([
        Constraint::Length(parent.height.saturating_sub(height) / 2),
        Constraint::Length(height.min(parent.height)),
        Constraint::Min(0),
    ]).split(parent);
    let horizontal = Layout::default().direction(Direction::Horizontal).constraints([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Min(0),
    ]).split(vertical[1]);
    horizontal[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_match_supports_substring_and_subsequence() {
        assert!(fuzzy_score("src/panel/input.rs", "input").is_some());
        assert!(fuzzy_score("src/panel/input.rs", "spi").is_some());
        assert!(fuzzy_score("src/panel/input.rs", "zzz").is_none());
    }
}
