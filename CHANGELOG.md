# Changelog — Tn 终端

本文件记录 Tn 各里程碑的变更,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。
版本对应开发蓝图([docs/架构蓝图.md](docs/架构蓝图.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 **Windows 优先、Rust、GPU 加速**的终端,为 vibe coding 设计:托管 Claude Code /
> Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite)·
> alacritty_terminal(VT 引擎)· portable-pty(ConPTY)· russh(SSH,M2)。许可证 GPL-3.0-or-later。

**当前状态(2026-05):M0–M5 全部落地**(执行顺序 M0→M1→M3→M4→M5→M2)。M1 已 tag 为 `[0.1.0]`;
M3/M4/M5/M2-WSL 在 `main` 上以单次提交落地(下方各 `[Unreleased]` 段,**新里程碑在上**),尚未打新 tag。
**唯一未完成:M2 的 SSH**——已编译 + headless 单测,owner 决定暂停(parked),等有远程登录需求再做端到端。

## [Unreleased] — 面板解耦:per-pane 工作区上下文(2026-06)

让每个终端窗格拥有自己的「工作区上下文」,文件树状态不再被全局单例串台;「打开文件夹」只影响当前焦点 pane。

### Added
- **per-pane 文件树状态(展开态 + 选中文件)**:`ExplorerSnapshot`(`crates/tn-ui/src/explorer.rs`)+ `ExplorerView::snapshot()`/`switch_pane()`;Workspace 按 `PaneId` 存 `explorer_states` 快照、`explorer_pane` 记当前展示的 pane。焦点在分屏 pane 间切换时保存旧 pane、恢复新 pane 的展开/选中,各 pane 文件树互不串台;同 pane 内 `cd` 仍走 `follow_root`(保留子目录展开态)。快照在保存时惰性裁掉已关闭 pane,无需逐 `remove` 钩子。纯函数 `snapshot_under_root` 把恢复过滤到新 root 内(headless 单测覆盖)。

### Changed
- **「打开文件夹」收敛到焦点 pane**:`cd_panes_to_root`(广播给所有非 agent pane)→ `cd_pane_to_root(id, …)`(单 pane);`menu_open_folder` 只 `cd` + `set_rail_root` 当前焦点 pane,其它 pane 保持各自目录,agent pane 永不被 `cd`。SSH pane 点「打开文件夹」本轮**禁用 + echo 提示**(远端浏览需 SFTP / 远端 FS 后端,后续支持),不把本机路径塞进远端 shell。

> Agent 身份/用量环/「本次改动」/git watcher 早已 per-pane(在 `TerminalView` 上),本轮只验证不回归。逐项见 [docs/架构蓝图.md](docs/架构蓝图.md);坑 + 操作见 [CLAUDE.md](CLAUDE.md)。

## [Unreleased] — Agent Host 平台化(2026-06)

把「Claude/Codex 特判」重构为**对具体 agent 零知识的 Agent Host 平台**,分 P0–P6 落地(每阶段独立编译 + 测试绿)。

### Added
- **`tn-agent` 平台 crate(headless)**:`AgentId`(开放字符串身份,替代闭合 `AgentKind`)· `AgentDescriptor` + `AgentCapabilities` + `AgentRuntimeKind`(身份/能力插槽/运行位置)· `AgentEvent` + `AgentStatus`(UI 唯一输入契约)· `AgentAdapter` trait + `GenericAdapter`(有身份无遥测)· `AgentRegistry`(按 id/命令解析,空 = 纯 shell 宿主;`register_manifest` 注册 config agent)· `AiUsage` + pricing(从 tn-ai 上移)。
- **config `[[agents]]` manifest**:用户写 TOML 即可让新 agent 进启动器/头/能力插槽(`AgentManifest` → `AgentDescriptor::from_manifest`,无遥测);`runtime_support` 声明 PTY / structured / http / websocket / remote-daemon 支持,`allow_network = true` 只进入“需用户确认”策略而非静默放行;`[agents.<id>]` 主题色表 + `[general.billing]` 按 id billing 覆盖(`accent_for`/`billing_for`)。
- **`LaunchSpec::runtime()` + 非 PTY runtime 契约**:从 ssh/file_namespace 派生 `AgentRuntimeKind`(PTY 家族),并开放 `RemoteDaemon`/`Http`/`WebSocket`/`Structured`;runtime 与 `FileNamespace` 严格分离。
- **外部 agent 实时事件 adapter**:`ExternalEventAdapter` 接 JSONL → `AgentEvent`, `ExternalProcessAdapter` 可拉起 stdio 子进程并从 stdout JSONL 实时入队,stderr 转 `ErrorReported`;UI 只对 `has_realtime_events()` 的 adapter 启动轻量事件 poller。
- **配置可达的 sidecar 遥测 + 网络确认(最后一公里)**:`[[agents]] sidecar = "cmd"`(`tn_config::AgentManifest.sidecar`)→ `AgentDescriptor.realtime_command`;`AgentDescriptor::sidecar_launch()` 是默认拒绝网络的单点决策(`SidecarLaunch::{None,SpawnNow,Confirm}`)。launched agent pane 据此**每 pane** 拉起自己的 `ExternalProcessAdapter`(`from_descriptor`)+ 事件 poller:本地 stdio sidecar 直接起、**networked sidecar 弹确认卡**(`SidecarConfirm`,拒绝/允许并连接)。adapter 归 `TerminalView.realtime_adapter` 持有,`clear_agent`/drop 即杀子进程。这让**配置声明的 agent 也能有真实用量/transcript/权限**,无需内置 adapter —— 兑现「配置可达 + 网络确认」的最后一公里。诚实:应用内编辑器仍只产 generic(不收 sidecar),sidecar 是 config 高级项。
- **AgentEvent 高级渲染槽**:`StatusChanged` / `ModelChanged` / `TranscriptAppended` / `PermissionRequested` / `ErrorReported` 进入同一个 `reduce_agent_event` 漏斗,agent 头显示状态 / 权限 / 错误 / transcript 摘要 chip;`CwdChanged` 只更新目标 pane 的上下文和活动栏 cwd。
- **守卫测试** `agent_host::guard::ui_has_no_closed_agent_enum`:扫 `tn-ui/src` 锁死 UI 零闭合枚举。

### Changed
- **tn-ai → 内置 Claude/Codex `AgentAdapter`**(平台两个种子 provider,可移除):薄包装 `claude.rs`/`codex.rs` 解析;`detect.rs` 泛化为 `resolve_pane_session(&dyn AgentAdapter)`(launch 后 stale→fresh,第三个 agent 无需新 match)。`builtin_registry()` 保留但**默认 app 不再注册**(出厂无内置 agent)。
- **tn-ui 全面去 `AgentKind`**:身份走 `AgentId` + `AgentRegistry`(gpui Global);per-pane 缓存 `agent_accent`/`label`/`short`/`manages_cursor`/`caps` 经 `resolve_agent_view` 在 agent 变更时解析;`force_hide_cursor` → 描述符 `manages_own_cursor`;header 用量环 gate `caps.usage`、活动栏 gate `caps.git_diff`;状态栏按 `AgentId` 聚合(无固定 Claude/Codex 槽);用量轮询和外部实时事件都经 `AgentEvent` 归约器入账(内置日志 adapter 仍不新增热路径)。

### Removed
- **闭合 `AgentKind` 枚举 + 其 kind-dispatch API**(`resolve_session`/`session_mtimes`/`parse_session`/`update_session`/`detect_subscription`/`agent_kind_for_command` 等)全删——agent 身份与解析全部走 `AgentId` + adapter。
- **`tn-ai` 对 Claude/Codex 原始解析器的 public re-export**:外部只拿 `ClaudeAdapter` / `CodexAdapter` / `builtin_registry`,具体 JSONL 解析函数继续留在 `tn-ai` 内部给 adapter 和单测使用。

## [Unreleased] — 应用内 Agent 编辑器(2026-06)

把「加 agent」从手编 `config.toml` `[[agents]]` 升级为应用内现代交互(用户反馈:编辑配置太极客)。建立在 Agent Host 平台之上 —— 编辑器只产出 config 数据,平台零改动。

### Added
- **欢迎页 launchpad「+ 添加 Agent」磁贴 + 居中玻璃浮层编辑器**(`workspace::render_agent_form`):收集 名称 / 命令 / 颜色(`tn_config::ACCENT_SWATCHES` 预设)/「由 Agent 自绘光标(Ink TUI)」开关 + 实时磁贴预览;`Tab` 切字段 · `Enter` 保存 · `Esc` 取消 · 点外关闭。名称字段支持中文(IME,复用 `EntityInputHandler` 多路复用,与 SSH 重命名同源),命令字段 ASCII(IME 关)。
- **自定义磁贴 hover ✎/✕**(`welcome::agent_tile_actions`):编辑预填表单回写、删除抹掉 `[[agents]]`+`[[profiles]]`(`EditAgentRequested`/`DeleteAgentRequested`/`AddAgentRequested` 事件回 workspace)。
- **保存即生效(无需重启)**:`workspace::reload_agents` 重读 config → 重建 `AgentRegistry` global → 刷新 `launch_profiles` → 重建 welcome(`subscribe_welcome` 复用订阅),新磁贴立即出现。
- **tn-config 持久化**:`append_agent[_to]` / `remove_agent[_from]`(块级追加/删除,保注释,泛化的 `block_ranges`)+ `agents_toml_fragment` + `ACCENT_SWATCHES` 颜色预设。
- **id 派生**:`slugify`(名称→命令首词,deduped)生成稳定 `AgentId`;命令首词进 `aliases` → 「shell 里敲它自动切 Agent 态」即时可用。诚实:config-only agent = generic(无用量遥测,需内置/外部 adapter)。
- **claude/codex 命令的配置 agent 自动获得真实用量环(2026-06)**:`agent_host::build_registry`(startup 与 `reload_agents` 共用)对每个 manifest 先试 `tn_ai::builtin_adapter_for_manifest` —— 命令/别名命中 claude/codex → 用内置日志解析器 + **用户自己的颜色/名称/Ink**(`ClaudeAdapter/CodexAdapter::with_descriptor`,`usage` 强制开),否则 generic。**用户「+ 添加 Agent」填命令 `claude` 即自动出用量环,不需要 sidecar、不需要懂遥测**。平台 `tn-agent` 仍零 agent 知识,只 `tn-ai` 认名字。
- **进阶项也在 GUI(2026-06)**:编辑器加「**高级 · 用量遥测**」(文案明示「不懂就留空」)—— `AgentField::Sidecar` 文本字段 + 「联网 sidecar」开关。留空 = generic;设了 sidecar(开发者用:自备吐 JSON 用量的伴随程序)→ `capabilities=["usage"]` + 写 manifest `sidecar`,勾联网 → **只设 `allow_network=true`**(启动走确认卡)。这样进阶遥测也纯图形可配,不必手编 config.toml。
- **设计真源**:[`design/panels/04-overlays.html`](design/panels/04-overlays.html) 新增编辑器原型(含高级遥测区)。

### Fixed
- **联网 sidecar 把 agent 开成 shell**:GUI「联网 sidecar」曾写成 agent 的 `runtime_support=["remote_daemon"]`(非 PTY)→ PTY 启动器「不支持 LocalPty 即拒绝」守卫把 `claude` 这种命令型 agent 当非 PTY 拒了 → 回退 pwsh shell。修:sidecar 的网络属性只走 `allow_network`(`AgentDescriptor::sidecar_launch` 改为只看 `network_policy`,与 `runtime_support` 解耦);命令型 agent 的 `runtime_support` 永远是 PTY。旧版存的 agent 重新保存一次即修。

### 后续(未做)
- Agent Protocol / JSON-RPC 的完整请求-响应语义、HTTP/WebSocket 网络客户端和 tool-call/checkpoint Inspector。当前已落地的是 stdio JSONL 事件 adapter + 网络 runtime 安全契约。
- Agent 编辑器:命令参数/cwd 字段 · sidecar 命令的带引号路径(当前 `split_whitespace`)· 编辑器内中文搜索 · 在 Quick Terminal/分屏启动器也暴露「+ 添加」。
