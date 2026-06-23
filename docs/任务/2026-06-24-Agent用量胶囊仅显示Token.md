# Agent 用量胶囊仅显示 Token

## 定位

调整 Agent pane 右上角用量区:保留左侧上下文读数与上下文圆环,右侧胶囊只显示 token 消耗,不再显示费用或 `CTX` 百分比。

## 背景

当前 `terminal_view/header.rs` 会让右侧 `usage-mode` chip 在费用、上下文百分比和 token 三种模式之间循环。用户希望右侧简化为 token 消耗,上下文信息仍由左侧读数和圆环承载。

## 具体内容

- 增加右侧 token chip 文案的回归测试。
- 调整 Agent header 渲染,去掉右侧费用 / `CTX` 模式展示。
- 保留左侧上下文读数、上下文圆环和点击打开额度浮层能力。

## 验证 / 状态

- 已完成。
- 红测: `cargo test -p tn-ui usage_token_chip_label_ignores_cost_and_context_modes` 初次失败于缺少 `usage_token_chip_label`。
- 相关测试: `cargo test -p tn-ui terminal_view::header` 5 通过。
- 兼容测试: `cargo test -p tn-ui usage_display` 1 通过。
- 编译验证: `cargo check -p tn-ui` 通过,保留既有 `local_dir_picker::open_selected` 与 `pet_lottie::render_rgba` dead_code warning。
- 全量验证: `cargo test -p tn-ui` 233 通过。
- 空白检查: `git diff --check -- TODO.md crates/tn-ui/src/terminal_view/header.rs crates/tn-ui/src/terminal_view/mod.rs crates/tn-ui/src/usage_display.rs docs/任务/2026-06-24-Agent用量胶囊仅显示Token.md` 通过。

## 反向链接

- [TODO.md](../../TODO.md)
