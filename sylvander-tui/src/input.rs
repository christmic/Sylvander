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

/// Inline-vs-attachment threshold per design §12.4 — "Pasted content under
/// eight lines stays inline." Larger pastes collapse to an attachment token.
pub const INLINE_PASTE_LINE_LIMIT: usize = 8;

/// What kinds of attachment the composer can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// Bulk text pasted from the clipboard (≥ `INLINE_PASTE_LINE_LIMIT` lines).
    Paste,
    /// A file/buffer reference (M-T2.4 — currently only populated by tests;
    /// production path arrives when file picker lands).
    File,
}

/// A collapsed payload attached above the draft. Tokens render as a
/// single-line object so multi-kilobyte pastes don't blow out the layout.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub content: String,
    pub line_count: usize,
    pub byte_count: usize,
    /// Truncated preview shown in the token (e.g. "lorem ipsum dolor sit amet…").
    pub preview: String,
    pub name: String,
    pub mime_type: String,
}

impl Attachment {
    pub fn new_paste(content: String) -> Self {
        let line_count = if content.is_empty() {
            0
        } else {
            content.matches('\n').count() + 1
        };
        let byte_count = content.len();
        let preview = make_preview(&content, 32);
        Self {
            kind: AttachmentKind::Paste,
            content,
            line_count,
            byte_count,
            preview,
            name: "pasted text".into(),
            mime_type: "text/plain".into(),
        }
    }

    pub fn from_file(workspace: &std::path::Path, path: &std::path::Path) -> Result<Self, String> {
        const MAX_FILE_BYTES: u64 = 512 * 1024;
        let root = workspace.canonicalize().map_err(|error| error.to_string())?;
        let absolute = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
        let absolute = absolute.canonicalize().map_err(|error| error.to_string())?;
        if !absolute.starts_with(&root) {
            return Err("file mention must stay inside the workspace".into());
        }
        let metadata = absolute.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() { return Err("file mention is not a regular file".into()); }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!("file is larger than {} KiB", MAX_FILE_BYTES / 1024));
        }
        let bytes = std::fs::read(&absolute).map_err(|error| error.to_string())?;
        let content = String::from_utf8(bytes).map_err(|_| "binary files require image attachment support".to_string())?;
        let relative = absolute.strip_prefix(&root).unwrap_or(&absolute).display().to_string();
        let line_count = content.lines().count();
        let byte_count = content.len();
        Ok(Self {
            kind: AttachmentKind::File,
            preview: relative.clone(),
            name: relative.clone(),
            mime_type: mime_for_path(&relative).into(),
            content,
            line_count,
            byte_count,
        })
    }

    pub fn to_message_attachment(&self, index: usize) -> sylvander_protocol::MessageAttachment {
        sylvander_protocol::MessageAttachment {
            id: format!("composer-attachment-{}", index + 1),
            kind: match self.kind {
                AttachmentKind::Paste => sylvander_protocol::AttachmentKind::Paste,
                AttachmentKind::File => sylvander_protocol::AttachmentKind::File,
            },
            name: self.name.clone(),
            mime_type: self.mime_type.clone(),
            content: sylvander_protocol::AttachmentContent::Text { text: self.content.clone() },
            byte_count: self.byte_count,
        }
    }

    /// Short label for the token, e.g. `[paste: 23 lines · 1.2kB] lorem…`.
    pub fn label(&self) -> String {
        let kind = match self.kind {
            AttachmentKind::Paste => "paste",
            AttachmentKind::File => "file",
        };
        let size = human_bytes(self.byte_count);
        format!(
            "[{kind}: {} lines · {size}] {}",
            self.line_count, self.preview
        )
    }
}

/// Outcome of a paste operation — the caller (panel) uses this to decide
/// whether to redraw an extra attachment row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteOutcome {
    /// Pasted text inserted inline into the draft.
    Inlined,
    /// Text was collapsed into a new attachment token above the draft.
    Attached,
}

/// Multiline composer with cursor, optional selection, history, attachments.
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
    pub(crate) history: VecDeque<String>,
    /// Position when navigating history. `None` means "editing the live buffer".
    pub(crate) history_idx: Option<usize>,
    /// Collapsed payloads above the draft.
    pub attachments: Vec<Attachment>,
    submitted_attachments: Vec<sylvander_protocol::MessageAttachment>,
    /// UX §18 IDLE/FOCUSED: whether the user has interacted with this
    /// composer at least once. `false` until the first state-mutating
    /// keystroke (or paste) flips it permanently to `true`. Until then
    /// the panel renders an IDLE muted border rather than the coral
    /// FOCUSED stroke.
    pub(crate) interacted: bool,
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
            attachments: Vec::new(),
            submitted_attachments: Vec::new(),
            interacted: false,
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

    pub fn can_open_file_mention(&self) -> bool {
        if self.cursor_col == 0 { return true; }
        self.rows[self.cursor_row][..self.cursor_col]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
    }

    /// Cursor `x` in chars (for rendering offset within the current row).
    pub fn row_char_len(&self, row: usize) -> usize {
        char_count(&self.rows[row])
    }

    /// Drain the current buffer, returning it, and clear composer state.
    /// Attachments are moved to a separate typed submission payload.
    pub fn take_submit(&mut self) -> String {
        let draft = self.text();
        let composed = draft;
        self.submitted_attachments = self.attachments.iter().enumerate()
            .map(|(index, attachment)| attachment.to_message_attachment(index))
            .collect();

        // History dedup only when the **whole** composed payload is identical
        // to the last submission (paste content makes collisions rarer).
        let normalized = composed.trim().to_string();
        if !normalized.is_empty() {
            match self.history.back() {
                Some(last) if last == &composed => {}
                _ => {
                    if self.history.len() == HISTORY_CAP {
                        self.history.pop_front();
                    }
                    self.history.push_back(composed.clone());
                }
            }
        }

        self.rows = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.anchor = None;
        self.history_idx = None;
        self.attachments.clear();
        composed
    }

    pub fn take_submitted_attachments(&mut self) -> Vec<sylvander_protocol::MessageAttachment> {
        std::mem::take(&mut self.submitted_attachments)
    }

    pub fn attach_file(&mut self, workspace: &std::path::Path, path: &std::path::Path) -> Result<(), String> {
        self.attachments.push(Attachment::from_file(workspace, path)?);
        self.mark_focused();
        Ok(())
    }

    pub fn remove_attachment(&mut self, index: usize) -> bool {
        if index >= self.attachments.len() { return false; }
        self.attachments.remove(index);
        true
    }

    pub fn move_attachment(&mut self, from: usize, to: usize) -> bool {
        if from >= self.attachments.len() || to >= self.attachments.len() { return false; }
        let attachment = self.attachments.remove(from);
        self.attachments.insert(to, attachment);
        true
    }

    /// Handle a paste event from the terminal. Per design §12.4:
    /// ≤8 newline-separated lines are inserted inline; larger pastes
    /// become an attachment token with metadata above the draft.
    pub fn paste(&mut self, text: &str) -> PasteOutcome {
        if text.is_empty() {
            return PasteOutcome::Inlined;
        }
        // A paste is user interaction — flip the focus flag.
        self.mark_focused();
        let line_count = if text.is_empty() {
            0
        } else {
            text.matches('\n').count() + 1
        };
        if line_count <= INLINE_PASTE_LINE_LIMIT {
            self.paste_inline(text);
            PasteOutcome::Inlined
        } else {
            self.attachments.push(Attachment::new_paste(text.to_string()));
            PasteOutcome::Attached
        }
    }

    /// Insert pasted text character-by-character, using `insert_char` so
    /// newline characters advance through `rows` like Enter would.
    fn paste_inline(&mut self, text: &str) {
        if self.is_empty() {
            // First row empty — promote cursor here.
        }
        // Walk the pasted string char-by-char so unicode surrogate
        // boundaries and newlines both flow through the normal composer
        // logic. Tab characters are converted to single spaces because
        // tabs in a paste are usually accidental indentation and would
        // bloat the cursor math.
        for ch in text.chars() {
            match ch {
                '\n' => self.insert_newline(),
                '\t' => {
                    for _ in 0..4 {
                        self.insert_char(' ');
                    }
                }
                c => self.insert_char(c),
            }
        }
    }

    /// Number of attachment tokens currently above the draft.
    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
    }

    /// Read-only access to the history ring (newest entries at the back).
    /// Used by the persistence layer and by tests.
    pub fn history(&self) -> &VecDeque<String> {
        &self.history
    }

    /// Load a previously-persisted history ring from disk. Falls back to
    /// empty on I/O / parse error so a corrupt file does not block startup.
    pub fn load_history_from(path: &std::path::Path) -> VecDeque<String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Vec<String>>(&bytes)
                .ok()
                .map(|v| v.into_iter().collect())
                .unwrap_or_default(),
            Err(_) => VecDeque::new(),
        }
    }

    /// Persist the history ring atomically (write to temp + rename) so a
    /// power cut mid-write cannot corrupt the file.
    pub fn save_history_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&self.history.iter().collect::<Vec<_>>())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, &bytes)?;
        // Best-effort atomic rename — on most platforms this is atomic.
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Reset the composer to an empty buffer (no history push).
    pub fn clear(&mut self) {
        self.rows = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.anchor = None;
        self.history_idx = None;
    }

    /// Process a key. Returns `Some(text)` on submit (plain Enter).
    ///
    /// Keymap (iMessage / Codex / Claude Code convention):
    /// - plain `Enter` → submit (returns `Some(submitted_text)`)
    /// - `Shift+Enter` (or `Ctrl+Enter` / `Alt+Enter` fallback) → newline
    pub fn handle_key(&mut self, key: &KeyEvent) -> Option<String> {
        // History navigation is independent of selection/shift; do it first.
        if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT {
            match key.code {
                KeyCode::Up => return self.history_move(-1),
                KeyCode::Down => return self.history_move(1),
                _ => {}
            }
        }
        // Any key that reaches past the history shortcuts is real user
        // interaction. Flip the focus flag once so the panel can drop
        // the IDLE border. (History navigation alone is observed by
        // `history_move` and does not flip focus.)
        self.mark_focused();
        // Submit on plain Enter. Shift / Ctrl / Alt on Enter insert a
        // newline (Shift+Enter is the canonical terminal convention; we
        // keep the alt/ctrl variants as fallbacks for terminals that
        // swallow Shift+Enter).
        let mods = key.modifiers;
        let newline = (mods.contains(KeyModifiers::SHIFT)
            || mods.contains(KeyModifiers::ALT)
            || mods.contains(KeyModifiers::CONTROL))
            && key.code == KeyCode::Enter;
        let submit = key.code == KeyCode::Enter
            && !mods.contains(KeyModifiers::SHIFT)
            && !mods.contains(KeyModifiers::ALT)
            && !mods.contains(KeyModifiers::CONTROL);

        // Selection-extending movement: Shift held, with plain arrows/Home/End.
        let shift = mods.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Enter if submit => {
                if self.is_empty() && self.attachments.is_empty() {
                    return None;
                }
                self.anchor = None;
                return Some(self.take_submit());
            }
            KeyCode::Enter if newline => {
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

    /// Mark the composer as having received user input. Called by
    /// `paste()`, `handle_key()`, and `take_submit()` whenever they
    /// mutate state. Drives the IDLE/FOCUSED border in `panel::input`
    /// per UX `18 IDLE/FOCUSED` states.
    pub fn mark_focused(&mut self) {
        self.interacted = true;
    }

    /// Whether the user has typed into this composer at least once.
    /// Read-only, set by `mark_focused`.
    pub fn has_focus_interaction(&self) -> bool {
        self.interacted
    }

    /// Reset focus state. Used by `panel::input` when an explicit
    /// "lost focus" signal arrives (e.g. user clicked away). Allows
    /// IDLE styling to return. Not currently wired in main; left for
    /// future mouse / Ctrl+W handlers.
    pub fn reset_focus(&mut self) {
        self.interacted = false;
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

/// Truncate to the first `max_chars` and squash newlines so a single-line
/// preview is safe to render above the draft.
fn make_preview(content: &str, max_chars: usize) -> String {
    let squashed: String = content.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    let trimmed = squashed.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn human_bytes(n: usize) -> String {
    const KB: usize = 1024;
    if n < KB {
        format!("{n}B")
    } else if n < KB * KB {
        format!("{:.1}kB", n as f64 / KB as f64)
    } else {
        format!("{:.1}MB", n as f64 / (KB * KB) as f64)
    }
}

fn mime_for_path(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "text/x-rust",
        "ts" | "tsx" => "text/typescript",
        "js" | "jsx" => "text/javascript",
        "py" => "text/x-python",
        "json" => "application/json",
        "md" => "text/markdown",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        _ => "text/plain",
    }
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
    fn shift_enter_inserts_newline_not_submit() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
        c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(c.text(), "a\nb");
        assert_eq!(c.row_count(), 2);
    }

    #[test]
    fn plain_enter_submits() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('i'), KeyModifiers::NONE));
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(submitted.as_deref(), Some("hi"));
        assert!(c.is_empty());
    }

    #[test]
    fn plain_enter_on_empty_does_nothing() {
        let mut c = Composer::default();
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(submitted, None);
    }

    #[test]
    fn ctrl_enter_inserts_newline_fallback() {
        // Ctrl+Enter is a fallback for terminals that swallow Shift+Enter.
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::CONTROL));
        c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(c.text(), "a\nb");
        assert_eq!(c.row_count(), 2);
    }

    #[test]
    fn multi_line_submit_joined_with_newline() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
        c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(submitted.as_deref(), Some("a\nb"));
    }

    #[test]
    fn backspace_at_start_of_row_joins_with_previous_row() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        // Shift+Enter inserts newline; new convention.
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::SHIFT));
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
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('w'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
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
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
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
            let _ = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        }
        assert_eq!(c.history.len(), HISTORY_CAP);
    }

    #[test]
    fn submit_pushes_into_history() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('z'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
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

    // ----- paste / attachment -----

    #[test]
    fn paste_under_8_lines_inserts_inline() {
        let mut c = Composer::default();
        let outcome = c.paste("hello\nworld\nfoo");
        assert_eq!(outcome, PasteOutcome::Inlined);
        assert_eq!(c.text(), "hello\nworld\nfoo");
        assert_eq!(c.attachment_count(), 0);
    }

    #[test]
    fn paste_exactly_8_lines_inserts_inline_at_threshold() {
        let mut c = Composer::default();
        let payload = "1\n2\n3\n4\n5\n6\n7\n8"; // 8 lines
        assert_eq!(c.paste(payload), PasteOutcome::Inlined);
    }

    #[test]
    fn paste_over_8_lines_collapses_to_attachment() {
        let mut c = Composer::default();
        let payload = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let outcome = c.paste(&payload);
        assert_eq!(outcome, PasteOutcome::Attached);
        assert_eq!(c.attachment_count(), 1);
        assert_eq!(c.attachments[0].kind, AttachmentKind::Paste);
        assert_eq!(c.attachments[0].line_count, 20);
        assert_eq!(c.attachments[0].content, payload);
        // Draft must remain untouched.
        assert!(c.is_empty());
    }

    #[test]
    fn empty_paste_is_noop() {
        let mut c = Composer::default();
        assert_eq!(c.paste(""), PasteOutcome::Inlined);
        assert_eq!(c.attachment_count(), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn submit_keeps_attachment_typed_and_out_of_visible_history() {
        let mut c = Composer::default();
        c.paste(&(1..=15).map(|i| format!("L{i}")).collect::<Vec<_>>().join("\n"));
        c.handle_key(&key(KeyCode::Char('q'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('u'), KeyModifiers::NONE));
        c.handle_key(&key(KeyCode::Char('e'), KeyModifiers::NONE));
        // Plain Enter submits in the new convention.
        let submitted = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        let submitted = submitted.expect("submit");
        assert_eq!(submitted, "que");
        let attachments = c.take_submitted_attachments();
        assert_eq!(attachments.len(), 1);
        assert!(matches!(
            &attachments[0].content,
            sylvander_protocol::AttachmentContent::Text { text }
                if text.starts_with("L1\n") && text.ends_with("L15")
        ));
        assert_eq!(c.history.back().map(String::as_str), Some("que"));
        // Everything cleared on submit.
        assert!(c.is_empty());
        assert_eq!(c.attachment_count(), 0);
    }

    #[test]
    fn attachment_label_is_human_friendly() {
        let payload = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor";
        let att = Attachment::new_paste(payload.to_string());
        let label = att.label();
        // Label should include kind, line count, and size.
        assert!(label.starts_with("[paste:"));
        assert!(label.contains("lines"));
        assert!(label.contains("lorem"));
        // Preview truncates long content.
        assert!(label.contains('…') || label.chars().count() < 80);
    }

    #[test]
    fn multiple_pastes_become_multiple_attachments() {
        let mut c = Composer::default();
        for _ in 0..3 {
            c.paste(&(1..=10).map(|i| format!("L{i}")).collect::<Vec<_>>().join("\n"));
        }
        assert_eq!(c.attachment_count(), 3);
    }

    #[test]
    fn workspace_file_attachment_is_scoped_typed_and_reorderable() {
        let root = tempdir();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut composer = Composer::default();
        composer.attach_file(&root, std::path::Path::new("src/main.rs")).unwrap();
        composer.attachments.push(Attachment::new_paste("one\ntwo".into()));
        assert_eq!(composer.attachments[0].mime_type, "text/x-rust");
        assert!(composer.move_attachment(1, 0));
        assert_eq!(composer.attachments[0].kind, AttachmentKind::Paste);
        assert!(composer.remove_attachment(0));
        assert_eq!(composer.attachments[0].name, "src/main.rs");

        let outside = root.parent().unwrap().join("outside-secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        assert!(composer.attach_file(&root, &outside).is_err());
        std::fs::remove_file(outside).ok();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn history_round_trips_through_disk() {
        let dir = tempdir();
        let path = dir.join("history.json");
        // Pre-populate one entry, then save.
        let mut c1 = Composer::default();
        c1.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
        let _ = c1.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        c1.save_history_to(&path).expect("save");
        // A fresh composer loads from disk; remembered history is there.
        let loaded = Composer::load_history_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.front().map(String::as_str), Some("h"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn history_load_returns_empty_on_missing_file() {
        let dir = tempdir();
        let path = dir.join("nonexistent.json");
        let loaded = Composer::load_history_from(&path);
        assert!(loaded.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn history_save_is_atomic_under_dir() {
        // Save to a path whose parent does not exist yet — save_history_to
        // must create the directory.
        let dir = tempdir();
        let nested = dir.join("nested").join("history.json");
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('q'), KeyModifiers::NONE));
        let _ = c.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        c.save_history_to(&nested).expect("save");
        assert!(nested.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sylvander-tui-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    // ----- M-T15.D focus state tests -----

    #[test]
    fn fresh_composer_is_idle() {
        let c = Composer::default();
        assert!(!c.has_focus_interaction());
    }

    #[test]
    fn typing_a_char_marks_focused() {
        let mut c = Composer::default();
        assert!(!c.has_focus_interaction());
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(c.has_focus_interaction());
    }

    #[test]
    fn paste_marks_focused() {
        let mut c = Composer::default();
        assert!(!c.has_focus_interaction());
        let _ = c.paste("hello world");
        assert!(c.has_focus_interaction());
    }

    #[test]
    fn reset_focus_returns_to_idle() {
        let mut c = Composer::default();
        c.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(c.has_focus_interaction());
        c.reset_focus();
        assert!(!c.has_focus_interaction());
    }
}
