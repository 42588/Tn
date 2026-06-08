# Changelog - Tn 终端

本文件只记录发布向变更摘要,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。详细设计、根因分析、验证记录和后续计划进入专题子文档。

版本对应开发蓝图([docs/系统架构索引.md](docs/系统架构索引.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 Windows 优先、Rust、GPU 加速的终端,为 vibe coding 设计:托管 Claude Code / Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite) / alacritty_terminal(VT 引擎) / portable-pty(ConPTY) / russh(SSH,M2)。许可证 GPL-3.0-or-later。

**当前状态(2026-05):M0-M5 全部落地**(执行顺序 M0->M1->M3->M4->M5->M2)。M1 已 tag 为 `[0.1.0]`;M3/M4/M5/M2-WSL 在 `main` 上以单次提交落地,尚未打新 tag。**唯一未完成:M2 的 SSH**:已编译 + headless 单测,owner 决定暂停(parked),等有远程登录需求再做端到端。

## [Unreleased] - 远端文件服务与改动流首版(2026-06)

详情:[远端文件服务与改动流](docs/新增模块/远端文件服务与改动流.md)

### Added
- 新增 `tn-pty::remote_fs` SFTP v3 后端、`tn-pty::remote_cmd` 远端命令执行、SSH Explorer root、Quick Look 远端预览与编辑写回、本地 guarded save、远端 git 数据流与远端 hunk apply 基础。
- 新增 `tn-editor` headless 编辑核心首版,把 Quick Look 编辑态接入 `Document` 主状态,为 LineLayout / EditorElement 上移打基础。

### Changed
- SSH pane 的「打开文件夹」改走应用内 SFTP 目录 picker,确认后只向当前焦点 SSH pane 发送远端 `cd`。

### Fixed
- 修复真机 SSH 场景暴露的远端文件树/picker、WSL picker、终端 resize/cursor、scrollback 和目录导航焦点问题。

### Still TODO
- SSH/SFTP 真机端到端回归剩余项见专题文档。

## [Unreleased] - 快速预览编辑器近期变更(2026-06)

详情:[快速预览编辑器近期变更](docs/修复与优化/快速预览编辑器近期变更.md)

### Added
- 新增自绘 File 预览、只读选区/复制/CJK 命中、Diff 装饰与 hunk 跳转模型、`[editor]` motion policy、LineLayout 软换行 headless 模型、prepaint 渲染模型和编辑器几何模型。

### Changed
- 自绘 File 预览/编辑器成为默认路径,`TN_QL_LEGACY=1` 继续作为旧 `uniform_list` 紧急回退。

### Fixed
- 修复查找跳转、高亮、横向跟随、中文输入、IME 候选框定位、退出编辑回预览旧内容、OS 关窗绕过 dirty-close 守卫等 Quick Look 编辑问题。

### Performance
- 补齐编辑核心增量化守卫,锁定每键不整 buffer 深拷的不变量。

## [Unreleased] - 面板解耦:per-pane 工作区上下文(2026-06)

详情:[每窗格工作区上下文](docs/修复与优化/每窗格工作区上下文.md)

### Added
- 新增 per-pane 文件树快照,焦点在分屏 pane 间切换时保存/恢复各自展开态与选中文件。

### Changed
- 「打开文件夹」从全局广播收敛为只影响当前焦点 pane;agent pane 不被 `cd`。

## [Unreleased] - 智能体宿主平台化(2026-06)

详情:[智能体宿主平台化](docs/新增模块/智能体宿主平台化.md)

### Added
- 新增 `tn-agent` 平台 crate、config `[[agents]]` manifest、开放 runtime 契约、外部 agent JSONL 实时事件 adapter、sidecar 遥测与网络确认、AgentEvent 高级渲染槽和 UI 去闭合 agent 枚举守卫测试。

### Changed
- `tn-ai` 改为内置 Claude/Codex `AgentAdapter` provider;`tn-ui` 身份全面改走 `AgentId` + `AgentRegistry`。

### Removed
- 移除闭合 `AgentKind` 枚举、kind-dispatch API,以及 `tn-ai` 对 Claude/Codex 原始解析器的 public re-export。

## [Unreleased] - 应用内智能体编辑器(2026-06)

详情:[应用内智能体编辑器](docs/新增模块/应用内智能体编辑器.md)

### Added
- 新增欢迎页「+ 添加 Agent」磁贴、居中浮层编辑器、自定义磁贴编辑/删除、保存即生效、`tn-config` 持久化、稳定 id 派生、Claude/Codex 命令自动获得真实用量环、高级 sidecar 遥测配置和设计真源原型。

### Fixed
- 修复联网 sidecar 把命令型 agent 开成 shell、同一 agent 显示两张磁贴、欢迎页「打开文件夹」失效。

### Still TODO
- Agent Protocol / JSON-RPC、HTTP/WebSocket 客户端、tool-call/checkpoint Inspector 和编辑器进阶字段仍未落地。
