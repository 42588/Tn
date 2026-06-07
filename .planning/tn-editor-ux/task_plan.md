# Tn Editor UX Upgrade Plan

Goal: Execute `docs/未修复.md` section "Tn Editor 用户体验升级:从 Quick Look 快编到可扩展编辑器" in the required P0 -> P7 order.

## Current Phase
P0

## Phases

| Phase | Status | Notes |
|---|---|---|
| Recover requirements and current state | complete | `docs/未修复.md` confirms P0 starts with save/close safety, then P1 `tn-editor`. |
| P0 save/close safety | in_progress | Add file guard metadata, conflict detection, newline/encoding preservation, and leave guards before navigation/close/tab switch. |
| P1 headless `tn-editor` extraction | pending | Create crate after P0 safety model exists. |
| P2 read-only renderer spike | pending | Depends on P1 `Document`. |
| P3 edit-mode renderer integration | pending | Depends on P2. |
| P4 Diff Review integration | pending | Reuse renderer and selection path. |
| P5 soft wrap | pending | Add logical-to-visual layout. |
| P6 typing animation | pending | Renderer-only optional effect. |
| P7 Editor Pane | pending | Promote Quick Look to full editor pane using shared session. |

## Decisions

- Work in the current checkout because the active goal says the current worktree is authoritative and contains relevant dirty state.
- Do not mutate the existing root SFTP planning files; this scoped plan tracks only the Tn Editor objective.
- Start with pure P0 tests before production edits: conflict detection and text serialization must be testable headlessly.

## Errors

- `planning-with-files` catch-up failed to print full unsynced context under GBK output; recovered durable files and git state directly with UTF-8 reads.
