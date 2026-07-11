//! Multiline composer — the only input surface in the TUI.
//!
//! Design:
//! - Rows are stored as `Vec<String>`, one `String` per line (UTF-8 safe).
//! - Cursor is `(row, col)` where `col` is a *byte* offset into `rows[row]`.
//! - All char-edge work uses `is_char_boundary` so multi-byte chars never desync.
//! - Enter inserts a newline; **Alt+Enter / Ctrl+Enter** submits (terminally
//!   Alt+Enter is the conventional multi-line send; we accept Ctrl+Enter as
//!   a fallback because some terminals swallow Alt).
//! - Up/Down arrows walk a history ring (`history`, capped at 100 entries).
//! - Shift+Left/Right extends a selection; selection is byte-wise inclusive of
//!   start and exclusive of end.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const HISTORY_CAP: usize = 100;

/// Multiline composer with cursor, optional selection, and history.
///
/// Returned by `AppState::handle_key` indirectly: when the user presses
/// `Alt+Enter` or `Ctrl+Enter` with non-empty buffer, `take_submit()` returns
/// the joined text and clears the buffer.
pub struct Composer {
    /// One String per visible row. Always non-empty (a "blank" line is `""`).
    rows: Vec<String>,
    /// Cursor row index, 0..rows.len().
    cursor_row: usize,
    /// Cursor *byte* offset within `rows[cursor_row]`.
    cursor_col: usize,
    /// Optional selection anchor (`row`, `col` byte offset).
    anchor: Option<(usize, usize)>,
    /// Past submissions, newest at the back.
    history: VecDeque<String>,
    /// Position when navigating history. `None` means "editing the live buffer".
    history_idx: Option<usize>,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            rows: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            anchor: None,
            history: VecDeque::new(),
            history_idx: None,
        }
    }
}

impl Composer {
    /// Current buffer concatenated with `\n` between rows.
    pub fn text(&self) -> String {
        self.rows.join("\n")
    }

    /// True if buffer is empty (single empty row).
    pub fn is_empty(&self) -> bool {
        self.rows.len() == 1 && self.rows[0].is_empty()
    }

    /// Number of visible rows. Drives the input panel height.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Borrow the i-th row's text. Panics if `i >= row_count()`.
    pub fn row(&self, i: usize) -> &str {
        &self.rows[i]
    }

    /// Convenience for callers that already know the row is in range.
    pub fn text_with_row(&self, i: usize) -> String {
        self.rows.get(i).cloned().unwrap_or_default()
    }

    /// Current cursor row (0-indexed).
    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    /// Current cursor col, in *chars* (not bytes — for rendering width).
    pub fn cursor_col_chars(&self) -> usize {
        char_count(&self.rows[self.cursor_row][..self.cursor_col])
    }

    /// Cursor `x` in chars (for rendering offset within the current row).
    pub fn row_char_len(&self, row: usize) -> usize {
        char_count(&self.rows[row])
    }

    /// Drain the current buffer, returning it, and clear composer state.
    pub fn take_submit(&mut self) -> String {
        let text = self.text();
        if !text.is_empty() {
            // Push into history (dedup against last).
            match self.history.back() {
                Some(last) if last == &text => {}
                _ => {
                    if self.history.len() == HISTORY_CAP {
                        self.history.pop_front();
                    }
                    self.history.push_back(text.clone());
                }
            }
        }
        self.rows = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.anchor = None;
        self.history_idx = None;
        text
    }

    /// Reset the composer to an empty buffer (no history push).
    pub fn clear(&mut self) {
        self.rows = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.anchor = None;
        self.history_idx = None;
    }

    /// Process a key. Returns `Some(text)` on submit (Alt/Ctrl+Enter).
    pub fn handle_key(&mut self, key: &KeyEvent) -> Option<String> {
        // History navigation is independent of selection/shift; do it first.
        if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT {
            match key.code {
                KeyCode::Up => return self.history_move(-1),
                KeyCode::Down => return self.history_move(1),
                _ => {}
            }
        }

        // Submit: Alt+Enter or Ctrl+Enter.
        let mods = key.modifiers;
        let submit = (mods.contains(KeyModifiers::ALT)
            || mods.contains(KeyModifiers::CONTROL))
            && key.code == KeyCode::Enter;

        // Selection-extending movement: Shift held, with plain arrows/Home/End.
        let shift = mods.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Enter if submit => {
                if self.is_empty() {
                    return None;
                }
                self.anchor = None;
                return Some(self.take_submit());
            }
            KeyCode::Enter => {
                self.insert_newline();
            }
            KeyCode::Backspace => {
                if self.backspace() == ActionEffect::SelectionDeleted
                    || self.cursor_row > 0
                    || self.cursor_col > 0
                {
                    // covered
                }
                self.clear_selection_if_empty();
            }
            KeyCode::Delete => {
                self.delete_forward();
                self.clear_selection_if_empty();
            }
            KeyCode::Left => {
                if shift {
                    self.extend_selection_left();
                } else {
                    self.collapse_selection_left();
                }
            }
            KeyCode::Right => {
                if shift {
                    self.extend_selection_right();
                } else {
                    self.collapse_selection_right();
                }
            }
            KeyCode::Up => { /* handled above */ }
            KeyCode::Down => { /* handled above */ }
            KeyCode::Home => {
                let at = (self.cursor_row, 0);
                if shift {
                    self.set_anchor(at);
                } else {
                    self.anchor = None;
                }
                self.cursor_col = 0;
            }
            KeyCode::End => {
                let len = self.rows[self.cursor_row].len();
                let at = (self.cursor_row, len);
                if shift {
                    self.set_anchor(at);
                } else {
                    self.anchor = None;
                }
                self.cursor_col = len;
            }
            KeyCode::Char(c) => {
                if mods.contains(KeyModifiers::CONTROL) && c == 'c' {
                    // let the global Ctrl+C handler in AppState pick this up.
                    return None;
                }
                self.insert_char(c);
                self.clear_selection_if_empty();
            }
            _ => return None,
        }
        None
    }

    // ---- internal helpers ---------------------------------------------------

    fn insert_char(&mut self, c: char) {
        if let Some((s, e)) = self.selection_range() {
            self.delete_range(s, e);
        }
        let row = &mut self.rows[self.cursor_row];
        row.insert(self.cursor_col, c);
        self.cursor_col += c.len_utf8();
    }

    fn insert_newline(&mut self) {
        if let Some((s, e)) = self.selection_range() {
            self.delete_range(s, e);
        }
        let current = std::mem::take(&mut self.rows[self.cursor_row]);
        let (left, right) = split_at_byte(&current, self.cursor_col);
        self.rows[self.cursor_row] = left;
        self.cursor_row += 1;
        self.rows.insert(self.cursor_row, right);
        self.cursor_col = 0;
        self.anchor = None;
    }

    /// `Backspace` action.
    fn backspace(&mut self) -> ActionEffect {
        if let Some((s, e)) = self.selection_range() {
            self.delete_range(s, e);
            self.anchor = None;
            return ActionEffect::SelectionDeleted;
        }
        if self.cursor_col > 0 {
            let row = &mut self.rows[self.cursor_row];
            let mut pos = self.cursor_col - 1;
            while pos > 0 && !row.is_char_boundary(pos) {
                pos -= 1;
            }
            row.drain(pos..self.cursor_col);
            self.cursor_col = pos;
        } else if self.cursor_row > 0 {
            let cur = self.rows.remove(self.cursor_row);
            self.cursor_row -= 1;
            let prev_len = self.rows[self.cursor_row].len();
            self.rows[self.cursor_row].push_str(&cur);
            self.cursor_col = prev_len;
        }
        ActionEffect::Nothing
    }

    /// `Delete` action.
    fn delete_forward(&mut self) {
        if let Some((s, e)) = self.selection_range() {
            self.delete_range(s, e);
            self.anchor = None;
            return;
        }
        let row_len = self.rows[self.cursor_row].len();
        if self.cursor_col < row_len {
            let mut end = self.cursor_col + 1;
            while end < row_len && !self.rows[self.cursor_row].is_char_boundary(end) {
                end += 1;
            }
            self.rows[self.cursor_row].drain(self.cursor_col..end);
        } else if self.cursor_row + 1 < self.rows.len() {
            let next = self.rows.remove(self.cursor_row + 1);
            self.rows[self.cursor_row].push_str(&next);
        }
    }

    fn extend_selection_left(&mut self) {
        self.set_anchor((self.cursor_row, self.cursor_col));
        self.move_cursor_left();
    }

    fn extend_selection_right(&mut self) {
        self.set_anchor((self.cursor_row, self.cursor_col));
        self.move_cursor_right();
    }

    fn collapse_selection_left(&mut self) {
        if let Some((s, _)) = self.selection_range() {
            self.cursor_row = s.0;
            self.cursor_col = s.1;
            self.anchor = None;
            return;
        }
        self.move_cursor_left();
    }

    fn collapse_selection_right(&mut self) {
        if let Some((_, e)) = self.selection_range() {
            self.cursor_row = e.0;
            self.cursor_col = e.1;
            self.anchor = None;
            return;
        }
        self.move_cursor_right();
    }

    fn move_cursor_left(&mut self) {
        if self.cursor_col > 0 {
            let mut pos = self.cursor_col - 1;
            while pos > 0 && !self.rows[self.cursor_row].is_char_boundary(pos) {
                pos -= 1;
            }
            self.cursor_col = pos;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.rows[self.cursor_row].len();
        }
        self.clear_selection_if_empty();
    }

    fn move_cursor_right(&mut self) {
        let row_len = self.rows[self.cursor_row].len();
        if self.cursor_col < row_len {
            let mut pos = self.cursor_col + 1;
            while pos < row_len && !self.rows[self.cursor_row].is_char_boundary(pos) {
                pos += 1;
            }
            self.cursor_col = pos;
        } else if self.cursor_row + 1 < self.rows.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
        self.clear_selection_if_empty();
    }

    fn set_anchor(&mut self, at: (usize, usize)) {
        if let Some(a) = self.anchor {
            if a == at && (self.cursor_row, self.cursor_col) == at {
                self.anchor = None;
                return;
            }
        }
        self.anchor = Some(at);
    }

    fn clear_selection_if_empty(&mut self) {
        if let Some(a) = self.anchor {
            if a == (self.cursor_row, self.cursor_col) {
                self.anchor = None;
            }
        }
    }

    /// Returns (start, end) in (row, col) byte-offset form. `start` and `end`
    /// are normalized so the range can be deleted with row-major order.
    fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let c = (self.cursor_row, self.cursor_col);
        if a == c {
            return None;
        }
        Some(if cmp_pos(a, c).is_lt() { (a, c) } else { (c, a) })
    }

    fn delete_range(&mut self, s: (usize, usize), e: (usize, usize)) {
        if s.0 == e.0 {
            // Same row.
            self.rows[s.0].drain(s.1..e.1);
        } else {
            // Stitch: keep `s.0` left part + drop `e.0` right part, remove rows between.
            let left = self.rows[s.0][..s.1].to_string();
            let right = self.rows[e.0][e.1..].to_string();
            // Remove rows in [s.0, e.0] inclusive.
            for _ in s.0..=e.0 {
                self.rows.remove(s.0);
            }
            self.rows.insert(s.0, format!("{left}{right}"));
        }
        self.cursor_row = s.0;
        self.cursor_col = s.1;
        self.anchor = None;
    }

    /// Navigate through history. `delta` is +1 (newer) or -1 (older).
    fn history_move(&mut self, delta: isize) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }
        // Compute next index. `None` ("live") only responds to `Up`.
        let next_idx: Option<usize> = match self.history_idx {
            None if delta < 0 => Some(self.history.len() - 1),
            None => return None,
            Some(i) => {
                let signed = i as isize + delta;
                if signed < 0 || (signed as usize) >= self.history.len() {
                    // Walked past either edge — back to live.
                    None
                } else {
                    Some(signed as usize)
                }
            }
        };

        match next_idx {
            None => {
                self.history_idx = None;
                self.rows = vec![String::new()];
            }
            Some(idx) => {
                self.history_idx = Some(idx);
                let snapshot = self.history[idx].clone();
                self.rows = snapshot.split('\n').map(String::from).collect();
                if self.rows.is_empty() {
                    self.rows.push(String::new());
                }
            }
        }

        // Move cursor to end of current rows.
        self.cursor_row = self.rows.len() - 1;
        self.cursor_col = self.rows[self.cursor_row].len();
        self.anchor = None;
        None
    }
}

#[derive(PartialEq)]
enum ActionEffect {
    Nothing,
    SelectionDeleted,
}

/// Compare two `(row, col)` positions row-major.
fn cmp_pos(a: (usize, usize), b: (usize, usize)) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then(a.1.cmp(&b.1))
}

fn split_at_byte(s: &str, byte: usize) -> (String, String) {
    if byte >= s.len() {
        (s.to_string(), String::new())
    } else {
        let mut cut = byte;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        (s[..cut].to_string(), s[cut..].to_string())
    }
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn enter_inserts_newline_not_submit() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(c.text(), "a\nb");
        assert_eq!(c.row_count(), 2);
    }

    #[test]
    fn alt_enter_submits() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('i'), KeyModifiers::NONE));
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(submitted.as_deref(), Some("hi"));
        assert!(c.is_empty());
    }

    #[test]
    fn ctrl_enter_submits_fallback() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(submitted.as_deref(), Some("x"));
        assert!(c.is_empty());
    }

    #[test]
    fn alt_enter_on_empty_does_nothing() {
        let mut c = Composer::default();
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(submitted, None);
    }

    #[test]
    fn multi_line_submit_joined_with_newline() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(submitted.as_deref(), Some("a\nb"));
    }

    #[test]
    fn backspace_at_start_of_row_joins_with_previous_row() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        // rows = ["ab", ""], cursor at (1, 0)
        c.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE));
        // joins row 1 into row 0 → "ab"
        assert_eq!(c.text(), "ab");
        assert_eq!(c.row_count(), 1);
        assert_eq!(c.cursor_row(), 0);
        assert_eq!(c.cursor_col_chars(), 2);
    }

    #[test]
    fn unicode_safe_round_trip() {
        let mut c = Composer::default();
        for ch in "你好".chars() {
            c.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        c.handle_key(&key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(c.text(), "你");
        // Cursor should be on the char boundary, not split a code point.
        assert!(c.rows[0].is_char_boundary(c.cursor_col));
    }

    #[test]
    fn history_up_recalls_previous_submission() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        c.handle_key(&key(KeyCode::Char('w'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        // Now in fresh empty buffer. Press Up → recall last "w".
        c.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(c.text(), "w");
        // Up again → "h".
        c.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(c.text(), "h");
        // Down once → back to "w".
        c.handle_key(&key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(c.text(), "w");
    }

    #[test]
    fn history_dedupes_consecutive_equal_submissions() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        c.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(c.history.len(), 1);
    }

    #[test]
    fn history_is_capped_at_history_cap_unique_entries() {
        // Each submission must be distinct to defeat dedup; otherwise the
        // ring would never fill. We append a counter to make every entry unique.
        let mut c = Composer::default();
        for i in 0..(HISTORY_CAP + 5) {
            let s = format!("q{i}");
            for ch in s.chars() {
                c.handle_key(&key(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            let _ = c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        }
        assert_eq!(c.history.len(), HISTORY_CAP);
    }

    #[test]
    fn submit_pushes_into_history() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('z'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(c.history.back().map(String::as_str), Some("z"));
    }

    #[test]
    fn take_submit_clears_buffer() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('q'), KeyModifiers::NONE));
        let _ = c.take_submit();
        assert!(c.is_empty());
    }

    #[test]
    fn delete_forward_at_end_of_row_merges_with_next_row() {
        // Construct state with cursor at end of "ab" and a second row containing
        // "cd". Then Delete should join them into "abcd".
        let mut c = Composer::default();
        c.rows = vec!["ab".to_string(), "cd".to_string()];
        c.cursor_row = 0;
        c.cursor_col = 2;
        c.anchor = None;
        c.handle_key(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(c.text(), "abcd");
        assert_eq!(c.row_count(), 1);
    }
}
