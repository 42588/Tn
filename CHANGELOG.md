# Changelog — Tn 终端

本文件记录 Tn 各里程碑的变更,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。
版本对应开发蓝图([docs/BLUEPRINT.md](docs/BLUEPRINT.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 **Windows 优先、Rust、GPU 加速**的终端,为 vibe coding 设计:托管 Claude Code /
> Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite)·
> alacritty_terminal(VT 引擎)· portable-pty(ConPTY)· russh(SSH,M2)。许可证 GPL-3.0-or-later。

---

## [Unreleased] — M4 托管 AI + 用量 + 命令面板 + 颜值(功能闭环,待窗口内颜值微调)

### 新增 (Added) — AI 用量(headless)
- **`tn-ai`**(新 crate):`AiUsage` 模型 + `pricing` 表(各模型每 MTok 价 + 上下文窗口)+
  **Claude UsageProvider**(`claude.rs`)——解析 `~/.claude/projects/<proj>/<session>.jsonl` 的 assistant
  `message.usage`(`input/output/cache_creation/cache_read_tokens` + `model`),累计 token、
  取**最后一轮总输入**为当前上下文大小、按 pricing 估算**等价 API 花费**;模型 id 未标 `1m` 但
  观测上下文超 200K 时**推断为 1M 窗口**(真实 `claude-opus-4-7` 1M 会话即如此)。真实数据验证。
- **Codex UsageProvider**(`codex.rs`):解析 `$CODEX_HOME/sessions/**/rollout-*.jsonl` 的
  `token_count` 事件——`total_token_usage`(累计;Codex 的 `input_tokens` 含 `cached_input_tokens`,
  拆成未缓存 input + cache_read)、`last_token_usage`(当前轮 = 上下文大小)、以及**日志里记录的真实
  `model_context_window`**(直接用,不靠 pricing 表猜)。`latest_codex_session_file` 按
  `session_meta.cwd` 大小写/分隔符无关匹配、newest-first 只读首行、限量扫描。
- **agent 检测 / 会话解析**(`detect.rs`):`resolve_session(cwd, hint)`——**启动意图**(launch intent)
  优先,否则按两家会话日志的 mtime **新鲜度**择一;`agent_kind_for_command` 从命令串识别 claude/codex;
  `parse_session(kind, text)` 分派。

### 新增 (Added) — UI(需窗口内肉眼验证)
- **用量状态栏跟随焦点**(`terminal_view.rs` + `workspace.rs`):每个 `TerminalView` 持有 `agent` +
  `usage`,**自轮询本 pane 的 agent 会话日志**(mtime 守卫、空闲只 stat、`cx.emit(UsageUpdated)`);
  `Workspace` `cx.subscribe` **仅在用量变化时重绘状态栏**(不随终端帧)。状态栏读**焦点 pane** 的
  agent(Claude 珊瑚 / Codex 青绿点 + 标签)+ 型号 + 上下文条(绿→黄→红)+ % + token,Codex 无 pricing
  时只显 token 不显花费。
- **命令面板 `Ctrl+Shift+P`**(`workspace.rs` overlay + `terminal_view::LaunchSpec`):暗化 scrim +
  居中磨砂面板,列出 config `[[profiles]]` 中可启动项;打字筛选 / ↑↓ 选择 / Enter 启动 / Esc 关闭 /
  点击。启动 = 新标签跑该 profile。`LaunchSpec.agent` 从 profile 命令/`agent` 字段识别(per-pane 用量提示)。
- **一键托管 agent**:`claude`/`codex` 这类 Windows npm shim **托管在 pwsh 里**
  (`-NoExit -Command "& '…'"`)以走 PATHEXT 解析 `.cmd`,agent 退出后回到 prompt。
- **标签关闭**:每个标签加可点 `×`(`stop_propagation`,关而非激活);关闭即**杀子进程**
  (`LocalPty` 新增 `Drop` → `clone_killer().kill()`,杜绝孤儿 agent/shell)。
- **Calm Glass 颜值落地**(`lib.rs` + `workspace.rs` + `block_view.rs`):窗口按主题
  `[ui.window].backdrop` 设 `WindowBackgroundAppearance::Blurred`(Windows acrylic 模糊背景);chrome
  改 alpha 半透玻璃(`cola()` + 令牌 `RIM`/`SHEEN`/`INSET`/`HOVER`)让材质透出;圆角(窗口 16 /
  面板 14 / 卡片 11)、**玻璃边 rim 替代硬描边**、顶部镜面高光 sheen、柔和投影(`soft_shadow` →
  `style().box_shadow`);焦点 pane 暖色细描边 + 浮起、非焦点平铺;标签 = agent 身份点 + 玻璃 pill;
  命令面板浮层带投影。**全程无发光**(Calm Glass 原则)。

### 新增 (Added) — Calm Glass UI 全量构建(10 轮逐步还原 mockup,需窗口内肉眼验证)
- **SVG 图标系统**(`assets.rs`):`Assets: AssetSource` 内嵌 ~16 个 Lucide 式线性图标 +
  **运行时合成的用量环**(`ring/<pct>.svg` 按百分比算 dashoffset);`Application::with_assets` 注册。
  gpui `svg()` 渲染为 alpha 掩膜按 `text_color` 着色(双色环 = 两层叠放)。
- **自绘集成标题栏**(`appears_transparent` + `window_control_area`):品牌渐变 mark + pill 标签
  (类型图标 + agent 强调顶条 + cwd 徽章)+ 窗口控制(min/max/close,OS 经 NC 命中执行)。
- **每 pane 头**:agent 头(头像 + 名称/型号 + 上下文环 + token/花费);shell 头(终端图标 + cwd + chip)。
- **文件浏览器侧栏**(`explorer.rs`,`Ctrl+Shift+B`):cwd 树、展开/折叠、图标、缩进、
  **git M/U/A/D/R 标记**(`git status --porcelain`)、点文件发 `OpenFile`。
- **文件/Diff 查看器**(`viewer.rs`,`Ctrl+Shift+J`/点文件自动开):File(行号 + 语法着色)+
  Diff(`git diff` 解析 + 行号跟踪 + `+/-` 着色)。
- **多段状态栏**:分支 · sessions · 各 agent ctx% · 文件·语言 · UTF-8 · 主题。
- **字体分层**:UI 无衬线(Segoe UI)做 chrome、等宽做终端/代码。
- **Warp block 卡片**:浮起圆角卡 + accent 左条 + ✓/✗/◆ exit chip(图标)。

### 修复 (Fixed)
- **"Codex 标签仍显示 Claude"**:旧状态栏全局只读 Claude 用量。改为**状态栏跟随焦点 pane 的 agent**,不再串台。
- **拉起 agent 崩溃**:直接 `CreateProcessW` 拉无扩展名 npm shim 报 os error 193 → spawn `.expect()`
  在 GPUI 窗口回调(non-unwinding)里 panic → 整进程 abort。改为 pwsh 托管 + **spawn 失败优雅回退 pwsh**(不再崩)。

### 修复 (Fixed) — 真机 dogfood 打磨(Windows 上肉眼跑出来的)
- **框外一层透明**:gpui `Blurred` 在 Windows = acrylic(透背模糊)非 Mica,亮壁纸从边缘/圆角缝透进来。
  默认改 `Opaque`(仅显式 `acrylic` 才透背);根 `div` 去掉 `rounded`,让 DWM 圆角(避免比 DWM 半径更圆露缝)。
- **圆角处露直角矩形**:gpui `overflow_hidden` 只裁矩形(`ContentMask` 无圆角)。终端根 `rounded(13)` +
  agent 头 `rounded_t(13)` 各自圆角,整块成一个圆角卡。
- **标签/头部显示 `…\powershell.exe` 全路径**:不再吃 pwsh 的 OSC 标题;`tab_label()` = `Claude`/`Codex`/`pwsh`。
- **普通 shell 冒充 Claude**:只有 launch-intent 起的 agent 才轮询用量 + 标记 agent;普通 shell 不再因
  "同目录有新鲜 Claude 会话(其实是你自己的 dev 进程)"而误标。
- **普通 shell 头部多余**:cwd 已由 shell 提示符显示,去掉重复的 phead;agent 窗格保留头部(环/用量不重复)。
- **Codex 头部空("贴图")**:codex 默认在 `~` 跑、cwd 与 app 目录不符 → 按 cwd 找不到会话。回退到
  "该 agent 最新会话"(`latest_codex_session_any`/`latest_claude_session_any`),环/型号/花费填上。
- **看不到光标**:`tn-core` 快照加 `cursor`/`cursor_visible`;在光标格画圆角块(聚焦实心半透 / 失焦空心 /
  app 隐藏或滚离时不画)。常亮不闪。
- **标签栏下的横线**:去掉标题栏 `border_b`,标签浮在玻璃上靠留白分隔。

### 待做 (Pending)
- 窗口内颜值微调 + 真机 Codex 用量复核 + 标题栏拖动/控制按钮真机点验;连续动画(运行/Thinking,
  需帧时钟且 agent 思考态 PTY 不可观测,未伪造);per-pane cwd 用 OSC 7 实时跟随。

测试总计:**71**(tn-core 10 / tn-config 14 / tn-ui 16 / tn-shell 11 / tn-blocks 5 / tn-ai 15)。

---

## [Unreleased] — M3 shell 集成 + block(集成完成,待 UI 肉眼复核)

> 计划调整(owner):**M3 → M4 先行,M2 WSL/SSH 后置**(M3/M4 作用于本地终端,不依赖 M2)。

### 新增 (Added) — M3 头部基础(headless)
- **`tn-shell`**(新 crate):旁路 `vte::Parser`(只处理 `osc_dispatch`)在 PTY 字节上提取
  shell-集成序列 → `BlockEvent`。识别 **OSC 133**(FTCS `A/B/C/D[;exit]`)、**OSC 633**
  (+`E` 命令行、`P;Cwd=`)、**OSC 7**(`file://`→cwd,含 `%XX` 解码与 Windows 盘符)。
  `Integration`:per-session nonce + pwsh 集成脚本(prompt 钩子发 `D/A/B`、PSReadLine Enter
  发 `C`)+ `encoded_command()`(脚本 → UTF-16LE base64,经 `-EncodedCommand` 注入)。原始流照常喂
  `tn-core`,此为纯旁路。**11 测试**。
- **`tn-blocks`**(新 crate):`BlockModel` 状态机 `Prompt→Input→Running→Finished`;
  `on_event(event, line, at_ms)` 把事件 + 绝对行 + 时间戳聚合成 `Block`(命令、cwd、prompt/
  输出行区间、退出码、时长);中断块(无 `D`)在新 prompt 到来时隐式收尾;`duration_ms`/
  `succeeded`/`is_running`/`last_finished`。block 是对滚动区的语义索引(行锚点),非替换网格。**5 测试**。

### 新增 (Added) — M3 集成 + block 底栏 UI
- **接线**(`tn-ui::terminal_view`):启动用 `-EncodedCommand` 注入 pwsh 集成脚本(无临时文件、不回显
  输入行;`TN_NO_SHELL_INTEGRATION` 可关)。reader 线程在喂 `tn-core` 的同时旁路跑 `ShellParser`,
  把事件 + **当前光标绝对行**(新增 `tn_core::Terminal::cursor_abs_line`:history + cursor 行,作
  scrollback 锚点)+ 会话时钟喂给共享 `BlockModel`。纯旁路、不回归(`TN_AUTOQUIT` 注入后网格仍正确渲染)。
- **`tn-ui::block_view`**:Warp 式命令 block 底栏(Calm Glass 半透磨砂、**无发光**)——状态条
  运行蓝/成功绿/失败红、命令、时长、退出码、cwd,带**复制 / 重跑**动作;**alt-screen 自动隐藏**
  (全屏 app 占据视口 = 正确性门槛)。canvas 改为只测量 block 栏之上的终端区,网格按其自适配。

### 待做 (Pending) — M3 精修(后置,需窗口内肉眼验证)
- **历史 block 的逐行覆盖 chrome**:当前底栏只装饰"当前/最近"一个 block;围住滚动区里每个历史
  block 的覆盖层需 abs-line→视口映射 + 随 reflow 重解析,后置。
- block 底栏外观的窗口内肉眼复核;pwsh `C`(PSReadLine)钩子在更多 prompt 配置下的鲁棒性真机验证。

测试总计:**53**(tn-core 10 / tn-config 14 / tn-ui 13 / tn-shell 11 / tn-blocks 5)。

---

## [0.1.0] — M1 可日用的本地终端(已完成并提交 `59b8b0e`;尚未打 tag/发布)

**目标达成**:能当主力终端日用。Tab / 分屏 / 滚动 / 复制粘贴 / 配置 / 主题全可用,可自我 dogfood。

### 新增 (Added)

**配置与主题 — `tn-config`(M1.3)**
- 全新 headless 配置 crate,取代原 stub:
  - `color.rs` — `#RRGGBB` 的 `Color` 类型(serde 收发)。
  - `theme.rs` — 完整主题模型(`Theme` / `Ansi16` / `TerminalColors` / `UiColors` / `WindowChrome` /
    `AgentColors`);内置 **Tn Dark** 经 `include_str!` 嵌入 `config/themes/tn-dark.toml`(单一真源),
    主题为完整文档,缺失/损坏时整体回退内置。
  - `config.rs` — `Config`:`[general]` / `[font]` / `[appearance]` + `[[profiles]]` / `[[actions]]` /
    `[[keybindings]]`,字段全 `#[serde(default)]`,局部配置逐字段继承默认。
  - `paths.rs` — 配置根 `%APPDATA%\Tn`。
  - `load()` / `load_from()` → `Loaded`;**首次运行写默认** `config.toml` + `themes/tn-dark.toml`;
    永不 panic(任何读取失败回退默认并经 `tracing` 记录)。
- 接线 `tn-ui`:`palette_from(theme) → tn_core::Palette` + `Terminal::set_palette`;字体
  family/size/line-height、工作区 chrome 颜色均来自配置(免重编译)。
- 14 项单测。

**输入层重写 — Windows Terminal `_encodeRegular`(M1.4)**
- `crates/tn-ui/src/input.rs` `encode_key(&Keystroke, InputMode)`:
  方向键 / Home / End 按 DECCKM 选 CSI(`ESC[A`)或 SS3(`ESC OA`);带修饰 `ESC[1;<mod><final>`
  (`<mod> = bits(SHIFT1/ALT2/CTRL4)+1`);F1–F4 SS3/CSI;F5–F20 DECFNK `ESC[<n>~`(跳号 LUT);
  Insert/Del/PgUp/PgDn `ESC[n~`;Backspace `0x7f`(Ctrl→`0x08`);Tab + Shift-Tab `ESC[Z`;
  Enter CR / LNM-CRLF / Ctrl-LF;`_makeCtrlChar`;Alt = ESC 前缀。
- `tn_core::InputMode` + `Terminal::input_mode()` 从 alacritty `Term::mode()` 读 DECCKM / DECKPAM /
  LNM / bracketed-paste / alt-screen。
- 10 项编码测试 + 1 项模式测试。

**滚动历史 / 复制粘贴 / 标题(M1.6b)**
- **滚动**:`tn_core::Terminal::scroll` / `scroll_to_bottom` / `with_scrollback` + `InputMode.alt_screen`;
  鼠标滚轮在主屏滚动历史、在备用屏(vim/less)转为方向键;输入时自动回到底部;
  `general.scrollback_lines` 已接线。
- **复制粘贴**:tn-core 选区(`selection_start/update`、`clear_selection`、`selection_text`、
  `has_selection`,基于 alacritty `Selection`),`Palette.selection_fg/bg`,快照把选区颜色烘焙进选中格;
  tn-ui 用透明 GPUI `canvas` 每帧捕获内容屏幕 bounds → 像素→格映射,左键拖拽选择,
  `Ctrl+Shift+C` 复制、`Ctrl+Shift+V` / `Shift+Insert` 粘贴(bracketed-paste 感知,CRLF→CR)。
- **标题**:reader 捕获 `Event::Title` / `ResetTitle` → `TerminalView::title()`;标签显示焦点会话的 OSC 标题。
- **分屏尺寸调整(键盘)**:`Ctrl+Shift+方向键`(`GrowWidth`/`ShrinkWidth`/`GrowHeight`/`ShrinkHeight`)
  按 `Node::resize` 调整焦点分屏在最近同轴 split 里的 `weights`(就近内层、夹在 0.1 下限);3 项 tn-ui 单测。
  (鼠标拖拽分隔线后置。)

**配置驱动的键位 + 健壮性**
- 键位可配置:`workspace::bind_keys(cx, &Loaded)` 读 `[[keybindings]]` / `[[actions]]`,叠加在内置默认之上。
- **崩溃保护**:`tn-app` panic hook → `tracing::error`(带位置)。
- **文件日志**:`%APPDATA%\Tn\logs\tn.log`(`tracing-appender` 非阻塞,与 stderr 分层)。
- **配置热重载**:`Ctrl+Shift+R`(`ReloadConfig`)重读配置、对所有活动分屏重应用调色板、刷新 chrome;
  字体 / 滚动历史仅对新分屏生效(diff-on-reload)。

### 变更 (Changed)
- **重绘循环(M1.5)**:8ms `dirty` 轮询 → **push + vsync 合并**——reader 线程经
  `futures::channel::mpsc::unbounded` 发 wake(`dirty` 去重,通道至多 1 个待处理),前台
  `cx.spawn` 任务 `await` 后 `cx.notify()`,GPUI 合并到 vsync 帧。空闲零唤醒。
  DEC 2026 同步输出由 alacritty `vte` `Processor`(`StdSyncHandler`)内部缓冲,快照恒为整帧。
- **分屏尺寸修正**:每个分屏按自身内容 bounds(canvas 捕获)计算行列,不再误用整窗尺寸。
- 分屏外框增加 `p_1` 内边距 + 终端底色填充。

### 修复 (Fixed)
- **下分屏溢出窗口**:flex 子项默认 `min-size: auto` 会让网格过高的分屏胀破其 `relative` 份额、
  进而污染 canvas 捕获的 bounds(尺寸永不收敛)、最终撑出窗口。修复:在 body / 分屏容器 / 每个
  分屏 wrap / 终端根 上统一加 `min-w/min-h 0` + `overflow_hidden`,使各层被窗口而非内容约束。

### 后置 / 已知限制 (Deferred)
> 均为蓝图标注的**精修项**,且属鼠标 / 视觉交互,无法在无人值守环境验证;现有 div 渲染器已满足 M1。
- **分隔线鼠标拖拽**调整尺寸(键盘 `Ctrl+Shift+方向键` 调整已实现)、**拖拽停靠**(拖到边=分屏、拖到中=标签组)。
- **M1.2b 自定义 `TerminalElement`**(字形图集 + typed-quad 批处理 + 光标/选区绘制)——性能精修,
  现用 div + run 批处理渲染器已可用。
- 选区高亮 / 鼠标拖拽 / 热重载的**视觉效果需交互验证**(逻辑已 build + 单测覆盖)。
- 输入层后置:kitty 键盘协议、DECKPAM 小键盘编码、win32-input-mode。
- 主题 / 配色导入(iTerm / Windows Terminal / base16);OSC 8 超链接(→ M3)。

### 测试
- `tn-core` 9 项、`tn-config` 14 项、`tn-ui` 13 项(输入编码 10 + 分屏 `Node::resize` 3),共 36 项。
- `cargo run -p tn-cli` ConPTY 烟雾测试 PASS;`TN_AUTOQUIT=1 cargo run -p tn-app` GUI 自测渲染正确。

---

## [0.0.1] — M0 骨架(2026-05-26,commit `aa53a98`)

### 新增 (Added)
- Cargo 工作区 + 工具链固定(stable, `x86_64-pc-windows-msvc`)+ `cargo-deny` 许可证门。
- `tn-core` — alacritty 包装:`Term` + VTE `Processor` + `TerminalSnapshot`(3 测试)。
- `tn-pty` — `PtyBackend` trait + `LocalPty`(ConPTY,经 portable-pty);处理 DSR / `PtyWrite` 回写、
  `try_wait` 退出轮询、保活 slave 句柄。
- `tn-ui::TerminalView` — GPUI 窗口在 Windows DX11 + DirectWrite 跑通;渲染 + 键盘输入 + resize。
- `tn-cli` — headless ConPTY 烟雾测试。

### 退出标准达成
- 窗口内跑真实交互式 PowerShell,输出正确渲染,键盘输入生效,resize 生效。

---

## 路线图(后续)
- **M2** — WSL + 远程 Linux(SSH,russh)。
- **M3** — shell 集成(OSC 133/633)+ Warp 式 block UI。
- **M4** — 托管 Claude Code / Codex + AI 用量 + 命令面板 + 颜值打磨。
- **M5** — Quick Terminal(全局热键悬浮终端)。

详见 [docs/BLUEPRINT.md](docs/BLUEPRINT.md) §8。
