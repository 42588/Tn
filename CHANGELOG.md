# Changelog — Tn 终端

本文件记录 Tn 各里程碑的变更,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。
版本对应开发蓝图([docs/BLUEPRINT.md](docs/BLUEPRINT.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 **Windows 优先、Rust、GPU 加速**的终端,为 vibe coding 设计:托管 Claude Code /
> Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite)·
> alacritty_terminal(VT 引擎)· portable-pty(ConPTY)· russh(SSH,M2)。许可证 GPL-3.0-or-later。

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
