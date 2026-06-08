# AGENTS.md

## 定位

本文是 Codex 的项目记忆入口,与 `CLAUDE.md` 同级。本文只保留 Codex 启动时必须读取的索引和最小工作规则;共享规则、踩坑和长期环境约束写入 `docs/共享记忆/` 的中文子文档。

## 入口索引

| 主题 | 文档 | 用途 |
|---|---|---|
| 当前任务状态 | [TODO.md](TODO.md) | 查看正在推进的任务、详情子文档和下一步 |
| 文档治理方案 | [docs/文档治理/文档盘点与首批拆分方案.md](docs/文档治理/文档盘点与首批拆分方案.md) | 判断新增长期信息的子文档落点和母文档索引格式 |
| 协作入口规则 | [docs/共享记忆/协作入口规则.md](docs/共享记忆/协作入口规则.md) | 维护 AGENTS/CLAUDE 同级入口与共享落点规则 |
| 共享记忆细节 | [docs/共享记忆/工作区与构建约定.md](docs/共享记忆/工作区与构建约定.md), [docs/共享记忆/踩坑记录.md](docs/共享记忆/踩坑记录.md) | 查看长期环境、构建命令、crate 边界和踩坑规则 |
| 架构与 crate 边界 | [docs/系统架构索引.md](docs/系统架构索引.md) | 查看系统边界、数据流、crate 职责和开发流程 |
| 产品体验 | [docs/产品体验索引.md](docs/产品体验索引.md) | 查看用户可见 UX 和产品决策 |
| UI / 样式规则 | [docs/界面样式实现规则.md](docs/界面样式实现规则.md) | UI 或 GPUI 样式改动前读取 |
| 已知限制 | [docs/已知问题索引.md](docs/已知问题索引.md) | 查看仍未完成的问题、限制和风险 |
| 实现历史 | [docs/修复与优化记录索引.md](docs/修复与优化记录索引.md), [docs/新增模块索引.md](docs/新增模块索引.md) | 按需追溯近期修复、优化和新增模块 |

## 工作规则

- 每次项目任务先看 [TODO.md](TODO.md),再进入对应 `docs/任务/YYYY-MM-DD-<中文主题>.md`。
- 新增长期有效信息时,先按 [docs/文档治理/文档盘点与首批拆分方案.md](docs/文档治理/文档盘点与首批拆分方案.md) 判断落点;母文档只保留索引、摘要、状态和链接。
- 每做一个项目改动,同步更新 `TODO.md` 和对应任务子文档,记录状态、验证结果和后续事项。
- 不要回滚或覆盖用户已有改动,除非用户明确要求。
- 优先沿用仓库现有模式和 crate 边界。
- UI 改动前先读 [docs/界面样式实现规则.md](docs/界面样式实现规则.md),并以 `design/mockup.html`、`design/panels/`、`design/calm-glass.css` 为视觉真源。

## 常用验证

```powershell
cargo build --workspace
cargo test --workspace --lib
cargo run -p tn-cli
$env:TN_AUTOQUIT="1"; cargo run -p tn-app
```

有 GUI 环境时,用 `cargo run -p tn-app` 做真实视觉检查。具体任务可按变更范围选择更窄的验证;文档重构任务不需要运行编译。

## 上下文压缩记录

- 暂无。
