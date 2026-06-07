# Findings

- `docs/未修复.md` requires strict P0 -> P7 execution for Tn Editor.
- Current `quick_look.rs` still owns edit state and writes directly with `std::fs::write`.
- `QuickLook::close`, workspace close events, file navigation, and tab switching do not yet guard dirty documents.
- Current text loading decodes UTF-8 BOM, UTF-16LE/BE, UTF-8, and GBK, but save always writes UTF-8 LF with a trailing newline.
- No `crates/tn-editor` crate exists yet, so P1 has not started in the current state.

## P0 Requirements

- Record `FileGuard { mtime, size, hash }` when a file is opened.
- Detect disk changes before save and avoid silent overwrite.
- Guard dirty close/navigation/tab switching with save/discard/cancel behavior.
- Preserve newline style and encoding on save.
- Cover pure conflict and newline/encoding logic with headless tests.
