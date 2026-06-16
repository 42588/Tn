# Tn — 人与智能体共用的终端

Tn 是一个面向 Windows 的原生终端,用 [GPUI](https://www.gpui.rs/)(Zed 的 GPU UI 框架,DirectX 11 + DirectWrite)构建。它把 **AI 编码智能体(Claude Code / Codex)** 与普通 shell、WSL、SSH 一视同仁,作为同级的会话类型托管在同一个窗口里 —— 你和智能体共用一套终端、布局与文件视图。

视觉上它遵循自研的 **「磷光 Phosphor」** 设计语言:不透明的仪表盘式底盘负责秩序,唯一的磷光生命色只点亮"活着的东西"(光标、运行态、焦点)。整套设计被刻意约束在 GPUI 能精准还原的能力内,不依赖任何 WebView。

> 状态:`0.0.0`,活跃开发中。多数核心能力已在代码中实现,部分端到端路径仍在真机验收中。

---

## 为什么用 Tn

- **智能体是一等公民,不是插件**。Claude Code、Codex 直接作为会话类型启动,带原生的用量/成本读数(token、上下文占用、$ 估算),不必在终端里手动拼命令。
- **块状终端(OSC 133)**。命令、输出、退出码、耗时被解析成独立的命令块,历史清晰可读,而非一片滚动文本。
- **QuickLook 速览**。在资源管理器里选中文件即弹浮层预览:文本带语法着色、**Markdown 直接渲染排版**(Enter 进编辑、Esc 回预览)、PDF / 图片预览、Diff 标签页逐 hunk 接受/拒绝;支持本地与远程(SFTP)读写,多编码(UTF-8/16、GBK)。
- **幽灵终端(Ghost)**。全局热键(默认 `Ctrl+Alt+Space`)从屏幕顶部滑下一个临时终端,可自动隐藏、会话常驻,随手起一个 shell / WSL / SSH / Agent。
- **本地、WSL、SSH 一体**。SSH 走 russh,远程文件树与 SFTP 读写内建;WSL 发行版直接列出可启。
- **分屏 / 标签 / 命令面板**。N 叉窗格树、横竖分屏、`Ctrl+Shift+P` 命令面板、7 槽布局保存与恢复。
- **像素宠物**。一只 14×12 像素小狗趴在状态栏上,只订阅命令事件与键入信号陪着你写码,可关闭、尊重 reduced-motion。
- **精心调校的字体系统**。等宽 JetBrains Mono Nerd Font(终端/编辑器)、UI 无衬线 Inter、展示字 Space Grotesk —— 全部打包进 exe,零系统字体依赖。

---

## 架构

Tn 是一个 Rust workspace。GPUI 只链接在 `tn-ui` / `tn-app` 两个 crate,其余全部 **headless**(可脱离 GUI 单测):

| Crate | 职责 |
| --- | --- |
| `tn-core` | 终端引擎:`alacritty_terminal` 封装(网格、解析、快照) |
| `tn-pty` | PTY 后端:本地 ConPTY + SSH(russh)+ WSL |
| `tn-config` | 配置与主题:TOML schema、路径、导入、热重载 |
| `tn-shell` | Shell 集成:旁路 VTE 解析 OSC 133/633/7 → BlockEvent |
| `tn-blocks` | 命令块状态机:BlockEvent → 命令块(prompt/命令/输出/退出/耗时) |
| `tn-editor` | Headless 编辑器内核:文本缓冲操作与文档模型 |
| `tn-agent` | 智能体宿主平台(agent-agnostic):身份/能力/注册表/用量/定价模型 |
| `tn-ai` | 智能体用量与侦测(Claude Code / Codex):解析本地 JSONL → token/成本 |
| `tn-cli` | Headless 调试台:不带 GUI 驱动 tn-core + tn-pty |
| `tn-ui` | GPUI 前端:终端视图、输入、渲染(唯一链接 GPUI 的库) |
| `tn-app` | 应用二进制:窗口引导与接线(产物 `tn.exe`) |

---

## 安装

### 方式一:用安装器(推荐普通用户)

从 Release 下载 `tn_<version>_x64-setup.exe`,双击安装。**当前用户级安装**(无需管理员、无 UAC),装到 `%LOCALAPPDATA%\Programs\Tn`。安装包内含 `tn.exe` 与原生依赖(ConPTY、pdfium);字体、图标、默认配置已编译进 exe。

### 方式二:从源码构建

**前置要求**

- Windows 10/11(x64)
- Rust 稳定版,MSRV **1.85**(仓库带 `rust-toolchain.toml`,会自动选用)
- MSVC 工具链(`x86_64-pc-windows-msvc`)

**运行 / 构建**

```powershell
# 克隆
git clone https://github.com/tingruiyi/Tn
cd Tn

# 开发运行
cargo run -p tn-app

# 发布构建(产物 target\release\tn.exe)
cargo build --release -p tn-app
```

构建脚本会自动把原生依赖(`conpty.dll` / `OpenConsole.exe` / `pdfium.dll`)拷到 exe 旁,`cargo run` 与打包产物都能直接找到。

**打 NSIS 安装包**

```powershell
cargo install cargo-packager --locked
cargo build --release -p tn-app
cargo packager --release -p tn-app --out-dir dist -f nsis
# 产物:crates\tn-app\dist\tn_<version>_x64-setup.exe
```

---

## 配置

配置走 TOML(详见 `config/config.toml` 内的注释)。常用项:

- `[font]` —— 等宽字体、字号、行高(默认 `JetBrainsMono Nerd Font` / 14 / 1.3)
- `[appearance] theme` —— 主题名(内建 `Tn Dark`,主题文件在 `config/themes/`)
- `[general] billing_mode` —— 智能体用量显示(`cost` / 上下文 % / `tokens`)
- `[[agents]]` / `[[profiles]]` —— 自定义智能体与启动项
- 幽灵终端热键默认 `Ctrl+Alt+Space`

常用快捷键:`Ctrl+Shift+P` 命令面板 · `Ctrl+Shift+N` 新会话 · `Ctrl+Alt+Space` 幽灵终端。

---

## 技术栈

- **UI**:gpui `0.2`(DirectX 11 + DirectWrite),Windows 专属
- **终端**:`alacritty_terminal` · `portable-pty` · `vte`
- **远程**:`russh`(SSH,ring 加密后端)+ SFTP
- **文档预览**:`pulldown-cmark`(Markdown)· `pdfium-render`(PDF)
- **原生依赖**:随仓库附带的现代 ConPTY(`conpty.dll` + `OpenConsole.exe`,让 Codex 这类全屏 TUI 拿到备用屏/鼠标转发)与 `pdfium.dll`,见 [crates/tn-app/vendor/README.md](crates/tn-app/vendor/README.md)

---

## 许可

GPL-3.0-or-later。详见各 crate 的 `Cargo.toml`(`license.workspace = true`)。

---

## 贡献

欢迎 issue 与 PR。改动前请先读仓库内的工作约束:[CLAUDE.md](CLAUDE.md)、[docs/共享记忆索引.md](docs/共享记忆索引.md) 与 [docs/共享记忆/踩坑记录.md](docs/共享记忆/踩坑记录.md)。
