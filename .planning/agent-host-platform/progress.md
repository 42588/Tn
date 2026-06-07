# Progress Log: Agent Host Platform Finish

## Session: 2026-06-07

### Phase 1: Audit Existing State
- Read `docs/未修复.md`, Agent Host references in `CLAUDE.md`, `docs/架构蓝图.md`, and relevant crate files.
- Confirmed a large dirty worktree already contains Agent Host platformization changes.
- Ran focused tests:
  - `cargo test -p tn-agent`: 13 passed.
  - `cargo test -p tn-ai`: 17 passed.
  - `cargo test -p tn-ui guard`: failed compiling `tn-pty` because `ClientHandler` was initialized with missing `allow_host_key_prompt`.
- Fixed the `tn-pty` compile blockers needed for Agent Host verification:
  - Added non-interactive host-key behavior to `ClientHandler::quiet`.
  - Reused a single `authenticate_for_remote_fs` helper.
  - Moved SFTP list/read calls into a runtime-backed future.
- Created scoped planning files under `.planning/agent-host-platform/` to avoid overwriting root SFTP planning files.

## Test Results

| Test | Result |
|---|---|
| `cargo test -p tn-agent` | Passed: 13 tests |
| `cargo test -p tn-ai` | Passed: 17 tests |
| `cargo test -p tn-ui guard` | Failed before guard: `ClientHandler` missing `allow_host_key_prompt` field |
| `cargo test -p tn-pty quiet_client_handler_disables_ui_host_key_prompt` | Passed |
| `cargo test -p tn-pty remote_path_normalizes_without_becoming_windows_path` | Passed |

## Error Log

| Error | Resolution |
|---|---|
| `E0560`: `ClientHandler` has no field named `allow_host_key_prompt` | Added the field, true for interactive SSH panes and false for quiet SFTP probes. |
| Missing `authenticate_for_remote_fs` / async SFTP methods called synchronously | Kept one non-interactive auth helper and ran SFTP futures inside the runtime thread. |
