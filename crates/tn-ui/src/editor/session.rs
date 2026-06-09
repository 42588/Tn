use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use tn_editor::{line_chars, Document, SearchState, TextRange};

pub type Pos = (usize, usize);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentSessionSource {
    Local(PathBuf),
    Remote(String),
}

#[derive(Clone, Debug)]
struct DocumentSessionInner {
    document: Document,
    lines: Rc<RefCell<Vec<String>>>,
    source: Option<DocumentSessionSource>,
}

#[derive(Clone, Debug)]
pub struct DocumentSession {
    inner: Rc<RefCell<DocumentSessionInner>>,
}

impl Default for DocumentSession {
    fn default() -> Self {
        Self::from_lines(Vec::new())
    }
}

impl DocumentSession {
    pub fn from_lines(lines: Vec<String>) -> Self {
        Self::from_lines_and_source(lines, None)
    }

    pub fn from_lines_and_source(
        lines: Vec<String>,
        source: Option<DocumentSessionSource>,
    ) -> Self {
        let document = Document::from_lines(lines);
        let lines = Rc::new(RefCell::new(document.lines().to_vec()));
        Self {
            inner: Rc::new(RefCell::new(DocumentSessionInner {
                document,
                lines,
                source,
            })),
        }
    }

    pub fn source(&self) -> Option<DocumentSessionSource> {
        self.inner.borrow().source.clone()
    }

    #[cfg(test)]
    pub fn document_lines(&self) -> Vec<String> {
        self.inner.borrow().document.lines().to_vec()
    }

    pub fn lines(&self) -> Rc<RefCell<Vec<String>>> {
        self.inner.borrow().lines.clone()
    }

    pub fn line_count(&self) -> usize {
        self.inner.borrow().lines.borrow().len()
    }

    pub fn cursor(&self) -> Pos {
        self.inner.borrow().document.cursor()
    }

    pub fn selection_anchor(&self) -> Option<Pos> {
        self.inner.borrow().document.selection_anchor()
    }

    pub fn sel_range(&self) -> Option<(Pos, Pos)> {
        self.inner
            .borrow()
            .document
            .selection_range()
            .map(|range| (range.start, range.end))
    }

    pub fn is_dirty(&self) -> bool {
        self.inner.borrow().document.is_dirty()
    }

    pub fn mark_clean(&self) {
        self.inner.borrow_mut().document.mark_clean();
    }

    pub fn line_chars(&self, row: usize) -> usize {
        let inner = self.inner.borrow();
        line_chars(inner.document.lines(), row)
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        self.inner.borrow().document.lines().get(row).cloned()
    }

    pub fn selected_text(&self) -> Option<String> {
        self.inner.borrow().document.selected_text()
    }

    pub fn place_cursor(&self, row: usize, col: usize, extend: bool) {
        let mut inner = self.inner.borrow_mut();
        let target = (row, col);
        if extend {
            let anchor = inner
                .document
                .selection_anchor()
                .unwrap_or(inner.document.cursor());
            inner.document.select_range(anchor, target);
        } else {
            inner.document.set_cursor(target);
        }
    }

    pub fn select_range(&self, start: Pos, end: Pos) {
        self.inner.borrow_mut().document.select_range(start, end);
    }

    pub fn select_all(&self) {
        self.inner.borrow_mut().document.select_all();
    }

    pub fn type_text(&self, text: &str) {
        self.mutate_document(|document| document.type_text(text));
    }

    pub fn type_char(&self, text: &str) {
        self.type_text(text);
    }

    pub fn newline(&self) {
        self.mutate_document(Document::newline);
    }

    pub fn indent(&self) {
        self.mutate_document(|document| document.insert_text("    "));
    }

    pub fn backspace(&self) -> bool {
        self.mutate_document_bool(Document::backspace)
    }

    pub fn delete_forward(&self) -> bool {
        self.mutate_document_bool(Document::delete_forward)
    }

    pub fn move_cursor(&self, key: &str, extend: bool) {
        self.inner.borrow_mut().document.move_cursor(key, extend);
    }

    pub fn page(&self, dir: i32, extend: bool) {
        self.inner.borrow_mut().document.page(dir, extend);
    }

    pub fn delete_current_line(&self) -> bool {
        self.mutate_document_bool(Document::delete_current_line)
    }

    pub fn insert_text(&self, text: &str) {
        self.mutate_document(|document| document.insert_text(text));
    }

    pub fn find_next(&self, query: &str, forward: bool) -> Option<TextRange> {
        let mut inner = self.inner.borrow_mut();
        let mut search = SearchState::new(query);
        inner.document.find_next(&mut search, forward)
    }

    pub fn replace_current(&self, query: &str, replacement: &str) -> bool {
        self.mutate_document_bool(|document| document.replace_current(query, replacement))
    }

    pub fn replace_all(&self, query: &str, replacement: &str) -> usize {
        let mut inner = self.inner.borrow_mut();
        let count = inner.document.replace_all(query, replacement);
        if count > 0 {
            Self::sync_lines(&mut inner);
        }
        count
    }

    pub fn undo(&self) -> bool {
        self.mutate_document_bool(Document::undo)
    }

    pub fn redo(&self) -> bool {
        self.mutate_document_bool(Document::redo)
    }

    fn mutate_document(&self, f: impl FnOnce(&mut Document)) {
        let mut inner = self.inner.borrow_mut();
        f(&mut inner.document);
        Self::sync_lines(&mut inner);
    }

    fn mutate_document_bool(&self, f: impl FnOnce(&mut Document) -> bool) -> bool {
        let mut inner = self.inner.borrow_mut();
        let changed = f(&mut inner.document);
        if changed {
            Self::sync_lines(&mut inner);
        }
        changed
    }

    fn sync_lines(inner: &mut DocumentSessionInner) {
        let Some(transaction) = inner.document.last_transaction() else {
            *inner.lines.borrow_mut() = inner.document.lines().to_vec();
            return;
        };
        let start = transaction
            .before()
            .start_row()
            .min(inner.lines.borrow().len());
        let end = (start + transaction.before().lines().len()).min(inner.lines.borrow().len());
        let mut lines = inner.lines.borrow_mut();
        lines.splice(start..end, transaction.after().lines().iter().cloned());
        if lines.is_empty() {
            lines.push(String::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn cloned_document_sessions_share_buffer_cursor_dirty_and_undo() {
        let session = DocumentSession::from_lines(lines(&["abc"]));
        let another = session.clone();

        session.place_cursor(0, 1, false);
        session.type_text("X");

        assert_eq!(another.lines().borrow().as_slice(), &lines(&["aXbc"]));
        assert_eq!(another.cursor(), (0, 2));
        assert!(another.is_dirty());

        another.undo();

        assert_eq!(session.lines().borrow().as_slice(), &lines(&["abc"]));
        assert_eq!(session.cursor(), (0, 1));
        assert!(session.is_dirty());

        session.mark_clean();

        assert!(!another.is_dirty());
    }
}
