# CLAUDE.md — Tn 终端(Claude Code 项目指南)

Tn 是一个 **Windows 优先、Rust 编写、GPU 加速、为 vibe coding 打造的终端**:把 AI 编码 CLI
(Claude Code、Codex)当一等公民托管,提供出色 UX、灵活平铺、WSL + SSH。技术栈:**GPUI**
(Zed 的框架,Windows 上走 DirectX 11 + DirectWrite)· **alacritty_terminal**(VT 引擎)·
**portable-pty**(ConPTY/WSL)+ **russh**(SSH)。许可证:GPL-3.0-or-later。

## 现状(先读)
- **M0 = 完成并已提交。** **M1 = 完成并已提交**(单次提交 `59b8b0e`,在 `main`):配置/主题、按键编码、push 重绘、滚动历史、选择/复制粘贴、分屏(切分 + 键盘改尺寸 + 热重载)。退出标准达成、已 dogfood 验证。变更见 [CHANGELOG.md](CHANGELOG.md)。
- **当前在做 = M3 + M4**(owner 调整计划:**M3 shell 集成 + M4 AI 托管/用量先行,M2 WSL/SSH 后置**——M3/M4 作用于本地终端,不依赖 M2)。**M3 已完成**:`tn-shell`(OSC 旁路解析)+ `tn-blocks`(block 状态机)+ 接线(`-EncodedCommand` 注入 pwsh 脚本、reader 旁路喂 `BlockModel`)+ `tn-ui::block_view`(Warp 式 block 底栏,Calm Glass、alt-screen 隐藏);`TN_AUTOQUIT` 验不回归,单测 61。**M4 进行中**:`tn-ai` 用量解析 + 实时用量状态栏 + 命令面板(一键起 Claude/Codex、pwsh 托管、`×` 关闭杀进程)已就绪;剩 Codex 用量解析 / agent 检测 / per-pane 用量跟随焦点 / 颜值。详见下方「## M3 + M4 计划与进度」。
- **延后**:M1 精修(分隔线鼠标拖拽——`Node::resize` 权重逻辑已就绪 + 测试、drag-dock、M1.2b 自定义 `TerminalElement`)、M2 WSL/SSH。
- 完整计划/路线图:[docs/BLUEPRINT.md](docs/BLUEPRINT.md)。UX(分屏/会话/AI 用量/查看器):[docs/UX-DESIGN.md](docs/UX-DESIGN.md)。从 Windows Terminal + Ghostty 提炼的设计要点:[docs/REFERENCES.md](docs/REFERENCES.md)。默认主题原型 [design/mockup.html](design/mockup.html)(**Calm Glass** 视觉系统已定稿——磨砂玻璃靠折射光+投影分层、**不做自发光/光污染**;设计令牌见 [UX-DESIGN §6.1](docs/UX-DESIGN.md)),主题 [config/themes/tn-dark.toml](config/themes/tn-dark.toml)。

## 工作区(crates)
```
tn-core    alacritty 包装:Term + VTE 解析 + TerminalSnapshot + Palette/RGB 解析 + row_runs
           + InputMode(模式位)+ 滚动 + 选区。无 GPUI、无 IO(headless)。
tn-pty     PtyBackend trait + LocalPty(ConPTY)。headless。(WSL/SSH = M2)
tn-config  配置 + 主题(TOML、路径、首次写默认、热重载源)。headless。M1.3 已实现。
tn-shell   shell 集成:旁路 vte::Parser 把 OSC 133/633/7 解析成 BlockEvent + 集成脚本/nonce。headless。M3。
tn-blocks  Warp 式 block 状态机:BlockEvent(+行/时间)→ Block(命令/输出区间/退出码/时长)。headless。M3。
tn-ai      AI agent 用量 + 检测:解析 ~/.claude/projects/**/*.jsonl(+ Codex rollout)→ token/上下文/
           估算花费 + pricing 表(AiUsage)。headless。M4。
tn-ui      GPUI 前端(唯一链接 gpui 的库):input.rs(按键编码)、terminal_view.rs(单个 pane + LaunchSpec)、
           block_view.rs(Warp block 底栏)、workspace.rs(标签/n-ary 分屏 + 用量状态栏 + 命令面板)。
tn-app     二进制 `tn`:开窗 + 接线 + 崩溃保护 + 文件日志。
tn-cli     headless ConPTY 烟雾测试工具。
```
依赖方向(无环):`tn-blocks → tn-shell`(BlockEvent);`tn-ui → 全部 headless crate`。
铁律:`gpui` 只能出现在 `tn-ui`/`tn-app`。`tn-core`/`tn-pty`/`tn-config`/`tn-shell`/`tn-blocks`/`tn-ai` 必须能 headless 编译与测试。别把 alacritty 的类型泄漏出 `tn-core`(例如 `CellRun` 暴露 bold/italic 布尔,而非 `Flags`);`tn-shell` 用 `vte` crate 直连(不经 alacritty)。

## 构建 / 运行 / 测试
`cargo` 在 `%USERPROFILE%\.cargo\bin\cargo.exe`(不在 bash PATH 上——PowerShell 里用全路径,或在新开的 shell 里它在 PATH 上)。
```powershell
cargo build --workspace
cargo test  --workspace                        # 单测共 61(tn-core 10 / tn-config 14 / tn-ui 13 / tn-shell 11 / tn-blocks 5 / tn-ai 8)
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

**M4 — 托管 Claude/Codex + 用量 + 命令面板**(进行中,均后于 M3)
- ✅ `tn-ai`(新 crate,headless):`AiUsage` 模型 + `pricing` 表 + **Claude UsageProvider**——解析 `~/.claude/projects/<proj>/<session>.jsonl` 的 assistant `message.usage`,累计 token、取最后一轮总输入为上下文大小、按 pricing 估算等价花费、观测超 200K 时推断 1M 窗口。8 单测 + 真实数据验证。
- ✅ **实时用量状态栏**(`workspace.rs`):底部状态栏显示本项目 Claude 用量(型号 / 上下文条 绿→黄→红 / % / token / 花费);后台线程轮询,仅会话文件 mtime 变化时重解析(空闲只 stat,不破坏空闲零唤醒)。
- ✅ **命令面板 `Ctrl+Shift+P`**(`workspace.rs` overlay + `terminal_view::LaunchSpec`):列出 config `[[profiles]]` 可启动项,打字筛选 / ↑↓ / Enter / Esc / 点击;启动 = 新标签跑该 profile。**agent(claude/codex)托管在 pwsh 里**(`-NoExit -Command "& '…'"`)以解析 npm shim;spawn 失败回退 pwsh(不崩)。标签 `×` 关闭 + `LocalPty` Drop 杀子进程。
- 🧭 `tn-ai::detect`(agent 识别:启动意图 > 进程树 > OSC 标题 > banner)+ **per-pane 用量跟随焦点**(状态栏/分屏头按焦点 pane 的 agent 切换读数)+ **Codex UsageProvider**(`$CODEX_HOME/sessions/**/rollout-*.jsonl` 的 `token_count`)。
- 🧭 颜值打磨(主题 / mica / 动画,可选 `gpui-component`)。

## M1 延后的精修项(交互式做,需肉眼/鼠标验证)
1. **分隔线鼠标拖拽** —— 权重数学 `Node::resize` 已实现 + 测试,只差把可拖拽的分隔把手接上去。
2. **拖拽停靠**(drag-dock)—— 拖到边=分屏、拖到中=标签组。
3. **M1.2b 自定义 `TerminalElement`** —— GPUI `Element`(layout/prepaint/paint)+ 字形图集 + typed-quad 批处理 + 光标/选区绘制(见 REFERENCES §2;GPUI 自管图集/DirectWrite,**不写裸 D3D**)。当前 div + run 批处理渲染器即 M1 版本。
- 其他后置:主题/配色导入(iTerm/WT/base16)、窗口 backdrop/opacity 应用(`[font].fallback`、`[appearance].opacity/backdrop` 已解析未应用)、OSC 8 超链接(→ M3)、kitty 键盘协议 / DECKPAM 小键盘 / win32-input-mode。

## 后续里程碑
执行顺序(owner 调整后):**M3 → M4 → M2**。即 M3 shell 集成 + block(✅ 已完成)、M4 Claude/Codex 托管 + AI 用量 + 命令面板(下一步),然后回头做 M2 WSL+SSH;M5 Quick Terminal 可任意时机插入。文件/Diff 查看器 = M3/M4(`PaneContent = Session | Viewer`)。详见 [docs/BLUEPRINT.md](docs/BLUEPRINT.md) §8。

## 踩过的坑(hard-won)
- **ConPTY**:必须把 alacritty `Event::PtyWrite` 回复写回 PTY writer(回应启动时的 `ESC[6n` DSR),否则子进程卡住(只读到 4 字节)。ConPTY 无可靠 EOF——用 `try_wait` 轮询而非 `read()==0`。整个会话保活 portable-pty 的 `SlavePty`。
- **Windows 上 `claude`/`codex` 是无扩展名的 npm shim**:`CreateProcessW` 直接拉起会 `%1 不是有效的 Win32 应用程序`(os error 193)。要么解析 `.cmd`,要么**用 pwsh 托管**——`powershell -NoExit -Command "& 'codex'"`(pwsh 走 PATHEXT 解析 `codex.cmd`,且 agent 退出后回到 prompt)。Tn 用后者(`LaunchSpec::from_profile`)。
- **pane 构造在 GPUI 窗口回调里 = non-unwinding**:`LocalPty::spawn(...).expect()` 之类的 panic 会让整个进程 **abort**(`STATUS_STACK_BUFFER_OVERRUN` / 退出码 9 / 0xc0000409),而非被 panic hook 接住。所以 spawn 失败**必须优雅回退**(Tn 回退到 pwsh),绝不能 panic。
- **portable-pty 的 `LocalPty` Drop 不杀子进程**:不显式 kill,关 pane 只是移除视图,agent/shell 变**孤儿进程**继续跑。解法:给 `LocalPty` 加 `Drop` → `child.clone_killer().kill()`;关标签时 drop 视图链(`panes.remove` → drop `TerminalView` → drop `Arc<LocalPty>` → kill)。
- **gpui 0.2.2 on Windows**:可直接编译(DX11+DirectWrite,无需 Vulkan);首次编译数分钟。运行时 `HRESULT(0x887A002D)` 只是可选的 DXGI *debug* 层缺失——无害。
- Debug 构建保留控制台窗口;release 隐藏(tn-app 的 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`)。
- **GPUI 窗口类在 Windows = `Zed::Window`**(标题不是 "Tn");截图/注入按键工具要按窗口类枚举顶层窗口,而非 `FindWindow(title)`。
- **工作区快捷键用 `Ctrl+Shift+*`。** 中文/多布局 Windows 上,系统"切换键盘布局"热键也是 `Ctrl+Shift`,可能在按键到达 app 前吞掉它。已(经合成 SendInput)验证绑定 + 动作派发正确——每个动作都会触发。键位现已可配置(`bind_keys` 读配置),可改键避开;或禁用 Windows 的 Ctrl+Shift 布局切换热键(设置 → 时间和语言 → 输入 → 高级键盘设置 → 输入语言热键 → 把"在输入语言之间"/布局切换设为*未分配*)。`Ctrl+Tab`(下个标签)与鼠标点击聚焦不受影响。
- `gpui::Pixels.0` 私有 → 用 `f32::from(px)`。GPUI async:`cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| ...)`、`WeakEntity::update(cx, |v, cx| ...)`、`bg_executor.timer(d).await`、`cx.quit()`。`Context<T>` 解引用为 `App`(故可 `cx.read_from_clipboard()` 等)。
- **Flexbox `min-size: auto`(Taffy)**:内容过高/过宽的子项会把 flex item 撑过其 `relative()` 份额、溢出窗口——又因 `terminal_view` 读 `canvas` 捕获的 bounds 来定网格大小,被撑高的高度反馈回去使网格永不收敛。解法:每个 flex 层(body、分屏 container、pane wrap、终端根)都加 `min_w(px(0.))`/`min_h(px(0.))` + `overflow_hidden()`。这正是"下分屏溢出窗口"那个 bug。
- 提交结尾带:`Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`。行尾 LF(`.gitattributes`)。PowerShell 里避免在 `git commit -m @'...'@` here-string 中用 `"`(破坏解析);多行信息用 Bash 工具的 `git commit -F -` + 单引号 heredoc 更稳。
