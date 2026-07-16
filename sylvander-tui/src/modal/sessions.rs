//! Sessions overlay — UX §10.
//!
//! Triggered by `Ctrl+P`. Provides a search-filtered list of known
//! sessions with status badges, working directory, and relative time.
//! In standalone TUI mode (this binary), "opening" a session replaces
//! the current view (the Agent switch happens server-side via Chat with
//! `session_id` set).
//!
//! Data is reconciled from the server's persisted session list and refreshed
//! by lifecycle events received after connection.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppMode, AppState};
use crate::modal::{Consumed, Modal, ModalPlacement, surface::focus_picker};
use crate::theme;

/// Status badge for a session row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Working,
    Waiting,
    Complete,
    Failed,
    Disconnected,
}

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub label: String,
    pub status: SessionStatus,
    pub workspace: String,
    pub last_seen_secs: u64,
}

impl SessionEntry {
    pub fn format_time(&self) -> String {
        let s = self.last_seen_secs;
        if s < 60 {
            format!("{s}s ago")
        } else if s < 3600 {
            format!("{}m ago", s / 60)
        } else if s < 86_400 {
            format!("{}h ago", s / 3600)
        } else {
            format!("{}d ago", s / 86_400)
        }
    }
}

pub struct SessionsOverlay {
    /// All known sessions, newest first.
    pub entries: Vec<SessionEntry>,
    /// Currently selected index in `entries` (after filtering).
    pub cursor: usize,
    /// Filter input.
    pub filter: String,
    /// True when the filter text is focused (typing).
    pub filter_focused: bool,
    /// When in delete-confirm mode: index of the entry pending delete.
    pub pending_delete: Option<usize>,
    /// Destructive delete is separate from archive and requires typing DELETE.
    pub pending_permanent_delete: Option<usize>,
    pub permanent_delete_buffer: String,
    /// Original entry index currently being renamed.
    pub renaming: Option<usize>,
    pub rename_buffer: String,
}

impl SessionsOverlay {
    pub fn new(mut entries: Vec<SessionEntry>) -> Self {
        entries.sort_by(|left, right| {
            left.workspace
                .cmp(&right.workspace)
                .then_with(|| left.last_seen_secs.cmp(&right.last_seen_secs))
        });
        Self {
            entries,
            cursor: 0,
            filter: String::new(),
            filter_focused: true,
            pending_delete: None,
            pending_permanent_delete: None,
            permanent_delete_buffer: String::new(),
            renaming: None,
            rename_buffer: String::new(),
        }
    }

    /// Entries that pass the filter (case-insensitive substring match).
    pub fn filtered(&self) -> Vec<(usize, &SessionEntry)> {
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                needle.is_empty()
                    || e.label.to_lowercase().contains(&needle)
                    || e.workspace.to_lowercase().contains(&needle)
                    || e.id.to_lowercase().contains(&needle)
            })
            .collect()
    }
}

impl Modal for SessionsOverlay {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Sessions"
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        let results = self.filtered().len().clamp(1, 7) as u16 + 2;
        ModalPlacement::BelowComposer {
            rows: results.saturating_add(3),
        }
    }

    fn render(&self, frame: &mut Frame, parent: Rect, state: &AppState) {
        let filtered = self.filtered();
        let visible = filtered.len().clamp(1, 7) as u16;
        let areas = focus_picker(frame, parent, visible.saturating_add(2));
        let count = if filtered.len() == 1 {
            "1 match".to_string()
        } else {
            format!("{} matches", filtered.len())
        };
        let title = "Resume a session";
        let gap = (areas.results.width as usize)
            .saturating_sub(UnicodeWidthStr::width(title) + UnicodeWidthStr::width(&*count));
        let mut lines = vec![
            Line::from(vec![
                Span::styled(title, theme::brand_violet()),
                Span::raw(" ".repeat(gap)),
                Span::styled(count, theme::text_muted()),
            ]),
            Line::from(Span::styled(
                "Loading one session replaces the current conversation.",
                theme::text_muted(),
            )),
        ];
        if filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No persisted sessions match",
                theme::subtle_emphasis(theme::text_muted()),
            )));
        } else {
            let list_rows = areas.results.height.saturating_sub(2) as usize;
            let start = self.cursor.saturating_add(1).saturating_sub(list_rows);
            for (row_i, (_, entry)) in filtered.iter().enumerate().skip(start).take(list_rows) {
                let is_active = state
                    .session_id
                    .as_deref()
                    .is_some_and(|sid| sid == entry.id.as_str());
                let is_cursor = row_i == self.cursor;
                let cursor_marker = if is_cursor { "› " } else { "  " };
                let color = if is_cursor {
                    theme::palette().active
                } else {
                    theme::palette().text_dim
                };
                lines.push(Line::from(vec![
                    Span::styled(cursor_marker, Style::default().fg(color)),
                    Span::styled(
                        format!("{:<24}", truncate(&entry.label, 24)),
                        Style::default().fg(color),
                    ),
                    Span::styled(
                        format!("{:<34}", truncate(&entry.workspace, 34)),
                        theme::text_muted(),
                    ),
                    Span::styled(entry.format_time(), theme::text_muted()),
                    Span::styled(
                        if is_active { "  current" } else { "" },
                        theme::text_muted(),
                    ),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(lines), areas.results);

        let query = if self.renaming.is_some() {
            Line::from(vec![
                Span::styled("Rename > ", theme::active()),
                Span::styled(&self.rename_buffer, theme::text()),
                Span::styled("  Enter save · Esc cancel", theme::text_muted()),
            ])
        } else if self.pending_permanent_delete.is_some() {
            Line::from(vec![
                Span::styled("Type DELETE > ", theme::danger()),
                Span::styled(&self.permanent_delete_buffer, theme::text()),
                Span::styled("  Enter confirm · Esc cancel", theme::text_muted()),
            ])
        } else if self.pending_delete.is_some() {
            Line::from(vec![
                Span::styled("Archive this session? ", theme::warning()),
                Span::styled("y confirm · n/esc cancel", theme::text_muted()),
            ])
        } else {
            Line::from(vec![
                Span::styled("/resume ", theme::brand_violet()),
                Span::styled(&self.filter, theme::text()),
                if self.filter_focused {
                    Span::raw("")
                } else {
                    Span::styled(
                        "  actions: r rename · d archive · D delete",
                        theme::text_muted(),
                    )
                },
            ])
        };
        frame.render_widget(Paragraph::new(query), areas.query);

        if self.filter_focused {
            let x = areas.query.x + 8 + UnicodeWidthStr::width(self.filter.as_str()) as u16;
            if x < areas.query.x + areas.query.width {
                frame.set_cursor_position((x, areas.query.y));
            }
        }
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        if let Some(index) = self.renaming {
            match key.code {
                KeyCode::Enter => {
                    let label = self.rename_buffer.trim();
                    if !label.is_empty() {
                        if let Some(entry) = self.entries.get_mut(index) {
                            entry.label = label.to_string();
                            state
                                .pending_actions
                                .push(crate::event::Action::RenameSession {
                                    session_id: entry.id.clone(),
                                    label: label.to_string(),
                                });
                        }
                        if let Some(entry) = state.sessions.get_mut(index) {
                            entry.label = label.to_string();
                        }
                    }
                    self.renaming = None;
                    self.rename_buffer.clear();
                }
                KeyCode::Esc => {
                    self.renaming = None;
                    self.rename_buffer.clear();
                }
                KeyCode::Backspace => {
                    self.rename_buffer.pop();
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.rename_buffer.push(character);
                }
                _ => {}
            }
            state.dirty.mark();
            return Consumed::Yes { dismiss: false };
        }

        if let Some(index) = self.pending_permanent_delete {
            match key.code {
                KeyCode::Esc => {
                    self.pending_permanent_delete = None;
                    self.permanent_delete_buffer.clear();
                }
                KeyCode::Backspace => {
                    self.permanent_delete_buffer.pop();
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.permanent_delete_buffer.push(character);
                }
                KeyCode::Enter if self.permanent_delete_buffer == "DELETE" => {
                    if let Some(entry) = self.entries.get(index) {
                        state
                            .pending_actions
                            .push(crate::event::Action::DeleteSession {
                                session_id: entry.id.clone(),
                            });
                        state.status = format!("Permanently deleting {}…", entry.label);
                    }
                    self.pending_permanent_delete = None;
                    self.permanent_delete_buffer.clear();
                    state.mode = AppMode::Normal;
                    state.dirty.mark();
                    return Consumed::Yes { dismiss: true };
                }
                KeyCode::Enter => {
                    state.status = "Type DELETE exactly to confirm permanent deletion".into();
                }
                _ => {}
            }
            state.dirty.mark();
            return Consumed::Yes { dismiss: false };
        }

        // Delete-confirm layer wins.
        if let Some(idx) = self.pending_delete {
            if let KeyCode::Char('y' | 'Y') = key.code {
                if idx < self.entries.len() {
                    let archived = self.entries[idx].clone();
                    let id = archived.id.clone();
                    state.last_archived_session = Some(archived);
                    state
                        .pending_actions
                        .push(crate::event::Action::ArchiveSession {
                            session_id: id.clone(),
                        });
                    self.entries.remove(idx);
                    state.sessions.retain(|entry| entry.id != id);
                    state.status = "Session archived · Ctrl+Z to undo".into();
                }
                self.pending_delete = None;
                let new_len = self.filtered().len();
                if self.cursor >= new_len && new_len > 0 {
                    self.cursor = new_len - 1;
                }
                state.dirty.mark();
                return Consumed::Yes { dismiss: false };
            }
            // Anything else (Esc / n / etc.) cancels.
            self.pending_delete = None;
            state.dirty.mark();
            return Consumed::Yes { dismiss: false };
        }

        // Key routing.
        match key.code {
            KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(session) = state.last_archived_session.take() {
                    let session_id = session.id.clone();
                    self.entries.insert(0, session.clone());
                    state.sessions.insert(0, session);
                    state
                        .pending_actions
                        .push(crate::event::Action::RestoreSession { session_id });
                    state.status = "Restoring archived session…".into();
                }
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Esc => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+P closes too.
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let _ = crate::command::execute(crate::command::parse("new").unwrap(), state);
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('/') => {
                self.filter_focused = true;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => {
                if state.turn_active {
                    state.status = "Interrupt active work before switching sessions".into();
                    state.dirty.mark();
                    return Consumed::Yes { dismiss: false };
                }
                if let Some((_, entry)) = self.filtered().get(self.cursor) {
                    state
                        .pending_actions
                        .push(crate::event::Action::LoadSession {
                            session_id: entry.id.clone(),
                        });
                    state.status = format!("Loading {}…", entry.label);
                    state.mode = AppMode::Normal;
                    state.dirty.mark();
                    return Consumed::Yes { dismiss: true };
                }
                Consumed::Ignored
            }
            KeyCode::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                let len = self.filtered().len();
                if self.cursor + 1 < len {
                    self.cursor += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('r') if !self.filter_focused => {
                let selected = self
                    .filtered()
                    .get(self.cursor)
                    .map(|(index, entry)| (*index, entry.label.clone()));
                if let Some((original_index, label)) = selected {
                    self.renaming = Some(original_index);
                    self.rename_buffer = label;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('d') if !self.filter_focused => {
                if let Some((original_index, _entry)) = self.filtered().get(self.cursor) {
                    self.pending_delete = Some(*original_index);
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char('D') if !self.filter_focused => {
                if let Some((original_index, _entry)) = self.filtered().get(self.cursor) {
                    self.pending_permanent_delete = Some(*original_index);
                    self.permanent_delete_buffer.clear();
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace if self.filter_focused => {
                if !self.filter.is_empty() {
                    self.filter.pop();
                    self.cursor = 0;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Tab => {
                self.filter_focused = !self.filter_focused;
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Char(c) if self.filter_focused => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.filter.push(c);
                    self.cursor = 0;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, label: &str, status: SessionStatus, ago: u64) -> SessionEntry {
        SessionEntry {
            id: id.into(),
            label: label.into(),
            status,
            workspace: format!("/p/{label}"),
            last_seen_secs: ago,
        }
    }

    fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(c, m)
    }

    #[test]
    fn empty_session_list_renders_no_match_line() {
        let overlay = SessionsOverlay::new(vec![]);
        let filtered = overlay.filtered();
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_is_case_insensitive_substring() {
        let overlay = SessionsOverlay::new(vec![
            entry("a", "Auth-Refactor", SessionStatus::Working, 120),
            entry("b", "JWT-Research", SessionStatus::Complete, 7200),
        ]);
        assert_eq!(overlay.filtered().len(), 2);
        let mut o = overlay;
        o.filter = "auth".into();
        assert_eq!(o.filtered().len(), 1);
        o.filter = "AUTH".into();
        assert_eq!(o.filtered().len(), 1);
        o.filter = "zzz".into();
        assert_eq!(o.filtered().len(), 0);
    }

    #[test]
    fn sessions_group_by_workspace_and_keep_recent_first() {
        let overlay = SessionsOverlay::new(vec![
            SessionEntry {
                workspace: "/b".into(),
                ..entry("b1", "B", SessionStatus::Complete, 5)
            },
            SessionEntry {
                workspace: "/a".into(),
                ..entry("a2", "A2", SessionStatus::Complete, 60)
            },
            SessionEntry {
                workspace: "/a".into(),
                ..entry("a1", "A1", SessionStatus::Complete, 5)
            },
        ]);
        assert_eq!(
            overlay
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["a1", "a2", "b1"]
        );
    }

    #[test]
    fn enter_requests_persisted_session_history() {
        let mut state = AppState::new();
        let mut overlay = SessionsOverlay::new(vec![
            entry("a", "Auth-Refactor", SessionStatus::Working, 120),
            entry("b", "Login-Tests", SessionStatus::Complete, 7200),
        ]);
        overlay.filter_focused = false;
        overlay.cursor = 1;
        let _ = overlay.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert!(matches!(
            state.pending_actions.as_slice(),
            [crate::event::Action::LoadSession { session_id }] if session_id == "b"
        ));
    }

    #[test]
    fn delete_confirm_cancels_on_n() {
        let mut state = AppState::new();
        let mut overlay = SessionsOverlay::new(vec![entry("a", "Foo", SessionStatus::Working, 60)]);
        overlay.pending_delete = Some(0);
        let result = overlay.handle_key(&key(KeyCode::Char('n'), KeyModifiers::NONE), &mut state);
        assert!(matches!(result, Consumed::Yes { dismiss: false }));
        assert!(overlay.pending_delete.is_none());
    }

    #[test]
    fn delete_confirm_removes_entry_on_y() {
        let mut state = AppState::new();
        state.sessions = vec![entry("a", "Foo", SessionStatus::Working, 60)];
        let mut overlay = SessionsOverlay::new(state.sessions.clone());
        overlay.pending_delete = Some(0);
        let result = overlay.handle_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &mut state);
        assert!(matches!(result, Consumed::Yes { dismiss: false }));
        assert_eq!(overlay.entries.len(), 0);
        assert!(state.sessions.is_empty());
        assert!(matches!(
            state.pending_actions.as_slice(),
            [crate::event::Action::ArchiveSession { session_id }] if session_id == "a"
        ));
        assert_eq!(
            state
                .last_archived_session
                .as_ref()
                .map(|session| session.id.as_str()),
            Some("a")
        );
    }

    #[test]
    fn ctrl_z_restores_the_last_archived_session() {
        let mut state = AppState::new();
        let archived = entry("a", "Foo", SessionStatus::Complete, 60);
        state.last_archived_session = Some(archived);
        let mut overlay = SessionsOverlay::new(vec![]);
        overlay.handle_key(&key(KeyCode::Char('z'), KeyModifiers::CONTROL), &mut state);
        assert_eq!(overlay.entries[0].id, "a");
        assert!(matches!(
            state.pending_actions.as_slice(),
            [crate::event::Action::RestoreSession { session_id }] if session_id == "a"
        ));
    }

    #[test]
    fn permanent_delete_requires_exact_typed_confirmation() {
        let mut state = AppState::new();
        let mut overlay = SessionsOverlay::new(vec![entry(
            "a",
            "Critical work",
            SessionStatus::Complete,
            60,
        )]);
        overlay.filter_focused = false;
        overlay.handle_key(&key(KeyCode::Char('D'), KeyModifiers::SHIFT), &mut state);
        for character in "DELETE".chars() {
            overlay.handle_key(
                &key(KeyCode::Char(character), KeyModifiers::SHIFT),
                &mut state,
            );
        }
        let result = overlay.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert!(matches!(result, Consumed::Yes { dismiss: true }));
        assert!(matches!(
            state.pending_actions.as_slice(),
            [crate::event::Action::DeleteSession { session_id }] if session_id == "a"
        ));
        assert_eq!(
            overlay.entries.len(),
            1,
            "server confirmation owns final removal"
        );
    }

    #[test]
    fn rename_updates_overlay_and_application_cache() {
        let mut state = AppState::new();
        state.sessions = vec![entry("a", "Old", SessionStatus::Working, 60)];
        let mut overlay = SessionsOverlay::new(state.sessions.clone());
        overlay.filter_focused = false;
        overlay.handle_key(&key(KeyCode::Char('r'), KeyModifiers::NONE), &mut state);
        overlay.rename_buffer = "New name".into();
        overlay.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE), &mut state);
        assert_eq!(overlay.entries[0].label, "New name");
        assert_eq!(state.sessions[0].label, "New name");
        assert!(matches!(
            state.pending_actions.as_slice(),
            [crate::event::Action::RenameSession { session_id, label }]
                if session_id == "a" && label == "New name"
        ));
    }

    #[test]
    fn new_session_on_ctrl_n_does_not_send_an_empty_prompt() {
        let mut state = AppState::new();
        state.session_id = Some("old".into());
        let mut overlay = SessionsOverlay::new(vec![]);
        let result =
            overlay.handle_key(&key(KeyCode::Char('n'), KeyModifiers::CONTROL), &mut state);
        assert!(matches!(result, Consumed::Yes { dismiss: true }));
        assert!(state.pending_actions.is_empty());
        assert!(state.session_id.is_none());
    }

    #[test]
    fn tab_toggles_filter_focus() {
        let mut state = AppState::new();
        let mut overlay = SessionsOverlay::new(vec![]);
        assert!(overlay.filter_focused);
        let _ = overlay.handle_key(&key(KeyCode::Tab, KeyModifiers::NONE), &mut state);
        assert!(!overlay.filter_focused);
    }
}
