# 移除独立 EditorPane

## 定位

本文记录“只移除独立 EditorPane 功能,不动 Quick Look 原本编辑与预览”的执行过程。

## 背景

用户明确不希望在终端产品中保留长期独立编辑器 pane。当前删除范围只包含 `EditorPane` 作为 workspace pane 的功能和 Quick Look 的“打开为编辑器”出口;Quick Look 自身的文件预览、编辑、保存、Diff、远端读写和冲突保护继续保留。

## 具体内容

- [x] 先写删除特征测试,确认当前仍暴露独立 EditorPane 入口时会失败。
- [x] 移除 `workspace` 的 `EditorPane` registry、render、关闭保护和 handoff 处理。
- [x] 移除 Quick Look 的 `OpenAsEditor` 事件、handoff 类型、快捷键和按钮。
- [x] 删除 `crates/tn-ui/src/editor/pane.rs`,保留 Quick Look 仍使用的 editor geometry/session/motion/prepaint/diff 模块。
- [x] 更新当前功能状态文档。
- [x] 运行 targeted Rust 测试和 `tn-ui` lib 回归测试。
- [x] 运行文档校验和最终源码扫描。
- [x] 在本文记录验证结果后,将 `TODO.md` 本任务移动到已完成。

## 验证 / 状态

- 状态:已完成。
- 红灯验证:`cargo test -p tn-ui standalone_editor_pane_feature_is_removed --lib` 已失败,失败点为 `editor/pane.rs` 仍存在。
- 绿灯验证:`cargo test -p tn-ui standalone_editor_pane_feature_is_removed --lib` 通过,1 个测试通过。
- 回归验证:`cargo test -p tn-ui --lib` 通过,167 个测试全部通过。
- 源码扫描:`rg -n "EditorPane|Editor Pane|OpenAsEditor|EditorHandoff|editor_panes|open_editor_handoff|insert_editor_leaf|request_editor_quit|force_quit_after_editor_close|打开为编辑器|pub mod pane" crates/tn-ui/src docs/项目当前功能与状态.md` 无输出。

## 反向链接

- [TODO](../../TODO.md)
- [项目当前功能与状态](../项目当前功能与状态.md)
