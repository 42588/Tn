use std::time::Duration;

use tn_pty::remote_cmd::RemoteCommandService;
use tn_pty::remote_fs::RemotePath;

use crate::gitutil::{parse_numstat, FileChange};

const REMOTE_GIT_TIMEOUT: Duration = Duration::from_millis(1800);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteGitFile {
    pub(crate) cfg: tn_pty::SshConfig,
    pub(crate) root: RemotePath,
    pub(crate) path: String,
}

impl RemoteGitFile {
    pub(crate) fn remote_path(&self) -> RemotePath {
        self.root.join(&self.path)
    }
}

pub(crate) fn changes_for_remote(
    service: &dyn RemoteCommandService,
    cfg: &tn_pty::SshConfig,
    root: &RemotePath,
) -> Vec<FileChange> {
    let out = service.run(
        cfg,
        root,
        "git",
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-color",
            "HEAD",
            "--numstat",
            "--relative",
        ],
        REMOTE_GIT_TIMEOUT,
    );
    match out {
        Ok(output) if output.success() => parse_numstat(&output.stdout),
        _ => Vec::new(),
    }
}

pub(crate) fn diff_for_remote_file(
    service: &dyn RemoteCommandService,
    file: &RemoteGitFile,
) -> anyhow::Result<String> {
    let output = service.run(
        &file.cfg,
        &file.root,
        "git",
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-color",
            "--",
            &file.path,
        ],
        REMOTE_GIT_TIMEOUT,
    )?;
    if output.success() {
        Ok(output.stdout)
    } else {
        anyhow::bail!("remote git diff failed: {}", output.stderr)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HunkAction {
    Apply,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Parsed now so remote hunk buttons can stay UI-only later.
pub(crate) struct DiffHunk {
    pub(crate) index: usize,
    pub(crate) old_start: u32,
    pub(crate) old_count: u32,
    pub(crate) new_start: u32,
    pub(crate) new_count: u32,
    pub(crate) lines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileDiff {
    pub(crate) path: String,
    pub(crate) hunks: Vec<DiffHunk>,
}

pub(crate) fn parse_file_diff(path: impl Into<String>, diff: &str) -> FileDiff {
    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;
    for line in diff.lines() {
        if let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(line) {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(DiffHunk {
                index: hunks.len(),
                old_start,
                old_count,
                new_start,
                new_count,
                lines: vec![line.to_string()],
            });
        } else if let Some(hunk) = current.as_mut() {
            hunk.lines.push(line.to_string());
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    FileDiff {
        path: path.into(),
        hunks,
    }
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    Some((old_start, old_count, new_start, new_count))
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

pub(crate) fn build_hunk_patch(file: &FileDiff, hunk_index: usize) -> Option<String> {
    let hunk = file.hunks.iter().find(|h| h.index == hunk_index)?;
    let mut patch = String::new();
    patch.push_str(&format!("diff --git a/{0} b/{0}\n", file.path));
    patch.push_str(&format!("--- a/{}\n", file.path));
    patch.push_str(&format!("+++ b/{}\n", file.path));
    for line in &hunk.lines {
        patch.push_str(line);
        patch.push('\n');
    }
    Some(patch)
}

pub(crate) fn hunk_command_args(action: HunkAction) -> &'static [&'static str] {
    match action {
        HunkAction::Apply => &["apply", "--cached", "-"],
        HunkAction::Reject => &["apply", "--reverse", "-"],
    }
}

pub(crate) fn apply_remote_hunk(
    service: &dyn RemoteCommandService,
    file: &RemoteGitFile,
    parsed: &FileDiff,
    hunk_index: usize,
    action: HunkAction,
) -> anyhow::Result<()> {
    let Some(patch) = build_hunk_patch(parsed, hunk_index) else {
        anyhow::bail!("remote hunk {hunk_index} not found");
    };
    let output = service.run_with_stdin(
        &file.cfg,
        &file.root,
        "git",
        hunk_command_args(action),
        patch.as_bytes(),
        REMOTE_GIT_TIMEOUT,
    )?;
    if output.success() {
        Ok(())
    } else {
        anyhow::bail!("remote git apply failed: {}", output.stderr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tn_pty::remote_cmd::{RemoteCommandOutput, RemoteCommandService};

    struct FakeRemoteCommand {
        stdout: String,
        status: Option<u32>,
        expect_stdin: Option<String>,
    }

    impl RemoteCommandService for FakeRemoteCommand {
        fn run(
            &self,
            _cfg: &tn_pty::SshConfig,
            cwd: &RemotePath,
            program: &str,
            args: &[&str],
            _timeout: Duration,
        ) -> anyhow::Result<RemoteCommandOutput> {
            assert_eq!(cwd.as_str(), "/repo");
            assert_eq!(program, "git");
            assert!(args.contains(&"core.quotePath=false"));
            Ok(RemoteCommandOutput {
                status: self.status,
                stdout: self.stdout.clone(),
                stderr: String::new(),
            })
        }

        fn run_with_stdin(
            &self,
            _cfg: &tn_pty::SshConfig,
            cwd: &RemotePath,
            program: &str,
            args: &[&str],
            stdin: &[u8],
            _timeout: Duration,
        ) -> anyhow::Result<RemoteCommandOutput> {
            assert_eq!(cwd.as_str(), "/repo");
            assert_eq!(program, "git");
            assert_eq!(args, hunk_command_args(HunkAction::Reject));
            assert_eq!(
                std::str::from_utf8(stdin).unwrap(),
                self.expect_stdin.as_deref().unwrap()
            );
            Ok(RemoteCommandOutput {
                status: self.status,
                stdout: self.stdout.clone(),
                stderr: String::new(),
            })
        }
    }

    fn cfg() -> tn_pty::SshConfig {
        tn_pty::SshConfig {
            host: "box".into(),
            port: 22,
            user: "alice".into(),
            key_path: None,
            password: None,
        }
    }

    #[test]
    fn remote_changes_parse_numstat_from_remote_command() {
        let service = FakeRemoteCommand {
            stdout: "2\t1\tsrc/main.rs\n-\t-\tasset.bin\n".into(),
            status: Some(0),
            expect_stdin: None,
        };
        let changes = changes_for_remote(&service, &cfg(), &RemotePath::new("/repo"));

        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].path, "src/main.rs");
        assert_eq!(changes[0].add, 2);
        assert_eq!(changes[0].del, 1);
        assert_eq!(changes[1].path, "asset.bin");
    }

    #[test]
    fn remote_git_file_maps_relative_path_to_remote_path() {
        let file = RemoteGitFile {
            cfg: cfg(),
            root: RemotePath::new("/repo"),
            path: "src/main.rs".into(),
        };
        assert_eq!(file.remote_path().as_str(), "/repo/src/main.rs");
    }

    #[test]
    fn file_diff_extracts_individual_hunks() {
        let raw = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {
+    println!(\"hi\");
 }
@@ -8 +9 @@
-old
+new
";
        let parsed = parse_file_diff("src/main.rs", raw);

        assert_eq!(parsed.hunks.len(), 2);
        assert_eq!(parsed.hunks[0].old_start, 1);
        assert_eq!(parsed.hunks[0].new_count, 3);
        assert_eq!(parsed.hunks[1].old_count, 1);
        assert!(parsed.hunks[1].lines.iter().any(|line| line == "+new"));
    }

    #[test]
    fn hunk_patch_contains_file_headers_and_only_selected_hunk() {
        let parsed = parse_file_diff(
            "src/main.rs",
            "@@ -1 +1 @@\n-old\n+new\n@@ -8 +8 @@\n-a\n+b\n",
        );
        let patch = build_hunk_patch(&parsed, 1).unwrap();

        assert!(patch.contains("diff --git a/src/main.rs b/src/main.rs"));
        assert!(patch.contains("@@ -8 +8 @@"));
        assert!(patch.contains("+b"));
        assert!(!patch.contains("-old"));
        assert_eq!(hunk_command_args(HunkAction::Apply), ["apply", "--cached", "-"]);
        assert_eq!(hunk_command_args(HunkAction::Reject), ["apply", "--reverse", "-"]);
    }

    #[test]
    fn remote_hunk_apply_sends_selected_patch_on_stdin() {
        let parsed = parse_file_diff("src/main.rs", "@@ -1 +1 @@\n-old\n+new\n");
        let expected = build_hunk_patch(&parsed, 0).unwrap();
        let service = FakeRemoteCommand {
            stdout: String::new(),
            status: Some(0),
            expect_stdin: Some(expected),
        };
        let file = RemoteGitFile {
            cfg: cfg(),
            root: RemotePath::new("/repo"),
            path: "src/main.rs".into(),
        };

        apply_remote_hunk(&service, &file, &parsed, 0, HunkAction::Reject).unwrap();
    }
}
