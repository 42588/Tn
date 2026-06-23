# QuickLook Diff 到 Markdown 跳转滚动修复

## 定位

修复 QuickLook Diff 视图跳转到 Markdown 文件预览时,目录索引已经跟随目标行高亮,但 Markdown 正文没有滚动到对应位置的问题。

## 背景

Diff 跳转逻辑会把 Diff 行映射为源文件行,并切换到 File 预览页。当前 File 页里的 Markdown 预览使用独立的 `md_scroll`,而普通文件/编辑器跳转使用 `scroll` 与自绘文件滚动定位,两条滚动链路不一致。

## 具体内容

- 检查 Diff 行到文件行的映射、Enter 跳转和大纲点击的 Markdown 跳转逻辑。
- 增加 Markdown 源文件行到渲染块索引的回归测试。
- 让 Diff 跳转进入 Markdown 预览时同步驱动 `md_scroll`。
- 复用同一映射逻辑,避免大纲点击和 Diff 跳转各自维护一套最近块计算。

## 验证 / 状态

- 已完成。
- 红测: `cargo test -p tn-ui markdown_jump_uses_nearest_rendered_block_for_source_line` 初次失败于缺少 `md_block_index_for_line`。
- 相关测试: `cargo test -p tn-ui jump` 4 通过。
- Diff 回归: `cargo test -p tn-ui diff` 22 通过。
- 全量验证: `cargo test -p tn-ui` 233 通过。
- 编译验证: `cargo check -p tn-ui` 通过,保留既有 `local_dir_picker::open_selected` 与 `pet_lottie::render_rgba` dead_code warning。
- 格式检查: `cargo fmt -p tn-ui --check` 因仓库既有多文件格式漂移失败,本任务未进行全包格式化以避免无关改动。
- 空白检查: `git diff --check -- TODO.md crates/tn-ui/src/quick_look.rs docs/任务/2026-06-23-QuickLook-Diff到Markdown跳转滚动修复.md` 通过。

## 反向链接

- [TODO.md](../../TODO.md)
