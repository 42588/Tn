# 真机截图 · 目录约定

本目录按**页面/screen**组织真机回归截图,每个子目录对应一个页面;同一页面的多个子状态(如 QuickLook 的 File/编辑/Diff/CJK)放同一目录内的多张图。命名对照 `design/panels/*.html` 图纸与实现入口,便于逐页像素级回归(见 `design/原型与真机截图差异总结.md`)。

> 放图后可删除各目录里的 `.gitkeep` 占位文件。

| 目录 | 对照图纸 | 实现入口 | 该拍的子状态 |
|---|---|---|---|
| `窗体外壳` | SHEET 01 窗体外壳 | `tn-ui/src/workspace.rs`(titlebar/statusbar)· `lib.rs` | 标题栏 + Tab 身份棒 + 窗控 · App Menu 展开 · 状态栏读数 |
| `工作区多板面` | SHEET 02 A 三栏总装 | `tn-ui/src/workspace.rs`(render_node/plate) | Explorer + Agent + Shell 同屏 · 2px 接缝 · 焦点角标 |
| `Agent板面` | SHEET 02 A/B Agent | `tn-ui/src/terminal_view/`(header.rs 身份头/用量环/活动栏) | 身份头 + model/context chip · 活动栏(git 改动卡)· 运行态 |
| `Shell板面` | SHEET 02 A · SHEET 07 B | `tn-ui/src/terminal_view/` · `block_view.rs` | 空 shell · 命令块 ok/run/fail 三态 · 运行块条 |
| `QuickLook编辑器` | SHEET 03 A/B/C | `tn-ui/src/quick_look.rs` | File 预览 · 编辑(选区/光标/未保存/冲突)· Diff hunk · CJK 固定单元格 |
| `幽灵终端` | SHEET 04 B/C | `tn-ui/src/quick_terminal.rs` | 启动器(GHOST_ 头 + 磁贴)· 运行态(760 顶垂窗 + 顶缘磷光 + 残影) |
| `宠物系统` | SHEET 05 | `tn-ui/src/pet.rs` | 欢迎页 2× 形态 · 状态栏席位 · 上下文状态 · 右键菜单 |
| `命令面板` | SHEET 06 A | `tn-ui/src/workspace.rs`(render_palette) | 输入行 + 结果行 · 选中左脊 · scrim |
| `分屏启动器` | SHEET 06 B | `tn-ui/src/workspace.rs`(render_split_launcher) | 方向格选择 · profile 行(逐行选中 L4+左脊) |
| `SSH连接器` | SHEET 06 C | `tn-ui/src/quick_terminal.rs`(render_ssh_prompt) | user@host 输入 · 最近/收藏记录(逐行选中)· 密码/密钥 chip |
| `SSH过程态` | SHEET 07 C | `tn-ui/src/terminal_view/mod.rs`(progress/TOFU/error/banner) | 三步进度(横排 .steps)· TOFU 指纹 · 认证失败 · 断线重连横幅 |
| `欢迎页` | SHEET 07 A | `tn-ui/src/welcome.rs` | 常驻 Explorer + 居中 Launchpad · 150 磁贴 · NO SESSION/N SESSIONS · 宠物 2× |
| `WSL二级启动器` | SHEET 07 A2 | `tn-ui/src/welcome.rs`(wsl_open 下钻) | 返回 tile + 发行版 tile · N SESSIONS 状态栏 |
