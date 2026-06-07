# Task Plan: Agent Host Platform Finish

## Goal
Complete the remaining Agent Host platformization work referenced from `docs/未修复.md`, without reverting the existing dirty worktree.

## Current Phase
Phase 1

## Phases

### Phase 1: Audit Existing State
- [x] Read repo guidance and `docs/未修复.md`.
- [x] Detect existing Agent Host changes already present in the dirty worktree.
- [x] Run focused `tn-agent` / `tn-ai` tests.
- [x] Restore `tn-pty` compile path blocked by the remote-FS half-finished helper.
- [ ] Run `tn-ui` guard after compile recovery.
- **Status:** in_progress

### Phase 2: Lock Remaining Agent Host Behavior
- [ ] Add/confirm tests for external realtime event ingestion.
- [ ] Add/confirm tests for AgentEvent UI reduction.
- [ ] Add/confirm tests for non-PTY/network runtime safety.
- **Status:** pending

### Phase 3: Minimal Implementation
- [ ] Implement only behavior gaps discovered by tests.
- [ ] Keep UI agent-agnostic and avoid reintroducing closed agent enums.
- **Status:** pending

### Phase 4: Documentation Cleanup
- [ ] Update `docs/未修复.md` so Agent Host is not listed as unresolved.
- [ ] Sync `docs/架构蓝图.md`, `docs/产品设计.md`, `CLAUDE.md`, and `CHANGELOG.md` where needed.
- **Status:** pending

### Phase 5: Verification
- [ ] Run focused crate tests.
- [ ] Run UI guard.
- [ ] Run broader feasible workspace verification.
- **Status:** pending

## Decisions

| Decision | Rationale |
|---|---|
| Treat existing Agent Host edits as user/pre-existing work | Worktree was already dirty across many Agent Host files. |
| Scope to old `未修复.md` Agent Host remaining items | User requested this section, not the SFTP/editor work also present in docs. |
| Do not overwrite root planning files | Root `task_plan.md` tracks a separate SFTP task. |

## Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| `tn-ui guard` failed in `tn-pty/src/ssh.rs` because `ClientHandler` was initialized with missing `allow_host_key_prompt` | First focused UI guard run | Added the field and made quiet SFTP host-key checks reject instead of prompting. |
| `tn-pty` then failed because `remote_fs.rs` called a missing SSH auth helper and used async SFTP methods from a sync closure | First `tn-pty` targeted test after the field fix | Kept one non-interactive `authenticate_for_remote_fs` helper and moved SFTP work into a runtime-backed future. |
