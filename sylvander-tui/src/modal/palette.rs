//! Command palette — UX §12. Triggered by typing `/` in the composer
//! area (when it is otherwise empty). Provides a fuzzy-filtered command
//! list the user can invoke without leaving the keyboard home row.
//!
//! The input is a real command line: entries can be selected, or commands with
//! arguments such as `theme midnight` can be typed and submitted directly.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::app::{AppMode, AppState};
pub use crate::command::{COMMANDS, CommandMatch, CommandSpec as Command};
use crate::modal::{Consumed, Modal, ModalPlacement};

pub struct CommandPalette {
    pub filter: String,
    pub cursor: usize,
    pub filtered: Vec<CommandMatch>,
    pub error: Option<String>,
}

impl CommandPalette {
    pub fn new(state: &AppState) -> Self {
        let mut s = Self {
            filter: composer_filter(state),
            cursor: 0,
            filtered: Vec::new(),
            error: None,
        };
        s.recompute(state);
        s
    }

    pub fn recompute(&mut self, state: &AppState) {
        self.filtered = crate::command::ranked_commands(&self.filter, state);
        if self.cursor >= self.filtered.len() {
            self.cursor = 0;
        }
    }

    fn complete_selection(&mut self, state: &mut AppState) {
        let Some(selected) = self.filtered.get(self.cursor) else {
            return;
        };
        let name = crate::command::match_name(selected, state);
        let suffix = self
            .filter
            .find(char::is_whitespace)
            .map(|index| self.filter[index..].trim_start().to_string());
        self.filter = suffix
            .filter(|suffix| !suffix.is_empty())
            .map_or_else(|| format!("{name} "), |suffix| format!("{name} {suffix}"));
        self.error = None;
        self.recompute(state);
        state.composer.replace_text(&format!("/{}", self.filter));
        state.dirty.mark();
    }

    /// Run the currently-selected command, pushing the appropriate
    /// side-effect onto `AppState`'s `pending_actions`.
    fn invoke(&mut self, state: &mut AppState) -> Consumed {
        let typed_name = self.filter.split_whitespace().next().unwrap_or("");
        let exact_typed = crate::command::resolve(typed_name).is_some()
            || state
                .platform
                .commands
                .iter()
                .any(|command| command.name.eq_ignore_ascii_case(typed_name));
        let line = if exact_typed {
            self.filter.clone()
        } else if let Some(command_match) = self.filtered.get(self.cursor) {
            crate::command::match_name(command_match, state).to_string()
        } else {
            self.error = Some("No matching command".into());
            return Consumed::Yes { dismiss: false };
        };
        match crate::command::execute_line(&line, state) {
            Ok(()) => {
                state.composer.clear();
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            Err(error) => {
                self.error = Some(error);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
        }
    }
}

impl Modal for CommandPalette {
    fn active(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Commands"
    }

    fn placement(&self, _state: &AppState, _viewport_width: u16) -> ModalPlacement {
        let results = self.filtered.len().clamp(1, 8) as u16 + u16::from(self.error.is_some());
        ModalPlacement::BelowComposer {
            rows: results.saturating_add(1),
        }
    }

    fn uses_composer_input(&self) -> bool {
        true
    }

    fn render(&self, frame: &mut Frame, parent: Rect, state: &AppState) {
        frame.render_widget(Clear, parent);
        frame.render_widget(Block::default().style(theme::text_on_canvas()), parent);
        let results_area = Rect {
            x: parent.x,
            width: parent.width,
            height: parent.height.saturating_sub(1),
            ..parent
        };
        let mut lines = Vec::new();
        if let Some(error) = self.error.as_deref() {
            lines.push(Line::from(Span::styled(error, theme::warning())));
        }

        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "No commands match",
                theme::subtle_emphasis(theme::text_muted()),
            )));
        } else {
            let visible_rows = results_area
                .height
                .saturating_sub(u16::from(self.error.is_some()))
                .max(1) as usize;
            let start = self.cursor.saturating_add(1).saturating_sub(visible_rows);
            let needs_more_row = self.filtered.len() > start + visible_rows;
            let command_rows = visible_rows.saturating_sub(usize::from(needs_more_row));
            let hidden_below = self.filtered.len().saturating_sub(start + command_rows);
            let name_width = self
                .filtered
                .iter()
                .skip(start)
                .take(command_rows)
                .map(|entry| crate::command::match_name(entry, state).chars().count())
                .max()
                .unwrap_or(13)
                .clamp(13, 18);
            for (row_i, command_match) in self
                .filtered
                .iter()
                .enumerate()
                .skip(start)
                .take(command_rows)
            {
                let name = crate::command::match_name(command_match, state);
                let is_cursor = row_i == self.cursor;
                let prefix = if is_cursor { "› " } else { "  " };
                let color = if is_cursor {
                    theme::palette().active
                } else {
                    theme::palette().text
                };
                let available = command_match.availability.is_available();
                let row_style = if available {
                    Style::default().fg(color)
                } else {
                    theme::text_muted()
                };
                let description = command_match.availability.reason().map_or_else(
                    || crate::command::match_description(command_match, state).to_string(),
                    str::to_string,
                );
                let detail = crate::command::match_source(command_match, state)
                    .filter(|_| command_match.availability.is_available())
                    .map_or(description.clone(), |source| {
                        format!("{description} · {source}")
                    });
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(color)),
                    Span::styled(
                        format!("/{name:<name_width$} "),
                        Style::default().fg(if is_cursor && available {
                            theme::palette().active
                        } else {
                            theme::palette().brand_violet
                        }),
                    ),
                    Span::styled(detail, row_style),
                ]));
            }
            if hidden_below > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  ↓ {hidden_below} more"),
                    theme::text_muted(),
                )));
            }
        }
        frame.render_widget(Paragraph::new(lines), results_area);
        if parent.height > 0 {
            frame.render_widget(
                Paragraph::new(Line::from("─".repeat(parent.width as usize)).style(theme::rule())),
                Rect {
                    y: parent.y + parent.height - 1,
                    height: 1,
                    ..parent
                },
            );
        }
    }

    fn handle_key(&mut self, key: &KeyEvent, state: &mut AppState) -> Consumed {
        match key.code {
            KeyCode::Esc => {
                state.composer.clear();
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.composer.clear();
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Down => {
                if self.cursor + 1 < self.filtered.len() {
                    self.cursor += 1;
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Backspace => {
                let _ = state.composer.handle_key(key);
                self.filter = composer_filter(state);
                if state.composer.is_empty() {
                    state.mode = AppMode::Normal;
                    return Consumed::Yes { dismiss: true };
                }
                self.error = None;
                self.recompute(state);
                state.dirty.mark();
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Delete if self.filter.is_empty() => {
                state.composer.clear();
                state.mode = AppMode::Normal;
                Consumed::Yes { dismiss: true }
            }
            KeyCode::Delete => Consumed::Yes { dismiss: false },
            KeyCode::Tab => {
                self.complete_selection(state);
                Consumed::Yes { dismiss: false }
            }
            KeyCode::Enter => self.invoke(state),
            KeyCode::Char(_) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    let _ = state.composer.handle_key(key);
                    self.filter = composer_filter(state);
                    self.error = None;
                    self.recompute(state);
                    state.dirty.mark();
                }
                Consumed::Yes { dismiss: false }
            }
            _ => Consumed::Ignored,
        }
    }
}

fn composer_filter(state: &AppState) -> String {
    let text = state.composer.text();
    text.strip_prefix('/').unwrap_or(&text).to_string()
}

use crate::theme;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "../../tests/unit/modal_palette.rs"]
mod tests;
