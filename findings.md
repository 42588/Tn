# Findings & Decisions

## Requirements
- Implement the SFTP / remote file backend that `docs/未修复.md` says is missing.
- SSH/macOS/Linux remote cwd must no longer be ignored by Explorer once the backend exists.
- Remote browsing and Quick Look must not fake remote POSIX paths as Windows `PathBuf`.
- Keep WSL behavior through `\\wsl$` and Host behavior through normal Windows paths.
- First pass should support remote directory listing and bounded file reads; remote editing is out of scope unless already cheap and safe.

## Research Findings
- `docs/未修复.md` explicitly says SSH/macOS remote file browsing and Quick Look are blocked on SFTP/remote FS.
- `crates/tn-pty/src/remote_fs.rs` already exists as an untracked file in the dirty worktree. It defines `RemotePath`, `RemoteId`, `RemoteDirEntry`, `RemoteFileService`, and a low-level SFTP v3 client sketch.
- `crates/tn-pty/src/lib.rs` does not yet export `remote_fs`.
- `crates/tn-ui/src/explorer.rs` currently models only `ExplorerFs::Host` and `ExplorerFs::Wsl`; root paths are `Option<PathBuf>`, and remote roots are deliberately absent.
- Current dirty worktree has already evolved `explorer.rs` toward `ExplorerFs::Ssh`, `ExplorerPath`, and `ExplorerFile`; this needs completion/compilation rather than a fresh design.
- `cargo test -p tn-pty remote_fs --lib` currently passes 4 remote_fs unit tests; the SFTP backend is exported and compiles.

## Technical Decisions
| Decision | Rationale |
|----------|-----------|
| Use a trait-backed `RemoteFileService` from `tn-pty` | UI can depend on filesystem-shaped operations without knowing SFTP packet details. |
| Represent remote roots as remote IDs/paths, not `PathBuf` | Preserves the existing namespace rule and avoids Windows path confusion. |
| Bound Quick Look remote reads | Prevents a remote file preview from hanging or downloading huge files unexpectedly. |

## Issues Encountered
| Issue | Resolution |
|-------|------------|
| Existing worktree is dirty across many files | Treat all pre-existing changes as user work; inspect and build on them without reverting. |

## Resources
- `docs/未修复.md`
- `crates/tn-pty/src/remote_fs.rs`
- `crates/tn-ui/src/explorer.rs`
- `crates/tn-ui/src/terminal_view/launch.rs`
- `crates/tn-ui/src/workspace.rs`

## Visual/Browser Findings
- Not applicable; this feature is backend/UI data-flow work, no browser visuals used.
