# Agent 自管 transcript 历史 — 设计

## 缘起

见 [agent 历史滚动与本地化](2026-06-13-agent历史滚动与本地化.md) 的「收敛」节:codex/claude
这类 TUI agent 从不把完整对话喂进终端 scrollback,只按视口高度重画可见区,所以**任何**在终端
scrollback 上做文章的方案(放大上限、滚轮转发、`CSI 3J` 过滤、inline 模式)都只能拿到"被挤出
视口的那几行",真机实测 `history` 永远 ~14 行。

用户拍板方向:**不再依赖终端 scrollback,改由 Tn 自己维护可滚动的完整 transcript**,数据来自
adapter 已经在读的会话日志。Phase 1(撤回 inline、修打字回归)已落地。本文是 Phase 2 设计。

## 北极星

在 agent 面板里,用户能**可靠地回滚整段对话历史**,与 agent 怎么启动(磁贴/shell 输入)、用
哪种屏幕模式(全屏/inline)无关,**resume 旧会话也不丢**;右侧有真实滚动条,滚轮即可进入历史。

## 关键事实(已核对)

- 数据源现成:
  - Claude:`~/.claude/projects/<encoded-proj>/<session>.jsonl`,逐行 JSONL(user / assistant /
    tool_use / tool_result …)。
  - Codex:`$CODEX_HOME/sessions/**/rollout-*.jsonl`。
- **会话定位与增量 tail 已实现**:`tn-ai` 的 usage poller(`spawn_usage_poller`)已经能 (a) 解析
  本 pane 的会话文件、(b) 钉死它、(c) 按字节 offset 增量读尾巴。transcript 复用同一套即可。
- **事件通道已预留**:`tn-agent::AgentEvent::TranscriptAppended(String)` 已存在(P4 占位,注释写
  明"structured turns"待补)。`reduce_agent_event` 是 UI 唯一输入口,已有 usage 走这条。
- `AgentCapabilities.transcript` 能力位已存在;内置 Claude/Codex 走 `full_capabilities()`。

## 架构(三层)

### 1. tn-ai / tn-agent:transcript 解析(纯函数,先做、最安全)

定义 render-ready 的归一化条目:

```rust
pub struct TranscriptEntry {
    pub role: TranscriptRole,   // User / Assistant / Tool / System
    pub kind: TranscriptKind,   // Message / ToolCall { name } / ToolResult / Reasoning
    pub text: String,           // 纯文本(markdown 源,先不渲染)
    pub ts: Option<u64>,
}
```

`AgentAdapter` 加 `fn parse_transcript(&self, jsonl: &str) -> Vec<TranscriptEntry>`(与 usage 解析
分离)。Claude / Codex 各自把自家 JSONL schema 映射过来,复用 `claude` / `codex` / `usage_windows`
模块已有的 jsonl 读取。**用真实日志样本写单测**(本机 `~/.claude/projects` 与 codex sessions 各
取一段脱敏样本)。

### 2. tn-ui:TranscriptModel(per-pane 状态)+ poller

- 新结构持有 `Vec<TranscriptEntry>` + 钉死的会话路径 + 字节 offset。
- 后台 poller tail 日志、把新条目 append,经 `reduce_agent_event` /
  `AgentEvent::TranscriptAppended`(本任务将其升级为携带结构化条目)funnel 进来,变化即 `notify()`。
- 复用 `spawn_usage_poller` 的会话解析/钉死逻辑(抽出共用,避免两个 poller 各扫一遍)。

### 3. tn-ui:历史区视图(滚动 + 滚动条)

**UX 形态(待用户定,见下「待决」)**。两个候选:

- **A. 滚轮进入叠层(贴合用户心智)**:live agent 在底部;用户在终端自身(极小)scrollback 滚到顶
  后继续上滚 → 弹出 Tn 渲染的 transcript 叠层覆盖正文区,可滚、有真实滚动条;滚回底部 / 按 End
  回到 live。一条滚轮手势连贯走「live → 终端 scrollback → Tn transcript」。"滚轮 + 滚动条"心智
  最贴,但叠层与终端网格的接缝最费工。
- **B. 显式开关(MVP 最稳)**:header 一个「历史」按钮 / 快捷键,翻到一个全屏可滚 transcript
  叠层,Esc/再点返回 live。最快落地、风险最低,但不是"滚轮"心智。

无论 A/B,**渲染层是同一个**:一个 GPUI 可滚列表,逐条渲染 role chip(You / Claude / Tool)+ 文本
体;tool call 折叠成一行摘要(名字 + 参数预览)。**不复刻 agent 的 TUI**,只做"可读、可复制的历史
日志"。markdown 渲染、搜索、跳转留到 2d。

## 分期(Phase 2 内部)

- **2a** adapter transcript 解析 + 真实日志样本单测(纯、安全,可独立合)。**✅ 已落地**
  - `tn-agent::transcript`:`TranscriptEntry{role,kind,tool,text}` + `TranscriptRole`
    (User/Assistant/Tool/System)+ `TranscriptKind`(Message/Reasoning/ToolCall/ToolResult)+
    `preview()`(截断)+ `push_collapsed()`(折叠连续重复 —— Codex 会重发同一 `user_message`)。
  - `AgentAdapter::parse_transcript(text) -> Vec<TranscriptEntry>`(默认空;每行自含,全量/增量
    delta 通用)。
  - `tn-ai`:`claude::parse_claude_transcript`(user/assistant 的 text/tool_use/tool_result 块,
    跳过 thinking 与 queue-operation/attachment/snapshot/title 等噪音)、
    `codex::parse_codex_transcript`(用 `event_msg/user_message`+`agent_message` 取干净对话,避开
    developer/context 注入与 output_text 重复;`function_call(_output)`/`custom_tool_call(_output)`
    取工具;跳过加密 reasoning)。
  - 真机实测:本会话 Claude 日志 → 284 条(1 user+66 assistant+109 tool call+108 result);
    Codex 大会话 → 1070/2095 条全部解析,证明能拿到**完整**历史。`cargo test -p tn-agent -p tn-ai`
    27 绿(含 `transcript_*`)、`cargo check --workspace` 通过。
- **2b** TranscriptModel + poller(tail → 条目),走 AgentEvent;升级 `TranscriptAppended` 为结构化。
- **2c** 历史叠层视图(可滚 + 滚动条)+ 进入/退出手势(A 或 B)。
- **2d** 打磨:markdown、复制、搜索、跳到某轮、与「本次改动」rail / Quick Look 的协同。

## 已定(2026-06-13 用户拍板)

1. **历史区露出 = 候选 A:滚轮直接进历史叠层**。向上滚超过 live 顶部 → 弹出可滚 transcript
   叠层 + 右侧真实滚动条;滚回底/按 End 回 live。一条滚轮手势连贯走「live → (终端极小
   scrollback) → Tn transcript」。
2. **渲染保真度 = 先可读纯文本日志**。role chip + 文本体,tool call 折一行摘要;markdown /
   搜索 / 跳转留到 2d。

> 2a(解析)与上面无关,先起步。

## 验证基线

- 每个 adapter 解析器:用真实日志样本断言条目数、role/kind、文本片段。
- 不在 UI render 里做文件 I/O(沿用 activity rail 的纪律:I/O 在后台,render 只读快照)。
- 真机:开一个长会话 + resume 一个旧会话,确认能滚到最早一轮。
