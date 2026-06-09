# TnE-18 typing effects + perf 降级 gate

## 状态

已完成。

## 目标

为 Quick Look 编辑态实现可关闭、可降级、只影响绘制层的轻量打字反馈:同一视觉行内打字/删除时使用与终端一致的 cursor glide + 块光标 squash/pop。动画不得改变真实 cursor、文本宽度、行高、命中测试、selection、undo 或保存语义。

## 范围

- 只认领 TnE-18,不合并 TnE-19/20/21。
- 复用 TnE-17 的 `[editor] animations` / `EffectiveMotion` 策略。
- 新增小型纯模型承载 motion state 和 snap/gate 决策。
- Quick Look 只在编辑态 File 自绘路径消费 motion 样式。
- 仅允许单个 16ms frame driver 驱动 cursor 动画;不积累 per-char 队列。
- 不做插入字符 settle / afterglow;避免快速输入时出现残影或多光标感。
- 光标运动模型对齐终端:保留当前绘制列,每帧按 0.4 chase factor 追逐最新目标列;连续输入时从 in-flight 绘制位置继续追,不是每键重置线性插值。
- 光标视觉几何对齐终端:Quick Look 自绘块光标使用 1px 圆角,视觉高度按 `CODE_FS + 4px` 居中到 `ROW_H`,不改变真实行高、IME 锚点、命中测试、selection 或 copy 语义。
- IME 合成、拖选、选区、普通方向移动、查找跳转、滚动、大文件、高负载、关闭动画或 reduced-motion 场景立即 snap。

## 执行清单

- [x] 读取执行包、配置 motion policy、Quick Look caret 绘制、`PerfStats` 和渲染/IME 坑位。
- [x] 确认当前工作区已有未提交改动,就地工作并避免覆盖。
- [x] TDD:新增 motion 纯逻辑测试并确认先失败。
- [x] 实现 motion 纯模型。
- [x] 接入 Quick Look 编辑输入与绘制路径。
- [x] TDD:新增 Quick Look caret visual geometry 回归测试并确认先失败。
- [x] 对齐自绘 File/Edit 光标与 legacy `cursor_block` 的 1px 圆角和视觉高度。
- [x] 运行 `cargo test -p tn-ui --lib`。
- [x] 同步执行包、用户体验和修复记录状态。

## 验证

- RED:`cargo test -p tn-ui --lib caret_motion` 初始因 `CaretMotionState` / `motion_snapshot` 等 API 不存在失败。
- GREEN:`cargo test -p tn-ui --lib caret_motion`:3 passed。
- `cargo test -p tn-ui --lib terminal_style_motion_is_cursor_only_for_typing_and_deleting`:1 passed。
- `cargo test -p tn-ui --lib motion_triggers_only_for_text_insert_and_delete`:1 passed。
- `cargo test -p tn-ui --lib helper_gates_multi_char_text_and_large_files`:1 passed。
- RED:`cargo test -p tn-ui --lib terminal_chase_continues_from_drawn_position_across_rapid_typing -- --nocapture` 在线性偏移模型下失败,暴露连续输入未沿用当前绘制位置。
- GREEN:`cargo test -p tn-ui --lib terminal_chase_continues_from_drawn_position_across_rapid_typing -- --nocapture`:1 passed。
- `cargo test -p tn-ui --lib motion -- --nocapture`:8 passed。
- RED:`cargo test -p tn-ui --lib self_painted_caret_visual_matches_terminal_radius_and_text_scale -- --nocapture` 初始因 `CaretVisualRect` / `caret_visual_rect` 不存在失败。
- GREEN:`cargo test -p tn-ui --lib self_painted_caret_visual_matches_terminal_radius_and_text_scale -- --nocapture`:1 passed。
- `cargo test -p tn-ui --lib ime_caret_rect_uses_soft_wrap_cjk_and_scroll_offsets -- --nocapture`:1 passed。
- `cargo test -p tn-ui --lib soft_wrapped_selection_projects_to_visual_rows_without_changing_copy_text -- --nocapture`:1 passed。
- `cargo test -p tn-ui --lib motion -- --nocapture`:8 passed。
- `cargo test -p tn-ui --lib`:169 passed,0 failed。

未做真机 GUI 复验:仍建议 owner 用 `TN_PERF=1` 覆盖连续输入、删除、IME、4000 行和大文件场景,确认 subtle motion 与终端光标动画手感一致,光标圆角/高度与终端观感一致,且 selection/copy/鼠标/查找/滚动等 snap 场景没有残留视觉偏移。

## 后续

下一包是 TnE-19 文档会话共享;不得在本任务顺手实现。
