//! Headless editor core for Tn.
//!
//! This crate intentionally has no GPUI dependency. UI hosts such as Quick Look
//! consume these primitives instead of owning editor behavior themselves.

pub mod line_layout;

pub use line_layout::{LineLayout, VisualLine, WrapMode};

/// Cursor position in logical `(row, char_column)` coordinates.
pub type Cursor = (usize, usize);

/// A normalized half-open text range `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRange {
    pub start: Cursor,
    pub end: Cursor,
}

impl TextRange {
    pub fn new(a: Cursor, b: Cursor) -> Self {
        if a <= b {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    pub fn is_collapsed(self) -> bool {
        self.start == self.end
    }
}

/// Single selection state for the current P1 editor model.
///
/// Future multi-cursor work can widen this without changing `Document` callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    anchor: Option<Cursor>,
    cursor: Cursor,
}

impl Selection {
    pub fn collapsed(cursor: Cursor) -> Self {
        Self {
            anchor: None,
            cursor,
        }
    }

    pub fn with_anchor(anchor: Cursor, cursor: Cursor) -> Self {
        Self {
            anchor: Some(anchor),
            cursor,
        }
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn anchor(&self) -> Option<Cursor> {
        self.anchor
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
        self.anchor = None;
    }

    pub fn set_range(&mut self, start: Cursor, end: Cursor) {
        self.anchor = Some(start);
        self.cursor = end;
    }

    pub fn clear_anchor(&mut self) {
        self.anchor = None;
    }

    pub fn range(&self) -> Option<TextRange> {
        let anchor = self.anchor?;
        let range = TextRange::new(anchor, self.cursor);
        (!range.is_collapsed()).then_some(range)
    }
}

/// Cursor collection, currently restricted to one primary cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorSet {
    primary: Selection,
}

impl CursorSet {
    pub fn single(cursor: Cursor) -> Self {
        Self {
            primary: Selection::collapsed(cursor),
        }
    }

    pub fn primary(&self) -> &Selection {
        &self.primary
    }

    pub fn primary_mut(&mut self) -> &mut Selection {
        &mut self.primary
    }

    pub fn cursor(&self) -> Cursor {
        self.primary.cursor()
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.primary.set_cursor(cursor);
    }

    pub fn range(&self) -> Option<TextRange> {
        self.primary.range()
    }
}

/// A line-range snapshot used by edit transactions and undo records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditSnapshot {
    start_row: usize,
    lines: Vec<String>,
    cursor: Cursor,
}

impl EditSnapshot {
    pub fn new(start_row: usize, lines: Vec<String>, cursor: Cursor) -> Self {
        Self {
            start_row,
            lines,
            cursor,
        }
    }

    pub fn start_row(&self) -> usize {
        self.start_row
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }
}

/// A before/after edit record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditTransaction {
    before: EditSnapshot,
    after: EditSnapshot,
}

impl EditTransaction {
    pub fn new(before: EditSnapshot, after: EditSnapshot) -> Self {
        Self { before, after }
    }

    pub fn before(&self) -> &EditSnapshot {
        &self.before
    }

    pub fn after(&self) -> &EditSnapshot {
        &self.after
    }

    pub fn lines(&self) -> &[String] {
        self.before.lines()
    }

    fn reversed(&self) -> Self {
        Self {
            before: self.after.clone(),
            after: self.before.clone(),
        }
    }
}

/// Incremental undo/redo stack matching Quick Look's current undo behavior.
#[derive(Clone, Debug)]
pub struct UndoStack {
    undo: Vec<EditTransaction>,
    redo: Vec<EditTransaction>,
    coalesce_insert: bool,
    limit: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            coalesce_insert: false,
            limit: 100,
        }
    }
}

impl UndoStack {
    pub fn clear_coalesce(&mut self) {
        self.coalesce_insert = false;
    }

    pub fn push(&mut self, transaction: EditTransaction, coalesce: bool) {
        if coalesce && self.coalesce_insert {
            if let Some(last) = self.undo.last_mut() {
                if last.before.start_row == transaction.before.start_row {
                    last.after = transaction.after;
                    self.redo.clear();
                    return;
                }
            }
        }
        self.undo.push(transaction);
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.coalesce_insert = coalesce;
    }

    fn undo(&mut self) -> Option<EditTransaction> {
        let transaction = self.undo.pop()?;
        self.redo.push(transaction.clone());
        self.coalesce_insert = false;
        Some(transaction)
    }

    fn redo(&mut self) -> Option<EditTransaction> {
        let transaction = self.redo.pop()?;
        self.undo.push(transaction.clone());
        self.coalesce_insert = false;
        Some(transaction)
    }
}

/// Query state and cached single-line matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchState {
    query: String,
    matches: Vec<TextRange>,
}

impl SearchState {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            matches: Vec::new(),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.matches.clear();
    }

    pub fn refresh(&mut self, lines: &[String]) {
        self.matches = all_matches(lines, &self.query);
    }

    pub fn matches(&self) -> &[TextRange] {
        &self.matches
    }
}

/// Headless text document model for Quick Look and future editor panes.
#[derive(Clone, Debug)]
pub struct Document {
    lines: Vec<String>,
    cursors: CursorSet,
    undo: UndoStack,
    last_transaction: Option<EditTransaction>,
    dirty: bool,
}

impl Document {
    pub fn from_lines(mut lines: Vec<String>) -> Self {
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            cursors: CursorSet::single((0, 0)),
            undo: UndoStack::default(),
            last_transaction: None,
            dirty: false,
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> Cursor {
        self.cursors.cursor()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn last_transaction(&self) -> Option<&EditTransaction> {
        self.last_transaction.as_ref()
    }

    pub fn selection_range(&self) -> Option<TextRange> {
        self.cursors.range()
    }

    pub fn selection_anchor(&self) -> Option<Cursor> {
        self.cursors.primary().anchor()
    }

    pub fn clear_selection(&mut self) {
        self.cursors.primary_mut().clear_anchor();
        self.undo.clear_coalesce();
    }

    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection_range()?;
        Some(selected_text(&self.lines, range))
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        let cursor = clamp_cursor(&self.lines, cursor);
        self.cursors.set_cursor(cursor);
        self.undo.clear_coalesce();
    }

    pub fn select_range(&mut self, start: Cursor, end: Cursor) {
        let start = clamp_cursor(&self.lines, start);
        let end = clamp_cursor(&self.lines, end);
        self.cursors.primary_mut().set_range(start, end);
        self.undo.clear_coalesce();
    }

    pub fn select_all(&mut self) {
        let last = self.lines.len().saturating_sub(1);
        self.cursors
            .primary_mut()
            .set_range((0, 0), (last, line_chars(&self.lines, last)));
        self.undo.clear_coalesce();
    }

    pub fn insert_text(&mut self, text: &str) {
        let before = self.capture_edit_span();
        self.delete_selection_inner();
        insert_text_at_cursor(
            &mut self.lines,
            &mut self.cursors.primary_mut().cursor,
            text,
        );
        self.cursors.primary_mut().clear_anchor();
        self.dirty = true;
        self.record_transaction(before, self.cursor().0 + 1, false);
    }

    pub fn type_text(&mut self, text: &str) {
        let had_selection = self.selection_range().is_some();
        let before = self.capture_edit_span();
        if self.selection_range().is_some() {
            self.delete_selection_inner();
        }
        insert_text_at_cursor(
            &mut self.lines,
            &mut self.cursors.primary_mut().cursor,
            text,
        );
        self.cursors.primary_mut().clear_anchor();
        self.dirty = true;
        self.record_transaction(before, self.cursor().0 + 1, !had_selection);
    }

    pub fn newline(&mut self) {
        let before = self.capture_edit_span();
        self.delete_selection_inner();
        op_newline(&mut self.lines, &mut self.cursors.primary_mut().cursor);
        self.cursors.primary_mut().clear_anchor();
        self.dirty = true;
        self.record_transaction(before, self.cursor().0 + 1, false);
    }

    pub fn backspace(&mut self) -> bool {
        if self.selection_range().is_some() {
            let before = self.capture_edit_span();
            self.delete_selection_inner();
            self.record_transaction(before, self.cursor().0 + 1, false);
            return true;
        }
        if self.cursor() == (0, 0) {
            return false;
        }
        let before = if self.cursor().1 > 0 {
            self.capture_line_span(self.cursor().0, self.cursor().0 + 1, self.cursor())
        } else {
            self.capture_line_span(self.cursor().0 - 1, self.cursor().0 + 1, self.cursor())
        };
        let changed = op_backspace(&mut self.lines, &mut self.cursors.primary_mut().cursor);
        self.dirty |= changed;
        if changed {
            self.record_transaction(before, self.cursor().0 + 1, false);
        }
        changed
    }

    pub fn delete_forward(&mut self) -> bool {
        if self.selection_range().is_some() {
            let before = self.capture_edit_span();
            self.delete_selection_inner();
            self.record_transaction(before, self.cursor().0 + 1, false);
            return true;
        }
        let (row, col) = self.cursor();
        if row + 1 >= self.lines.len() && col >= line_chars(&self.lines, row) {
            return false;
        }
        let before_end = if col < line_chars(&self.lines, row) {
            row + 1
        } else {
            row + 2
        };
        let before = self.capture_line_span(row, before_end, self.cursor());
        let changed = op_delete(&mut self.lines, &mut self.cursors.primary_mut().cursor);
        self.dirty |= changed;
        if changed {
            self.record_transaction(before, self.cursor().0 + 1, false);
        }
        changed
    }

    pub fn delete_current_line(&mut self) -> bool {
        let row = self.cursor().0.min(self.lines.len().saturating_sub(1));
        let before = self.capture_line_span(row, row + 1, self.cursor());
        let after_end;
        if self.lines.len() > 1 {
            self.lines.remove(row);
            let new_row = row.min(self.lines.len() - 1);
            self.cursors.set_cursor((new_row, 0));
            after_end = row;
        } else {
            if self.lines.is_empty() {
                self.lines.push(String::new());
            } else {
                self.lines[0].clear();
            }
            self.cursors.set_cursor((0, 0));
            after_end = 1;
        }
        self.cursors.primary_mut().clear_anchor();
        self.dirty = true;
        self.record_transaction(before, after_end, false);
        true
    }

    pub fn move_cursor(&mut self, key: &str, extend: bool) {
        self.undo.clear_coalesce();
        if !extend {
            if let Some(range) = self.selection_range() {
                self.cursors.primary_mut().clear_anchor();
                match key {
                    "left" | "up" | "home" => {
                        self.cursors.set_cursor(range.start);
                        return;
                    }
                    "right" | "down" | "end" => {
                        self.cursors.set_cursor(range.end);
                        return;
                    }
                    _ => {}
                }
            }
            self.cursors.primary_mut().clear_anchor();
        } else if self.cursors.primary().anchor().is_none() {
            let cursor = self.cursor();
            self.cursors.primary_mut().anchor = Some(cursor);
        }
        op_move(&self.lines, &mut self.cursors.primary_mut().cursor, key);
    }

    pub fn page(&mut self, dir: i32, extend: bool) {
        self.undo.clear_coalesce();
        if extend && self.cursors.primary().anchor().is_none() {
            let cursor = self.cursor();
            self.cursors.primary_mut().anchor = Some(cursor);
        } else if !extend {
            self.cursors.primary_mut().clear_anchor();
        }
        op_page(&self.lines, &mut self.cursors.primary_mut().cursor, dir);
    }

    pub fn find_next(&mut self, search: &mut SearchState, forward: bool) -> Option<TextRange> {
        search.refresh(&self.lines);
        if search.matches.is_empty() {
            return None;
        }
        let cursor = self.cursor();
        let idx = if forward {
            search
                .matches
                .iter()
                .position(|range| range.start > cursor)
                .unwrap_or(0)
        } else {
            search
                .matches
                .iter()
                .rposition(|range| range.start < cursor)
                .unwrap_or(search.matches.len() - 1)
        };
        let range = search.matches[idx];
        self.cursors.primary_mut().set_range(range.start, range.end);
        Some(range)
    }

    pub fn replace_current(&mut self, query: &str, replacement: &str) -> bool {
        let Some(range) = self.selection_range() else {
            return false;
        };
        if selected_text(&self.lines, range) != query {
            return false;
        }
        let before = self.capture_line_span(range.start.0, range.end.0 + 1, self.cursor());
        op_delete_range(&mut self.lines, range.start, range.end);
        self.cursors.set_cursor(range.start);
        insert_text_at_cursor(
            &mut self.lines,
            &mut self.cursors.primary_mut().cursor,
            replacement,
        );
        self.cursors.primary_mut().clear_anchor();
        self.dirty = true;
        self.record_transaction(before, self.cursor().0 + 1, false);
        true
    }

    pub fn replace_all(&mut self, query: &str, replacement: &str) -> usize {
        if query.is_empty() {
            return 0;
        }
        let Some(first) = self.lines.iter().position(|line| line.contains(query)) else {
            return 0;
        };
        let last = self
            .lines
            .iter()
            .rposition(|line| line.contains(query))
            .unwrap_or(first);
        let before = self.capture_line_span(first, last + 1, self.cursor());
        let count = replace_all_in(&mut self.lines, query, replacement);
        if count > 0 {
            self.dirty = true;
            self.cursors.set_cursor((0, 0));
            self.record_transaction(before, last + 1, false);
        }
        count
    }

    pub fn undo(&mut self) -> bool {
        let Some(transaction) = self.undo.undo() else {
            return false;
        };
        self.apply_transaction(&transaction, true);
        self.dirty = true;
        self.last_transaction = Some(transaction.reversed());
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(transaction) = self.undo.redo() else {
            return false;
        };
        self.apply_transaction(&transaction, false);
        self.dirty = true;
        self.last_transaction = Some(transaction);
        true
    }

    fn delete_selection_inner(&mut self) {
        if let Some(range) = self.selection_range() {
            op_delete_range(&mut self.lines, range.start, range.end);
            self.cursors.set_cursor(range.start);
            self.dirty = true;
        }
    }

    fn capture_edit_span(&self) -> EditSnapshot {
        if let Some(range) = self.selection_range() {
            self.capture_line_span(range.start.0, range.end.0 + 1, self.cursor())
        } else {
            self.capture_line_span(self.cursor().0, self.cursor().0 + 1, self.cursor())
        }
    }

    fn capture_line_span(&self, start_row: usize, end_row: usize, cursor: Cursor) -> EditSnapshot {
        let start = start_row.min(self.lines.len());
        let end = end_row.min(self.lines.len()).max(start);
        EditSnapshot::new(start, self.lines[start..end].to_vec(), cursor)
    }

    fn record_transaction(&mut self, before: EditSnapshot, after_end_row: usize, coalesce: bool) {
        let after = self.capture_line_span(before.start_row(), after_end_row, self.cursor());
        let transaction = EditTransaction::new(before, after);
        self.undo.push(transaction.clone(), coalesce);
        self.last_transaction = Some(transaction);
    }

    fn apply_transaction(&mut self, transaction: &EditTransaction, undo: bool) {
        let (old, new) = if undo {
            (&transaction.after, &transaction.before)
        } else {
            (&transaction.before, &transaction.after)
        };
        let start = old.start_row().min(self.lines.len());
        let end = (start + old.lines().len()).min(self.lines.len());
        self.lines.splice(start..end, new.lines().iter().cloned());
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursors.set_cursor(new.cursor());
        self.cursors.primary_mut().clear_anchor();
    }
}

/// Convert a character column into a byte offset within `line`.
///
/// Cursor columns are char-based throughout the editor core so multibyte text
/// can be edited without splitting UTF-8 code points.
pub fn char_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(byte, _)| byte)
        .unwrap_or(line.len())
}

/// Character length of buffer line `row`, returning 0 when out of range.
pub fn line_chars(buf: &[String], row: usize) -> usize {
    buf.get(row).map(|line| line.chars().count()).unwrap_or(0)
}

/// Insert `text` at the cursor and advance the cursor past it.
///
/// This operation treats `text` as a single-line fragment. Use
/// [`op_insert_multiline`] for pasted text containing line breaks.
pub fn op_insert(buf: &mut Vec<String>, cur: &mut Cursor, text: &str) {
    if buf.is_empty() {
        buf.push(String::new());
    }
    let (row, col) = *cur;
    let byte = char_to_byte(&buf[row], col);
    buf[row].insert_str(byte, text);
    cur.1 = col + text.chars().count();
}

/// Split the current line at the cursor and move the cursor to the new line.
pub fn op_newline(buf: &mut Vec<String>, cur: &mut Cursor) {
    if buf.is_empty() {
        buf.push(String::new());
    }
    let (row, col) = *cur;
    let byte = char_to_byte(&buf[row], col);
    let tail = buf[row].split_off(byte);
    buf.insert(row + 1, tail);
    *cur = (row + 1, 0);
}

/// Delete the character before the cursor, or merge with the previous line.
///
/// Returns `true` when the buffer changed.
pub fn op_backspace(buf: &mut Vec<String>, cur: &mut Cursor) -> bool {
    let (row, col) = *cur;
    if col > 0 {
        let b0 = char_to_byte(&buf[row], col - 1);
        let b1 = char_to_byte(&buf[row], col);
        buf[row].replace_range(b0..b1, "");
        cur.1 = col - 1;
        true
    } else if row > 0 {
        let line = buf.remove(row);
        let prev_len = line_chars(buf, row - 1);
        buf[row - 1].push_str(&line);
        *cur = (row - 1, prev_len);
        true
    } else {
        false
    }
}

/// Delete the character at the cursor, or join the next line.
///
/// Returns `true` when the buffer changed.
pub fn op_delete(buf: &mut Vec<String>, cur: &mut Cursor) -> bool {
    let (row, col) = *cur;
    let len = line_chars(buf, row);
    if col < len {
        let b0 = char_to_byte(&buf[row], col);
        let b1 = char_to_byte(&buf[row], col + 1);
        buf[row].replace_range(b0..b1, "");
        true
    } else if row + 1 < buf.len() {
        let next = buf.remove(row + 1);
        buf[row].push_str(&next);
        true
    } else {
        false
    }
}

/// Move the cursor for an arrow, Home, or End key.
pub fn op_move(buf: &[String], cur: &mut Cursor, key: &str) {
    let (row, col) = *cur;
    match key {
        "left" => {
            if col > 0 {
                cur.1 = col - 1;
            } else if row > 0 {
                *cur = (row - 1, line_chars(buf, row - 1));
            }
        }
        "right" => {
            if col < line_chars(buf, row) {
                cur.1 = col + 1;
            } else if row + 1 < buf.len() {
                *cur = (row + 1, 0);
            }
        }
        "up" => {
            if row > 0 {
                *cur = (row - 1, col.min(line_chars(buf, row - 1)));
            }
        }
        "down" => {
            if row + 1 < buf.len() {
                *cur = (row + 1, col.min(line_chars(buf, row + 1)));
            }
        }
        "home" => cur.1 = 0,
        "end" => cur.1 = line_chars(buf, row),
        _ => {}
    }
}

/// Move the cursor one editor page, using Quick Look's current 12-row page.
pub fn op_page(buf: &[String], cur: &mut Cursor, dir: i32) {
    const PAGE: usize = 12;
    let (row, col) = *cur;
    let new_row = if dir < 0 {
        row.saturating_sub(PAGE)
    } else {
        (row + PAGE).min(buf.len().saturating_sub(1))
    };
    *cur = (new_row, col.min(line_chars(buf, new_row)));
}

/// Delete the normalized range `[start, end)` from the buffer.
pub fn op_delete_range(buf: &mut Vec<String>, start: Cursor, end: Cursor) {
    if buf.is_empty() {
        return;
    }
    if start.0 == end.0 {
        let b0 = char_to_byte(&buf[start.0], start.1);
        let b1 = char_to_byte(&buf[start.0], end.1);
        buf[start.0].replace_range(b0..b1, "");
    } else {
        let head: String = buf[start.0].chars().take(start.1).collect();
        let tail: String = buf[end.0].chars().skip(end.1).collect();
        buf.drain(start.0 + 1..=end.0.min(buf.len() - 1));
        buf[start.0] = head + &tail;
    }
}

/// Insert `text`, which may contain `\n`, at the cursor.
pub fn op_insert_multiline(buf: &mut Vec<String>, cur: &mut Cursor, text: &str) {
    let parts: Vec<&str> = text.split('\n').collect();
    if parts.len() == 1 {
        op_insert(buf, cur, parts[0]);
        return;
    }
    if buf.is_empty() {
        buf.push(String::new());
    }
    let (row, col) = *cur;
    let byte = char_to_byte(&buf[row], col);
    let tail = buf[row].split_off(byte);
    buf[row].push_str(parts[0]);

    let mut at = row + 1;
    for mid in &parts[1..parts.len() - 1] {
        buf.insert(at, mid.to_string());
        at += 1;
    }

    let last = parts[parts.len() - 1];
    let last_col = last.chars().count();
    buf.insert(at, format!("{last}{tail}"));
    *cur = (at, last_col);
}

/// The selected text for a normalized range, joining lines with `\n`.
pub fn selected_text(buf: &[String], range: TextRange) -> String {
    let start = range.start;
    let end = range.end;
    if start.0 == end.0 {
        return buf
            .get(start.0)
            .map(|line| {
                line.chars()
                    .skip(start.1)
                    .take(end.1.saturating_sub(start.1))
                    .collect()
            })
            .unwrap_or_default();
    }
    let mut out: String = buf[start.0].chars().skip(start.1).collect();
    for line in buf.iter().take(end.0).skip(start.0 + 1) {
        out.push('\n');
        out.push_str(line);
    }
    out.push('\n');
    out.push_str(&buf[end.0].chars().take(end.1).collect::<String>());
    out
}

/// All single-line matches of `query` in document order.
pub fn all_matches(buf: &[String], query: &str) -> Vec<TextRange> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    for (row, line) in buf.iter().enumerate() {
        let (mut last_byte, mut last_char) = (0usize, 0usize);
        for (byte_idx, matched_str) in line.match_indices(query) {
            last_char += line[last_byte..byte_idx].chars().count();
            last_byte = byte_idx;
            let len_chars = matched_str.chars().count();
            out.push(TextRange::new(
                (row, last_char),
                (row, last_char + len_chars),
            ));
        }
    }
    out
}

/// Replace every occurrence of `query` with `replacement`, per line.
pub fn replace_all_in(buf: &mut Vec<String>, query: &str, replacement: &str) -> usize {
    if query.is_empty() {
        return 0;
    }
    let mut count = 0;
    for line in buf.iter_mut() {
        let occurrences = line.matches(query).count();
        if occurrences > 0 {
            *line = line.replace(query, replacement);
            count += occurrences;
        }
    }
    count
}

fn clamp_cursor(buf: &[String], cursor: Cursor) -> Cursor {
    if buf.is_empty() {
        return (0, 0);
    }
    let row = cursor.0.min(buf.len() - 1);
    (row, cursor.1.min(line_chars(buf, row)))
}

fn insert_text_at_cursor(buf: &mut Vec<String>, cursor: &mut Cursor, text: &str) {
    if text.contains('\n') {
        op_insert_multiline(buf, cursor, text);
    } else {
        op_insert(buf, cursor, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn char_to_byte_handles_multibyte_text() {
        assert_eq!(char_to_byte("a中b", 0), 0);
        assert_eq!(char_to_byte("a中b", 1), 1);
        assert_eq!(char_to_byte("a中b", 2), 4);
        assert_eq!(char_to_byte("a中b", 3), 5);
        assert_eq!(char_to_byte("a中b", 99), 5);
    }

    #[test]
    fn insert_text_is_multibyte_safe() {
        let mut b = buf(&["a中b"]);
        let mut cur = (0, 2);

        op_insert(&mut b, &mut cur, "X");
        assert_eq!(b, buf(&["a中Xb"]));
        assert_eq!(cur, (0, 3));

        op_insert(&mut b, &mut cur, "你");
        assert_eq!(b, buf(&["a中X你b"]));
        assert_eq!(cur, (0, 4));
    }

    #[test]
    fn newline_splits_and_backspace_merges_lines() {
        let mut b = buf(&["hello"]);
        let mut cur = (0, 2);

        op_newline(&mut b, &mut cur);
        assert_eq!(b, buf(&["he", "llo"]));
        assert_eq!(cur, (1, 0));

        assert!(op_backspace(&mut b, &mut cur));
        assert_eq!(b, buf(&["hello"]));
        assert_eq!(cur, (0, 2));

        cur = (0, 0);
        assert!(!op_backspace(&mut b, &mut cur));
        assert_eq!(b, buf(&["hello"]));
    }

    #[test]
    fn delete_forward_joins_next_line() {
        let mut b = buf(&["ab", "cd"]);
        let mut cur = (0, 2);

        assert!(op_delete(&mut b, &mut cur));
        assert_eq!(b, buf(&["abcd"]));
        assert_eq!(cur, (0, 2));

        cur = (0, 4);
        assert!(!op_delete(&mut b, &mut cur));
    }

    #[test]
    fn move_wraps_lines_and_clamps_columns() {
        let b = buf(&["abc", "de"]);
        let mut cur = (0, 3);

        op_move(&b, &mut cur, "right");
        assert_eq!(cur, (1, 0));

        op_move(&b, &mut cur, "left");
        assert_eq!(cur, (0, 3));

        cur = (0, 3);
        op_move(&b, &mut cur, "down");
        assert_eq!(cur, (1, 2));

        cur = (0, 1);
        op_move(&b, &mut cur, "end");
        assert_eq!(cur, (0, 3));

        op_move(&b, &mut cur, "home");
        assert_eq!(cur, (0, 0));
    }

    #[test]
    fn page_moves_twelve_rows_and_clamps_to_buffer() {
        let b: Vec<String> = (0..30).map(|i| i.to_string()).collect();
        let mut cur = (0, 0);

        op_page(&b, &mut cur, 1);
        assert_eq!(cur.0, 12);

        op_page(&b, &mut cur, 1);
        assert_eq!(cur.0, 24);

        op_page(&b, &mut cur, 1);
        assert_eq!(cur.0, 29);

        op_page(&b, &mut cur, -1);
        assert_eq!(cur.0, 17);
    }

    #[test]
    fn delete_range_handles_same_and_multiple_lines() {
        let mut b = buf(&["hello"]);
        op_delete_range(&mut b, (0, 1), (0, 4));
        assert_eq!(b, buf(&["ho"]));

        let mut b = buf(&["abc", "def", "ghi"]);
        op_delete_range(&mut b, (0, 1), (2, 2));
        assert_eq!(b, buf(&["ai"]));
    }

    #[test]
    fn insert_multiline_splits_at_cursor_and_preserves_tail() {
        let mut b = buf(&["axz"]);
        let mut cur = (0, 1);

        op_insert_multiline(&mut b, &mut cur, "B\nC\nD");
        assert_eq!(b, buf(&["aB", "C", "Dxz"]));
        assert_eq!(cur, (2, 1));
    }

    #[test]
    fn document_replaces_selection_and_reports_selected_text() {
        let mut doc = Document::from_lines(buf(&["abc", "def", "ghi"]));

        doc.select_range((0, 1), (2, 2));
        assert_eq!(doc.selection_range(), Some(TextRange::new((0, 1), (2, 2))));
        assert_eq!(doc.selected_text().as_deref(), Some("bc\ndef\ngh"));

        doc.insert_text("X");
        assert_eq!(doc.lines(), &buf(&["aXi"]));
        assert_eq!(doc.cursor(), (0, 2));
        assert_eq!(doc.selection_range(), None);
        assert!(doc.is_dirty());
    }

    #[test]
    fn document_coalesces_typed_text_into_one_undo_step() {
        let mut doc = Document::from_lines(buf(&[""]));

        doc.type_text("a");
        doc.type_text("b");
        doc.type_text("中");
        assert_eq!(doc.lines(), &buf(&["ab中"]));
        assert_eq!(doc.cursor(), (0, 3));

        assert!(doc.undo());
        assert_eq!(doc.lines(), &buf(&[""]));
        assert_eq!(doc.cursor(), (0, 0));

        assert!(doc.redo());
        assert_eq!(doc.lines(), &buf(&["ab中"]));
        assert_eq!(doc.cursor(), (0, 3));
    }

    #[test]
    fn undo_history_does_not_store_full_buffer_for_single_line_edit() {
        const MAX_TEST_LINES: usize = 4000;
        let lines: Vec<String> = (0..MAX_TEST_LINES).map(|i| format!("line {i}")).collect();
        let mut doc = Document::from_lines(lines);

        doc.set_cursor((2000, 4));
        doc.type_text("X");

        assert_eq!(doc.undo.undo.len(), 1);
        assert!(
            doc.undo.undo[0].lines().len() < 8,
            "undo record should retain only edited lines, not the whole document"
        );
    }

    #[test]
    fn continuous_typing_keeps_undo_records_line_bounded() {
        // TnE-07 invariant: typing a long run into a large buffer must coalesce
        // into a single, line-bounded undo record — never a per-key whole-buffer
        // snapshot. Guards the cost of the "连续输入" acceptance criterion.
        const MAX_TEST_LINES: usize = 4000;
        let lines: Vec<String> = (0..MAX_TEST_LINES).map(|i| format!("line {i}")).collect();
        let mut doc = Document::from_lines(lines);

        doc.set_cursor((2000, 4));
        for _ in 0..500 {
            doc.type_text("x");
        }

        // 500 keystrokes coalesce into one line-bounded undo step, not 500 buffer copies.
        assert_eq!(doc.undo.undo.len(), 1);
        assert!(
            doc.undo.undo[0].before().lines().len() < 8
                && doc.undo.undo[0].after().lines().len() < 8,
            "coalesced undo record must retain only edited lines, not the whole document"
        );
        assert_eq!(
            doc.lines()[2000].chars().count(),
            "line 2000".chars().count() + 500
        );
        assert_eq!(doc.lines().len(), MAX_TEST_LINES);

        // A newline on the same start row coalesces into the same record; its
        // span grows to the two touched lines but stays bounded, and the rest of
        // the buffer is untouched.
        doc.type_text("\n");
        assert_eq!(doc.undo.undo.len(), 1);
        assert!(doc.undo.undo[0].after().lines().len() < 8);
        assert_eq!(doc.lines().len(), MAX_TEST_LINES + 1);
        assert_eq!(doc.lines()[MAX_TEST_LINES], "line 3999");

        // Moving the cursor breaks the coalesce run, so the next edit is a new,
        // still line-bounded record rather than an ever-growing snapshot.
        doc.set_cursor((100, 0));
        doc.type_text("z");
        assert_eq!(doc.undo.undo.len(), 2);
        assert!(doc.undo.undo[1].before().lines().len() < 8);

        // Undo walks the records back, restoring the original line in bounded steps.
        assert!(doc.undo()); // undo "z"
        assert!(doc.undo()); // undo the coalesced typed run + newline
        assert_eq!(doc.lines()[2000], "line 2000");
        assert_eq!(doc.lines().len(), MAX_TEST_LINES);
    }

    #[test]
    fn document_movement_extends_and_collapses_selection_like_quick_look() {
        let mut doc = Document::from_lines(buf(&["abc", "de"]));
        doc.set_cursor((0, 1));

        doc.move_cursor("right", true);
        doc.move_cursor("right", true);
        assert_eq!(doc.selection_range(), Some(TextRange::new((0, 1), (0, 3))));

        doc.move_cursor("left", false);
        assert_eq!(doc.cursor(), (0, 1));
        assert_eq!(doc.selection_range(), None);

        doc.select_range((0, 1), (1, 1));
        doc.move_cursor("right", false);
        assert_eq!(doc.cursor(), (1, 1));
        assert_eq!(doc.selection_range(), None);
    }

    #[test]
    fn document_exposes_host_selection_and_clean_state() {
        let mut doc = Document::from_lines(buf(&["abc"]));
        doc.set_cursor((0, 1));

        doc.move_cursor("right", true);
        assert_eq!(doc.selection_anchor(), Some((0, 1)));
        assert_eq!(doc.selection_range(), Some(TextRange::new((0, 1), (0, 2))));

        doc.clear_selection();
        assert_eq!(doc.selection_anchor(), None);
        assert_eq!(doc.selection_range(), None);
        assert_eq!(doc.cursor(), (0, 2));

        doc.type_text("X");
        assert!(doc.is_dirty());
        doc.mark_clean();
        assert!(!doc.is_dirty());

        assert!(doc.undo());
        assert!(
            doc.is_dirty(),
            "undo after save makes the document dirty again"
        );
    }

    #[test]
    fn document_deletes_current_line_like_quick_look_cut() {
        let mut doc = Document::from_lines(buf(&["one", "two", "three"]));
        doc.set_cursor((1, 2));

        assert!(doc.delete_current_line());
        assert_eq!(doc.lines(), &buf(&["one", "three"]));
        assert_eq!(doc.cursor(), (1, 0));
        assert!(doc.is_dirty());

        doc.mark_clean();
        doc.set_cursor((1, 3));
        assert!(doc.delete_current_line());
        assert_eq!(doc.lines(), &buf(&["one"]));
        assert_eq!(doc.cursor(), (0, 0));

        let mut single = Document::from_lines(buf(&["only"]));
        single.set_cursor((0, 2));
        assert!(single.delete_current_line());
        assert_eq!(single.lines(), &buf(&[""]));
        assert_eq!(single.cursor(), (0, 0));
    }

    #[test]
    fn search_state_finds_selects_and_replaces_matches() {
        let mut doc = Document::from_lines(buf(&["foo bar foo", "baz foo"]));
        let mut search = SearchState::new("foo");

        search.refresh(doc.lines());
        assert_eq!(
            search.matches(),
            &[
                TextRange::new((0, 0), (0, 3)),
                TextRange::new((0, 8), (0, 11)),
                TextRange::new((1, 4), (1, 7)),
            ]
        );

        doc.set_cursor((0, 0));
        assert_eq!(
            doc.find_next(&mut search, true),
            Some(TextRange::new((0, 8), (0, 11)))
        );
        assert_eq!(doc.selection_range(), Some(TextRange::new((0, 8), (0, 11))));

        let replaced = doc.replace_all("foo", "X");
        assert_eq!(replaced, 3);
        assert_eq!(doc.lines(), &buf(&["X bar X", "baz X"]));
        assert_eq!(doc.cursor(), (0, 0));
        assert_eq!(doc.selection_range(), None);

        assert!(doc.undo());
        assert_eq!(doc.lines(), &buf(&["foo bar foo", "baz foo"]));
    }

    #[test]
    fn document_records_recent_edit_transaction() {
        let mut doc = Document::from_lines(buf(&["ab"]));
        doc.set_cursor((0, 1));

        doc.type_text("X");

        let tx = doc.last_transaction().expect("last edit transaction");
        assert_eq!(tx.before().lines(), &buf(&["ab"]));
        assert_eq!(tx.before().cursor(), (0, 1));
        assert_eq!(tx.after().lines(), &buf(&["aXb"]));
        assert_eq!(tx.after().cursor(), (0, 2));
    }
}
