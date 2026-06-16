# Tn 使用说明书

Tn 是一个用 GPUI (Zed 绘图框架) 构建的 Windows 原生 GPU 加速终端。它将 shell 会话与 AI 编码智能体 (Claude Code / Codex) 统一托管，提供命令块化解析、QuickLook 文件速览与极简编辑、Ghost 全局下拉终端，以及状态栏像素宠物等特性。

---

## 1. 快速开始

### 运行与构建
* **开发模式启动**：
  ```powershell
  cargo run -p tn-app
  ```
* **Release 构建**（产物生成于 `target\release\tn.exe`）：
  ```powershell
  cargo build --release -p tn-app
  ```
* **打包安装包 (NSIS)**：
  ```powershell
  cargo install cargo-packager --locked
  cargo build --release -p tn-app
  cargo packager --release -p tn-app --out-dir dist -f nsis
  ```
  安装包将输出至 `dist\tn_<version>_x64-setup.exe`。默认执行当前用户级安装，无需管理员权限，安装路径为 `%LOCALAPPDATA%\Programs\Tn`。

---

## 2. 窗口与窗格布局

新标签页默认进入 Welcome 欢迎页（启动器），支持选择不同的 shell、WSL、SSH 或 Agent 会话。

### 布局与分屏操作
* **新建标签页**：使用快捷键 `Ctrl+Shift+T`（新标签页会加载欢迎页）。
* **切换标签页**：使用快捷键 `Ctrl+Tab`。
* **分屏窗格**：
  * 向右分屏 (Split Right)：`Ctrl+Shift+D`
  * 向下分屏 (Split Down)：`Ctrl+Shift+E`
* **关闭窗格**：`Ctrl+Shift+W`。若关闭的窗格中含有 QuickLook 未保存的编辑内容，将触发防丢保护弹窗。
* **切换焦点窗格**：`Ctrl+Shift+]` 在窗格间循环切换。
* **调整窗格尺寸**：
  * 调整宽度：`Ctrl+Shift+Left` (收缩) / `Ctrl+Shift+Right` (拉宽)
  * 调整高度：`Ctrl+Shift+Up` (收缩) / `Ctrl+Shift+Down` (拉高)
  * 拖拽操作：鼠标拖拽分隔线时会显示预览线，松手后触发 PTY 一次性重绘，避免拖拽过程因重绘导致内容闪烁或卡顿。

### 布局槽位管理
点击左上角 `Tn` 标志唤出 App 菜单，进入“布局 (Layouts)”，系统提供 7 个独立的布局槽位。可将当前 Tab 的窗格拓扑结构与启动条目 (Launch Spec) 保存至槽位中，随时加载或清除（仅记录窗格关系与启动配置，不保存运行中的进程状态）。

---

## 3. Shell 与 AI 智能体集成

### AI 智能体 (Claude Code / Codex) 窗口
* AI 智能体会话在窗格中为一等公民，拥有专属的头部状态栏 (Agent Header)，展示当前状态、模型版本及 Token 消耗/成本百分比。
* **用量药丸 (Usage Pill)**：展示当前的 Token/额度占用。点击该药丸可以在 **美元估算 ($X.XX)** ➔ **上下文百分比 (%)** ➔ **Token 消耗数** 之间循环切换。
* **活动变更侧栏 (Agent Activity Rail)**：当 Agent 会话激活时，右侧会展示当前已变更的文件卡片列表（本地通过 git diff 统计，远程通过 ssh 抓取）。点击卡片可直接以 QuickLook 模式预览对应的 diff。

### OSC 133 命令块解析
普通 Shell 会话（如内置 PowerShell）支持 OSC 133/633 协议，自动将每一条命令的输入、输出、退出码及耗时解析为独立的“命令块 (Command Block)”。
* 悬停在命令块上可触发快捷栏，支持复制命令或重新运行。
* 当进入全屏 TUI 交互或 AI 占屏状态时，命令块操作栏会自动隐藏，避免干扰终端正文。

---

## 4. Ghost (幽灵) 终端

Ghost 终端是一个 Quake 式的全局下拉悬浮终端，用以在不打断当前工作流的前提下，快速唤出 Shell 或 AI 助手。
* **唤出/隐藏**：全局热键默认为 `Ctrl+Alt+Space`。
* **展示行为**：从屏幕顶部滑入（可在配置中修改停靠位置与尺寸百分比），失焦时会自动隐藏。
* **常驻会话**：隐藏后进程与会话状态依然保留，再次唤出可直接继续使用。

---

## 5. QuickLook 速览与极简编辑

工作区内置文件速览器，无缝处理本地及远端文件（WSL / SSH），无需调用 WebView。

### 打开与预览
* 在 Explorer 文件树中，**单击** 或使用键盘 `上下键` 选中文件并按 `Enter`/`Space`，即可打开 QuickLook 浮层。
* 支持 File (正文预览) 与 Diff (Git 差异) 两个标签页。
* 预览类型支持：
  * **文本文件**：带语法高亮，后台线程异步解码。支持 UTF-8、UTF-8 BOM、UTF-16 LE/BE、GBK 编码。
  * **Markdown**：直接进行原生排版渲染，支持渲染代码围栏与行高。
  * **图片**：支持 PNG、JPG、WEBP、GIF、BMP 等常见格式。
  * **PDF**：通过内置 Pdfium 引擎渲染预览。
  * **大文件/二进制**：若文件超过大小限制（远端 SFTP 限制为 2MB）或包含 Null 字节，将自动降级为二进制只读展示。

### 极简编辑模式
* **进入编辑**：在文本预览态下，按下 `Enter` 键即可切换进入自绘编辑器。支持常规编辑、选择、查找与替换 (`Ctrl+F`/`Ctrl+H`) 等操作。
* **退出编辑/保存**：
  * 按下 `Esc` 键返回预览状态，未保存的修改会暂存在内存镜像中，并反映在预览页上。
  * 按下 `Ctrl+S` 执行保存（本地写入或通过 SFTP 写回远端）。
* **保存冲突检测**：保存前系统会检查文件的修改时间 (mtime)、大小与样本哈希，如果检测到文件已被外部修改，会弹出冲突警告，可选择强制覆盖或保留当前修改。

### 远端 Diff 与 Hunk 级别操作
* QuickLook 切换到 Diff 标签页时，系统会懒加载 `git diff` 结果。
* 针对 SSH 远端文件，支持逐个 Hunk（差异块）展示“接受 (Accept)”或“拒绝 (Reject)”按钮，并对应执行 `git apply` 更新，修改将实时同步回 Agent Activity Rail。

---

## 6. 状态栏像素宠物

在状态栏右侧有一只 14x12 像素的小狗宠物（基于自绘像素渲染器渲染）。
* **互动操作**：
  * **领养与命名**：首次启动或重置时，可点击它打开命名卡进行起名。命名输入框支持中文 IME 输入法（最多 8 个字）。
  * **喂食小饼干**：通过菜单可给小狗喂食，伴随投喂动画。
  * **逗弄与玩耍**：双击小狗会触发逗弄动作（蹦跳、爱心气泡、摇尾巴）。
  * **抚摸与交互**：鼠标悬停会触发眯眼动画。
* **状态联动**：
  * 小狗会实时订阅终端的击键输入和命令执行事件：输入繁忙时小狗会变得兴奋，终端闲置 90 秒后会进入睡眠打盹状态 (Zz)。
  * 根据当前系统时间，小狗会在深夜、早晨等时段触发专属问候与动作彩蛋。
  * 终端执行命令失败（如非零退出码）时，小狗会表现出难过或安慰的情绪。
* **系统设置**：在 App 菜单中进入宠物设置，可调整宠物显示、重新命名、更换窝巢、选择玩具，或一键重置羁绊档案。小狗遵守系统 `reduced-motion`（减弱动态效果）配置，可随时在设置中将其收起。

---

## 7. SSH & WSL 多环境会话

### WSL 集成
欢迎页与启动器会自动扫描并检测系统已安装的 WSL 发行版，可一键在独立窗格中建立对应的 Linux 会话，文件路径命名空间将自动映射为 WSL 格式（使用 `\\wsl$` UNC 路径）。

### SSH 会话与远端文件
* **连接交互**：在启动器选择 SSH 后，将弹出内置连接卡片，在连接、认证和 Shell 建立阶段会实时展示连接进度。
* **TOFU 指纹卡片**：首次连接未知的 SSH 主机时，会弹出 TOFU 主机指纹确认，可选择“仅本次信任”或“写入 known_hosts”。
* **认证方式**：支持秘钥对验证（依次尝试本地 `~/.ssh/id_ed25519` / `id_ecdsa` / `id_rsa`）、密码输入（带掩码与显示切换）以及 keyboard-interactive 认证。密码可选择“记住本会话”。
* **断开重试**：连接失败或意外断开后会展示带重试次数（上限 3 次）与明确错误归因的失败卡片。
* **最近连接**：支持收藏、重命名、模糊过滤，并自动展示上次连接所采用的认证方式 (密钥/密码/交互)。
* **远端文件选择器**：在 SSH 会话中，“打开文件夹”动作会唤起基于 SFTP 的远端目录浏览器，支持使用键盘导航（`↑`/`↓` 选择，`←` 上级，`→` 进入，`Enter` 确认）。

---

## 8. 快捷键速查表

| 快捷键 | 动作功能 | 备注 |
| :--- | :--- | :--- |
| `Ctrl+Shift+P` | 打开全局命令面板 | 可检索并执行 profiles / 功能指令 |
| `Ctrl+Shift+T` | 新建欢迎标签页 (Welcome Tab) | |
| `Ctrl+Tab` | 切换到下一个标签页 | |
| `Ctrl+Shift+D` | 向右分屏 (Split Right) | 创建并平铺新窗格 |
| `Ctrl+Shift+E` | 向下分屏 (Split Down) | 创建并平铺新窗格 |
| `Ctrl+Shift+W` | 关闭当前活动窗格 | 包含 QuickLook 未保存防丢弹窗 |
| `Ctrl+Shift+]` | 循环切换焦点窗格 | |
| `Ctrl+Shift+L/R` | 调整当前窗格宽度 | 对应 grow_width / shrink_width |
| `Ctrl+Shift+U/D` | 调整当前窗格高度 | 对应 grow_height / shrink_height |
| `Ctrl+Shift+R` | 热重载配置文件 | 重新加载设置并热更新主题调色板 |
| `Ctrl+Alt+Space` | 全局唤出/隐藏 Ghost 终端 | 可在 `config.toml` 中自定义热键 |
| `Ctrl+Shift+N` | 新建分屏启动器 | 可选择分屏方向与 profile 载入新窗格 |

### 辅助选择器导航键 (Explorer / 文件与目录选择器)
* `↑` / `↓`：在列表或目录树中上下移动光标。
* `←`：返回上一级目录。
* `→`：展开或进入选中的子目录。
* `Space` / `Enter`：展开/折叠目录，或确认打开文件。

### QuickLook 辅助键
* `Enter`：从预览态进入极简编辑态。
* `Esc`：从编辑态退回预览态，或关闭 QuickLook 浮层。
* `Ctrl+S`：在编辑态下保存文件。

---

## 9. 配置文件与自定义

配置文件采用 TOML 格式。主配置文件路径为：
```
%APPDATA%\Tn\config.toml
```
自定义主题文件存放于：
```
%APPDATA%\Tn\themes\*.toml
```
首次启动 Tn 时，系统会自动在上述路径下释放默认配置文件与 `Tn Dark` 默认主题文件。

### 常见自定义配置项说明
* `[general].scrollback_lines`：配置终端每个会话的最大滚动历史行数，默认 50000 行。
* `[general].billing_mode`：配置用量药丸的默认计费维度，可选值为 `auto`、`api` (显示美元 $)、`subscription` (显示百分比 %)、`tokens` (显示 tokens 数)。
* `[font]`：可更改 `family` (字体族，默认 `JetBrainsMono Nerd Font` 且已内置)、`size` (字号，默认 14.0) 和 `line_height` (行高倍数，默认 1.3)。
* `[editor].animations`：编辑器打字时的光标动画级别，可选 `off` (无动画)、`subtle` (轻量平滑动画，默认值)、`full` (完整动效)。当系统开启“减弱动态效果”时，自动降级为 `off` 以保证输入性能。
* `[quick_terminal]`：可在此配置全局下拉终端的 `hotkey` 唤出键、`position` 滑出方向 (top/bottom/left/right/center) 以及高度/宽度占比。
* `[[agents]]` / `[[profiles]]`：可通过追加对应 block 自定义内置启动项（Shell 路径、WSL 实例、远端服务器 SSH 配置及特定 AI 智能体前置参数）。

---

## 10. 调试与排错

### 运行日志
如果遇到崩溃或异常，可查看应用自动记录的本地日志：
```
%APPDATA%\Tn\logs\tn.log
```
在 Debug 模式构建下，日志会同时输出到启动控制台；在 Release 模式下，控制台会被隐藏，错误和 panic 栈会写入上述文件中。

### 内部环境变量开关
开发与调试时，可使用以下环境变量运行 `tn.exe` 或 `tn-cli` 进行特定行为控制：
* `TN_AUTOQUIT=1`：使 GUI 窗口在完成启动加载后自动退出，常用于 headless CI 管道测试。
* `TN_DEMO=1`：进入脚本演示模式，自动按步骤模拟输出彩文、多标签切换、分屏和 resize 行为。
* `TN_QL_BENCH=<file_path>`：打开 QuickLook 渲染指定的本地文件并在几秒后自动关闭，用以测试大文件渲染性能和防卡死机制。
* `TN_QL_LEGACY=1`：强制 QuickLook 回退到传统的渲染管线，而非默认的自绘极简编辑管线。
