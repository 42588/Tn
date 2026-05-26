# Tn 技术文档与开发蓝图

> 本文是 Tn 终端的**工程参考手册 + 开发蓝图**。读者对象:接手或参与 Tn 开发的工程师。
> 全文用「✅ 现状」标记**已实现**的部分,用「🧭 规划」标记**蓝图设计**(尚未落地)。
> 决策背景与多方案权衡见计划文件:`~/.claude/plans/windows-vibe-coding-claude-codex-rust-w-hazy-token.md`。
> **从 Windows Terminal 与 Ghostty 源码精读提炼的可落地设计要点见 [REFERENCES.md](REFERENCES.md)**(输入编码、GPU 渲染、OSC 133/shell 集成、ConPTY 纪律、IO 合并、配置模型)。
> **分屏 / 多会话 / 一键 AI / 用量 / 颜值 的完整体验设计见 [UX-DESIGN.md](UX-DESIGN.md)**(灵活平铺 n-ary 容器 + 拖拽停靠、会话启动器、AI 上下文与 token 用量、视觉设计语言)。

---

## 0. 一句话定位

Tn 是一个 **Windows 优先、Rust 编写、GPU 加速**的终端模拟器,主打**性能与颜值**,围绕「**vibe coding**」体验设计:把 Claude Code / Codex 等 AI CLI 当一等公民来托管,提供 Warp 风格的 block 历史,并原生支持 WSL 与远程 Linux(SSH)。

---

## 1. 核心原理:终端 ≠ shell

这是理解整个项目的基础。**Shell 和终端模拟器是两个完全不同的层**,经常被混淆:

| | 是什么 | 在 Tn 里谁负责 |
|---|---|---|
| **Shell**(PowerShell / cmd / bash) | 命令解释器。解析命令、跑程序、管道。**它本身没有窗口、没有 GUI**,只从一个管道读字节(stdin)、往另一个管道写字节(stdout)。 | **复用现成的**,不重写 |
| **终端模拟器**(Tn / Windows Terminal / iTerm) | 屏幕 + 键盘。把 shell 输出的字节流(文字 + ANSI/VT 转义码)解析成二维字符网格并渲染;把键盘事件编码成字节喂回 shell。 | **我们写** |

历史上 Windows 默认终端是 `conhost.exe`。**Tn 做的事 = 用我们自己的实现取代 conhost**,PowerShell 本身不变,而且**可替换**——同一套机制能跑 `cmd`、`wsl`、`bash`、`vim`,乃至 `claude` / `codex`。

如果不做解析直接显示,你看到的是这种东西:

```
PS C:\> ls
\x1b[32m\x1b[1mDirectory:\x1b[0m C:\... \x1b[?25l\x1b[2K\x1b[38;5;39m...
```

终端的实活就是把这些 **VT/ANSI 转义序列**翻译成「光标移到第 N 行」「这段变绿」「清屏」「进入全屏(alt-screen)」并维护网格 + 滚动历史 + 光标状态。这部分极繁琐(VT100 几百条序列),所以我们**复用 alacritty 的解析引擎**(WezTerm、Zed 同理),不自己重写。

类比:**Tn 是「屏幕和键盘」,PowerShell 是「插上去的大脑」,ConPTY 是中间的「电线」。**

---

## 2. 系统架构

### 2.1 Cargo 工作区 ✅

多 crate 工作区。这样划分的核心理由:① 把 **GPUI(pre-1.0,API 易变)隔离**在 `tn-ui` 一个 crate;② 让终端内核 **headless、可单测**,不依赖 GPU;③ 职责清晰。

```
Tn/
├── Cargo.toml            # [workspace] + [workspace.dependencies](统一版本)
├── rust-toolchain.toml   # stable, x86_64-pc-windows-msvc
├── .cargo/config.toml    # cargo 别名(smoke/lint/...)
├── deny.toml             # cargo-deny 许可证/安全门
├── crates/
│   ├── tn-core/   # 终端引擎:alacritty 包装(Term+解析+快照)。无 GPUI/无 IO  ✅
│   ├── tn-pty/    # PTY 后端:PtyBackend trait + LocalPty(ConPTY)            ✅
│   ├── tn-config/ # 配置 + 主题(TOML、路径、热重载)                          ✅ M1.3
│   ├── tn-shell/  # OSC 133/633/7 旁路解析 → BlockEvent;shell 集成脚本        ✅ M3(headless 基础)
│   ├── tn-blocks/ # Warp 式 block 模型(命令/输出/退出码/时长)               ✅ M3(headless 基础)
│   ├── tn-ai/     # 检测 + 托管 claude/codex;agent 会话与状态               🧭
│   ├── tn-ui/     # GPUI 前端(唯一链接 gpui 的库):TerminalView 等          ✅
│   └── tn-app/    # 二进制 `tn`:开窗 + 接线 + 日志                          ✅
└── crates/tn-cli/ # headless 调试/烟雾测试工具                               ✅
```

### 2.2 依赖关系图(无环,评审 + deny.toml 把守)

```
tn-app ──► tn-ui ──► gpui            ◄── 许可证 & API 抖动的防火墙(gpui 只出现在这里 + tn-app)
              ├─► tn-blocks ─► tn-shell ─► tn-core
              ├─► tn-ai ─────► tn-pty
              └─► tn-config
tn-cli ──► tn-core + tn-pty           # headless,验证内核
```

**铁律**:`gpui` 只能出现在 `tn-ui` / `tn-app`;`tn-core` / `tn-pty` / `tn-config` 必须能在无 GPU 环境编译和测试。

---

## 3. 数据流

### 3.1 全景图 ✅

```
   你按键盘                                          子进程输出
      │                                                   ▲
      ▼  键 → 字节(Enter→"\r", Ctrl+C→0x03, ↑→"\x1b[A")    │ 字节流(文字 + ANSI/VT)
┌──────────────┐            ConPTY(内核伪终端)            ┌──────────────┐
│ TerminalView │◄──────────── 双向字节管道 ──────────────►│ powershell.exe│
│ (tn-ui/GPUI) │                                          │ (或 cmd/wsl…)│
└──────┬───────┘                                          └──────────────┘
       │ reader 线程读到字节
       ▼
┌──────────────────────────────────┐
│ Terminal (tn-core)               │
│  Processor.advance(&mut Term, …) │  把字节喂给 alacritty:更新网格、光标、滚动
│  → 同时产生 PtyWrite 事件(见3.3) │
└──────┬───────────────────────────┘
       │ snapshot() → TerminalSnapshot(可见网格)
       ▼
   GPUI div/text → DirectWrite 画成像素(tn-ui)
```

### 3.2 输出路径 ✅

1. `LocalPty` 通过 ConPTY 开伪终端,把 shell 挂为子进程([crates/tn-pty/src/local.rs](../crates/tn-pty/src/local.rs))。
2. **reader 线程**循环 `reader.read(buf)`,把字节交给 `Terminal::advance`([crates/tn-core/src/lib.rs](../crates/tn-core/src/lib.rs))。
3. `advance` = `vte::ansi::Processor::advance(&mut Term, bytes)`,alacritty 维护 `Term` 的网格/光标/滚动。
4. `Terminal::snapshot()` 遍历 `term.renderable_content()` 的可见行,产出不可变的 `TerminalSnapshot`(行×列的 `SnapshotCell`)。
5. `TerminalView::render` 把每行渲染成一个等宽 `div`([crates/tn-ui/src/terminal_view.rs](../crates/tn-ui/src/terminal_view.rs))。

### 3.3 输入路径 ✅

键盘事件经 `on_key_down` → `crate::input::encode_key(&Keystroke, InputMode)`([crates/tn-ui/src/input.rs](../crates/tn-ui/src/input.rs))把 GPUI `Keystroke` 映射成终端字节,写回 PTY 的 writer(shell 的 stdin)。**已照搬 Windows Terminal `_encodeRegular` 算法**:方向键/Home/End 按 DECCKM 选 CSI(`ESC[A`)或 SS3(`ESC OA`),带修饰 → `ESC[1;<mod><final>`(`<mod> = bits(SHIFT1/ALT2/CTRL4)+1`);F1–F4 SS3/CSI;F5–F20 DECFNK `ESC[<n>~`(跳号 LUT);Insert/Del/PgUp/PgDn `ESC[n~`;Backspace `0x7f`(Ctrl→`0x08`);Tab + Shift-Tab `ESC[Z`;Enter CR / LNM-CRLF / Ctrl-LF;`_makeCtrlChar`;Alt = ESC 前缀(CSI 键则折进 `<mod>`)。模式位经 `tn_core::Terminal::input_mode()`(读 alacritty `Term::mode()`)。`Ctrl+Shift+*`、`Ctrl+Tab` 保留给 UI(返回 None)。🧭 后置:kitty 键盘协议、DECKPAM 小键盘编码、win32-input-mode、bracketed-paste 包裹(标志已暴露)。详见 [REFERENCES.md](REFERENCES.md) §一。

### 3.4 PtyWrite 回写(关键坑)✅

**alacritty 在解析到设备查询时,会通过 EventListener 产生 `Event::PtyWrite(reply)` 事件,这些回复必须写回 PTY。** 否则:ConPTY 启动时发 `ESC[6n`(查询光标位置)并**阻塞等待回复**,不回复 → 子进程永远卡住(实测:只读到 4 字节、永不退出)。所以 reader 线程在 `advance` 后会 `drain_events()`,把所有 `PtyWrite` 写回 writer。

详见记忆 `conpty-dsr-ptywrite`。另一个 ConPTY 坑:**它不可靠地发 EOF**,所以判断子进程退出要用 `try_wait` 轮询,而不是等 `read()==0`。

### 3.5 重绘循环 ✅

✅ 现状(M1.5,[crates/tn-ui/src/terminal_view.rs](../crates/tn-ui/src/terminal_view.rs)):**push + vsync 合并**——reader 线程在有新输出时往 `futures::channel::mpsc::unbounded` 发一个 wake(由 `dirty` 原子标志去重,通道里至多 1 个待处理 wake);前台 `cx.spawn` 任务 `await` 该 wake 后 `cx.notify()`,GPUI 把多次 notify 合并到 vsync 帧时钟。空闲时**零唤醒**(无轮询)。**DEC 2026 同步输出由 alacritty `vte` `Processor`(`StdSyncHandler`)内部处理**——网格仅在 BSU→ESU 完成或其超时时才变更,故每次 `snapshot()` 都是整帧、无半更新撕裂,tn-ui 侧无需额外代码。遵 Ghostty 经验:不过度工程化批渲染,让 vsync 节流。*边角:同步超时仅在下次 `advance` 时复查,中途卡住的同步流会延迟显示——可接受,真实程序会及时发 ESU。*(见 [REFERENCES.md](REFERENCES.md) §六)

### 3.6 resize 联动 ✅

`render` 里用 `window.viewport_size()` 除以**实测的等宽字符宽度**(`text_system().advance(font_id, size, 'm')`)算出列数、除以行高算出行数;若与当前网格不同,则 `Terminal::resize` + `PtyBackend::resize`(ConPTY `window_change`)。

---

## 4. 各子系统设计

### 4.1 `tn-core` — 终端引擎 ✅

- `GridSize { rows, cols }`:实现 alacritty 的 `Dimensions`(`total_lines/screen_lines/columns`)。
- `Terminal`:持有 `Term<ChannelListener>` + `Processor` + 事件 `Receiver`。
  - `advance(&[u8])` / `resize(GridSize)` / `snapshot() -> TerminalSnapshot` / `drain_events() -> Vec<Event>`。
- `TerminalSnapshot`:`rows/cols/cursor/cells`;`rows_text()`、`to_text()` 便于渲染与测试。
- 单测 3 个:写文本入网格、CR/LF 换行、resize 改尺寸。
- 🧭 待加:`SnapshotCell` 当前只有 `char + flags`,**需补 fg/bg 颜色**(`alacritty …::Color` → RGB,需配色板/主题);damage 脏行追踪用于局部重绘。
- 🧭 取经(见 [REFERENCES.md](REFERENCES.md) §四,源自 Ghostty PageList):① 滚动区在 alacritty 行数上限之外**再加字节上限**,防超宽行/长跑会话爆内存;② 每行加 **styled/has_link/is_prompt/dirty 提示位**(假阳性可接受),让 block 命中测试与重绘 damage 整行跳过;③ block 快照做成**自包含、样式驻留的拥有式拷贝**,与活网格解耦。

### 4.2 `tn-pty` — PTY 后端 ✅ / 🧭

- `PtyBackend` trait:`resize / take_reader / writer / killer / wait / try_wait`——**统一同步的 Read/Write 接口**,让驱动层与后端无关。
- `PtySize`、`SpawnSpec`(program/args/cwd/env,builder 风格)、`Killer`。
- ✅ `LocalPty`(ConPTY,经 portable-pty):`openpty` → `spawn_command` → `try_clone_reader`/`take_writer`;**保留 slave 句柄**存活(ConPTY 上过早 drop 会扰动伪控制台)。
- 🧭 `WslBackend`:同样管线,只是命令换成 `wsl.exe -d <distro> --cd ~ -- <login-shell>`(用 `wsl -l -q` 枚举发行版)。
- 🧭 `SshBackend`(russh,**最高风险**):russh 给的是 async channel 而非 fd → 用专属线程跑 current-thread tokio,`request_pty`→shell,读泵把 `ChannelMsg::Data` 写进 `os_pipe`(同步 Read 端),写任务把 writer 字节经 mpsc → `channel.data`,`resize`→`window_change`,`wait`→ oneshot。认证链:agent→key→password;keepalive + 重连退避。

### 4.3 `tn-ui` — GPUI 前端 ✅ / 🧭

- ✅ `TerminalView`(`Render` 实体):持有共享 `Terminal`、`writer`、`pty`、`focus_handle`、当前 `size`、实测 `cell_width`。`new` 里 spawn shell + reader 线程 + 重绘任务;`render` 做 resize 计算 + 把快照逐行渲染;`on_key` 走输入路径。`TN_AUTOQUIT=1` 时跑内置自测(打一条命令、dump 网格、退出)——这是无人值守验证渲染的手段。
- 🧭 待做:**自定义 `TerminalElement`**(GPUI `Element` 的 layout/prepaint/paint),用 `paint_quad` 画背景/光标/选区、`shape_line`/`paint_glyph` 画字形,支持**每格颜色**、连字、选择、滚动条;Tab 栏、分屏(panes)、命令面板(palette)。
- 🧭 取经(见 [REFERENCES.md](REFERENCES.md) §二,源自 Windows Terminal AtlasEngine):**把可见网格拍平成带类型的 quad 批量提交**(背景/字形/光标/选区/下划线一把画)、**按 run(同字体同样式连续段)整形而非按 cell**、fg/bg 分离上色、连字按 cell 边界切分;box-drawing/Powerline 用 quad **自绘 sprite** 保证像素对齐。注意:GPUI 自带字形图集与 DirectWrite,**不重写 D3D**,只在 Element 内组织批处理。
- 🧭 会话/Tab/分屏与查看器(完整设计见 [UX-DESIGN.md](UX-DESIGN.md)):`PaneContent = Session | Viewer`;**灵活平铺**(n-ary 容器树 + 拖拽停靠)、标签组;文件树 + 文件/Diff 查看器;会话启动器(`+` / 命令面板,一键 Claude/Codex)与会话管理器。默认布局 `Explorer | Claude(大)+ 小 shell | Diff`。默认主题 `Tn Dark`([config/themes/tn-dark.toml](../config/themes/tn-dark.toml)),原型 [design/mockup.html](../design/mockup.html)。

### 4.4 `tn-config` — 配置与主题 🧭(目前 stub)

TOML,分层覆盖:内置默认 → `%APPDATA%\Tn\config.toml` → env → CLI。schema:`[general]/[font]/[appearance]/[blocks]/[agents]/[[profiles]](local|wsl|ssh)/[keybindings]`。主题模型 `ansi[16] + terminal{} + ui{}`;支持导入 iTerm2/.itermcolors、Windows Terminal、base16、Alacritty 配色。

🧭 取经(见 [REFERENCES.md](REFERENCES.md) §七,源自 Windows Terminal + Ghostty):① **profile 里 font / appearance 是嵌套对象**,`color_scheme` 按名引用、profile 可覆盖 fg/bg/cursor;② **键位解耦成两张表**——`[[actions]]`(`{id, command}`)与 `[[keybindings]]`(`{keys, id}`),一个动作可绑多键;③ 字段全 `#[serde(default)]` 实现可继承覆盖;④ 加**迁移表** `old_key → handler`,旧配置跨版本不炸;⑤ **diff-on-reload**:只重新应用变化字段,部分字段标"仅新会话生效";⑥ **默认手动重载命令**,`notify` 文件监听为可选(150ms 防抖 → 校验 → `arc-swap` 热替换),避免编辑器临时文件触发重载风暴。

### 4.5 `tn-shell` / `tn-blocks` / `tn-ai` 🧭

- **`tn-shell`**:alacritty **不解析** OSC 133/633 →在 PTY 字节上挂一个**旁路 `vte::Parser`**(只处理 `osc_dispatch`),产出 `BlockEvent`,同时原始流照常喂 `Term`。注入 pwsh/bash/zsh/fish 的 OSC 633 集成脚本(带 per-session nonce 防伪)。
- **`tn-blocks`**:block 是**对 alacritty 滚动区的语义索引**(行锚点 `(generation, abs_line)`,reflow 时重解析),不是替换网格。状态机 `AwaitingPrompt→PromptOpen→OutputOpen→finalize`。**alt-screen(vim/agent TUI)进入时整体关闭 block chrome,全幅渲染**,退出再恢复——这是「让位给全屏程序」的关键。
- **`tn-ai`**:检测 claude/codex(启动意图 > 进程树匹配 > OSC 标题 > banner);把一次 agent 会话当**一个 SurfaceBlock**,只装饰外框(状态条:名称/状态/耗时/cwd),**不爬它的 TUI 内部**;可选 opt-in 桥(Claude hooks 发私有 `OSC 1737;tn;<json>`;Codex 走命名管道 JSON-RPC)。**AI 用量**(上下文占用 + token + 估算花费)由 `UsageProvider` 解析本地会话 JSONL(Claude `~/.claude/projects`、Codex `$CODEX_HOME/sessions`),展示为分屏头环形读数 + AI 状态栏 + 用量面板——详见 [UX-DESIGN.md](UX-DESIGN.md) §5。

---

## 5. 关键技术决策(锁定)

| 领域 | 决策 | 理由 |
|---|---|---|
| UI/渲染 | **GPUI**(Windows 走 DX11+DirectWrite) | 性能+颜值兼顾、纯 Rust、为文字密集型应用而生;参考 Zed 的 terminal crate |
| 引擎 | **alacritty_terminal** 库 | 久经考验的 VT 解析+网格,绝不自己重写 |
| PTY/远程 | **portable-pty**(ConPTY+WSL)+ **russh**(SSH) | 纯 Rust、可内置、连接管理可自控 |
| AI 模式 | **托管 CLI** + Warp 式 **block UI** | 不做原生侧栏/内联补全(暂);深度 = 优秀托管 + chrome 装饰 + 优雅降级 |
| 许可证 | **开源 GPL-3.0-or-later** | 规避 GPUI 依赖树里 GPL-3.0 传递依赖的冲突([zed#55470](https://github.com/zed-industries/zed/issues/55470)) |
| MVP 顺序 | 本地内核 → WSL/SSH → blocks → AI | 先有能日用的扎实终端 |

---

## 6. 依赖与许可证

| Crate | 版本 | 许可证 | 用途 |
|---|---|---|---|
| `gpui` | **0.2.2**(crates.io) | Apache-2.0 | UI/GPU 渲染。**Windows 实测可直接编译**;blade/wayland/x11/ashpd 等都是 `cfg(linux)` 门控,Windows 用原生 DX11+DirectWrite,**不需要 Vulkan** |
| `alacritty_terminal` | 0.26 | Apache-2.0 | Term/Grid/VTE Processor/Selection/damage |
| `portable-pty` | 0.9 | MIT | ConPTY + WSL |
| `russh`(+keys/config) | 0.52–0.54 | Apache-2.0 | 纯 Rust SSH(🧭 M2) |
| `tokio` | 1 | MIT | async(🧭 SSH 后端用) |
| `serde`/`toml`/`directories`/`notify`/`arc-swap` | — | MIT/Apache | 配置/主题/热重载 |
| `tracing`(+subscriber) | — | MIT | 日志 |

**GPUI 在 Windows 的事实**:`gpui 0.2.2` 用 `windows` crate 的 `Direct3D11 / DirectWrite / DirectComposition / Dxgi` 特性;首次编译几分钟(整棵树几百个 crate);运行时日志会出现 `HRESULT(0x887A002D)` —— 那只是**可选的 DXGI debug 层**缺失(SDK 组件),**与渲染设备无关**,真正的 D3D11.1 设备正常创建。

🧭 后续可引入 `gpui-component`(0.5.x)获取现成的面板/输入框/命令面板等组件,加速「颜值」与 chrome。

---

## 7. 已知坑与经验(踩过的)

1. **ConPTY 启动 DSR 阻塞**:必须把 alacritty 的 `PtyWrite` 回复写回 PTY(见 §3.4)。
2. **ConPTY 无 EOF**:用 `try_wait` 轮询判断退出;烟雾测试加硬超时 + watchdog kill。
3. **保留 slave 句柄**:ConPTY 上过早 drop `SlavePty` 会出问题。
4. **CommandBuilder 默认继承父环境**(`std::env::vars_os`),不是空环境——所以 `powershell.exe` 能正常启动(早期误判过这点)。
5. **`gpui::Pixels.0` 私有**:取 f32 用 `f32::from(pixels)`,不能 `.0`。
6. **gpui async 接口**:`cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| …)`;`WeakEntity::update(cx, |v, cx| cx.notify())`;定时器 `background_executor().timer(dur).await`;退出 `cx.quit()`。
7. **cargo 不在 bash PATH**:在 `%USERPROFILE%\.cargo\bin\cargo.exe`,PowerShell 里用全路径或重开 shell。

---

## 8. 开发蓝图 / 路线图

> 每个里程碑都给出**交付物 + 退出标准 + 任务清单**。M0 已完成。

### M0 — 骨架 ✅ 已完成(2026-05-26,commit `aa53a98`)
- 工作区/工具链/cargo-deny;GPUI 窗口在 Windows DX11 跑通;`tn-core` 引擎(3 测试);`tn-pty::LocalPty`(ConPTY);`tn-ui::TerminalView`(渲染+输入+resize)。
- **退出标准达成**:窗口里跑真实交互式 pwsh,输出正确渲染,键盘输入生效,resize 生效。

### M1 — 可日用的本地终端 ✅ 完成(单次提交 `59b8b0e`,在 `main`;细分任务见 [CLAUDE.md](../CLAUDE.md),变更见 [CHANGELOG.md](../CHANGELOG.md))
**目标**:能当主力终端用。
- [x] `tn-core`:`SnapshotCell`/`TerminalSnapshot` 加 fg/bg(`Color`→RGB,ANSI16+256+OSC+INVERSE)、`Palette`(默认 Tn Dark)、`CellRun`+`row_runs()` 批处理、`set_palette()`。5 测试。 *(damage 脏行后置)*
- [x] **每格颜色渲染**:`tn-ui` 以 run 批处理的样式盒渲染(每格 fg/bg + 粗体),窗口内验证通过。 *(自定义 `TerminalElement`/光标/选区/连字 = M1.2b 后置)*
- [x] Tab + **灵活平铺分屏(n-ary 容器)** + 方向/点击聚焦 + 分屏/关闭/切标签快捷键(`workspace.rs`)。 *(分隔线拖拽 + drag-dock + 会话启动器 = M1.6b 后置)*
- [x] **M1.3** `tn-config`:schema(`[general]/[font]/[appearance]` + `[[profiles]]/[[actions]]/[[keybindings]]`,字段全 `#[serde(default)]` 可继承覆盖)/加载/路径(`%APPDATA%\Tn`)/首次写默认(`config.toml` + `themes/tn-dark.toml`,内嵌 `include_str!` 为单一真源)+ 主题加载(`Theme` 全量文档,缺失/损坏整体回退内置 Tn Dark)+ 字体(family/size/line-height)。`tn-ui` 经 `palette_from(theme)→tn_core::Palette` + `set_palette` 接线,字体与窗口 chrome 颜色来自配置(免重编译);`tn-config` 不依赖 `tn-core`(遵 §2.2 图),GPUI 层做桥。14 测试。 *(导入 iTerm/WT/base16、配置热重载、窗口 backdrop/opacity 应用 = 后置;`[font].fallback`、`[appearance].opacity/backdrop` 已解析未应用)*
- [x] **M1.4** 输入层重写([crates/tn-ui/src/input.rs](../crates/tn-ui/src/input.rs) `encode_key` + `tn_core::InputMode`/`Terminal::input_mode()`):Windows Terminal `_encodeRegular`——DECCKM CSI/SS3、`mod+1`、DECFNK F5–F20 跳号 LUT、Alt=ESC、`_makeCtrlChar`、Shift-Tab/`ESC[Z`、Enter LNM、Ctrl+Tab/Ctrl+Shift 保留。模式位读 alacritty `Term::mode()`。10 编码测试 + 1 mode 测试。*(kitty 协议、DECKPAM 小键盘、win32-input-mode、bracketed-paste 包裹后置)*
- [x] **M1.5** 重绘改为 push `notify`(reader→`mpsc::unbounded` wake,`dirty` 去重)+ GPUI vsync 合并,替换 8ms 轮询;DEC 2026 同步输出由 `vte` `Processor`(`StdSyncHandler`)内部缓冲处理(整帧快照,无撕裂)。见 [crates/tn-ui/src/terminal_view.rs](../crates/tn-ui/src/terminal_view.rs)。
- [x] **M1.6b** 滚动历史(滚轮:主屏滚历史/备用屏→方向键,输入回底)✅ · 粘贴(`Ctrl+Shift+V`/`Shift+Insert`,bracketed-paste 感知)✅ · 标题(OSC→标签)✅ · 选择+复制(透明 `canvas` 捕获内容 bounds、像素→格、左键拖拽、`Ctrl+Shift+C`)✅ · 键盘改尺寸(`Ctrl+Shift+方向键`→`Node::resize` 调最近同轴 split 权重)✅ · 多分屏尺寸修正 + **下分屏溢出修复**(各 flex 层 `min-size 0` + `overflow_hidden`)✅。 *(分隔线鼠标拖拽 + drag-dock + OSC 8 后置)*
- [ ] **M1.2b** 自定义 `TerminalElement`(字形图集 + typed-quad + 光标/选区,见 REFERENCES §2)。*(后置精修;当前 div + run 批处理渲染器即 M1 版本)*
- [x] 键位绑定可配置(`bind_keys(cx, &Loaded)` 读 `[[keybindings]]`/`[[actions]]`,叠加默认)+ 配置热重载(`Ctrl+Shift+R`:重读配置、对活动 pane 重应用调色板/chrome,字体/滚动历史仅新 pane 生效)+ 崩溃保护(panic hook→tracing)+ `tracing` 文件日志(`%APPDATA%\Tn\logs\tn.log`,tracing-appender)。
- **退出标准 ✅(达成,已提交 `59b8b0e`)**:Tab/分屏/滚动/复制粘贴/配置/主题全可用,能自我 dogfood。

> **执行顺序调整(owner)**:先做 **M3 → M4**,再回头做 **M2**。M3/M4 作用于本地终端、不依赖 M2。

### M2 — WSL + 远程 Linux 🧭(后置到 M3/M4 之后)
- [ ] `tn-pty::WslBackend`(`wsl -l -q` 枚举 + 每发行版默认 + cwd)。
- [ ] `tn-pty::SshBackend`(russh:连接、认证链、远程 pty、窗口尺寸传播、keepalive、重连)。
- [ ] Profile 选择器 + 主机列表(可选导入 `~/.ssh/config`);断连 UX。
- **退出标准**:pwsh / WSL / SSH 三种 Tab 并存,SSH 空闲不掉线。

### M3 — shell 集成 + block UI ✅ 完成(待窗口内肉眼复核 UI)
- [x] `tn-shell`([crates/tn-shell](../crates/tn-shell)):旁路 `vte::Parser` 只处理 `osc_dispatch`,解析 OSC 133(FTCS A/B/C/D[;exit])、633(+E 命令行、P;Cwd=)、7(file://→cwd,%XX 解码 + Windows 盘符)→ `BlockEvent`;`Integration`(per-session nonce + pwsh 集成脚本,prompt 钩子发 D/A/B、PSReadLine Enter 发 C)+ `encoded_command()`(UTF-16LE base64,经 `-EncodedCommand` 注入)。11 测试。
- [x] `tn-blocks`([crates/tn-blocks](../crates/tn-blocks)):`BlockModel` 状态机 `Prompt→Input→Running→Finished`,`on_event(ev,line,at_ms)` → `Block`(命令/cwd/prompt+输出行区间/退出码/时长);中断块无 D 时新 prompt 隐式收尾;`last_finished`。5 测试。 *(跨 WSL/SSH = M2 后)*
- [x] **接线**(`terminal_view.rs`):启动用 `-EncodedCommand` 注入 pwsh 脚本(无临时文件/不回显,`TN_NO_SHELL_INTEGRATION` 可关);reader 旁路跑 `ShellParser` → 用 `tn_core::Terminal::cursor_abs_line()` + 会话时钟喂共享 `BlockModel`。`TN_AUTOQUIT` 验不回归。
- [x] `tn-ui::block_view`:Warp 式命令 block 底栏(Calm Glass、状态条 蓝/绿/红、命令/时长/退出码/cwd、复制/重跑);**alt-screen 自动隐藏(正确性门槛)**。
- [ ] **后置精修**:历史 block 的逐行覆盖 chrome(锚行随 reflow 重解析、置顶/跳转/搜索);block 栏外观肉眼复核;pwsh `C` 钩子真机鲁棒性。
- **退出标准**:命令聚合成带状态/时长的 block,底栏可见且 alt-screen 隐藏。✅

### M4 — Claude/Codex 托管 + 命令面板 + 颜值 🧭
- [ ] `tn-ai::detect`(启动意图→进程树→标题);agent SurfaceBlock 状态条。
- [ ] **AI 用量**:`UsageProvider` 解析 Claude(`~/.claude/projects/**/*.jsonl`)/ Codex(`$CODEX_HOME/sessions/**/rollout-*.jsonl`)本地会话,展示**上下文占用 + token + 估算花费**(分屏头环形读数 + AI 状态栏 + 用量面板)。详见 [UX-DESIGN.md](UX-DESIGN.md) §5。
- [ ] 命令面板(`Ctrl+Shift+P`):agent 快启磁贴、新建 pwsh/WSL/SSH、最近命令、block 动作。
- [ ] 颜值打磨(主题、mica/acrylic、动画,用 gpui-component);可选 opt-in 桥。
- **退出标准**:在 Tn 里启动 Claude Code/Codex 明显优于普通终端。

### M5 — Quick Terminal(幽灵模式)🧭
**目标**:Quake/Guake 风格的下拉/滑入式悬浮终端——任意 app 里按全局快捷键即唤出一个置顶悬浮终端(直接跟 Claude/Codex 对话),用完滑走。**对 vibe coding 价值极高**:不打断当前工作即可召唤 AI 终端。设计取自 Ghostty 的 Quick Terminal(见 [REFERENCES.md](REFERENCES.md);源码 `src/cli/toggle_quick_terminal.zig`、`Config.zig` 的 `quick-terminal-*`)。
- 依赖:仅需 M0 的窗口能力即可起步;与 M4 的 AI 快启叠加最香。**独立特性,可在 M1 之后任意时机插入**。
- 实现三要素(Windows/GPUI):
  - [ ] **全局热键**:Win32 `RegisterHotKey`(必要时低级键盘钩子),前台任意 app 都能唤出;动作 `toggle_quick_terminal`。
  - [ ] **悬浮窗**:GPUI 开一个**无边框、`WS_EX_TOPMOST`、不进任务栏**的窗口(macOS 走 overlay window / Linux 走 wlr-layer-shell —— 跨平台时再分支)。
  - [ ] **边缘滑入/滑出动画**(GPUI 动画),位置 `top/bottom/left/right/center`。
  - [ ] **失焦自动隐藏**(autohide,监听 blur);跟随当前虚拟桌面。
- 配置(`[quick_terminal]`,镜像 Ghostty 命名):`enabled / position / size(% 或 px)/ animation_duration / autohide / hotkey / screen`。
- **退出标准**:全局热键一键唤出/隐藏悬浮终端,带滑动动画与失焦自动隐藏;可一键在其中起 Claude/Codex。

---

## 9. 开发指南

### 9.1 环境
- Rust **stable**,目标 `x86_64-pc-windows-msvc`(`rust-toolchain.toml` 已固定)。需 **VS C++ 生成工具 + Windows SDK**(GPUI/DX 必需)。
- cargo 路径:`%USERPROFILE%\.cargo\bin\cargo.exe`。

### 9.2 常用命令
```powershell
cargo build --workspace
cargo test  -p tn-core               # 引擎单测
cargo run   -p tn-cli                # ConPTY 烟雾测试(headless,打印网格)
cargo run   -p tn-app                # 开窗(主程序)
$env:TN_AUTOQUIT="1"; cargo run -p tn-app   # GUI 自测:dump 网格后自动退出
# 别名(.cargo/config.toml):cargo smoke / cargo lint / cargo t
```

### 9.3 约定
- **`gpui` 只准出现在 `tn-ui` / `tn-app`**;`tn-core`/`tn-pty`/`tn-config` 必须 headless 可编译可测。
- 不要把 alacritty 的类型泄漏到 crate 边界外——在 `tn-core` 里包装(目前 `Event` 经 `TermEvent` 重导出是唯一例外,慎用)。
- 改依赖版本走 `[workspace.dependencies]`;git 依赖必须钉死 `rev=`。
- 提交信息结尾带 `Co-Authored-By` 行(若用 AI 协作);行尾 LF(`.gitattributes` 已设)。

### 9.4 新增一个 crate
1. `crates/<name>/{Cargo.toml, src/lib.rs}`,字段用 `*.workspace = true`。
2. 在根 `Cargo.toml` 的 `members` 加入;若被复用,在 `[workspace.dependencies]` 加 `tn-<name> = { path = ... }`。
3. 遵守 §2.2 依赖方向。

### 9.5 验证清单(对应里程碑退出标准)
- M0:`cargo run -p tn-app` 开窗、跑 pwsh、`dir`/`vim`/`cat 大文件` 不卡、resize 重排;`TN_AUTOQUIT=1` dump 出含命令输出的网格;`cargo test -p tn-core` 绿。
- M1 ✅(退出标准达成):Tab/分屏/滚动/复制粘贴/配置/主题全可用;`Ctrl+Shift+R` 热重载颜色;变更见 [CHANGELOG.md](../CHANGELOG.md)。*(分隔线拖拽/拖拽停靠/自定义 TerminalElement/主题导入 = 后置精修。)*
- M2:开 WSL Tab 与 SSH Tab,各跑 `htop`/`vim`;SSH 空闲过 keepalive 仍在;拔网线断连 UX 干净。
- M3:pwsh 集成注入后,每条命令成 block(命令文本、退出色、时长);进 `vim` 时 block chrome 消失、退出恢复;`tn-shell` 用录制的 OSC 流单测。
- M4:`Ctrl+Shift+P` →「Start Claude Code here」拉起 agent 成带实时状态条的 SurfaceBlock;最近命令面板可重跑已验证 block。

---

## 10. 术语表

- **PTY / ConPTY**:伪终端。一对内核管道,让程序以为自己连着真终端。ConPTY 是 Windows 10+ 的实现。
- **VT / ANSI 转义序列**:`ESC[...` 这类控制码,表达颜色、光标移动、清屏等。
- **OSC**:Operating System Command(`ESC ] … ST`),用于标题、cwd(OSC 7)、shell 集成(OSC 133/633)、超链接(OSC 8)。
- **DSR**:Device Status Report,`ESC[6n` 查询光标位置,终端须回 `ESC[<row>;<col>R`(见 §3.4)。
- **alt-screen**:全屏程序(vim、agent TUI)切换到的备用屏缓冲(DECSET 1049),退出后恢复主屏。
- **block**:Warp 式概念——一条命令及其输出作为一个可折叠/复制/重跑的单元。
- **damage / 脏行**:本帧相对上帧变化的行,用于局部重绘。
- **snapshot**:`tn-core` 产出的不可变可见网格视图,供渲染层消费。
```
