# QuickLook Markdown 渲染美化

- 日期: 2026-06-17
- 范围: `crates/tn-ui/src/quick_look.rs`
- 状态: 已完成（真机验证通过 · 2026-06-19）

## 缘起

用户反馈 QuickLook 渲染 Markdown 的显示效果比较单调，希望对其进行视觉美化。
我们需要在遵循「磷光」设计契约的前提下，提升 Markdown 预览的精致感与科技感。

## 设计决策

1. **阅读视口**：增加左右内边距，引入居中的 `720px` 最大宽度限制以提升大屏下的阅读舒适度。
2. **西文标题**：标题使用 Space Grotesk 字体（`UI_DISPLAY`）。（应用户反馈，已移除 H1/H2 的下划线与左侧发丝竖边，保持整洁自然的排版）。
3. **代码块**：顶部增加 macOS 风格的三色窗口控制点与右侧大写语言标识，行内新增等宽行号指示器，背景使用 recessed `L1` 底色与 `H1` 边框。
4. **行内代码**：使用 `R_CHIP` 圆角与 `H0` 发丝边背景，文本使用 `PH` 磷光绿高亮。
5. **引用块**：增加 `L2` 背景底色与 `PH` 磷光边线，使层级清晰。
6. **任务列表**：使用 GPUI 渲染精美复选框组件，替代 Unicode checkbox。
7. **无序列表**：使用 5x5 的 `PH_DIM` 磷光菱形/方形作为列表 bullet。
8. **表格**：实现交替斑马纹。

## 实现队列

- [x] 整体阅读视口限宽与边距调整。
- [x] 标题字族升级为 `Space Grotesk` 并去除下划线与左侧装饰竖边（根据反馈微调）。
- [x] 代码块（Mock macOS 窗口顶栏与语言标签、行号渲染）。
- [x] 行内代码高亮、引用块背板精细化与左侧 PH 竖条。
- [x] 列表子项（自定义菱形/方形 bullet、自绘 TaskList Checkbox）。
- [x] 表格 zebra 交替背景与边框优化。
- [x] 修复最后一行被底栏裁切问题（为滚动容器内容区增加 `pb(px(60.))` 底部边距）。
- [x] 自动化测试与本地编译回归验证。

## 验证记录

- **静态编译检查**：在 `d:/coder/Tn` 执行 `cargo check --workspace` 编译通过。
- **单元测试**：执行 `cargo test --workspace`，共 208 项单元测试全部通过。
- **测试用例**：
  ```
  test quick_look::tests::markdown_path_detection ... ok
  test quick_look::tests::markdown_file_uses_visual_soft_wrap_while_code_keeps_horizontal_scroll ... ok
  test quick_look::tests::markdown_code_fence_collects_lines ... ok
  test quick_look::tests::tight_list_item_emits_inline_events_without_paragraph_wrapper ... ok
  ```

## 真机验证（2026-06-19）

用户真机确认通过：Markdown 各类段落、代码块、任务复选框在真机界面渲染正常，行间距合理。任务收尾，移入 TODO 已完成。
