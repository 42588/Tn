# Findings: Agent Host Platform Finish

## Current State
- `tn-agent` already contains the open `AgentId`, descriptor/capability/runtime model, registry, external event adapters, event contract, and pricing/usage model.
- `tn-ai` already contains Claude/Codex as built-in adapters with `builtin_registry()` preserved but not wired into default app startup.
- `tn-ui` already uses a GPUI `AgentHost` global, per-pane `AgentId`, capability gates, realtime event poller, and `reduce_agent_event`.
- The working-tree version of `docs/未修复.md` no longer has the old Agent Host section; Agent Host is documented as completed in `CLAUDE.md`, `docs/架构蓝图.md`, and `CHANGELOG.md`.

## Test Baseline
- `cargo test -p tn-agent`: passed 13 tests.
- `cargo test -p tn-ai`: passed 17 tests.
- `cargo test -p tn-ui guard`: initially failed before running guard due to a `tn-pty` compile error.

## Compile Recovery
- `crates/tn-pty/src/remote_fs.rs` is already exported from `tn-pty` and participates in builds.
- Its quiet SFTP path needs a non-interactive SSH handler: no password prompt and no host-key trust prompt from a background file-service probe.
- SFTP async operations must be awaited inside the runtime thread; the service trait stays synchronous for UI callers.

## Open Questions
- Whether launch-time non-PTY runtime declarations are currently rejected before spawn, or merely represented in descriptors.
- Whether `reduce_agent_event` has direct unit coverage for status/model/transcript/permission/error slots.
- Whether docs should remove any remaining "not implemented" statements for the external realtime adapter and advanced event UI slots.
