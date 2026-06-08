# AGENTS.md

这是本仓库给 Codex / agent 使用的轻量入口索引。保持简短，避免和真正的项目记忆长期漂移。

## 先读

- 先读 `CLAUDE.md`；它是主要项目指南和工作记忆。
- 当前任务状态和任务详情索引看根目录 `TODO.md`。
- 架构、crate 边界、数据流、开发流程看 `docs/架构蓝图.md`。
- 用户可见 UX 和产品决策看 `docs/产品设计.md`。
- UI / 样式改动前先读 `docs/样式还原手册.md`，再编辑 GPUI 代码。
- 已知限制看 `docs/未修复.md`。
- 近期实现历史按需看 `docs/优化日志.md` 和 `docs/新增模块.md`。

## 工作规则

- 不要在这里复制 `CLAUDE.md` 的长篇指导；需要更新时改源文档，让本文件只做索引。
- 每做一个项目改动，必须同步更新 `TODO.md` 和对应任务子文档（`docs/todo/YYYY-MM-DD-<topic>.md`），让任务状态、验证结果和后续事项可追踪。
- 新增长期有效信息时，必须先找明确子文档落点；母文档只保留索引、摘要、状态和链接。没有合适子文档时，先创建落点明确的新子文档，再更新母文档索引。
- 每做一个项目改动，必须同步更新对应源文档（如 `CLAUDE.md`、`docs/架构蓝图.md`、`docs/产品设计.md`、`docs/样式还原手册.md`、`docs/优化日志.md`、`docs/新增模块.md` 或 `docs/未修复.md`），让项目记忆跟代码一起前进；具体细节优先进入对应子文档，避免在多个母文档重复铺开。
- 每次预计会发生上下文压缩前，必须先把上一轮已执行操作追加到本文末尾的“上下文压缩记录”：一轮一条，写清目标、关键改动、验证结果和未完成事项，保持简短可追踪。
- 不要回滚或覆盖用户已有改动，除非用户明确要求。
- 优先沿用仓库现有模式和 crate 边界。
- UI 改动以 `design/mockup.html`、`design/panels/`、`design/calm-glass.css` 为视觉真源。

## 常用验证

```powershell
cargo build --workspace
cargo test --workspace --lib
cargo run -p tn-cli
$env:TN_AUTOQUIT="1"; cargo run -p tn-app
```

有 GUI 环境时，用 `cargo run -p tn-app` 做真实视觉检查。

## 上下文压缩记录

- 暂无。
