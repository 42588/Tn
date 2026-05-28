# CLAUDE.md — Tn 终端(Claude Code 项目指南)

Tn 是一个 **Windows 优先、Rust 编写、GPU 加速、为 vibe coding 打造的终端**:把 AI 编码 CLI
(Claude Code、Codex)当一等公民托管,提供出色 UX、灵活平铺、WSL + SSH。技术栈:**GPUI**
(Zed 的框架,Windows 上走 DirectX 11 + DirectWrite)· **alacritty_terminal**(VT 引擎)·
**portable-pty**(ConPTY/WSL)+ **russh**(SSH)。许可证:GPL-3.0-or-later。

## 协作约定(最优先,先读)
- **每次回复结构**:先**规划** → 再**划分任务** → 再**执行** → 再**测试** → 最后**输出**。
- **默认中文**回答。
- **文档同步(单一真源,不重复)**:每个有效优化/设计**完成即更新**——`docs/架构蓝图.md`(路线图 + 架构层"做了什么/为什么")· `docs/产品设计.md`(用户可见的 UX/交互/视觉决策)· 本 `CLAUDE.md`(操作指南 + 踩过的坑)。**三处各司其职、互相引用而非复制**;逐项明细只进 `待优化清单.md` / [CHANGELOG.md](CHANGELOG.md)。
- **节奏**:完成一轮任务后**自动提交**(提交规范见文末「约定」)。

## 图形界面 / 样式开发(**必走此路**)
任何 UI / 颜值改动都沿这条链走,别绕:
1. **设计真源 = [`design/mockup.html`](design/mockup.html)**(浏览器打开即标准)。要改样子,先改/看 mockup。**逐面板原型** = [`design/panels/`](design/panels/)(共享 [`design/calm-glass.css`](design/calm-glass.css),入口 `index.html`);**面板 ↔ 真实实现 清单** = [`docs/产品设计.md`](docs/产品设计.md) §6.4。
2. **查确切值 → [`docs/样式还原手册.md`](docs/样式还原手册.md)**:§1–§15 是 CSS→gpui 译法(**HOW**),**§16(自动生成)是每个组件的权威数值(**WHAT**)——照抄不估**。改了 mockup/主题后跑 `TN_GEN_SPEC=1 cargo test -p tn-ui spec_gen` 刷新 §16。
3. **写 gpui 必用 token、禁止硬编码主题色**:颜色一律 `col(ui.xxx)` / `cola(色, a)`(半透明 alpha 写小数,**别手算 hex**);圆角/间距用 `style.rs` 令牌或精确 `px()`;字重 `FontWeight(N.)` 精确(gpui 接受任意 f32)。
4. **三道机器守卫**(`cargo test -p tn-ui`,headless 即可跑):
   - `style::token_drift` —— mockup 令牌 ↔ 主题 / `style.rs` 常量一致(漂了即红、点名);
   - `style::no_hardcoded_theme_colors` —— UI 代码出现"RGB 命中主题色的裸 hex"即红、指出 file:line + 该用哪个 `col/cola`;
   - `style::spec_gen` —— 生成 / 校验 §16。
   - 合理例外(白叠加非整档 / fg-dim·faint 无 token / g1 渐变基色 / 透明 / scrim):裸 hex 但**带 `round(α×255)` 算式注释**。
5. **真机肉眼验**:`cargo run -p tn-app` 跑真实 app,与浏览器打开的 [`design/mockup.html`](design/mockup.html) / [`design/panels/`](design/panels/) 原型并排比(headless 环境用 `TN_AUTOQUIT` 只验网格收敛;颜值/动画须真机)。

> **现状(2026-05)**:全 UI 已**逐组件严格对齐 mockup**(tab / explorer / viewer / 各面板 g1 玻璃 / 终端正文落玻璃 / agent 头 / shell 头 / 状态栏 / block 卡 / 标题栏),**主题色硬编码清零**。新组件照此链做,三道守卫自动把关。
> 面板已补 **specular 柔光洗(`style::specular_top`,顶 36% 白 .04 渐隐折射)+ 浮起投影 + `.work` 11px 间距 + 分屏面板间 11px 间距**;**砍掉各窗格顶部 1px sheen 白线**(owner 取向同 tab,见坑)。**窗口保持 Opaque**——试过默认开 acrylic 透背被 owner 否(见坑)。

## 现状(先读)
- **里程碑 M0–M5 全部落地**(owner 执行顺序 `M0 → M1 → M3 → M4 → M5 → M2`)。可日用:本地 pwsh/cmd、
  WSL、托管 Claude/Codex、Warp 式 block、灵活分屏、文件浏览器/查看器、Quick Terminal 幽灵下拉终端、
  Calm Glass 颜值。单测共 **120** + 1 集成测试。变更见 [CHANGELOG.md](CHANGELOG.md),路线图见 [docs/架构蓝图.md](docs/架构蓝图.md) §8。
- **唯一未完成:M2 的 SSH ⏸ parked。** `tn-pty::SshBackend`(russh)已编译 + headless 单测过、代码原地保留,
  但 owner 决定**等有远程登录需求时再做端到端**(及 ssh-agent / known_hosts 校验 / 密码交互 / 重连)。
  **在此之前别主动推进 SSH。**
- **验证边界**:此环境只能验 **headless**(VT/OSC 解析、block 模型、用量 JSONL、几何/热键解析、WSL 枚举,
  以及经 `tn-cli`/`TN_AUTOQUIT` 的 ConPTY 跑通)。**颜值 / 动画 / 焦点 / 全局热键等窗口行为须真机肉眼验**——
  沿用既有节奏:`main` 上 WIP,里程碑完成时单次提交。
- 设计依据:UX(分屏/会话/AI 用量/查看器/Quick Terminal)见 [docs/产品设计.md](docs/产品设计.md);从 Windows
  Terminal + Ghostty 提炼的落地要点见 [docs/参考资料.md](docs/参考资料.md)。**Calm Glass** 视觉系统:磨砂玻璃靠
  折射光 + 投影分层、**不做自发光/光污染**;原型 [design/mockup.html](design/mockup.html),令牌见 [产品设计 §6.1](docs/产品设计.md),
  主题 [config/themes/tn-dark.toml](config/themes/tn-dark.toml)。

## 工作区(crates)
```
tn-core    alacritty 包装:Term + VTE 解析 + TerminalSnapshot(每格 fg/bg + cursor)+ Palette/RGB
           (ANSI16 + 256 + OSC + INVERSE)+ CellRun/row_runs(run 批处理)+ InputMode + 滚动 + 选区 + 子串搜索(search→SearchMatch,跨历史)+ URL 检测(snapshot.urls→UrlSpan)+ generation(渲染缓存代际)+ resize/resize_conpty(后者顶锚定长高,防 ConPTY repaint 吃滚动历史,见坑)。
tn-pty     PtyBackend trait + LocalPty(ConPTY,Drop 杀子进程)+ wsl.rs(parse_distros 解码
           `wsl --list --quiet` 的 UTF-16LE + list_distros)+ ssh.rs(SshBackend 实现 PtyBackend——
           专属 tokio 线程把 russh 的 async channel 桥成同步 Read/Write;⏸ parked)。WSL 复用 LocalPty 跑 wsl.exe。
tn-config  配置 + 主题(TOML schema、%APPDATA%\Tn、首次写默认、热重载源)+ quick_terminal.rs
           ([quick_terminal] schema + 滑入几何 shown/hidden/frame_rect + ease_out_cubic + 热键解析 parse_hotkey)。
tn-shell   shell 集成:旁路 vte::Parser 把 OSC 133/633/7 解析成 BlockEvent + pwsh 集成脚本/nonce。
tn-blocks  Warp 式 block 状态机:BlockEvent(+行/时间)→ Block(命令/输出区间/退出码/时长)。
tn-ai      AI 用量 + 检测:claude.rs / codex.rs 解析本地会话 JSONL → token/上下文/估算花费 + pricing 表;
           detect.rs resolve_session(启动意图 > 日志新鲜度)。
tn-ui      GPUI 前端(唯一链接 gpui 的库):style(共享 Calm Glass 令牌 + col/cola/soft_shadow/shadowed/icon,
           单一真源)· assets(内嵌 SVG 图标 + 动态用量环)· input(按键编码)·
           terminal_view/(文件夹模块:mod 渲染核心 + io 后台线程 + header agent 头 + launch LaunchSpec;可见光标 + generation 渲染缓存)· perf(PerfStats,TN_PERF 渲染日志)· block_view(block 卡片)·
           explorer(文件树侧栏)· viewer(文件/Diff 查看器)· workspace(标题栏 + 标签/n-ary 分屏 + 侧栏 +
           状态栏 + 命令面板 + Calm Glass chrome)· platform(Windows-only:全局热键 + 置顶/滑动 SetWindowPos)·
           quick_terminal(无边框置顶 PopUp 窗口 + 启动器)。
tn-app     二进制 `tn`:开窗 + 接线 + 崩溃保护 + 文件日志。
tn-cli     headless ConPTY 烟雾测试工具(可 `-- <program> [args]` 测任意子进程;`TN_RESIZE_EXP=1|locked|topgrow|interactive|prompt`
           = ConPTY resize 探针:naive 实测增高吃滚动历史、locked 验证行锁定(已撤销)、**topgrow 验证 resize_conpty 顶锚定长高零丢失**)+ `tests/conpty_pipeline.rs` 全链路集成测试。
```
依赖方向(无环):`tn-blocks → tn-shell → tn-core`;`tn-ui → 全部 headless crate`。
**铁律**:`gpui` 只能出现在 `tn-ui`/`tn-app`;其余 crate 必须能 **headless** 编译与测试。别把 alacritty 的类型
泄漏出 `tn-core`(如 `CellRun` 暴露 bold/italic 布尔而非 `Flags`);`tn-shell` 用 `vte` crate 直连(不经 alacritty)。

## 构建 / 运行 / 测试
`cargo` 在 `%USERPROFILE%\.cargo\bin\cargo.exe`(不在 bash PATH 上——PowerShell 里用全路径,或新开 shell)。
```powershell
cargo build --workspace
cargo test  --workspace --lib                  # 库单测(tn-core 25 / tn-config 27 / tn-ui 32 / tn-shell 11 / tn-blocks 5 / tn-ai 15 / tn-pty 8)+ `--test conpty_pipeline` 1 集成
#   注:`--lib` 只跑库单测;去掉 `--lib` 还会跑集成测试(`conpty_pipeline`,需起 ConPTY)。设计复刻 example 已移除(原型改对照 design/panels)
$env:TN_PERF="1"; cargo run -p tn-app          # 开渲染性能日志(tn::perf,~1/s:渲染率/缓存命中率/重建耗时)
cargo bench -p tn-core --bench engine          # criterion 热路径基准:advance(VT 解析)+ snapshot/row_runs(每帧渲染数据)
cargo run   -p tn-cli                          # ConPTY 烟雾测试:起 shell、把网格渲染到 stdout、PASS/FAIL
cargo run   -p tn-cli -- wsl.exe -d Ubuntu -- echo HELLO_TN_MARKER   # 验证 WSL 经 ConPTY 跑通
cargo run   -p tn-app                          # 开终端窗口
$env:TN_AUTOQUIT="1"; cargo run -p tn-app      # headless 自测:首个 pane 跑命令、dump 网格、退出(exit 0)
$env:TN_DEMO="1";     cargo run -p tn-app      # 演示:窗口里自动步进滚动/选区/分屏/改尺寸,然后退出
```
参考源码(仓库外浅克隆,供设计研读):`d:\coder\_refs\terminal`(Windows Terminal)、`d:\coder\_refs\ghostty`。

## 已实现(按领域)
> 不再按里程碑罗列;各里程碑的提交与逐项变更在 [CHANGELOG.md](CHANGELOG.md)。

- **终端内核 + 渲染**(`tn-core` + `terminal_view`):每格 fg/bg 颜色(`row_runs()` run 批处理成样式盒,div 渲染)·
  可见光标块(聚焦实心 + **~530ms 闪烁**[键入即点亮]/失焦空心稳定,滚离/alt 隐藏)· **响铃**(BEL→`TermEvent::Bell`→reader 置 `bell` 原子→前台 `handle_bell_if_rung`:视觉闪光 `spawn_bell_fade` ~180ms 渐隐覆盖层[默认开]+ 可选 `MessageBeep` 蜂鸣[默认关];`[appearance].visual_bell`/`audio_bell`)· **push + vsync 重绘**(reader→`mpsc::unbounded` wake,`dirty`
  原子去重,前台 `cx.spawn` await 后 `cx.notify()`;DEC 2026 同步输出由 vte `Processor` 内部缓冲 → 整帧快照无撕裂)·
  resize 联动(按 pane 自身 canvas bounds 算行列;ConPTY 精确跟随 alacritty——曾试行锁定防 resize 吃历史,因 ConPTY≠alacritty 致高频空白已撤销;现改 `resize_conpty` **顶锚定长高**,把被提升的历史 `scroll_up` 推回滚动区,使拖大窗格零丢失,见坑)·
  滚动历史(主屏滚历史 / alt 屏→方向键)+ **右缘滚动条**
  (snapshot 带 scroll_offset/history,thumb 按视口/总量,滚动时变亮)· 选择(左键拖拽 +
  **双击选词 / 三击选行**,`MouseDownEvent.click_count`→`SelectKind`)+ 复制(`Ctrl+Shift+C`)/ 粘贴
  (`Ctrl+Shift+V`、`Shift+Insert`,bracketed-paste 感知)。
- **输入**(`input.rs` + `tn-core::InputMode`):`encode_key` 照搬 Windows Terminal `_encodeRegular`——DECCKM CSI/SS3、
  `mod+1`、DECFNK F5–F20 跳号 LUT、Alt=ESC、`_makeCtrlChar`、Shift-Tab `ESC[Z`、Enter LNM。模式位读 alacritty
  `Term::mode()`。`Ctrl+Shift+*`、`Ctrl+Tab` 保留给 UI(返回 None);Win/super → None。
- **分屏 + 工作区**(`workspace.rs`):标签 + **n-ary 容器树**(`Node` Leaf/Split,同轴=对齐兄弟、跨轴=嵌套)·
  键盘切分/`Ctrl+Shift+方向键`改尺寸(`Node::resize` 调最近同轴 split 权重)/ 点击聚焦 + 焦点描边 · 关标签 `×`
  drop 视图链杀子进程。
- **配置 + 主题**(`tn-config`):`[general]/[font]/[appearance]`(含 `visual_bell` 默认开 / `audio_bell` 默认关)`/[quick_terminal]` + `[[profiles]]/[[actions]]/[[keybindings]]`,
  字段全 `#[serde(default)]` 可继承;`%APPDATA%\Tn`,首次写默认(`config.toml` + `themes/tn-dark.toml`,`include_str!`
  单一真源);永不 panic(回退 + 记日志)。`tn-ui` 经 `palette_from(theme)→tn_core::Palette` 接线;键位可配
  (`bind_keys` 叠加 `default_bindings`);热重载 `Ctrl+Shift+R`。
- **shell 集成 + block**(`tn-shell` + `tn-blocks` + `block_view`):旁路 `vte::Parser` 只处理 `osc_dispatch`,解析
  OSC 133(FTCS)/ 633(命令行 + cwd)/ 7(file://→cwd)→ `BlockEvent`;`BlockModel` 状态机
  `Prompt→Input→Running→Finished` → `Block`;pwsh 集成脚本(nonce)经 `-EncodedCommand` 注入(`TN_NO_SHELL_INTEGRATION`
  可关);reader 旁路喂 `BlockModel`(`cursor_abs_line()` 锚行)。**Warp block 底栏**:状态条蓝/绿/红 + 命令/时长/
  退出码/cwd + 复制/重跑,**alt-screen 自动隐藏**。
- **AI 用量 + agent 检测**(`tn-ai` + `terminal_view` + `workspace`):`claude.rs` 解析 `~/.claude/projects/**/*.jsonl`、
  `codex.rs` 解析 `$CODEX_HOME/sessions/**/rollout-*.jsonl`(`token_count` + 日志里真实 `model_context_window`)→
  token/上下文/估算花费;`detect.rs::resolve_session` **启动意图优先,否则按日志新鲜度**。每个 `TerminalView` 自轮询
  本 pane 用量(mtime 守卫)、`UsageUpdated` 事件驱动 `Workspace` 订阅重绘;**状态栏读焦点 pane 的 agent**
  (Claude 珊瑚 / Codex 青绿点 + 型号 + 上下文条 + token/花费)。
- **命令面板 + 托管**(`Ctrl+Shift+P`,`workspace.rs` + `LaunchSpec`):`discover_profiles` = config `[[profiles]]` +
  **自动发现的 WSL 发行版**(`wsl --list --quiet`,去重 + 滤 docker-desktop*);打字筛选 / ↑↓ / Enter / 点击启动 =
  新标签。**agent 托管在 pwsh 里**(`-NoExit -Command "& '…'"` 解析 npm shim);spawn 失败优雅回退 pwsh(不崩)。
- **Calm Glass UI**(`assets`/`lib`/`workspace`/`explorer`/`viewer`):SVG 线性图标 + 动态用量环 · 自绘集成标题栏
  (`appears_transparent` + `window_control_area`,品牌 mark + pill 标签 + 窗口控制)· 每 pane 头(agent 上下文环 /
  shell cwd)· **文件浏览器侧栏**(`Ctrl+Shift+B`,git M/U 标记)· **文件/Diff 查看器**(`Ctrl+Shift+J`,行号 + 语法
  着色 + `git diff`;侧栏带 `✕` 鼠标关闭)· 多段状态栏 · 玻璃材质(默认 `Opaque`、圆角 16/14/11、rim/sheen、
  `box_shadow` 柔影,**无发光**)。
- **Quick Terminal 幽灵下拉终端**(`platform` + `quick_terminal` + `tn-config::quick_terminal`):全局热键
  (默认 `Ctrl+Alt+Space`)经 `RegisterHotKey` 专属线程唤出**无边框置顶 `WindowKind::PopUp`** 窗口;从屏幕边缘
  **滑入**(纯几何 + 16ms 帧循环 `SetWindowPos`)+ **失焦自动隐藏**(`observe_window_activation`);**唤出弹启动器**
  选 Claude/Codex/pwsh/WSL,**退出当前会话即回启动器**(`ProcessExited` 事件,ephemeral 启动省 `-NoExit`)。
- **WSL**(`tn-pty::wsl` + `LaunchSpec`):`kind="wsl"` → `wsl.exe -d <distro>`(ConPTY 托管,无需专属 backend);
  启动器自动发现所有已装发行版。端到端验证:`tn-cli -- wsl.exe -d Ubuntu -- echo …` SMOKE PASS。
- **健壮性**:崩溃保护(panic hook → `tracing::error` 带位置)· 文件日志(`%APPDATA%\Tn\logs\tn.log`,tracing-appender)·
  pane 构造失败优雅回退 pwsh(GPUI 窗口回调 non-unwinding,见坑)· **reader 线程双层 `catch_unwind`**(外层防静默死、内层**持锁内**包 alacritty advance 防 Mutex 中毒连累前台 abort,见坑)。

**快捷键**:`Ctrl+Alt+Space` Quick Terminal(全局)· `Ctrl+Shift+P` 命令面板 · `Ctrl+Shift+T` 新标签 ·
`Ctrl+Shift+D`/`E` 右/下分屏 · `Ctrl+Shift+W` 关窗格 · `Ctrl+Shift+]`/`Ctrl+Tab` 下个窗格/标签 · `Ctrl+Shift+方向键`
改尺寸 · `Ctrl+Shift+B`/`J` 浏览器/查看器 · `Ctrl+Shift+C`/`V` 复制/粘贴 · `Ctrl+Shift+R` 热重载。

## 未做 / 后续(打磨项)
- **M2 SSH 恢复**(parked,见现状):真机端到端 + ssh-agent + known_hosts 校验 + 密码交互 + 重连 + `~/.ssh/config` 导入。
- **分屏交互**:✅ 分隔线鼠标拖拽(**commit-on-release**:拖动只移动一条 2px 预览线、释放才改权重 resize 一次——避免 ConPTY 每帧 resize 抖动;配合 `resize_conpty` 顶锚定长高,**拖大窗格不再丢滚动历史**;把手平时隐形、hover 微亮)· 🧭 拖拽停靠(drag-dock:拖到边=分屏、拖到中=标签组)。
- **自定义 `TerminalElement`**(M1.2b):GPUI `Element`(layout/prepaint/paint)+ 字形图集 + typed-quad 批处理 + 光标/选区绘制
  (见 参考资料 §2;GPUI 自管图集/DirectWrite,**不写裸 D3D**)。当前 div + run 批处理即现版本。
- **真机肉眼项**:颜值微调 · 标题栏拖动/控制点验 · 光标闪烁/连续动画(需帧时钟;agent 思考态 PTY 不可观测,不伪造)·
  per-pane cwd 经 OSC 7 实时跟随 · Quick Terminal 动画顺滑/多显示器·高 DPI 定位/首帧不空白。
- **其他后置**:主题/配色导入(iTerm/WT/base16)· 窗口 `[appearance].opacity/backdrop`、`[font].fallback`(已解析未应用)·
  OSC 8 超链接 · kitty 键盘协议 / DECKPAM 小键盘 / win32-input-mode · 历史 block 的逐行覆盖 chrome。

## 踩过的坑(hard-won)
**ConPTY / 进程**
- **ConPTY DSR**:必须把 alacritty `Event::PtyWrite` 回复写回 PTY writer(回应启动时的 `ESC[6n`),否则子进程卡住(只读 4 字节)。ConPTY 无可靠 EOF——用 `try_wait` 轮询而非 `read()==0`。整个会话保活 portable-pty 的 `SlavePty`。
- **Windows 上 `claude`/`codex` 是无扩展名 npm shim**:`CreateProcessW` 直接拉起会 os error 193。**用 pwsh 托管**——`powershell -NoExit -Command "& 'codex'"`(走 PATHEXT 解析 `.cmd`,agent 退出后回 prompt)。
- **pane 构造在 GPUI 窗口回调 = non-unwinding**:`spawn(...).expect()` panic 会让整进程 **abort**(0xc0000409),不被 panic hook 接住。所以 spawn 失败**必须优雅回退**(回退 pwsh),绝不能 panic。
- **portable-pty `LocalPty` Drop 不杀子进程**:给 `LocalPty` 加 `Drop → child.clone_killer().kill()`;关标签时 drop 视图链(`panes.remove` → drop `TerminalView` → drop `Arc<...>` → kill)。
- **主窗口 agent 窗格托管在 `pwsh -NoExit` 里,退出 agent 后没有 `ProcessExited`**:`-NoExit` 让 pwsh 在 claude/codex 退出后存活(窗格变回 shell),但 PTY 子进程(pwsh)没死 → `spawn_exit_watcher` 不触发,`self.agent` 永不清,头部+陈旧用量一直挂着。**解法**:持久 agent 的启动命令在 `& 'claude'` 后追加 `; $Host.UI.RawUI.WindowTitle = 'TN::agent-exited'`(`AGENT_EXIT_SENTINEL`)——agent 退出后 `-NoExit` 的 pwsh 执行它、发出 OSC 标题哨兵;reader 在字节流里**有序**看到它(无竞争,即便随后 prompt 又改标题)→ 置原子位 → 前台 `clear_agent_if_exited` 清 `agent`/`usage`、标签回落 shell 名。ephemeral(快捷终端)**不**追加(它走 `ProcessExited` 回启动器)。别用进程树轮询找 grandchild(脆弱);哨兵标题确定且复用既有 OSC 通路。
- **reader 线程里 alacritty panic 会经 Mutex 中毒拖垮整个 app**:reader 持 `terminal.lock()` 时 `t.advance` panic → guard 在 unwind 中 drop → **Mutex 中毒** → 前台 render(GPUI 非 unwinding 回调)下一次 `.lock().unwrap()` panic → **进程 abort**(0xc0000409)。解法:在 reader 里**于持锁作用域内**用 `catch_unwind(AssertUnwindSafe(|| t.advance(...)))` 包住——栈只回退到 catch、guard 随后**正常析构**故 Mutex 不中毒,前台照常 lock(渲染半残状态总好过 abort);panic 后 break 停该 reader(grid 半改、不再喂)。外层再包一层 `catch_unwind` 把任何 reader panic 记 `tracing::error`(否则线程静默死、pane 无声冻结)。**别**改成"前台所有 `.lock()` 都 `unwrap_or_else(into_inner)`"(22 处、易漏)——持锁内 catch 一处搞定。
- **russh(SSH)默认 crypto `aws-lc-rs` 本机编译失败**(要 NASM + cl.exe stdalign 探测):换 `russh = { default-features = false, features = ["ring", "flate2", "rsa"] }`(`ring` 自带预生成汇编,无需 NASM)。
- **russh 是 async,PtyBackend 同步**:SshBackend 用专属线程跑 current-thread tokio + `select!` 循环,`ChannelMsg::Data` 经 `std::mpsc` 喂同步 reader(recv 阻塞=自然 EOF)。`select!` 里只让 `channel.wait()`(&mut)出现在分支表达式;`data_bytes`/`window_change`(&self)放分支体里(wait 借用此时已释放)——照搬 russh 官方 example,否则 `channel` &mut+& 冲突。
- **ConPTY 行增高的 resize-repaint 既吃滚动历史、又顶飞提示符 → 但"行锁定"修法已撤销,改回 ConPTY 精确跟随 alacritty**。事实(仍成立):拖分隔线把 pane 拉大时 alacritty 把 `delta` 行从滚动历史**拉进可视区顶部**(reflow 单独不丢——见 `tn-core::resize_preserves_content_via_scrollback`),紧接着 ConPTY 的 resize-repaint 用空白/陈旧缓冲覆盖这些行 → 丢 `delta` 行(`TN_RESIZE_EXP=1` 实测:12→24 丢 LINE_18–29);且**任何 ConPTY 行增高都会把光标/内容重定位、把提示符顶出可视区留大片空白**(`TN_RESIZE_EXP=prompt TN_SPAWN_ROWS=34` 实测 34→120 后 fresh prompt 全空白)。
  - **曾试"普通 shell 行锁定"**(ConPTY 锁在 120 不增高)→ 反而引入更糟的**高频空白**:ConPTY(120)≠ alacritty(40),输出一多 alacritty 滚了而 ConPTY 没滚,**光标坐标错位**,普通命令(ls/dir 等)的提示符落位变形/空白。owner 实测确认后**决定撤销行锁定**。
  - **现行(双管):① ConPTY 行/列始终精确跟随 alacritty**(`render` 里 `new_size != self.size` 时同时 resize 两者)——**别再引入 ConPTY≠alacritty 的行锁定**,任何尺寸不匹配都会复活空白。**② 长高改走 `tn-core::resize_conpty` 而非 `resize`**:病根是"锚定方向相反"——alacritty 标准 resize **底锚定**长高(把 `delta` 行从历史**提升进视口顶部**),而 ConPTY 紧接着用**顶锚定** repaint 覆盖、且 alacritty 已把这些行移出历史环 → 永久丢失。`resize_conpty` 在 ConPTY repaint **之前**用 `grid_mut().scroll_up(0..rows, pulled)` 把被提升的 `pulled = min(长高前 history_size, delta)` 行**推回滚动区**,使长高变**顶锚定**(底部补空行、历史留环里不被 repaint 触及)→ 与 ConPTY repaint 对齐、**拖大窗格零丢失**(`TN_RESIZE_EXP=topgrow` 实测 grow/shrink/regrow 全 40/40;naive 对照 12→24 丢 12 行)。顶锚定的可见效果(拖大=内容留原位、下方开空白)恰合**原生 Windows 控制台**长高语义。注意:`resize_conpty` 只特判**长高**(缩小/列变/alt 屏 history=0 时即退化为普通 resize);分隔线拖动通常只动一轴(横分隔线=改行高、列不变 → 推回精确)。**已真机肉眼确认(2026-05-28,owner)**:拖大窗格不再吃历史,交互式 shell 重绘观感正常(`scroll_up` 不动 `cursor.point`,靠 ConPTY repaint 的绝对 CUP 自校正)。`TN_RESIZE_EXP=prompt`/`topgrow`(tn-cli)探针留作回归证据。

**gpui 渲染 / 材质**
- **gpui 0.2.2 on Windows**:可直接编译(DX11+DirectWrite,无需 Vulkan);首次编译数分钟。运行时 `HRESULT(0x887A002D)` 只是可选 DXGI *debug* 层缺失——无害。
- **gpui `Blurred` = acrylic ≠ Mica**:Windows 上走真·透背模糊(无 `DWMWA_SYSTEMBACKDROP_TYPE`),亮壁纸会从边缘/圆角缝透进来。Tn 默认 `Opaque`,玻璃感靠**内部面板层叠**;根 `div` 别 `rounded`(让 DWM 圆角,否则露 acrylic 缝)。要透背才显式用 `acrylic`(根 `div` 背景须半透 alpha<1 才透得出)。**(2026-05 试过默认开 acrylic + 接通 `window.opacity` 旋钮让面板透出 blurred 桌面 → owner 试用后否决:透明观感不喜欢、面板与边距磨砂浓度不一致显"透明矩形框"、大面积半透还色带。已回退保持 Opaque;`window_glass()` 的 Acrylic 分支 + `opacity` 旋钮代码留存备用。**根因:gpui 做不了*逐元素* `backdrop-filter` 模糊**,只有窗口级 acrylic,所以面板"真磨砂折射"无法复刻——玻璃感只能靠层叠 + specular + 投影。)**
- **gpui `overflow_hidden`/`ContentMask` 既裁矩形又裁投影**——两个后果都踩过:(a) **不按 `corner_radii` 裁子元素**,圆角卡里"有独立背景的子层"(agent 头 / `specular_top` 洗光 / 曾经的 1px sheen 线)会在圆角处**露直角或戳出白线**——让子层**自己**带 `rounded`/`rounded_t`(`specular_top` 已 `rounded_t(R_PANEL)`;1px sheen **硬线**正因这个在圆角戳成扎眼白线、被 owner 砍掉,改只留 specular 柔光);(b) **会裁掉子元素的 `box_shadow`**——面板的浮起投影画在框外,**任何祖先 `overflow_hidden` 都把它切掉**(所以面板外层 wrapper 不能叠 `overflow_hidden`,见下 Flexbox 坑)。
- **投影**:gpui 0.2.2 无 fluent `.shadow_*`;`el.style().box_shadow = Some(vec![BoxShadow{...}])`(见 `workspace::shadowed`,`spread_radius` 可负收拢)。`rounded(px)`/`border_t(px)` 等前缀带长度形式存在。
- **gpui svg 图标**:`svg().path("icons/x.svg")` 经 `AssetSource` 渲染成 **alpha 掩膜**按 `text_color` 着色(SVG 自身色被忽略;双色 = 两层叠放)。`AssetSource::load` 可**动态合成** SVG(用量环按 % 算 `stroke-dashoffset`)。Raw-string 含 `"#RRGGBB` 用 `r##"..."##`(`r#"..."#` 会被 `"#` 提前闭合)。
- **自绘标题栏**:`TitlebarOptions{ appears_transparent:true }` 隐藏 OS 标题栏;`.window_control_area(Drag/Min/Max/Close)` 由 **OS 直接执行**(NC 命中码),**别再加 on_click**(双触发)。命中测试取最先设 control area 的 hitbox:只把品牌/spacer 设 `Drag`,标签/按钮不设(保持可点)。
- **Flexbox `min-size: auto`(Taffy)**:内容过高的子项会撑过 `relative()` 份额、溢出窗口(又因 canvas bounds 反馈使网格不收敛)。**真正兜底的是 `min_w(px(0.))`/`min_h(px(0.))`**(覆盖 taffy 的 min-size:auto,让 flex 子项能缩到分配份额)+ **叶子面板自身的 `overflow_hidden`** 裁自己的内容。**别再在每层外层 wrapper 叠 `overflow_hidden`**——对裁内容是多余的(叶子已自裁),却会**吃掉面板的浮起投影**(见上 ContentMask 坑)。`body`(.work)同理**不裁**:投影靠 OS 窗口边界兜、并透过半透状态栏显出来。**(2026-05 先前为这个坑在 split 容器/子 wrap/三列 wrapper/body 全叠了 `overflow_hidden`,导致投影完全不显;去掉后投影才"浮起",headless `TN_AUTOQUIT` 验证 grid 仍收敛、溢出坑未复活。)**
- **分屏面板间距用「wrap 内侧 padding」,不用 flex `gap`/不用 wrap margin**:`render_node` 的 split 子 wrap 是 `w(relative(frac))` **精确平铺**(和占满容器);加 `gap` 或给 wrap 加 margin 会让"显式尺寸 + 间距"**溢出**、且**挪动分隔线 seam**。改在 wrap **内侧**加 padding(`when(i>0) pl/pt`、`when(i<last) pr/pb`,各 ~5.5px = 间距 11)把面板从 seam 往里收——padding 在 `relative(frac)` **之内** → 不溢出、wrap 仍精确平铺,故分隔线把手 `relative(cum)` 与拖拽权重算法**完全不动**。
- **大面积半透明渐变在 8-bit 会断层色带**(尤其大窗/最大化):mockup 靠 `feTurbulence` 噪点 dither + backdrop 模糊抹平,我们两样都没有 → **慎用整窗大渐变**(曾加整窗玻璃竖渐变,大窗色带明显已撤;`replica` 小窗看不出是因面积小)。面板 g1 / `specular_top` 的渐变面积小、色带轻可接受;真要彻底治,走"噪点叠层"近似(gpui 无 overlay 混合 + SVG 仅 alpha 掩膜,只能低透明度白噪点)。

**gpui 焦点 / keymap(真机证实)**
- **聚焦 `track_focus` 浮层要在 `render` 里、别在动作回调里**:动作(如 `toggle_palette`)里 `handle.focus(window)` 时浮层**本帧还没渲染**,焦点落不上 → 键漏给底层焦点元素(实测:命令面板 ↑↓/Enter/Esc 全跑到底层终端)。解法:置 `*_needs_focus` 标志,在 `render` 里聚焦(Quick Terminal 启动器本就这样,所以一直正常)。
- **`WindowKind::PopUp` 窗口里 keymap/action 不派发**:`key_context`+`on_action` 绑的 `Ctrl+Shift+L`/`Ctrl+Tab` 在 quick 窗口**都无反应**(原始 key_down 能到焦点终端、能打字,但 binding 不匹配);主窗口(Normal)经 SendInput 验证可派发。**结论**:别依赖 PopUp 窗口内的 keymap 动作;quick 窗口"换 agent"改用**退出会话→`ProcessExited`→回启动器**(走实体事件,可靠)。
- **(M5)外部 `SetWindowPos`/`ShowWindow` 不能在 gpui 更新回调里同步调**:它们**同步**把 `WM_SIZE` 派回 gpui 窗口过程并 `borrow_mut` 窗口状态;若此时正处在 `window.update` / `observe_window_activation` / `Context` 回调里(已持该 `RefCell`),就**重入借用** → resize 被静默丢弃("RefCell already borrowed"),窗口停在旧尺寸。**解法**:所有窗口操作(`make_topmost`/`set_bounds`/`show`)丢进 `cx.spawn` 前台任务(借用释放后),取焦放 `render`。诊断:reveal 时打 `scale` + `GetMonitorInfoW` 工作区 + 算出的 shown 矩形——几何对却不生效 = 重入借用,非 DPI。
- **`Ctrl+Shift+*` 在中文/多布局 Windows 可能被 IME/布局切换吞掉**(系统热键也是 `Ctrl+Shift`)。已(SendInput)验证绑定/派发正确,但真机键入可能不达 app:可改键避开,或把系统"输入语言热键"设未分配。`Ctrl+Tab` 与鼠标点击聚焦不受影响。侧栏面板(查看器/浏览器)因此**都给了鼠标 `✕` 关闭**,不靠 `Ctrl+Shift+J/B`。

**Windows / 构建 / 工具**
- **pwsh 的 OSC 标题 = exe 全路径**(`…\powershell.exe`):标签/头部用干净名(`Claude`/`Codex`/`pwsh` via `shell_name_of`)+ OSC 7 的 cwd,别直接吃 OSC 0/2 标题。
- **agent 用量按 cwd 匹配会落空**:codex 默认在 `~` 跑(rollout `session_meta.cwd` ≠ app cwd)→ 回退"该 agent 最新会话"(`latest_*_session_any`)。普通 shell **不要**靠"会话新鲜度"反推 agent(同目录的 dev Claude 会误标)——只认 launch intent。
- **`wsl --list --quiet` 输出是 UTF-16LE**(CR/LF,无 BOM);`list_distros` 带 `CREATE_NO_WINDOW` 不闪控制台。
- Debug 构建保留控制台窗口;release 隐藏(`#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`)。**GPUI 窗口类 = `Zed::Window`**(标题不是 "Tn");截图/注入工具按窗口类枚举,别 `FindWindow(title)`。`PrintWindow` 抓 DX11 swapchain 多为黑屏——肉眼验 chrome 直接跑 release。
- `gpui::Pixels.0` 私有 → `f32::from(px)`。GPUI async:`cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| …)`、`WeakEntity::update(cx, |v, cx| …)`、`bg_executor.timer(d).await`、`cx.quit()`;`Context<T>` 解引用为 `App`。

## 约定
- 提交结尾带:`Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`。行尾 LF(`.gitattributes` + `.editorconfig`,UTF-8)。
- 多行提交信息用 Bash 工具的 `git commit -F -` + 单引号 heredoc(PowerShell here-string 里的 `"` 会破坏解析)。
- 改依赖版本走根 `Cargo.toml` 的 `[workspace.dependencies]`。`main` 上 WIP,里程碑完成时单次提交。
