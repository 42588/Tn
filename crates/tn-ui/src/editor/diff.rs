//! Diff decoration + hunk-navigation model (TnE-13 / TnE-14 foundation).
//!
//! Pure, headless classification of unified-diff lines into the kinds a read-only
//! Diff renderer decorates (hunk header / addition / deletion / context / file
//! meta), plus hunk-jump helpers the navigation (TnE-14) needs. No GPUI, no theme
//! — the renderer maps [`DiffRowKind`] to colors; this only decides *what* a row
//! is and *where* the hunks are. The existing `quick_look::parse_diff` keeps its
//! own richer `DiffLine` (line numbers, remote hunk indices); this is the
//! renderer-facing decoration layer the future `EditorElement` Diff path consumes.

/// What a single unified-diff line represents, for decoration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    /// `@@ -a,b +c,d @@` hunk header.
    HunkHeader,
    /// An added line (`+...`, but not the `+++` file header).
    Addition,
    /// A removed line (`-...`, but not the `---` file header).
    Deletion,
    /// An unchanged context line (leading space or empty).
    Context,
    /// File-level metadata: `diff --git`, `index`, `+++`/`---` headers, mode /
    /// rename / similarity lines, `Binary files`, and `\ No newline at end of file`.
    Meta,
}

impl DiffRowKind {
    /// The marker drawn in the diff gutter for this row kind.
    pub fn gutter(self) -> char {
        match self {
            DiffRowKind::HunkHeader => '@',
            DiffRowKind::Addition => '+',
            DiffRowKind::Deletion => '-',
            DiffRowKind::Context | DiffRowKind::Meta => ' ',
        }
    }

    /// Whether this row carries actual diff content (add/del/context) vs structural
    /// rows (hunk header / file meta) that selection and hunk math treat specially.
    pub fn is_content(self) -> bool {
        matches!(
            self,
            DiffRowKind::Addition | DiffRowKind::Deletion | DiffRowKind::Context
        )
    }
}

/// Classify one raw unified-diff line. File-header lines that begin with `+++` /
/// `---` are checked **before** the `+`/`-` content tests so they aren't mistaken
/// for additions / deletions.
pub fn classify_diff_line(raw: &str) -> DiffRowKind {
    if raw.starts_with("@@") {
        return DiffRowKind::HunkHeader;
    }
    if raw.starts_with("+++")
        || raw.starts_with("---")
        || raw.starts_with("diff ")
        || raw.starts_with("index ")
        || raw.starts_with("new file")
        || raw.starts_with("deleted file")
        || raw.starts_with("old mode")
        || raw.starts_with("new mode")
        || raw.starts_with("rename ")
        || raw.starts_with("copy ")
        || raw.starts_with("similarity ")
        || raw.starts_with("dissimilarity ")
        || raw.starts_with("Binary files")
        || raw.starts_with('\\')
    {
        return DiffRowKind::Meta;
    }
    match raw.as_bytes().first() {
        Some(b'+') => DiffRowKind::Addition,
        Some(b'-') => DiffRowKind::Deletion,
        _ => DiffRowKind::Context,
    }
}

/// Row indices of every hunk header, in order. Drives hunk navigation.
pub fn hunk_header_rows(kinds: &[DiffRowKind]) -> Vec<usize> {
    kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| **k == DiffRowKind::HunkHeader)
        .map(|(i, _)| i)
        .collect()
}

/// Next hunk header strictly after `from`, or `None` past the last one (no wrap —
/// the caller decides whether to wrap to the first).
pub fn next_hunk(headers: &[usize], from: usize) -> Option<usize> {
    headers.iter().copied().find(|&h| h > from)
}

/// Previous hunk header strictly before `from`, or `None` before the first.
pub fn prev_hunk(headers: &[usize], from: usize) -> Option<usize> {
    headers.iter().copied().rev().find(|&h| h < from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_each_diff_line_kind() {
        use DiffRowKind::*;
        assert_eq!(classify_diff_line("@@ -1,3 +1,4 @@ fn x()"), HunkHeader);
        assert_eq!(classify_diff_line("+added line"), Addition);
        assert_eq!(classify_diff_line("-removed line"), Deletion);
        assert_eq!(classify_diff_line(" context line"), Context);
        assert_eq!(classify_diff_line(""), Context);
        // File headers that start with +++ / --- must NOT be add/del.
        assert_eq!(classify_diff_line("+++ b/src/main.rs"), Meta);
        assert_eq!(classify_diff_line("--- a/src/main.rs"), Meta);
        assert_eq!(classify_diff_line("diff --git a/x b/x"), Meta);
        assert_eq!(classify_diff_line("index e69de29..0cfbf08 100644"), Meta);
        assert_eq!(classify_diff_line("new file mode 100644"), Meta);
        assert_eq!(classify_diff_line("\\ No newline at end of file"), Meta);
        assert_eq!(classify_diff_line("Binary files a/x and b/x differ"), Meta);
    }

    #[test]
    fn gutter_and_is_content() {
        assert_eq!(DiffRowKind::Addition.gutter(), '+');
        assert_eq!(DiffRowKind::Deletion.gutter(), '-');
        assert_eq!(DiffRowKind::HunkHeader.gutter(), '@');
        assert_eq!(DiffRowKind::Context.gutter(), ' ');
        assert_eq!(DiffRowKind::Meta.gutter(), ' ');
        assert!(DiffRowKind::Addition.is_content());
        assert!(!DiffRowKind::HunkHeader.is_content());
        assert!(!DiffRowKind::Meta.is_content());
    }

    #[test]
    fn hunk_navigation_finds_next_and_prev() {
        // A small diff: meta, hunk, +, -, ctx, hunk, +.
        let raw = [
            "diff --git a/x b/x",
            "@@ -1,2 +1,2 @@",
            "+a",
            "-b",
            " c",
            "@@ -10,1 +10,2 @@",
            "+d",
        ];
        let kinds: Vec<DiffRowKind> = raw.iter().map(|l| classify_diff_line(l)).collect();
        let headers = hunk_header_rows(&kinds);
        assert_eq!(headers, vec![1, 5]);

        // From the top, next hunk is row 1, then row 5, then none.
        assert_eq!(next_hunk(&headers, 0), Some(1));
        assert_eq!(next_hunk(&headers, 1), Some(5));
        assert_eq!(next_hunk(&headers, 5), None);
        // Previous walks backward.
        assert_eq!(prev_hunk(&headers, 6), Some(5));
        assert_eq!(prev_hunk(&headers, 5), Some(1));
        assert_eq!(prev_hunk(&headers, 1), None);
        // No hunks → no navigation targets.
        assert_eq!(next_hunk(&[], 0), None);
        assert_eq!(prev_hunk(&[], 9), None);
    }
}
