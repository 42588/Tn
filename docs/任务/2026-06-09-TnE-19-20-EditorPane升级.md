# TnE-19/20 EditorPane 升级

## 定位

记录 Quick Look / Tn Editor 收尾包中 TnE-19 与 TnE-20 的实现范围、验证结果和仍需真机复验事项。

## 背景

TnE-19 要求 Quick Look 可把当前文档会话升级为正式 Editor Pane,并保持 buffer、cursor、undo、dirty 连续。TnE-20 在此基础上提供可长期编辑的 pane/tab 形态、状态栏和 dirty close guard。

## 具体内容

- 新增 `tn-ui::editor::session::DocumentSession`,用共享 handle 承载 `tn_editor::Document`、行镜像、cursor、selection、undo/redo 和 dirty 状态。
- Quick Look 的编辑态从私有 `QuickLookEditState` 切到 `DocumentSession`;`Ctrl+Enter` 与底部“打开为编辑器”入口会导出 `EditorHandoff`。
- 新增 `tn-ui::editor::pane::EditorPane`,从 handoff 接收同一份 session,提供基础文本输入、撤销/重做、全选、复制/粘贴、本地保存、行列/编码/换行/dirty 状态栏。
- Workspace 新增 `editor_panes` map,让 pane tree leaf 可指向 terminal 或 editor。Quick Look handoff 会在当前 tab 右侧插入 Editor Pane;welcome tab 则直接替换为 Editor Pane。
- dirty Editor Pane 关闭时会显示“保存 / 放弃 / 取消”提示,覆盖关闭 pane、关闭 tab、应用菜单退出、快捷键退出和标题栏 / Alt+F4 窗口关闭路径,避免静默丢失。

## 验证 / 状态

- `cargo test -p tn-ui --lib` 通过,177 passed。
- 新增/覆盖测试:
  - `cloned_document_sessions_share_buffer_cursor_dirty_and_undo`
  - `editor_handoff_shares_quicklook_session_state`
  - `status_tracks_shared_session_cursor_and_dirty_state`
  - `dirty_session_requires_close_confirmation`
  - `dirty_close_request_opens_visible_prompt_state`
  - `dirty_close_prompt_matches_close_intent`
  - `inserting_editor_leaf_keeps_existing_terminal_leaf_and_focuses_editor`
  - `inserting_editor_leaf_replaces_welcome_tab`
- 待真机复验:Quick Look 编辑几步 -> 打开为编辑器 -> undo / 输入 / 保存 / 关闭 dirty 路径。
- 当前 Editor Pane 保存使用基础 UTF-8/LF 本地写盘路径;Quick Look 原有格式保持和冲突 guard 未抽成公共保存模块,后续若要完全同等保存语义需继续提取。

## 反向链接

- 当前队列:[TODO](../../TODO.md)
- 执行包:[快速预览编辑器执行包指导](../已知问题/快速预览编辑器执行包指导.md)
- 架构:[编辑器与快速预览](../架构/编辑器与快速预览.md)
- 产品体验:[快速预览与编辑](../产品体验/快速预览与编辑.md)
