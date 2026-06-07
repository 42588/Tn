# Task Plan: SFTP Remote File Service

## Goal
Implement the SSH/SFTP remote file service described in `docs/未修复.md` so SSH panes can drive Explorer and Quick Look without treating remote POSIX paths as local Windows paths.

## Current Phase
Phase 1

## Phases

### Phase 1: Requirements & Discovery
- [x] Read `docs/未修复.md` and repo guidance.
- [x] Detect existing dirty worktree and remote FS half-finished file.
- [x] Map current Explorer, Quick Look, Workspace, SSH, and PTY extension points.
- [x] Document findings in `findings.md`.
- **Status:** complete

### Phase 2: Design & TDD Plan
- [ ] Define the smallest safe SFTP feature slice.
- [ ] Decide exported types and UI integration boundaries.
- [ ] Add failing tests for remote path normalization, SFTP packet parsing, and Explorer remote state behavior.
- **Status:** pending

### Phase 3: Backend Implementation
- [x] Finish and export `tn-pty::remote_fs`.
- [x] Add SSH auth reuse for SFTP without UI prompts in the first pass.
- [x] Verify backend unit tests.
- **Status:** complete

### Phase 4: Explorer / Workspace Integration
- [ ] Add remote Explorer root/state without local `PathBuf`.
- [ ] Let focused SSH panes browse their remote cwd using SFTP.
- [ ] Keep Host/WSL behavior unchanged.
- **Status:** pending

### Phase 5: Quick Look Remote Read Integration
- [ ] Open remote file entries through the remote file service.
- [ ] Preserve local Quick Look behavior.
- [ ] Bound remote reads and show explicit errors.
- **Status:** pending

### Phase 6: Verification & Docs
- [ ] Run targeted tests and workspace build/tests that are feasible.
- [ ] Update `docs/未修复.md`, `docs/架构蓝图.md`, `docs/产品设计.md`, `CLAUDE.md`, and changelog/optimization docs as appropriate.
- [ ] Commit if verification passes and repo state allows.
- **Status:** pending

## Key Questions
1. Does existing `remote_fs.rs` compile once exported, or is it still a sketch?
2. How can Explorer represent remote entries without `PathBuf`?
3. How should Quick Look identify/read a remote file while preserving local file APIs?
4. Can SFTP auth reuse current SSH config without new UI prompts in this first pass?

## Decisions Made
| Decision | Rationale |
|----------|-----------|
| Build on existing `crates/tn-pty/src/remote_fs.rs` | It is already present in the dirty worktree and appears intended for this feature; overwriting it would risk losing user work. |
| First slice is browse + bounded read only | `docs/未修复.md` asks for remote tree and Quick Look; editing/writing remote files is a larger editor-safe-save problem. |

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|

## Notes
- Do not revert or overwrite existing dirty worktree changes.
- Keep `AgentRuntimeKind` and `FileNamespace` separate; remote runtime only gets Explorer/Quick Look after an actual remote FS backend exists.
