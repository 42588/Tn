//! Shared, **bounded** git helpers: run git off the UI thread with a hard timeout
//! and no console flash, so a slow / `.git`-locked / AV-scanned git can never
//! freeze the window (踩过的坑: synchronous git on the UI thread froze the app).
//! Used by Quick Look (file diff) and the agent pane's activity rail (本次改动).
//!
//! Everything here is pure or a thin subprocess wrapper — the parsers are headless
//! unit-tested; the capture is `#[cfg(windows)]`-aware (`CREATE_NO_WINDOW`).

use std::path::Path;
use std::time::Duration;

/// Run `git <args>` in `root`, stdout captured, **bounded** to `timeout`, with **no
/// console flash**. `None` on timeout / spawn failure (caller treats that as "no
/// output"). The blocking `.output()` runs on a throwaway thread + `recv_timeout`,
/// so the caller blocks at most `timeout` and a stuck git can't hang anything —
/// **but never call this on the UI thread** (it blocks up to `timeout`); call it
/// from a background task. `.output()` drains stdout, avoiding the pipe-buffer
/// deadlock a `try_wait` loop would hit on big diffs.
pub(crate) fn capture_bounded(root: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let root = root.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(&root).args(&args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let _ = tx.send(out);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(s)) => Some(s),
        _ => None, // timeout or spawn error
    }
}

/// Working-tree change-watcher noise filter: paths under these dirs don't affect a
/// `git diff` / `git status` (or churn constantly — `.git` ticks on every git op,
/// including our own), so a change there must not trigger a refresh. Shared by the
/// agent rail watcher (terminal_view/io.rs) and the explorer tree watcher (explorer.rs)
/// so the two stay in sync (审查⑨: was copy-pasted in both).
pub(crate) fn is_noise_path(p: &Path) -> bool {
    p.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".git" | "target" | "node_modules" | ".cargo" | "dist" | ".next")
        )
    })
}

/// Whether `root` lives inside a git work tree (`git rev-parse --is-inside-work-tree`).
/// **Bounded** — blocking, **call off the UI thread**. Used to gate the agent rail's
/// recursive change-watcher: watching a non-repo directory (e.g. the user's home dir
/// when an agent runs in `~`) churns endlessly on AppData/cache writes for a `git diff`
/// that is always empty — the cause of the periodic file-tree flicker. Returns `false`
/// on timeout / spawn failure (no repo → don't watch).
pub(crate) fn is_inside_repo(root: &Path) -> bool {
    capture_bounded(
        root,
        &["rev-parse", "--is-inside-work-tree"],
        Duration::from_millis(800),
    )
    .map(|s| s.trim() == "true")
    .unwrap_or(false)
}

/// One changed file from `git diff --numstat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileChange {
    /// Path relative to the repo root (as git prints it).
    pub path: String,
    pub add: u32,
    pub del: u32,
}

impl FileChange {
    /// Display name = the path's final component (mockup `.afile .nm` shows the
    /// filename, e.g. `element.rs`).
    pub fn name(&self) -> &str {
        self.path
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.path)
    }
}

/// Parse `git diff --numstat` — one `add<TAB>del<TAB>path` per line (binary files
/// print `-<TAB>-<TAB>path`, counted as 0/0). Pure → headless unit-tested.
pub(crate) fn parse_numstat(text: &str) -> Vec<FileChange> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        out.push(FileChange {
            add: a.trim().parse().unwrap_or(0),
            del: d.trim().parse().unwrap_or(0),
            path: p.to_string(),
        });
    }
    out
}
/// Tracked changes vs `HEAD` in `root` (staged + unstaged), **bounded**. Blocking —
/// call from a background task. Empty when not a repo / no HEAD / no changes.
/// `--relative` makes the returned paths relative to `root` (not the repo toplevel),
/// so a caller can resolve a path back to an absolute one via `root.join(path)`.
pub(crate) fn changes_for(root: &Path) -> Vec<FileChange> {
    let out = capture_bounded(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-color",
            "HEAD",
            "--numstat",
            "--relative",
        ],
        Duration::from_millis(1200),
    );
    parse_numstat(out.as_deref().unwrap_or(""))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_parses_counts_and_path() {
        let s = "3\t1\tcrates/tn-ui/src/element.rs\n1\t0\tlib.rs\n";
        let v = parse_numstat(s);
        assert_eq!(v.len(), 2);
        assert_eq!(
            v[0],
            FileChange {
                path: "crates/tn-ui/src/element.rs".into(),
                add: 3,
                del: 1
            }
        );
        assert_eq!(v[0].name(), "element.rs");
        assert_eq!(
            v[1],
            FileChange {
                path: "lib.rs".into(),
                add: 1,
                del: 0
            }
        );
        assert_eq!(v[1].name(), "lib.rs");
    }

    #[test]
    fn numstat_treats_binary_dashes_as_zero_and_skips_blank() {
        let v = parse_numstat("-\t-\tassets/logo.png\n\n");
        assert_eq!(
            v,
            vec![FileChange {
                path: "assets/logo.png".into(),
                add: 0,
                del: 0
            }]
        );
    }

    #[test]
    fn numstat_empty_is_empty() {
        assert!(parse_numstat("").is_empty());
    }

    #[test]
    fn name_handles_windows_separators() {
        let f = FileChange {
            path: r"crates\tn-ui\src\mod.rs".into(),
            add: 0,
            del: 0,
        };
        assert_eq!(f.name(), "mod.rs");
    }
}
