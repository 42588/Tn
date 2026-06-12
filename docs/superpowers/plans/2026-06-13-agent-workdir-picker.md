# Agent 工作目录选择实现计划

## 目标

幽灵终端和主窗口欢迎页的 Agent 磁贴在启动前必须先选择本地主机工作目录,避免 Agent 只能进入默认目录。

## 架构

新增共享 `local_dir_picker` 状态模块,负责目录导航、最近工作目录和本地目录读取。幽灵终端使用卡片内单层二级面板;欢迎页使用临时 overlay,两处最终都通过 `LaunchSpec::with_cwd` 注入 cwd。

## 任务 1:共享选择器状态

文件:

- 新增 `crates/tn-ui/src/local_dir_picker.rs`
- 修改 `crates/tn-ui/src/lib.rs`
- 修改 `crates/tn-ui/src/terminal_view/launch.rs`

清单:

- [x] 增加 `LocalDirPicker`、`LocalDirFocus`、`LocalDirAction`、`WorkdirRecents` 和 `read_local_dirs`。
- [x] 覆盖 `Tab` 焦点循环、当前区域 `↑↓` 移动、`←/→` 导航、最近目录排序/seed、git 目录标记、Windows 盘符入口、当前高亮目录启动 cwd。
- [x] 增加 `LaunchSpec::with_cwd` 和回归测试。

## 任务 2:幽灵终端接入

文件:

- 修改 `crates/tn-ui/src/quick_terminal.rs`

清单:

- [x] 给 `QuickTerminal` 增加 `local_dir_picker: Option<LocalDirPicker>`。
- [x] 拦截 Agent `PickerItem::Launch`,打开本地目录选择器而不是直接启动。
- [x] 在目录选择器打开时路由 `Tab`、`↑↓`、`←`、`→`、`Enter` 和 `Esc`。
- [x] 渲染 Ghost 卡片内工作目录面板,包含最近目录、子目录、系统目录选择器和启动按钮。
- [x] 使用 `LaunchSpec::from_profile_ephemeral(...).with_cwd(...)` 启动 Agent,并记录最近目录。

## 任务 3:欢迎页 overlay 接入

文件:

- 修改 `crates/tn-ui/src/workspace.rs`

清单:

- [x] 拦截欢迎页 Agent `LaunchRequested`,打开 Workspace 级 `agent_dir_picker`。
- [x] 以 Explorer 当前 Host root 作为最近目录 seed。
- [x] 渲染临时 modal overlay,不移动欢迎页原布局。
- [x] 复用幽灵终端相同键位规则,并在导航 overlay 中关闭 IME。
- [x] 使用 `LaunchSpec::from_profile(...).with_cwd(...)` 启动 Agent,并记录最近目录。

## 任务 4:验证与文档

文件:

- 修改 `TODO.md`
- 修改 `docs/任务/2026-06-12-BUG发现清单处理.md`
- 新增 `docs/任务/2026-06-13-Agent工作目录选择.md`
- 新增 `docs/superpowers/specs/2026-06-13-agent-workdir-picker-design.md`
- 新增 `docs/superpowers/plans/2026-06-13-agent-workdir-picker.md`

清单:

- [x] `cargo fmt`
- [x] `cargo test -p tn-ui local_dir_picker::tests`
- [x] `cargo test -p tn-ui with_cwd_sets_spawn_directory`
- [x] `cargo test -p tn-ui quick_look_open_counts_as_focus_freezing_overlay`
- [x] `cargo test -p tn-ui --lib`
- [x] `cargo build -p tn-ui`
