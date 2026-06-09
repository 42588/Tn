# Quick Look 外部变更焦点修复

## 目标

修复 Quick Look 打开期间,目录下文件被其它软件修改后触发 workspace / explorer / agent rail 刷新,导致浮层仍显示但键盘焦点落到底层的问题。用户可见症状是 `Esc` 不能退出编辑或预览,`Up` / `Down` 不能继续切换文件。

## 根因

Quick Look 自身只在打开后的首帧通过 `needs_focus` 抢一次焦点;workspace 里 palette、split launcher、layout manager、SSH prompt、远端目录 picker 和 agent form 都有“打开期间每次 render 持续确保 focus”的守卫,但 Quick Look 没有同等守卫。外部文件变更会触发 `FilesChanged` / explorer rebuild / rail refresh 等 workspace 通知和重渲染,此时 focus 可能被重新停到 workspace 或底层 pane,Quick Look 的 `on_key` 就收不到 `Esc`、`Up`、`Down`。

## 改动

- `QuickLook` 暴露只读 `focus_handle()` 给 workspace 使用。
- workspace 渲染阶段在 Quick Look 打开且没有更高优先级 overlay 时持续确保 Quick Look focus。
- 抽出 `workspace_overlay_freezes_pane_focus(...)` 纯函数,把 Quick Look 纳入 overlay focus 冻结策略,避免刷新期间底层 pane focus 同步覆盖当前浮层语义。
- 新增 `quick_look_open_counts_as_focus_freezing_overlay` 回归测试。

## 验证

- RED: `cargo test -p tn-ui --lib quick_look_open_counts_as_focus_freezing_overlay` 初始失败,确认测试能捕获 Quick Look 未纳入 overlay focus 策略。
- GREEN: `cargo test -p tn-ui --lib quick_look_open_counts_as_focus_freezing_overlay` 通过。
- 回归: `cargo test -p tn-ui --lib` 通过,157 passed,0 failed。

## 后续

仍建议真机复验:Quick Look 打开文本文件后,用外部编辑器或命令修改同目录其它文件,确认浮层仍持有键盘焦点,`Esc`、`Up`、`Down`、编辑态输入和保存不失效。
