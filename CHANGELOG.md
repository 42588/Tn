# Changelog — Tn 终端

本文件记录 Tn 各里程碑的变更,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。
版本对应开发蓝图([docs/架构蓝图.md](docs/架构蓝图.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 **Windows 优先、Rust、GPU 加速**的终端,为 vibe coding 设计:托管 Claude Code /
> Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite)·
> alacritty_terminal(VT 引擎)· portable-pty(ConPTY)· russh(SSH,M2)。许可证 GPL-3.0-or-later。

**当前状态(2026-05):M0–M5 全部落地**(执行顺序 M0→M1→M3→M4→M5→M2)。M1 已 tag 为 `[0.1.0]`;
M3/M4/M5/M2-WSL 在 `main` 上以单次提交落地(下方各 `[Unreleased]` 段,**新里程碑在上**),尚未打新 tag。
**唯一未完成:M2 的 SSH**——已编译 + headless 单测,owner 决定暂停(parked),等有远程登录需求再做端到端。

---

## [Unreleased] — 原型同步轨道:主界面 1:1 复刻(2026-05-29)

> [`design/mockup.html`](design/mockup.html) 重设计后的主界面端口进 gpui,逐组件 1:1 还原。
> 数据流取向不变:活动栏数据来自 `git diff` + 已解析 JSONL,**不解析终端正文**(见 [CLAUDE.md](CLAUDE.md))。
> 四道守卫(`token_drift` / `roots_mirror` / `no_hardcoded` / `spec_gen`)全绿,33 lib 测试通过。

### 新增 (Added)
- **agent 活动栏(`.arail`)**:agent 面板正文右侧并排活动栏(`render_activity_rail`,[terminal_view/header.rs](crates/tn-ui/src/terminal_view/header.rs))
  ——运行状态行(状态点 + 运行中 · Update + 时长)+ 「本次改动」diff 卡(文件 + `+N/−N` + 迷你 diff)+ 提示。
  正文与栏同处 `.abody` flex 行(正文 `min_w(0)`);**仅 agent 面板有栏,shell 面板正文满宽**。
  **视觉外壳完成、当前为 mockup 占位示例内容**;真实 git/JSONL 数据接线为紧接的下一步。
- **品牌 caret `Tn ▾`**:标题栏品牌名后加 `chev-d`(muted @ .55);点击展开 app 菜单 popup 为后续项。

### 变更 (Changed)
- **explorer 改干净定宽面板**:侧栏 `214px → 224px` 定宽,**去掉外层「资源管理器」标签栏 + 关闭 ×**
  (1:1 贴 mockup `.sidebar`);开合走 `Ctrl+Shift+B`。viewer(legacy)的关闭栏保留至 Quick Look 端口。
- **玻璃面板保真打磨(真机肉眼对齐 mockup)**:
  - **g1 玻璃渐变**修正——原型 `--g1` 早先改冷加深(`rgba(34,42,70,.46)→rgba(16,20,38,.58)`)但 gpui 侧漏跟、
    各抄旧值致面板偏灰偏透;抽成单一真源 `G1_TOP/G1_BOT`(render_node + explorer 共用)+ **新增第 5 道
    `--g1` 守卫**(`token_drift` 解析 mockup 渐变两停)。
  - **面板浮起**:单层软投影 → mockup `.pane` **投影栈**(分层柔投影 + "切出背景"的边缘暗晕;`pane_shadows`)。
    边缘暗用 **3px 软暗晕**(非 mockup 的硬 1px 暗线)——硬线紧贴亮渐变描边会显「接缝」(原型靠
    backdrop-blur 抹平、我们没有),软晕过渡丝滑、无硬缝。
  - **冷能量渐变描边**(mockup `.pane::before`):gpui 边框单色无法渐变 → 用 **1px padding reveal**(`glass_pane`):
    外层冷白→accent 竖渐变底 + 1px 内边距,内容圆角内缩 1px、**fill 烤成不透明**(`pane_fill`,防渐变透底洗白)→
    1px 环即"顶冷白承光 / 底 accent 回光 / 侧渐变"的连续描边,聚焦更亮(**去掉旧暖橙焦点边**)。
  - **窗口底色** `chrome_bg #16161E → #0E0F19`(贴 mockup `.app` over desktop 合成色,gap 不洗白面板);
    **specular** 顶洗光对齐 `.035`/32%;**explorer** 目录名 fg-dim→fg(亮)、缩进引导竖线、缩进 16/树边距 6。

## [Unreleased] — M4 颜值打磨(面板逐组件对齐 mockup · 2026-05-28)

> 把面板从"数值对齐但发平"打磨到"磨砂玻璃 + 悬浮"。详见 [docs/产品设计.md](docs/产品设计.md) §6.1/§6.3、
> 经验坑见 [CLAUDE.md](CLAUDE.md)「踩过的坑」。设计真源仍是 [design/mockup.html](design/mockup.html),三道守卫把关。

### 变更 (Changed)
- **面板补回 mockup 玻璃层**:终端 pane / explorer / viewer 三处面板根加 **specular 柔光洗**
  (`style::specular_top`,顶 36% 白 .04→透明、顶角随面板圆角)+ **浮起投影**(全 pane 24/58/-36/.88,
  聚焦 30/64/-36/.9);`.work` 间距 `p_1/gap_2` → **pt5 px12 pb11 + gap 11**;**分屏面板之间补 11px 间距**
  (split 子 wrap 内侧 padding,不挪分隔线 seam)。
- **去掉面板外层 wrapper 的 `overflow_hidden`**(split 容器/子 wrap/三列/body):它会裁掉 `box_shadow`
  → 投影本来全被裁没;叶子面板自身 `overflow_hidden` + `min:0` 已兜内容,去掉外层裁剪后投影才"浮起"。
  headless `TN_AUTOQUIT` 验证 grid 仍收敛、taffy 溢出坑未复活。
- **窗口底材改回纯色**:去掉整窗半透玻璃竖渐变层——大窗下断层色带明显(mockup 靠噪点+模糊抹平,我们没有)。

### 移除 (Removed)
- **各窗格顶部的 1px sheen 白线**:`overflow_hidden` 不跟圆角 → 这条硬线在圆角戳出来扎眼(owner 取向同 tab)。
  改只留 specular 柔光;`style::sheen_line` 助手删除(`SHEEN` 令牌仍用于状态栏/命令面板)。

### 试验后回退 (Reverted)
- **窗口级 acrylic 真模糊**:曾默认开 acrylic(`Blurred`)+ 接通 `window.opacity` 旋钮让面板透出 blurred 桌面
  →**owner 试用后否决**(透明观感不喜欢、面板比磨砂边距更实显"透明矩形框"、大面积半透还色带),
  **回退保持 `Opaque`**。`window_glass()` 的 Acrylic 分支 + `opacity` 旋钮代码留存备用。
  根因:gpui 做不了*逐元素* `backdrop-filter` 模糊。

## [Unreleased] — M2 WSL + 远程 Linux(SSH)

> owner 执行顺序:M3 → M4 → M5 → **M2**。**WSL ✅ 完成**(端到端验证 + 自动发现发行版)。
> **SSH 暂停**:已落地编译 + headless 单测,但 owner 决定**等有远程登录需求时再继续**(代码原地保留)。

### 新增 (Added) — WSL(已端到端验证)
- **`tn-pty::wsl`**:`parse_distros`(解码 `wsl --list --quiet` 的 **UTF-16LE** 输出 → 发行版名,
  剥 BOM/空行/NUL,纯函数 3 单测)+ `list_distros()`(shell out 到 `wsl.exe`,输出捕获、无控制台)。
- **`LaunchSpec::from_profile` 支持 `kind = "wsl"`**:`wsl.exe -d <distro>`(distro 省略 = 默认发行版),
  无 pwsh 集成(发行版跑 bash/zsh)。WSL 会话复用现有 `LocalPty`——ConPTY 托管 `wsl.exe` 如同普通程序,
  **不需要新 PtyBackend**。2 单测。
- **命令面板 + Quick Terminal 启动器纳入 WSL profile**(`is_launchable`:命令型 或 带 distro 的 wsl)。
- **自动发现所有已装发行版**(`discover_profiles`):启动器 = config `[[profiles]]` + `wsl --list --quiet`
  枚举到的发行版(去重 config 已有的、滤掉 Docker 内部的 `docker-desktop*`),给个柔蓝点;无需为每个
  发行版手写 profile(默认配置只有一个 Ubuntu,之前就只显示一个——这是修复)。`wsl.exe` 带
  `CREATE_NO_WINDOW`,不闪控制台。
- **`tn-cli` 支持自定义子进程**:`cargo run -p tn-cli -- <program> [args...]`(默认仍是 cmd echo)。
  用它端到端验证 WSL:`tn-cli -- wsl.exe -d Ubuntu -- echo HELLO_TN_MARKER` → **SMOKE PASS**
  (ConPTY 托管 wsl、输出回灌引擎、网格正确)。

### 新增 (Added) — SSH(russh;编译通过 + headless 单测,端到端 owner 自验)
- **`tn-pty::SshBackend`**(实现 `PtyBackend`):专属线程跑 current-thread tokio,
  `client::connect` → 认证 → `channel_open_session` → `request_pty` → `request_shell`,然后一个 `select!`
  循环把 **async channel 桥成同步 Read/Write**——远程 `ChannelMsg::Data` 经 `std::mpsc` 喂同步 reader
  (recv 阻塞 = 自然 EOF),同步 writer 把输入推上 tokio channel → `channel.data_bytes`,`resize` →
  `window_change`,`ExitStatus`/Close → `Mutex<Option<i32>>`+`Condvar`(wait/try_wait),drop 即断开。
  keepalive 30s(空闲不掉线)。`SshConfig`(host[:port] / user / 自动找 `~/.ssh/id_*`)5 单测。
- **`TerminalView` 抽象到 `Box<dyn PtyBackend>`**(原硬编码 `LocalPty`):`LaunchSpec` 加 `ssh: Option<SshConfig>`;
  `from_profile` 支持 `kind="ssh"`(host+user → `SshConfig`);命令面板/启动器纳入 SSH profile(`is_launchable`)。
  本地 pwsh 路径 `TN_AUTOQUIT` 验不回归。
- **russh 用 `ring` crypto 后端**(非默认 `aws-lc-rs`——后者要 NASM + cl.exe stdalign 探测,本地不一定有)。
- 默认 `config.toml` 加**注释版 SSH 示例 profile**。

### 修复 (Fixed) — 真机 dogfood
- **命令面板(Ctrl+Shift+P)键盘导航失灵**(↑↓/Enter/Esc 漏到底层终端):`toggle_palette` 在动作里
  `palette_focus.focus()`——但那时浮层还没渲染、焦点没落上,键就被底层 `TerminalView` 接走了。改为在
  `render` 里聚焦(浮层的 `track_focus` 元素此帧已存在),与 Quick Terminal 启动器同一套(那个本就在
  render 聚焦,所以一直正常)。

### 暂停 (Parked) — SSH(owner 决定:等有远程登录需求时再继续)
- SSH 后端代码已落地(编译 + headless 单测过)并**原地保留**,但**端到端未验证、暂不继续打磨**。
  恢复时要做:用真实主机端到端验;**ssh-agent**(`russh::keys::agent`,Windows OpenSSH/Pageant)+
  **known_hosts 校验**(当前 `check_server_key` 接受任意主机密钥——真用前必须接入)+ 密码交互输入 +
  断连重连 UX + `~/.ssh/config` 导入。

---

## [Unreleased] — M5 Quick Terminal(幽灵下拉终端,headless 闭环 + 待真机肉眼验证)

> Quake/Guake 式悬浮终端:任意 app 里按全局热键唤出一个置顶悬浮终端(直接跟 Claude/Codex 对话),
> 边缘滑入,失焦自动隐藏。**headless 部分**(配置 schema、滑入几何、热键解析、热键注册)已在此环境验证;
> **窗口外观 / 滑动动画 / 失焦隐藏 / 取焦输入** = 真机肉眼验证(沿用 M3/M4 节奏)。

### 新增 (Added) — headless(可单测/已验证)
- **`tn-config::quick_terminal`**(新模块,纯函数 + schema):`[quick_terminal]` 配置
  (`enabled / position(top·bottom·left·right·center) / height_percent / width_percent /
  animation_ms / autohide / hotkey / profile`,字段全 `#[serde(default)]` 可继承)+ **滑入几何**
  (`shown_rect/hidden_rect/frame_rect(work_area)` 按停靠边算屏上/屏外矩形 + `ease_out_cubic` 缓动,
  单位无关 f32,平台层换算物理像素)+ **热键串解析** `parse_hotkey("ctrl+alt+space")→HotkeySpec`
  (`+` 分隔、大小写/别名无关、要求至少一个 ctrl/alt/win)。12 单测(总 83)。
- **默认 `config.toml`** 加 `[quick_terminal]` 段(带注释,默认 `ctrl+alt+space` / `top` / 45%)。

### 新增 (Added) — GPUI/Win32 接线(编译通过,行为待真机验证)
- **`tn-ui::platform`**(Windows-only,非 Windows 有 no-op stub):**全局热键监听线程**
  (`RegisterHotKey(None,…)` + `GetMessageW` 私有消息循环 → `WM_HOTKEY` 经 channel 通知前台;
  VK/MOD 映射含字母/数字/F1–F24/space/grave 等)+ **置顶/滑动/取焦**(经 raw HWND:`WS_EX_TOPMOST` +
  `SetWindowPos` 物理像素移动 + `ShowWindow`/`SetForegroundWindow` + `GetMonitorInfoW` 工作区)。
  HWND 从 gpui `Window` 的 `HasWindowHandle`(UFCS 绕开同名 inherent 方法)取。
- **`tn-ui::quick_terminal`**(`QuickTerminal` GPUI 视图):独立**无边框置顶 `WindowKind::PopUp` 窗口**;
  `toggle/reveal/hide` + **滑入动画**(前台执行器 16ms 帧循环驱动 `SetWindowPos`,`anim_token` 反向 toggle
  取消在途动画,故 `SetWindowPos` 恒在窗口自己的线程、无跨线程封送)+ **失焦自动隐藏**
  (`cx.observe_window_activation`)。**唤出时弹启动器**(镜像命令面板):无会话时列出可启动 `[[profiles]]`
  (Claude/Codex/pwsh,↑↓/Enter/Esc/点击),选中即起一个普通 `TerminalView`(agent 自带头部 + 用量环);
  会话隐藏后保留;**换 agent = 退出当前会话**(它经 `ProcessExited` 回到启动器,再选别的),旧会话 drop 即杀。
  Calm Glass 暖描边。
- **退出 agent/shell 自动回启动器**:`TerminalView` 加 `spawn_exit_watcher`(400ms `try_wait` 轮询 →
  `ProcessExited` 事件;ConPTY 不可靠 EOF,故用 try_wait)。quick 窗口用 `LaunchSpec::from_profile_ephemeral`
  起 agent(**省掉 `-NoExit`**,退出 claude 即退出 PTY)→ 订阅 `ProcessExited` 回到启动器(`exit` 退出 pwsh
  同理)。主窗口不订阅、无影响。
- **`tn-ui::run`**:启动时开**隐藏**的 quick 窗口(`show:false`,shell 预启动)+ 起热键线程 +
  `App::spawn` 前台循环把热键 → `qt_window.update(|qt,window,cx| qt.toggle(…))`;`TN_AUTOQUIT` 下跳过
  (避免第二个自测 `TerminalView` 争抢 quit)。热键不可解析/`enabled=false` 优雅跳过(记日志,不崩)。

### 修复 (Fixed) — 真机 dogfood
- **窗口尺寸不生效(卡在占位尺寸)**:外部 `SetWindowPos`/`ShowWindow` 会**同步**把 `WM_SIZE` 派回
  gpui 窗口过程并 `borrow_mut` 窗口状态;原先在 `toggle`(处于 `window.update` 借用中)里**内联**调用 →
  **重入借用**被 gpui 静默丢弃("RefCell already borrowed"),窗口停在占位尺寸(几何其实算对了:
  2560×693 物理、scale 1.5)。改为把**所有**窗口操作(topmost/set_bounds/show)丢进 `cx.spawn` 前台任务
  (借用释放后跑)、取焦移到 `render`;autohide 隐藏也走同一延迟路径。详见 CLAUDE.md「踩过的坑」。
- **关主窗口后进程残留**:quick 窗口是**常开**的(隐藏≠关闭),故 `on_window_closed` 里的
  `windows().is_empty()` 永不为真 → 关掉主窗口后 `tn.exe` 带着预启动 shell 在后台残留。改为**记录主窗口
  id、仅当它从 `cx.windows()` 消失时 `cx.quit()`**(退出会一并销毁 quick 窗口 + 杀其 shell)。
  **隐藏语义**(回答常见疑问):点别处/再按热键**只隐藏不杀进程**——会话(历史/对话/cwd)保留,
  下次唤出即原会话;子进程只在 **app 退出**时经 `LocalPty::Drop` 终止。
- **右上"切换"chip 与 agent 头部重叠**(真机发现):agent 会话的 `TerminalView` 头部本就占满顶栏
  (左名字、右用量环),浮动 chip 压在用量环上、还重复显示 agent 名 → 看着乱。**移除浮动 chip**,改用
**改用"退出当前会话即回启动器"**作为换 agent 的路径(见上 `ProcessExited`)。**注**:曾尝试在 quick 窗口里
  绑 `Ctrl+Shift+L` / `Ctrl+Tab`(`key_context`+`on_action`,镜像主窗口)——但**在 PopUp 窗口里两个都无反应**
  (动作派发未到达 quick 窗口根;非 IME,因 `Ctrl+Tab` 也不触发)。既然"退出会话回启动器"已能换 agent、且与单会话
  模型一致,遂**移除该 in-window 切换键**,不留无效提示。真正的窗口内切换键留待排查 gpui PopUp 的 keymap 派发。
- **退出 claude 后界面没回到普通 shell/启动器**(真机发现):agent 原以 `-NoExit` 托管,退出 claude 只回到
  一个挂着**陈旧 Claude 头部**的 pwsh 提示符。改为 ephemeral 启动 + `ProcessExited` 监听(见上),退出即回启动器。
- **主窗口文件/Diff 查看器打开后关不掉**(真机发现):查看器靠点文件(explorer `OpenFile`)打开、只能用
  `Ctrl+Shift+J` 关——而 `Ctrl+Shift` 在中文 Windows 被 IME 吞,面板就**卡死打开、无鼠标关闭路径**。给查看器
  与浏览器侧栏各加一个**鼠标 `✕` 关闭按钮**(右上角,`absolute`),不依赖键盘。同根因(`Ctrl+Shift` 被吞)。

### 待办 (TODO) — 真机肉眼验证
- 滑动动画顺滑度;取焦后键入直达 agent;失焦自动隐藏不误触;多显示器/高 DPI 定位;首帧不空白。

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

详见 [docs/架构蓝图.md](docs/架构蓝图.md) §8。
