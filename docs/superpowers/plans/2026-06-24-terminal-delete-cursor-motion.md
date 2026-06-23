# Terminal Delete Cursor Motion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve terminal pane typing/deletion cursor feel with a small programmatic motion state, especially for Backspace and DeleteForward.

**Architecture:** Keep terminal input and PTY encoding unchanged. Add a focused cursor-motion classifier/envelope in `crates/tn-ui/src/terminal_view/mod.rs`, feed it from existing keydown/render state, and use the computed geometry offsets when drawing the existing GPUI cursor `Div`.

**Tech Stack:** Rust, GPUI `Div` rendering, existing `cargo test -p tn-ui terminal_view` tests.

---

### Task 1: Cursor Motion Classifier

**Files:**
- Modify: `crates/tn-ui/src/terminal_view/mod.rs`
- Test: `crates/tn-ui/src/terminal_view/mod.rs`

- [x] **Step 1: Write failing tests**

Add unit tests for:

```rust
#[test]
fn terminal_cursor_motion_classifies_backspace_delete_forward_and_jumps() {
    let prev = (4, 10);
    assert_eq!(
        classify_terminal_cursor_motion(Some(TerminalCursorKeyIntent::Backspace), prev, (4, 9), true, false),
        TerminalCursorMotionKind::BackspaceEcho
    );
    assert_eq!(
        classify_terminal_cursor_motion(Some(TerminalCursorKeyIntent::DeleteForward), prev, prev, true, false),
        TerminalCursorMotionKind::DeleteForward
    );
    assert_eq!(
        classify_terminal_cursor_motion(Some(TerminalCursorKeyIntent::Insert), prev, (4, 11), true, false),
        TerminalCursorMotionKind::InsertEcho
    );
    assert_eq!(
        classify_terminal_cursor_motion(Some(TerminalCursorKeyIntent::Backspace), prev, (5, 0), true, false),
        TerminalCursorMotionKind::Snap
    );
    assert_eq!(
        classify_terminal_cursor_motion(Some(TerminalCursorKeyIntent::Backspace), prev, (4, 9), false, false),
        TerminalCursorMotionKind::Snap
    );
    assert_eq!(
        classify_terminal_cursor_motion(Some(TerminalCursorKeyIntent::Backspace), prev, (4, 9), true, true),
        TerminalCursorMotionKind::Snap
    );
}
```

- [x] **Step 2: Run red test**

Run: `cargo test -p tn-ui terminal_cursor_motion_classifies --lib`

Expected: fail because classifier/types do not exist.

- [x] **Step 3: Implement minimal classifier**

Add `TerminalCursorKeyIntent`, `TerminalCursorMotionKind`, and `classify_terminal_cursor_motion`.

- [x] **Step 4: Run green test**

Run: `cargo test -p tn-ui terminal_cursor_motion_classifies --lib`

Expected: pass.

### Task 2: Cursor Motion Envelope

**Files:**
- Modify: `crates/tn-ui/src/terminal_view/mod.rs`
- Test: `crates/tn-ui/src/terminal_view/mod.rs`

- [x] **Step 1: Write failing tests**

Add unit tests verifying:

```rust
#[test]
fn terminal_cursor_motion_envelope_makes_backspace_harder_than_insert() {
    let insert = terminal_cursor_motion_envelope(TerminalCursorMotionKind::InsertEcho, 0.35, 12.0, 20.0);
    let backspace = terminal_cursor_motion_envelope(TerminalCursorMotionKind::BackspaceEcho, 0.35, 12.0, 20.0);
    assert!(insert.width_offset > 0.0);
    assert!(insert.height_offset < 0.0);
    assert!(backspace.width_offset < 0.0);
    assert!(backspace.height_offset > 0.0);
    assert!(backspace.duration_ms < insert.duration_ms);
}

#[test]
fn terminal_cursor_motion_envelope_delete_forward_has_in_place_bite() {
    let bite = terminal_cursor_motion_envelope(TerminalCursorMotionKind::DeleteForward, 0.25, 12.0, 20.0);
    assert!(bite.width_offset < 0.0);
    assert!(bite.height_offset > 0.0);
    assert!(bite.duration_ms <= 45);
}
```

- [x] **Step 2: Run red test**

Run: `cargo test -p tn-ui terminal_cursor_motion_envelope --lib`

Expected: fail because envelope helper does not exist.

- [x] **Step 3: Implement minimal envelope**

Add `TerminalCursorMotionEnvelope` and `terminal_cursor_motion_envelope`.

- [x] **Step 4: Run green test**

Run: `cargo test -p tn-ui terminal_cursor_motion_envelope --lib`

Expected: pass.

### Task 3: Wire Motion Into TerminalView

**Files:**
- Modify: `crates/tn-ui/src/terminal_view/mod.rs`
- Test: `crates/tn-ui/src/terminal_view/mod.rs`

- [x] **Step 1: Add intent field**

Add `cursor_key_intent: Option<TerminalCursorKeyIntent>` to `TerminalView`, initialize it to `None`, set it in `on_key_down` before encoding named keys:

```rust
self.cursor_key_intent = match key {
    "backspace" => Some(TerminalCursorKeyIntent::Backspace),
    "delete" => Some(TerminalCursorKeyIntent::DeleteForward),
    _ => None,
};
```

For printable text input path, set `Insert` before returning to IME/WM_CHAR path if feasible through existing input handler; otherwise keep insert classification based on positive same-row movement.

- [x] **Step 2: Replace boolean forward animation**

Replace `cursor_action_forward: bool` with `cursor_motion_kind: TerminalCursorMotionKind`. In render, classify current movement with `classify_terminal_cursor_motion`, start animation only when classification is not `Snap`, and use the envelope helper for offsets.

- [x] **Step 3: Verify existing tests**

Run: `cargo test -p tn-ui terminal_view --lib`

Expected: pass.

### Task 4: Documentation and Verification

**Files:**
- Modify: `TODO.md`
- Modify: `docs/任务/2026-06-24-终端删除光标手感优化.md`

- [x] **Step 1: Record verification**

Add red/green commands and final test commands to the task document.

- [x] **Step 2: Run final checks**

Run:

```powershell
cargo test -p tn-ui terminal_cursor_motion --lib
cargo test -p tn-ui terminal_view --lib
cargo check -p tn-ui
git diff --check -- TODO.md crates/tn-ui/src/terminal_view/mod.rs docs/任务/2026-06-24-终端删除光标手感优化.md docs/superpowers/plans/2026-06-24-terminal-delete-cursor-motion.md
```

Expected: all pass.

- [ ] **Step 3: Commit**

Only stage non-`design/`, non-`dist/` files:

```powershell
git add TODO.md crates/tn-ui/src/terminal_view/mod.rs docs/任务/2026-06-24-终端删除光标手感优化.md docs/superpowers/plans/2026-06-24-terminal-delete-cursor-motion.md
git commit -m "fix: tighten terminal delete cursor motion"
```

---

## Self-Review

- Scope is limited to terminal pane cursor motion.
- No Lottie/runtime image work is included.
- No `design/` or `dist/` file should be staged or committed.
- Final verification additionally ran `cargo test -p tn-ui` with 239 passing tests.
