# Changelog — Tn 终端

本文件记录 Tn 各里程碑的变更,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。
版本对应开发蓝图([docs/架构蓝图.md](docs/架构蓝图.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 **Windows 优先、Rust、GPU 加速**的终端,为 vibe coding 设计:托管 Claude Code /
> Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite)·
> alacritty_terminal(VT 引擎)· portable-pty(ConPTY)· russh(SSH,M2)。许可证 GPL-3.0-or-later。

**当前状态(2026-05):M0–M5 全部落地**(执行顺序 M0→M1→M3→M4→M5→M2)。M1 已 tag 为 `[0.1.0]`;
M3/M4/M5/M2-WSL 在 `main` 上以单次提交落地(下方各 `[Unreleased]` 段,**新里程碑在上**),尚未打新 tag。
**唯一未完成:M2 的 SSH**——已编译 + headless 单测,owner 决定暂停(parked),等有远程登录需求再做端到端。

## [Unreleased] — Agent Host 平台化(2026-06)

把「Claude/Codex 特判」重构为**对具体 agent 零知识的 Agent Host 平台**,分 P0–P6 落地(每阶段独立编译 + 测试绿)。

### Added
- **`tn-agent` 平台 crate(headless)**:`AgentId`(开放字符串身份,替代闭合 `AgentKind`)· `AgentDescriptor` + `AgentCapabilities` + `AgentRuntimeKind`(身份/能力插槽/运行位置)· `AgentEvent` + `AgentStatus`(UI 唯一输入契约)· `AgentAdapter` trait + `GenericAdapter`(有身份无遥测)· `AgentRegistry`(按 id/命令解析,空 = 纯 shell 宿主;`register_manifest` 注册 config agent)· `AiUsage` + pricing(从 tn-ai 上移)。
- **config `[[agents]]` manifest**:用户写 TOML 即可让新 agent 进启动器/头/能力插槽(`AgentManifest` → `AgentDescriptor::from_manifest`,无遥测);`[agents.<id>]` 主题色表 + `[general.billing]` 按 id billing 覆盖(`accent_for`/`billing_for`)。
- **`LaunchSpec::runtime()`**:从 ssh/file_namespace 派生 `AgentRuntimeKind`(PTY 家族),与 `FileNamespace` 严格分离。
- **守卫测试** `agent_host::guard::ui_has_no_closed_agent_enum`:扫 `tn-ui/src` 锁死 UI 零闭合枚举。

### Changed
- **tn-ai → 内置 Claude/Codex `AgentAdapter`**(平台两个种子 provider,可移除):薄包装 `claude.rs`/`codex.rs` 解析;`detect.rs` 泛化为 `resolve_pane_session(&dyn AgentAdapter)`(launch 后 stale→fresh,第三个 agent 无需新 match)。`builtin_registry()` 保留但**默认 app 不再注册**(出厂无内置 agent)。
- **tn-ui 全面去 `AgentKind`**:身份走 `AgentId` + `AgentRegistry`(gpui Global);per-pane 缓存 `agent_accent`/`label`/`short`/`manages_cursor`/`caps` 经 `resolve_agent_view` 在 agent 变更时解析;`force_hide_cursor` → 描述符 `manages_own_cursor`;header 用量环 gate `caps.usage`、活动栏 gate `caps.git_diff`;状态栏按 `AgentId` 聚合(无固定 Claude/Codex 槽);用量轮询经 `AgentEvent` 归约器入账(复用既有后台线程,热路径不变)。

### Removed
- **闭合 `AgentKind` 枚举 + 其 kind-dispatch API**(`resolve_session`/`session_mtimes`/`parse_session`/`update_session`/`detect_subscription`/`agent_kind_for_command` 等)全删——agent 身份与解析全部走 `AgentId` + adapter。

## [Unreleased] — 应用内 Agent 编辑器(2026-06)

把「加 agent」从手编 `config.toml` `[[agents]]` 升级为应用内现代交互(用户反馈:编辑配置太极客)。建立在 Agent Host 平台之上 —— 编辑器只产出 config 数据,平台零改动。

### Added
- **欢迎页 launchpad「+ 添加 Agent」磁贴 + 居中玻璃浮层编辑器**(`workspace::render_agent_form`):收集 名称 / 命令 / 颜色(`tn_config::ACCENT_SWATCHES` 预设)/「由 Agent 自绘光标(Ink TUI)」开关 + 实时磁贴预览;`Tab` 切字段 · `Enter` 保存 · `Esc` 取消 · 点外关闭。名称字段支持中文(IME,复用 `EntityInputHandler` 多路复用,与 SSH 重命名同源),命令字段 ASCII(IME 关)。
- **自定义磁贴 hover ✎/✕**(`welcome::agent_tile_actions`):编辑预填表单回写、删除抹掉 `[[agents]]`+`[[profiles]]`(`EditAgentRequested`/`DeleteAgentRequested`/`AddAgentRequested` 事件回 workspace)。
- **保存即生效(无需重启)**:`workspace::reload_agents` 重读 config → 重建 `AgentRegistry` global → 刷新 `launch_profiles` → 重建 welcome(`subscribe_welcome` 复用订阅),新磁贴立即出现。
- **tn-config 持久化**:`append_agent[_to]` / `remove_agent[_from]`(块级追加/删除,保注释,泛化的 `block_ranges`)+ `agents_toml_fragment` + `ACCENT_SWATCHES` 颜色预设。
- **id 派生**:`slugify`(名称→命令首词,deduped)生成稳定 `AgentId`;命令首词进 `aliases` → 「shell 里敲它自动切 Agent 态」即时可用。诚实:config-only agent = generic(无用量遥测,需内置/外部 adapter)。
- **设计真源**:[`design/panels/04-overlays.html`](design/panels/04-overlays.html) 新增编辑器原型。

### 后续(未做)
- 外部进程 adapter **实时** stdio/JSON-RPC 通道(目前只定契约 + 最小实现)· 非 PTY 运行时 · AgentEvent transcript/permission/tool-call/status 渲染槽。
- Agent 编辑器:capability 勾选(usage 等,需先接 adapter,否则误导)· 命令参数/cwd 字段 · 编辑器内中文搜索 · 在 Quick Terminal/分屏启动器也暴露「+ 添加」。
