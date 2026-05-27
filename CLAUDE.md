# CLAUDE.md — Tn 终端(Claude Code 项目指南)

Tn 是一个 **Windows 优先、Rust 编写、GPU 加速、为 vibe coding 打造的终端**:把 AI 编码 CLI
(Claude Code、Codex)当一等公民托管,提供出色 UX、灵活平铺、WSL + SSH。技术栈:**GPUI**
(Zed 的框架,Windows 上走 DirectX 11 + DirectWrite)· **alacritty_terminal**(VT 引擎)·
**portable-pty**(ConPTY/WSL)+ **russh**(SSH)。许可证:GPL-3.0-or-later。

## 现状(先读)
- **M0 = 完成并已提交。** **M1 = 完成并已提交**(单次提交 `59b8b0e`,在 `main`):配置/主题、按键编码、push 重绘、滚动历史、选择/复制粘贴、分屏(切分 + 键盘改尺寸 + 热重载)。退出标准达成、已 dogfood 验证。变更见 [CHANGELOG.md](CHANGELOG.md)。
- **当前在做 = M3 + M4**(owner 调整计划:**M3 shell 集成 + M4 AI 托管/用量先行,M2 WSL/SSH 后置**——M3/M4 作用于本地终端,不依赖 M2)。**M3 已完成**:`tn-shell`(OSC 旁路解析)+ `tn-blocks`(block 状态机)+ 接线(`-EncodedCommand` 注入 pwsh 脚本、reader 旁路喂 `BlockModel`)+ `tn-ui::block_view`(Warp 式 block 底栏,Calm Glass、alt-screen 隐藏);`TN_AUTOQUIT` 验不回归。**M4 功能闭环**:`tn-ai` Claude **+ Codex** 用量解析 + 实时用量状态栏(**跟随焦点 pane 的 agent**,修掉"Codex 标签仍显示 Claude")+ agent 检测(启动意图 > 会话日志新鲜度)+ 命令面板(一键起 Claude/Codex、pwsh 托管、`×` 关闭杀进程)。**Calm Glass UI 大改(10 轮逐步还原 [mockup.png](design/mockup.html))**:SVG 线性图标系统(`tn-ui::assets`,内嵌图标 + 动态用量环)、自绘集成标题栏(品牌 mark + pill 标签 + 窗口控制,`appears_transparent` + `window_control_area`)、每 pane 头(agent 头含上下文环 / shell 头含 cwd)、文件浏览器侧栏(`explorer.rs`,git M/U 标记)、文件/Diff 查看器(`viewer.rs`,行号 + 语法着色 + `git diff`)、多段状态栏(分支/会话/各 agent ctx/文件·语言/编码/主题)、玻璃材质(acrylic 模糊 + 渐变 + rim/sheen/圆角/柔影,UI 无衬线 chrome / 等宽代码)、标签 agent 强调条 + cwd 徽章、Warp block 卡片(✓/✗ exit chip)。单测 71(tn-ai 8→15、tn-ui 13→16)。**剩**:窗口内肉眼微调 + 真机 Codex 用量复核 + agent body/Thinking(终端内容,非原生 chrome)。详见下方「## M3 + M4 计划与进度」。
- **M5 Quick Terminal = headless 闭环、待真机肉眼验证**(owner 选在 M2 之前先做):`tn-config::quick_terminal`(`[quick_terminal]` schema + 滑入几何 + 热键解析,纯函数 11+ 单测)+ `tn-ui::platform`(Windows-only:全局热键 `RegisterHotKey`+`GetMessageW` 线程、置顶/滑动/取焦 `SetWindowPos`、HWND 取自 gpui `Window`)+ `tn-ui::quick_terminal`(独立无边框置顶 `WindowKind::PopUp` 窗口,toggle/slide/autohide;**唤出弹启动器** Claude/Codex/pwsh[镜像命令面板]起普通 `TerminalView`[agent 自带头部+用量环],会话隐藏保留 + 右上切换 chip)+ `run()` 启动开隐藏窗口 + `App::spawn` 前台循环驱动 toggle、记录主窗口 id 仅它关闭才退出。`TN_AUTOQUIT` 验不回归 + 热键注册成功(headless 日志确认)。**剩(真机肉眼)**:外观/动画顺滑、取焦键入直达 agent、失焦不误触、多显示器/高 DPI 定位、首帧不空白。详见 [CHANGELOG.md](CHANGELOG.md) / [BLUEPRINT §8 M5](docs/BLUEPRINT.md)。
- **延后**:M1 精修(分隔线鼠标拖拽——`Node::resize` 权重逻辑已就绪 + 测试、drag-dock、M1.2b 自定义 `TerminalElement`)、M2 WSL/SSH(owner 定:WSL 可在此环境端到端验证,SSH 无远程主机故先写 russh 逻辑 + 单测、端到端 owner 自验)。
- 完整计划/路线图:[docs/BLUEPRINT.md](docs/BLUEPRINT.md)。UX(分屏/会话/AI 用量/查看器):[docs/UX-DESIGN.md](docs/UX-DESIGN.md)。从 Windows Terminal + Ghostty 提炼的设计要点:[docs/REFERENCES.md](docs/REFERENCES.md)。默认主题原型 [design/mockup.html](design/mockup.html)(**Calm Glass** 视觉系统已定稿——磨砂玻璃靠折射光+投影分层、**不做自发光/光污染**;设计令牌见 [UX-DESIGN §6.1](docs/UX-DESIGN.md)),主题 [config/themes/tn-dark.toml](config/themes/tn-dark.toml)。

## 工作区(crates)
```
tn-core    alacritty 包装:Term + VTE 解析 + TerminalSnapshot + Palette/RGB 解析 + row_runs
           + InputMode(模式位)+ 滚动 + 选区。无 GPUI、无 IO(headless)。
tn-pty     PtyBackend trait + LocalPty(ConPTY)。headless。(WSL/SSH = M2)
tn-config  配置 + 主题(TOML、路径、首次写默认、热重载源)。headless。M1.3 已实现。
           + quick_terminal.rs(M5):[quick_terminal] schema + 滑入几何(shown/hidden/frame_rect
           + ease_out_cubic,单位无关 f32)+ 热键串解析 parse_hotkey。纯函数,单测覆盖。
tn-shell   shell 集成:旁路 vte::Parser 把 OSC 133/633/7 解析成 BlockEvent + 集成脚本/nonce。headless。M3。
tn-blocks  Warp 式 block 状态机:BlockEvent(+行/时间)→ Block(命令/输出区间/退出码/时长)。headless。M3。
tn-ai      AI agent 用量 + 检测:claude.rs 解析 ~/.claude/projects/**/*.jsonl、codex.rs 解析
           $CODEX_HOME/sessions/**/rollout-*.jsonl(token_count + 真实 context window)→ token/
           上下文/估算花费 + pricing 表(AiUsage);detect.rs 按启动意图或日志新鲜度解析每个 cwd 的
           会话(resolve_session)。headless。M4。
tn-ui      GPUI 前端(唯一链接 gpui 的库):assets.rs(AssetSource:内嵌 SVG 线性图标 + 动态用量环
           svg)、input.rs(按键编码)、terminal_view.rs(单个 pane + LaunchSpec{agent} + per-pane 用量
           轮询 + UsageUpdated 事件 + agent/shell 头 + 用量环)、block_view.rs(Warp block 卡片)、
           explorer.rs(文件树侧栏,git 状态标记,OpenFile 事件)、viewer.rs(文件/Diff 查看器:行号 +
           语法着色 + git diff)、workspace.rs(标题栏 + 标签/n-ary 分屏 + 浏览器/查看器列 + 跟随焦点的
           多段状态栏 + 命令面板 + Calm Glass chrome)、platform.rs(M5,Windows-only:全局热键监听线程
           RegisterHotKey+GetMessageW、置顶/滑动/取焦 SetWindowPos、HWND 从 gpui Window 取;非 win stub)、
           quick_terminal.rs(M5:QuickTerminal 视图 = 独立无边框置顶 PopUp 窗口,toggle/slide/autohide;
           唤出弹启动器[Claude/Codex/pwsh,镜像命令面板]起普通 TerminalView,会话隐藏保留;换 agent = 退出
           当前会话(经 ProcessExited 回到启动器,ephemeral 启动省 -NoExit 使退出 agent 即退出 PTY)。
tn-app     二进制 `tn`:开窗 + 接线 + 崩溃保护 + 文件日志。
tn-cli     headless ConPTY 烟雾测试工具。
```
依赖方向(无环):`tn-blocks → tn-shell`(BlockEvent);`tn-ui → 全部 headless crate`。
铁律:`gpui` 只能出现在 `tn-ui`/`tn-app`。`tn-core`/`tn-pty`/`tn-config`/`tn-shell`/`tn-blocks`/`tn-ai` 必须能 headless 编译与测试。别把 alacritty 的类型泄漏出 `tn-core`(例如 `CellRun` 暴露 bold/italic 布尔,而非 `Flags`);`tn-shell` 用 `vte` crate 直连(不经 alacritty)。

## 构建 / 运行 / 测试
`cargo` 在 `%USERPROFILE%\.cargo\bin\cargo.exe`(不在 bash PATH 上——PowerShell 里用全路径,或在新开的 shell 里它在 PATH 上)。
```powershell
cargo build --workspace
cargo test  --workspace                        # 单测共 83(tn-core 10 / tn-config 26 / tn-ui 16 / tn-shell 11 / tn-blocks 5 / tn-ai 15)
cargo run   -p tn-cli                          # ConPTY 烟雾测试:起 shell、把网格渲染到 stdout、PASS/FAIL
cargo run   -p tn-app                          # 开终端窗口
$env:TN_AUTOQUIT="1"; cargo run -p tn-app      # headless 自测:首个 pane 跑命令、dump 网格、退出(exit 0)
$env:TN_DEMO="1";     cargo run -p tn-app      # 演示:窗口里自动步进(每态 5s)滚动/选区/分屏/改尺寸,然后退出
```
参考源码(仓库外浅克隆,供设计研读):`d:\coder\_refs\terminal`(Windows Terminal)、`d:\coder\_refs\ghostty`。

## M1 已实现
- **M1.1 tn-core 颜色**(`crates/tn-core/src/lib.rs`):`Rgb`、`Palette`(默认 Tn Dark,含 `selection_fg/bg`)、把 alacritty `Color`→RGB(ANSI16 + 256 立方/灰阶 + OSC 覆盖 + INVERSE)、`SnapshotCell.fg/bg`、`TerminalSnapshot.fg/bg`、`CellRun` + `row_runs()`(run 批处理)、`Terminal::set_palette()`。
- **M1.2 每格颜色渲染**(`terminal_view.rs`):用 `row_runs()` 渲染成 run 批处理的样式盒(每格 fg/bg + 粗体)。窗口内验证通过。
- **M1.6a 标签 + n-ary 分屏**(`workspace.rs`):`Workspace`(标签)+ `Node`(Leaf/Split n-ary 树)。同轴切分=对齐兄弟(真 n-ary),跨轴=嵌套;点击聚焦;焦点描边。
- **M1.3 tn-config + 主题接线**(`crates/tn-config/*`,被 `tn-ui` 消费):headless 配置 crate —— `color.rs`(`#RRGGBB` `Color` + serde)、`theme.rs`(`Theme`/`Ansi16`/`TerminalColors`/`UiColors`/`WindowChrome`/`AgentColors`;Tn Dark 经 `include_str!` 内嵌 `config/themes/tn-dark.toml`,整体回退)、`config.rs`(`Config`:`[general]/[font]/[appearance]` + `[[profiles]]/[[actions]]/[[keybindings]]`,字段全 `#[serde(default)]` 可继承)、`paths.rs`(`%APPDATA%\Tn`)、`lib.rs`(`load()`/`load_from()` → `Loaded`;首次写默认 `config.toml` + `themes/tn-dark.toml`;永不 panic——回退 + 记日志)。接线:`tn-ui` `palette_from(theme) → tn_core::Palette` + `set_palette`;字体 family/size/line-height + 工作区 chrome 颜色来自配置(免重编译)。`tn-config` 不依赖 `tn-core`(遵 BLUEPRINT §2.2 图),GPUI 层做桥。
- **M1.4 输入层重写**(`input.rs` + `tn-core` `InputMode`):`encode_key(&Keystroke, InputMode)` 照搬 Windows Terminal `_encodeRegular` —— 方向键/Home/End 按 DECCKM 选 CSI(`ESC[A`)或 SS3(`ESC OA`);带修饰 → `ESC[1;<mod><final>`(`<mod>=bits(SHIFT1/ALT2/CTRL4)+1`);F1–F4 SS3/CSI;F5–F20 DECFNK `ESC[<n>~`(跳号 LUT);Insert/Del/PgUp/PgDn `ESC[n~`;Backspace `0x7f`(Ctrl→`0x08`);Tab + Shift-Tab `ESC[Z`;Enter CR / LNM-CRLF / Ctrl-LF;`_makeCtrlChar`;Alt = ESC 前缀(CSI 键折进 `<mod>`)。模式位经 `tn_core::Terminal::input_mode()`(读 alacritty `Term::mode()`:DECCKM/DECKPAM/LNM/bracketed-paste/alt-screen)。`Ctrl+Shift+*`、`Ctrl+Tab` 保留(→ None);Win/super → None。
- **M1.5 重绘 push + vsync**(`terminal_view.rs`):8ms `dirty` 轮询 → push 模型——reader 线程往 `futures::channel::mpsc::unbounded` 发 wake(`dirty` 原子标志去重,通道至多 1 个待处理),前台 `cx.spawn` 任务 await 后 `cx.notify()`,GPUI 合并到 vsync 帧。空闲零唤醒。**DEC 2026 同步输出由 alacritty `vte` `Processor`(`StdSyncHandler`)内部缓冲处理**——网格仅在 BSU→ESU 完成或超时时变更,故 `snapshot()` 恒为整帧、无半更新撕裂。
- **M1.6b 分屏打磨**:
  - ✅ **滚动历史**(`tn_core::Terminal::scroll`/`scroll_to_bottom`/`with_scrollback` + `InputMode.alt_screen`;`on_scroll`:主屏滚历史、备用屏→方向键;输入回底;`general.scrollback_lines` 接线)。
  - ✅ **粘贴**(`Ctrl+Shift+V` / `Shift+Insert` → 剪贴板 → PTY,bracketed-paste 感知,CRLF→CR)。
  - ✅ **标题**(reader 捕获 `Event::Title`/`ResetTitle` → `TerminalView::title()`;标签显示焦点 pane 的 OSC 标题,否则 "Term N")。
  - ✅ **选择 + 复制**(tn-core `selection_start`/`update`/`clear_selection`/`selection_text`/`has_selection`,基于 alacritty `Selection`+`viewport_to_point`,`Palette.selection_fg/bg`,快照把选区色烘焙进选中格;tn-ui 用透明 GPUI `canvas` 每帧捕获内容屏幕 bounds 到 `content_bounds`,`cell_at` 像素→格,左键拖拽选择,`Ctrl+Shift+C` 经 `cx.write_to_clipboard` 复制)。
  - ✅ **多分屏尺寸修正**:各 pane 按自身 bounds(canvas 捕获)算行列,不再误用整窗;分屏外框 `p_1` + 终端底色填充;各 flex 层加 `min_w/min_h 0` + `overflow_hidden`(修复下分屏溢出窗口,见坑)。
  - ✅ **键盘改尺寸**(`Ctrl+Shift+方向键` → `GrowWidth`/`ShrinkWidth`/`GrowHeight`/`ShrinkHeight` → `Node::resize`:就近内层同轴 split 的 `weights`,夹 ≥0.1)。
- **配置与健壮性**:✅ 键位可配置(`workspace::bind_keys(cx, &Loaded)` 读 `[[keybindings]]`/`[[actions]]`,叠加在 `default_bindings()` 之上)。✅ 崩溃保护(`tn-app` panic hook → `tracing::error` 带位置,再调默认 hook)。✅ 文件日志(`%APPDATA%\Tn\logs\tn.log`,`tracing-appender` 非阻塞,与 stderr fmt 层分层)。✅ 配置热重载(`ReloadConfig` = `ctrl-shift-r`:`tn_config::load()` 重读、换 `Workspace.config`、对每个活动 pane 调 `TerminalView::apply_palette` 重应用调色板、刷新 chrome;字体/滚动历史仅新 pane 生效,遵 REFERENCES §7 diff-on-reload)。

测试(M1):tn-core 9 / tn-config 14 / tn-ui 13(输入编码 10 + 分屏 `Node::resize` 3),共 36。

## M3 + M4 计划与进度(当前焦点)
> owner 调整:**M3/M4 先于 M2**。两者作用于本地终端、不依赖 M2。此环境只能验证 **headless** 部分(OSC 解析、block 模型、用量 JSONL 解析);UI(block 卡片、命令面板、状态条、颜值)需在窗口里肉眼验证。沿用 M1 节奏:`main` 上 WIP,里程碑完成时单次提交。

**M3 — shell 集成 + Warp 式 block**
- ✅ `tn-shell`(`crates/tn-shell`):旁路 `vte::Parser` 只处理 `osc_dispatch`,识别 OSC 133(FTCS `A/B/C/D[;exit]`)、OSC 633(+`E` 命令行、`P;Cwd=`)、OSC 7(`file://`→cwd,含 `%XX` 解码与 Windows 盘符)→ `BlockEvent`。`Integration`:per-session nonce + pwsh 集成脚本(prompt 钩子发 D/A/B,PSReadLine Enter 发 C)+ `encoded_command()`(脚本 → UTF-16LE base64,经 `-EncodedCommand` 注入)。11 测试。
- ✅ `tn-blocks`(`crates/tn-blocks`):`BlockModel` 状态机 `Prompt→Input→Running→Finished`;`on_event(ev, line, at_ms)` 聚合成 `Block{command,cwd,prompt_line,output_start/end,exit,started/finished_at}`;中断块(无 `D`)在新 prompt 隐式收尾;`duration_ms`/`succeeded`/`is_running`/`last_finished`。5 测试。
- ✅ **接线**(`terminal_view.rs`):启动用 `-EncodedCommand` 注入 pwsh 脚本(无临时文件/不回显,`TN_NO_SHELL_INTEGRATION` 可关);reader 喂 Term 的同时旁路跑 `ShellParser` → 用 `tn_core::Terminal::cursor_abs_line()`(history+cursor 行,scrollback 锚点)+ 会话时钟喂共享 `Arc<Mutex<BlockModel>>`。`TN_AUTOQUIT` 验不回归(注入后网格仍正确渲染)。
- ✅ **`tn-ui::block_view`**:Warp 式命令 block 底栏(Calm Glass 半透磨砂、**无发光**)——状态条 成功绿/失败红/运行蓝、命令、时长、退出码、cwd,带**复制/重跑**;**alt-screen 自动隐藏**(正确性门槛);canvas 改为只测量 block 栏之上的终端区、网格按其自适配。**后置**:历史 block 的逐行覆盖 chrome(锚行随 reflow 重解析)+ block 栏外观窗口内肉眼复核 + pwsh `C` 钩子更多真机验证。

**M4 — 托管 Claude/Codex + 用量 + 命令面板 + 颜值**(功能闭环,均后于 M3)
- ✅ `tn-ai`(新 crate,headless):`AiUsage` 模型 + `pricing` 表 + **Claude UsageProvider**(`claude.rs`)——解析 `~/.claude/projects/<proj>/<session>.jsonl` 的 assistant `message.usage`,累计 token、取最后一轮总输入为上下文大小、按 pricing 估算等价花费、观测超 200K 时推断 1M 窗口。真实数据验证。
- ✅ **Codex UsageProvider**(`codex.rs`):解析 `$CODEX_HOME/sessions/**/rollout-*.jsonl` 的 `token_count` 事件——`total_token_usage`(累计,input 含 cached → 拆出 cache_read)+ `last_token_usage`(当前上下文)+ **日志里的真实 `model_context_window`**(不靠 pricing 表猜);`latest_codex_session_file` 按 `session_meta.cwd` 大小写/分隔符无关匹配、只读首行、newest-first 限量扫描。
- ✅ **agent 检测 + per-pane 用量跟随焦点**(`detect.rs` + `terminal_view.rs` + `workspace.rs`):`resolve_session(cwd, hint)` ——**启动意图**(`LaunchSpec.agent`,从 profile 命令/`agent` 字段识别)优先,否则按两家会话日志**新鲜度**择一(覆盖在 pwsh 里手敲 claude 的 dogfood)。每个 `TerminalView` 自轮询本 pane 的 agent 会话(mtime 守卫、空闲只 stat)、变更时 `cx.emit(UsageUpdated)`;`Workspace` `cx.subscribe` 仅在用量变化时重绘状态栏(不随终端帧)。**状态栏读焦点 pane 的 agent**(Claude 珊瑚 / Codex 青绿点 + 标签),Codex 无 pricing 时只显 token 不显花费——**修掉"Codex 标签仍显示 Claude"**。
- ✅ **命令面板 `Ctrl+Shift+P`**(`workspace.rs` overlay + `terminal_view::LaunchSpec`):列出 config `[[profiles]]` 可启动项,打字筛选 / ↑↓ / Enter / Esc / 点击;启动 = 新标签跑该 profile。**agent(claude/codex)托管在 pwsh 里**(`-NoExit -Command "& '…'"`)以解析 npm shim;spawn 失败回退 pwsh(不崩)。标签 `×` 关闭 + `LocalPty` Drop 杀子进程。
- ✅ **Calm Glass 颜值落地**(`lib.rs` + `workspace.rs` + `block_view.rs`):窗口材质按主题 `[ui.window].backdrop`(**默认 `Opaque`**——见下「真机打磨」为何不用 acrylic);chrome 用 alpha 半透玻璃(`cola()` + 令牌 RIM/SHEEN/INSET/HOVER)透出材质,圆角(窗口 16 / 面板 14 / 卡片 11)、玻璃边(rim,替代硬描边)、顶部镜面高光(sheen)、柔和投影(`soft_shadow`,gpui 0.2.2 无 `.shadow_*` → 经 `style().box_shadow`);焦点 pane 暖色细描边 + 浮起、标签 agent 身份点 + 玻璃 pill、命令面板浮层。**无发光**。
- ✅ **Calm Glass UI 全量构建(10 轮,逐步还原 [mockup.png](design/mockup.html))**:
  - **SVG 图标系统**(`assets.rs`):`Assets: AssetSource` 内嵌 ~16 个 Lucide 式线性图标(`icons/<name>.svg`)+ **动态用量环**(`ring/<pct>.svg`、`ring/track.svg` 运行时合成);经 `Application::with_assets` 注册。gpui `svg()` 把 SVG 渲染成 **alpha 掩膜**按 `text_color` 着色(故双色环 = 两层 svg 叠放)。
  - **自绘集成标题栏**(`lib.rs` `appears_transparent` + `workspace.rs`):品牌 mark(accent→violet 渐变)+ pill 标签(类型图标 + agent 强调顶条 + cwd 徽章)+ 窗口控制(min/max/close);拖动/控制经 `.window_control_area(WindowControlArea::Drag/Min/Max/Close)`(OS 走 NC 命中测试执行,无需 on_click)。
  - **每 pane 头**(`terminal_view.rs`):agent 头 = 头像 + 名称/型号 + **上下文环**(token/花费);shell 头 = 终端图标 + cwd + chip。UI 无衬线字体(`UI_SANS = "Segoe UI"`),终端/代码保持等宽。
  - **文件浏览器侧栏**(`explorer.rs`,`Ctrl+Shift+B`):读 cwd 树、展开/折叠(缓存重建)、文件图标、缩进、**git M/U/A/D/R 标记**(`git status --porcelain`)、点击文件发 `OpenFile` 事件。
  - **文件/Diff 查看器**(`viewer.rs`,`Ctrl+Shift+J`、点文件自动开):File 标签(行号 + 轻量语法着色:关键字/类型/串/注释/调用/数字)+ Diff 标签(`git diff` 解析 + 行号跟踪 + `+/-` 着色)。
  - **多段状态栏**:分支(`git branch --show-current`)· N sessions · 各 agent ctx%(跨 pane 聚合)· 文件·语言 · UTF-8 · 主题名;段间细分隔线。
  - **玻璃材质**:acrylic 模糊 + 渐变 + rim/sheen/圆角(16/14/11)/柔影(`box_shadow`),焦点 pane 暖描边浮起;标签 agent 强调条;Warp block 卡片(✓/✗/◆ exit chip)。**全程无发光**。
- ✅ **真机 dogfood 打磨(在 Windows 上肉眼跑出来的一批修复)**:
  - **窗口默认不透明**:gpui 0.2.2 的 `Blurred` 在 Windows = **acrylic(真·透背模糊)**,不是 `mica`(近乎不透明),亮壁纸会从边缘透进来。改成 `mica`/`solid` → `Opaque`(默认),只有显式 `acrylic` 才开透背;玻璃质感靠**内部面板层叠**而非窗口底材。根 `div` 不再 `rounded`(让 DWM 圆角,避免比 DWM 半径更圆而露出 acrylic 缝)。
  - **内层圆角对齐**:gpui `ContentMask` 只裁矩形(不按圆角裁子元素),故终端根 `rounded(13)` + agent 头 `rounded_t(13)` 自己圆角,圆角处不再露直角矩形。
  - **干净标签**:tab/header 不再用 OSC 标题(pwsh 把它设成 `…\powershell.exe`);`TerminalView::tab_label()` = `Claude`/`Codex`/`pwsh`(`shell_name_of(program)`),cwd 走徽章。
  - **普通 shell 不冒充 agent、不要多余头部**:只有 launch-intent 起的 agent 窗格轮询用量 + 有头部;普通 shell 无头部(cwd 由它自己的提示符显示一次,不重复)。
  - **agent 用量回退**:cwd 匹配不到时回退到"该 agent 最新会话"(`latest_codex_session_any`/`latest_claude_session_any`)——修掉 Codex 头部空(codex 默认在 `~` 跑、cwd 与 app 目录不符)。
  - **可见光标**:`tn-core` 快照加 `cursor`/`cursor_visible`;`terminal_view` 在光标格画圆角块(聚焦实心半透、失焦空心、app 隐藏/滚离时不画)。**常亮不闪**(闪烁需帧时钟,后置)。
  - 去掉标题栏下那条横贯全宽的 `border_b`(标签浮在玻璃上,靠留白分隔)。
- 🧭 **剩(肉眼/真机)**:窗口内颜值继续微调 + 标题栏拖动/控制按钮真机点验 + 光标闪烁/连续动画(需帧时钟,agent 思考态 PTY 不可观测,未伪造)+ per-pane cwd 经 OSC 7 实时跟随。键位:`Ctrl+Shift+B` 浏览器、`Ctrl+Shift+J` 查看器、`Ctrl+Shift+P` 命令面板、`Ctrl+Shift+T/D/E/W` 标签/分屏。

## M1 延后的精修项(交互式做,需肉眼/鼠标验证)
1. **分隔线鼠标拖拽** —— 权重数学 `Node::resize` 已实现 + 测试,只差把可拖拽的分隔把手接上去。
2. **拖拽停靠**(drag-dock)—— 拖到边=分屏、拖到中=标签组。
3. **M1.2b 自定义 `TerminalElement`** —— GPUI `Element`(layout/prepaint/paint)+ 字形图集 + typed-quad 批处理 + 光标/选区绘制(见 REFERENCES §2;GPUI 自管图集/DirectWrite,**不写裸 D3D**)。当前 div + run 批处理渲染器即 M1 版本。
- 其他后置:主题/配色导入(iTerm/WT/base16)、窗口 backdrop/opacity 应用(`[font].fallback`、`[appearance].opacity/backdrop` 已解析未应用)、OSC 8 超链接(→ M3)、kitty 键盘协议 / DECKPAM 小键盘 / win32-input-mode。

## 后续里程碑
执行顺序(owner 调整后):**M3 → M4 → M5 → M2**。M3 shell 集成 + block(✅)、M4 Claude/Codex 托管 + 用量 + 命令面板(✅ 功能闭环)、**M5 Quick Terminal(✅ headless 闭环,待真机肉眼)**,然后回头做 M2 WSL+SSH(owner 定:WSL 此环境可端到端验证、SSH 先写 russh 逻辑 + 单测、端到端自验)。文件/Diff 查看器 = M3/M4(`PaneContent = Session | Viewer`)。详见 [docs/BLUEPRINT.md](docs/BLUEPRINT.md) §8。

## 踩过的坑(hard-won)
- **ConPTY**:必须把 alacritty `Event::PtyWrite` 回复写回 PTY writer(回应启动时的 `ESC[6n` DSR),否则子进程卡住(只读到 4 字节)。ConPTY 无可靠 EOF——用 `try_wait` 轮询而非 `read()==0`。整个会话保活 portable-pty 的 `SlavePty`。
- **Windows 上 `claude`/`codex` 是无扩展名的 npm shim**:`CreateProcessW` 直接拉起会 `%1 不是有效的 Win32 应用程序`(os error 193)。要么解析 `.cmd`,要么**用 pwsh 托管**——`powershell -NoExit -Command "& 'codex'"`(pwsh 走 PATHEXT 解析 `codex.cmd`,且 agent 退出后回到 prompt)。Tn 用后者(`LaunchSpec::from_profile`)。
- **pane 构造在 GPUI 窗口回调里 = non-unwinding**:`LocalPty::spawn(...).expect()` 之类的 panic 会让整个进程 **abort**(`STATUS_STACK_BUFFER_OVERRUN` / 退出码 9 / 0xc0000409),而非被 panic hook 接住。所以 spawn 失败**必须优雅回退**(Tn 回退到 pwsh),绝不能 panic。
- **portable-pty 的 `LocalPty` Drop 不杀子进程**:不显式 kill,关 pane 只是移除视图,agent/shell 变**孤儿进程**继续跑。解法:给 `LocalPty` 加 `Drop` → `child.clone_killer().kill()`;关标签时 drop 视图链(`panes.remove` → drop `TerminalView` → drop `Arc<LocalPty>` → kill)。
- **gpui 0.2.2 on Windows**:可直接编译(DX11+DirectWrite,无需 Vulkan);首次编译数分钟。运行时 `HRESULT(0x887A002D)` 只是可选的 DXGI *debug* 层缺失——无害。
- **gpui 0.2.2 玻璃材质**:`WindowOptions.window_background = WindowBackgroundAppearance::Blurred` 在 Windows 上走 `ACCENT_ENABLE_ACRYLICBLURBEHIND`(acrylic 模糊背景)——**但只有当根 `div` 背景是半透(alpha<1)时才透得出**,否则不可见。本版没有 fluent 的 `.shadow_sm/_lg()`(zed 的 gpui 才有);要加投影得 `el.style().box_shadow = Some(vec![BoxShadow{ color: Hsla, offset, blur_radius, spread_radius }])`(见 `workspace::shadowed`)。`spread_radius` 可负(把投影收拢)。`rounded(px)` / `border_t(px)` 等"前缀带长度参数"形式存在(后缀如 `rounded_md()` 也在)。
- **gpui svg 图标**:`svg().path("icons/x.svg")` 经 `AssetSource` 加载,渲染成 **alpha 掩膜**按元素 `text_color` 着色(SVG 自身颜色被忽略——故描边要用不透明色让 usvg 产生 alpha;双色用两层 svg 叠放)。`AssetSource::load` 可**动态合成** SVG(如用量环 `ring/<pct>.svg` 按百分比算 `stroke-dashoffset`)。`Svg` 实现 `Styled`(`.w/.h/.text_color` 需 `use gpui::Styled` 或 prelude)。Raw-string 含 `"#RRGGBB` 会被 `r#"..."#` 的 `"#` 提前闭合——用 `r##"..."##`。
- **自绘标题栏**:`TitlebarOptions{ appears_transparent:true }` 隐藏 OS 标题栏;给元素打 `.window_control_area(WindowControlArea::Drag/Min/Max/Close)`,Windows 后端把这些区域映射成 NC 命中码(HTCAPTION/HTMINBUTTON/HTMAXBUTTON/HTCLOSE)由 **OS 直接执行**(拖动/最小化/最大化/关闭),**不要再加 on_click**(否则双触发)。命中测试取光标下"最先设了 control area 的 hitbox":故只把品牌/空白 spacer 设 `Drag`,标签/按钮不设(保持可点)。
- **(M5)外部 `SetWindowPos`/`ShowWindow` 不能在 gpui 更新回调里同步调** —— 它们会**同步**把 `WM_SIZE`/`WM_WINDOWPOSCHANGED` 派回 gpui 的窗口过程,后者 `self.state.borrow_mut()`;若此时你正处在 `window.update(...)` / `cx.observe_window_activation` / `Context` 回调里(已持该 `RefCell` 借用),就**重入借用** → gpui 把这次 resize **静默丢弃**("RefCell already borrowed"),窗口停在旧尺寸(实测:quick terminal 卡在占位 1265×743 而非全宽)。**解法**:所有窗口操作(`make_topmost`/`set_bounds`/`show`)一律丢进 `cx.spawn` 的前台任务里跑(借用已释放后),**绝不内联**;取焦放在 `render`(那里有 `&mut Window` 且不在 Win32 重入中)。诊断手段:reveal 时打 `scale=window.scale_factor()` + `GetMonitorInfoW` 工作区 + 算出的 shown 矩形对照——几何对(2560×693 物理、scale 1.5)却不生效 = 重入借用,非 DPI。
- **截图 GPUI 窗口**:`PrintWindow`(连 `PW_RENDERFULLCONTENT=2`)抓 DX11 swapchain 多为黑屏,不可靠;debug 构建还有个**控制台窗口**(类名像 `C`/`ConsoleWindowClass`)会被窗口枚举先抓到。要肉眼验证 chrome 就直接跑 release(`--release` 无控制台)在真机看。
- **gpui `Blurred` = acrylic ≠ Mica**:Windows 上 `WindowBackgroundAppearance::Blurred` 走 ACRYLIC(真·透背模糊),不是近乎不透明的 Mica(gpui 0.2.2 没暴露 `DWMWA_SYSTEMBACKDROP_TYPE`)。半透根 `div` 叠在 acrylic 上,亮壁纸会从边缘/圆角缝透进来,像"框外一层透明"。Tn 默认用 `Opaque`,玻璃感靠**内部面板层叠**;根 `div` 别再 `rounded`(否则比 DWM 的 ~8px 圆角更圆,露出 acrylic 缝)——让 DWM 圆角。
- **gpui `overflow_hidden` 是矩形裁剪**:`ContentMask` 只有 `bounds`(矩形),不按 `corner_radii` 裁子元素。圆角只影响**该元素自身**的背景/边框绘制。所以圆角卡里"有独立背景的子元素"(如 agent 头)会在圆角处露出直角——得让这些子元素**自己**也带圆角(`rounded`/`rounded_t`)。
- **pwsh 的 OSC 标题 = exe 全路径**:Windows PowerShell 启动把窗口标题设成 `…\WindowsPowerShell\v1.0\powershell.exe`,拿来当标签很丑。标签/头部用干净名(`Claude`/`Codex`/`pwsh`)+ OSC 7 的 cwd,别直接吃 OSC 0/2 标题。
- **agent 用量按 cwd 匹配会落空**:codex 默认在 `~` 跑(rollout `session_meta.cwd` ≠ Tn 的 app cwd),严格按 cwd 找会话会找不到 → 头部空。回退到"该 agent 最新会话"(`latest_*_session_any`)。普通 shell **不要**靠"会话新鲜度"反推 agent(你自己的 dev Claude 在同目录跑会误标)——只认 launch intent。
- Debug 构建保留控制台窗口;release 隐藏(tn-app 的 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`)。
- **GPUI 窗口类在 Windows = `Zed::Window`**(标题不是 "Tn");截图/注入按键工具要按窗口类枚举顶层窗口,而非 `FindWindow(title)`。
- **工作区快捷键用 `Ctrl+Shift+*`。** 中文/多布局 Windows 上,系统"切换键盘布局"热键也是 `Ctrl+Shift`,可能在按键到达 app 前吞掉它。已(经合成 SendInput)验证绑定 + 动作派发正确——每个动作都会触发。键位现已可配置(`bind_keys` 读配置),可改键避开;或禁用 Windows 的 Ctrl+Shift 布局切换热键(设置 → 时间和语言 → 输入 → 高级键盘设置 → 输入语言热键 → 把"在输入语言之间"/布局切换设为*未分配*)。`Ctrl+Tab`(下个标签)与鼠标点击聚焦不受影响。**M5 真机证实(更强结论)**:在 **`WindowKind::PopUp` 的 quick 窗口**里,`key_context`+`on_action` 绑定 **`Ctrl+Shift+L` 和 `Ctrl+Tab` 都无反应**(后者非 IME 键也不触发)——说明 PopUp 窗口里 keymap/action 派发**根本没到达 quick 窗口根**(原始 key_down 能到焦点终端、能打字,但 binding 不匹配)。主窗口(Normal)的 `Ctrl+Shift+*` 经 SendInput 验证可派发,故差异疑在 PopUp 窗口的 dispatch tree / 焦点链。**结论**:别依赖 PopUp 窗口内的 keymap 动作;quick 窗口的"换 agent"改用**退出当前会话→`ProcessExited`→回启动器**(走实体事件,非 keymap,可靠)。真正的 PopUp 内快捷键待后续排查 gpui dispatch。
- `gpui::Pixels.0` 私有 → 用 `f32::from(px)`。GPUI async:`cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| ...)`、`WeakEntity::update(cx, |v, cx| ...)`、`bg_executor.timer(d).await`、`cx.quit()`。`Context<T>` 解引用为 `App`(故可 `cx.read_from_clipboard()` 等)。
- **Flexbox `min-size: auto`(Taffy)**:内容过高/过宽的子项会把 flex item 撑过其 `relative()` 份额、溢出窗口——又因 `terminal_view` 读 `canvas` 捕获的 bounds 来定网格大小,被撑高的高度反馈回去使网格永不收敛。解法:每个 flex 层(body、分屏 container、pane wrap、终端根)都加 `min_w(px(0.))`/`min_h(px(0.))` + `overflow_hidden()`。这正是"下分屏溢出窗口"那个 bug。
- 提交结尾带:`Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`。行尾 LF(`.gitattributes`)。PowerShell 里避免在 `git commit -m @'...'@` here-string 中用 `"`(破坏解析);多行信息用 Bash 工具的 `git commit -F -` + 单引号 heredoc 更稳。
