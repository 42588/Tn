# Findings

- docs/未修复.md defines a strict P0→P7 route for "Tn Editor 用户体验升级", with all phases currently unchecked.
- The current workspace has no `crates/tn-editor` member and no `tn-editor` workspace dependency. `Cargo.toml` members still list only existing crates through `tn-app` plus `test_load`.
- `crates/tn-ui/src/lib.rs` only declares `mod quick_look;`; there is no `mod editor` or `editor::element` path.
- `crates/tn-ui/src/quick_look.rs` still owns editor state (`buf`, cursor, selection, undo/redo, dirty), pure edit ops, diff parsing, renderer rows, and `EntityInputHandler`.
- P0 is not implemented: `save()` directly writes with `std::fs::write`, `open()` resets `dirty`, and `close()` evicts state without dirty-close confirmation or disk conflict guard.
- Diff tab still uses a separate `diff_row` path; File/Edit have mouse drag selection, but Diff does not share the selection path.
- Config has no `[editor]` / `editor.animations` schema yet.
- Therefore the documented path is directionally correct and still matches current anchors, but implementation has not yet started the required P0/P1 steps.
