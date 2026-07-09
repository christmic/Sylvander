//! Text input state machine.

use crossterm::event::KeyCode;

/// Single-line text input with cursor movement.
#[derive(Default)]
pub struct InputState {
    pub buffer: String,
    pub cursor: usize, // byte offset
}

impl InputState {
    /// Process a key event. Returns `Some(text)` when Enter is pressed.
    pub fn handle_key(&mut self, key: &KeyCode) -> Option<String> {
        match key {
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.buffer);
                self.cursor = 0;
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            KeyCode::Char(c) => {
                self.buffer.insert(self.cursor, *c);
                self.cursor += c.len_utf8();
                None
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // Find the start byte of the previous char
                    let mut pos = self.cursor - 1;
                    while pos > 0 && !self.buffer.is_char_boundary(pos) {
                        pos -= 1;
                    }
                    self.buffer.drain(pos..self.cursor);
                    self.cursor = pos;
                }
                None
            }
            KeyCode::Delete => {
                if self.cursor < self.buffer.len() {
                    let mut end = self.cursor + 1;
                    while end < self.buffer.len() && !self.buffer.is_char_boundary(end) {
                        end += 1;
                    }
                    self.buffer.drain(self.cursor..end);
                }
                None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    let mut pos = self.cursor - 1;
                    while pos > 0 && !self.buffer.is_char_boundary(pos) {
                        pos -= 1;
                    }
                    self.cursor = pos;
                }
                None
            }
            KeyCode::Right => {
                if self.cursor < self.buffer.len() {
                    let mut pos = self.cursor + 1;
                    while pos < self.buffer.len() && !self.buffer.is_char_boundary(pos) {
                        pos += 1;
                    }
                    self.cursor = pos;
                }
                None
            }
            KeyCode::Home => {
                self.cursor = 0;
                None
            }
            KeyCode::End => {
                self.cursor = self.buffer.len();
                None
            }
            _ => None,
        }
    }
}
