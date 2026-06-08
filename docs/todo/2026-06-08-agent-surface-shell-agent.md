# shell 内敲 claude / codex 后只有 agent 外壳没有正文

## 状态

已完成。用户复跑确认修复正确。

## 目标

修复普通 shell 中直接输入 `claude` / `codex` 后 UI 切到 agent 头和活动栏,但终端正文区域不显示的问题。

## 根因摘要

shell-detected agent 的 activity rail 走 overlay,用于避免命令运行中改变终端正文宽度;但 overlay 宿主原先不是 flex-column,导致内部 `term_area.flex_1()` 在普通 relative 父级里拿不到可布局高度,表现为只有 agent 外壳没有正文。磁贴启动路径走 side-by-side flex row,所以不复现。

## 改动摘要

- overlay 宿主补成 flex-column。
- agent surface 统一拥有 viewport,隐藏 shell block bar,滚轮交给 agent / alt-screen 程序。
- 增加 `tn::agent_surface` 诊断日志,用于记录 shell 命令识别、布局、bounds、grid 等证据。
- 将用户复跑确认写入项目文档。

## 验证

- `cargo test -p tn-ui --lib` 通过。
- `cargo build --workspace` 通过。
- 用户复跑确认修复正确。

## 权威记录

- 修复日志: [`docs/优化日志.md`](../优化日志.md)
- 主记忆踩坑: [`CLAUDE.md`](../../CLAUDE.md)
- 架构说明: [`docs/架构蓝图.md`](../架构蓝图.md)
- 产品说明: [`docs/产品设计.md`](../产品设计.md)

## 后续

无。
