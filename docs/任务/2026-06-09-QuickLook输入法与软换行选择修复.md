# Quick Look 输入法与软换行选择修复

## 定位

本任务修复 TnE-18 真机复验暴露的 Quick Look 编辑体验回归:IME 候选窗口不跟随真实光标、连续输入时卡顿、软换行长行中 `Shift+Up/Down` 无法按视觉行扩展选区。

## 根因

- 编辑区 IME `bounds_for_range` 仍用旧模型:横向按 `逻辑列 * char_w`,纵向近似为代码区中线。自绘 File/Edit 已经接入 `LineLayout`、CJK 双宽、软换行和 `el_scroll_y/hscroll_px`,候选框坐标与真实绘制光标分叉。
- `replace_text_in_range` 每次文本提交后都调用 `scroll_to_item(cursor, Center)`。自绘编辑器已有按 cursor 变化去抖的 `el_follow_caret`;每字强制居中会在连续输入时增加滚动/布局抖动。
- Markdown/txt/log 软换行后,一个很长的逻辑行会显示成多个视觉行;原 `Document::move_cursor("up/down")` 只按逻辑行移动,因此长逻辑行内 `Shift+Up/Down` 没有目标行。

## 改动

- 在 `quick_look.rs` 新增 `quicklook_caret_paint_rect`,复用 `quicklook_file_layout` 计算真实 caret paint rect,覆盖软换行、CJK 双宽、横滚和纵滚偏移。
- `EntityInputHandler::bounds_for_range` 在正文编辑态使用上述 caret rect,候选框对齐真实 caret 底部;查找框场景仍继续贴 `find_field_bounds`。
- 在软换行文件中,`Up/Down` 先尝试 `quicklook_visual_vertical_cursor` 做视觉行移动;命中时通过 `QuickLookEditState::place_cursor(..., extend)` 保持 `Shift` 选区语义。代码文件和 legacy renderer 仍走原逻辑行移动。
- `replace_text_in_range` 自绘路径不再每次提交后强制 `scroll_to_item(...Center)`;legacy `uniform_list` fallback 仍保留旧居中行为。

## 验证

- RED: `cargo test -p tn-ui --lib ime_caret_rect_uses_soft_wrap_cjk_and_scroll_offsets` 最初因缺少 `CaretPaintRect` / `quicklook_caret_paint_rect` / 视觉行 helper 编译失败。
- GREEN:
  - `cargo test -p tn-ui --lib ime_caret_rect_uses_soft_wrap_cjk_and_scroll_offsets`
  - `cargo test -p tn-ui --lib soft_wrapped_vertical_motion_moves_between_visual_rows`
  - `cargo test -p tn-ui --lib text_commit_only_centers_legacy_uniform_list_renderer`
  - `cargo test -p tn-ui --lib` -> 164 passed
  - `git diff --check -- crates/tn-ui/src/quick_look.rs`

## 待复验

- 真机打开长 Markdown/txt 文件,在软换行视觉行内用微软拼音连续输入:候选框应贴当前 caret,输入不应每字卡顿或跳到视口中心。
- 在同一长逻辑行内按 `Shift+Down` / `Shift+Up`:选区应跨视觉行扩展;代码文件长行仍应横滚而非软换行。
