# Changelog — Tn 终端

本文件记录 Tn 各里程碑的变更,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。
版本对应开发蓝图([docs/架构蓝图.md](docs/架构蓝图.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 **Windows 优先、Rust、GPU 加速**的终端,为 vibe coding 设计:托管 Claude Code /
> Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite)·
> alacritty_terminal(VT 引擎)· portable-pty(ConPTY)· russh(SSH,M2)。许可证 GPL-3.0-or-later。

**当前状态(2026-05):M0–M5 全部落地**(执行顺序 M0→M1→M3→M4→M5→M2)。M1 已 tag 为 `[0.1.0]`;
M3/M4/M5/M2-WSL 在 `main` 上以单次提交落地(下方各 `[Unreleased]` 段,**新里程碑在上**),尚未打新 tag。
**唯一未完成:M2 的 SSH**——已编译 + headless 单测,owner 决定暂停(parked),等有远程登录需求再做端到端。

## [Unreleased] — SFTP 远端文件服务与远端改动流首版(2026-06)

### Added
- **`tn-editor` headless 编辑核心首版**:新增 workspace crate,无 GPUI 依赖;迁移 Quick Look 的 `char_to_byte`、`line_chars`、insert/newline/backspace/delete/move/page/delete-range/insert-multiline 等纯文本编辑函数,并新增 `Document`、`Selection`、`CursorSet`、`EditTransaction`、`UndoStack`、`SearchState` headless 模型和独立单测。`tn-ui::quick_look` 编辑态已通过 `QuickLookEditState` 薄壳把打字、选区、剪切/粘贴、查找/替换、撤销/重做和保存接入 `Document` 主状态;旧 `uniform_list` renderer 暂继续读取适配后的 `Rc<Vec<String>>` 快照,为 LineLayout / EditorElement 上移打基础,用户可见行为保持不变。
- **`tn-pty::remote_fs` SFTP v3 后端**:`RemoteFileService` + `SftpFileService` 支持 SSH 远端列目录、有界读文件、stat 元数据和写回文件,复用 SSH 配置/密钥/内存密码,后台探测不弹 host-key/password UI。
- **`tn-pty::remote_cmd` 远端命令执行**:新增有界 SSH exec 服务,支持 POSIX shell quoting、stdout/stderr/exit-status 捕获和 stdin 传入,供远端 git / hunk patch 使用。
- **SSH Explorer root**:`ExplorerRoot::Ssh` / `ExplorerPath::Remote` 用 `RemoteId` 表示远端路径,焦点 SSH pane 的 cwd 可驱动左侧远端文件树;展开目录通过 SFTP 枚举,不再把 `/home/...` 伪装成本机 `PathBuf`。
- **Quick Look 远端预览 + 编辑写回**:Explorer 打开远端文件时走 `QuickLook::open_remote`,最多读取 `REMOTE_READ_LIMIT`;文本文件可进入编辑态,保存前用 SFTP stat + 内容 hash guard 检测远端变化,冲突时显示「重新载入 / 取消 / 覆盖远端」,避免静默覆盖。
- **Quick Look 本地 guarded save**:本地文本打开时记录 `TextFormat` 与 `FileGuard`;保存前检测磁盘外部修改/删除,冲突时显示「重新载入 / 取消 / 覆盖保存」,正常写回保留原编码与 LF/CRLF 风格。
- **远端 git 数据流**:`remote_git` 通过远端 `git diff --numstat` 渲染 SSH agent 活动栏,改动卡可打开远端 full diff;新增 hunk 解析、单 hunk patch 构造和 `git apply -` stdin helper。

### Changed
- **SSH pane 的「打开文件夹」**不走本机 picker:打开当前远端 root 的应用内 SFTP 目录 picker,支持父目录、目录过滤、刷新/错误状态和确认/取消;确认后向目标 SSH pane 发送远端 `cd`,仍保持只影响当前焦点 pane。

### Added(2026-06-07 续:远端 hunk 可视按钮 + 刷新 + 失败提示)
- **Quick Look Diff tab 远端 hunk「接受 / 拒绝」按钮**:仅在远端 diff(`remote_diff_file` 存在)时,每个 `@@` 行右侧渲染接受(绿)/拒绝(红)按钮。点击 → `QuickLook::apply_hunk`:后台**重新拉取**当前 `git diff` → `remote_git::parse_file_diff` → `apply_remote_hunk` 经 SSH 跑 `git apply --cached -`(接受)/ `--reverse -`(拒绝),patch 走 stdin。`DiffLine` 新增 `hunk_index`,`parse_diff` 与 `remote_git::parse_file_diff` 同序计数 `@@` 保证点击行映射到正确 hunk(单测 `parse_diff_numbers_hunks_in_lockstep_with_remote_file_diff`)。
- **应用后刷新**:成功 → `diff_dirty` 重拉 diff(已应用/撤销 hunk 自动消失)+ `QuickLookEvent::RemoteChangesDirty` → workspace 刷新每个 pane 的「本次改动」(远端经 `changes_for_remote`)+ `explorer.mark_stale()`,与本地保存同路径(远端 FS 编辑文件监听不可见)。
- **失败提示 + 防并发**:apply 期间 `hunk_busy` 禁用按钮(防双击发两条冲突补丁);失败 → `hunk_error` 红色横幅(复用 save_error 范式,显示 `git apply` stderr + 关闭);开新文件即清。

### Fixed(2026-06-07 续:真机连 SSH 后暴露的远端文件树/picker bug)
- **标签切换 / 最小化恢复后终端光标偶发落到左上角**:终端正文 canvas 在隐藏 / 恢复帧可能回报很小但非零的 bounds,旧 resize 逻辑把不足一个 cell / row 的区域兜成 `1x1` 并写给 alacritty + ConPTY。新增 `fit_grid_size_from_bounds`,不足一个完整单元格 / 行的临时 bounds 本帧跳过 resize,避免真实 PTY 被缩成 `1x1` 后覆盖历史。进一步收紧鼠标命中:只有 `BODY_PAD` 内侧的真实网格矩形才映射成 cell,恢复帧临时 `(0,0)` / padding / 右下空白不再 clamp 到左上角或最后一格;文本拖选也补 `pressed_button` 兜底,标签切换/最小化吞掉 `mouse_up` 后下一次 move 会结束旧拖选状态,不继续改历史区选区。
- **scrollback 光标误投影与 Codex 滚轮翻出重复旧帧**:`Terminal::snapshot` 现在在 `display_offset > 0` 时隐藏 live cursor;输入前回到底部改按 `display_offset > 0` 判断,避免滚到历史顶部(`offset == history`)时仍停在历史视图。agent / alt-screen pane 的鼠标滚轮改交给程序自身(方向键),普通 shell 才滚 Tn scrollback;ConPTY grow resize 增 `ResizeAnchoring`,普通 shell 顶锚定保历史,agent / TUI 底锚定避免把当前主屏帧推进 scrollback。
- **SSH 文件树 / 「打开文件夹」一直停在 Windows 主机目录**:`TerminalView::cwd()` 只读逐 block cwd(`current()`/`last_finished()`),而 SSH 注入脚本只发裸 `OSC 633;P;Cwd`(无 A/B/C/D 标记)→ 不建 block → cwd 恒 None。修:`cwd()` 兜底 `BlockModel::cwd()`(模型级);注入脚本钩好后立即 `__tn_pc` 当下报 cwd。
- **WSL「打开文件夹」开成 Windows 原生选择器**:把 `RemoteDirPicker` 泛化为 `PickerSource::{Ssh,Wsl}` + `PickerEntry`(解耦 SSH `RemoteId`),WSL 经 `\\wsl$\<distro>` 本地 `std::fs` 列目录。`open_folder_should_use_native_picker` 现只对 Host/欢迎页返回 true;SSH 与 WSL 都走应用内导航 picker。`fallback_remote_root` 在 cwd 未知时从 `/` 起。
- **远端目录 picker 无法切目录 + 列表被裁切**:① 顶部加可点击「`..` 上级目录」行(鼠标上行路径);② 目录列表改 `uniform_list` 虚拟化 + `track_scroll`(滚轮可滚),键盘 `↑↓` 配 `scroll_to_item(Center)`。
- **远端目录 picker 键盘完全无反应(真凶)**:`Workspace::render` 的「焦点反射块」gate 在 `overlay_focused`,该列表**漏了 `remote_dir_picker`** → picker 开着时该块判定「无 pane 持焦点」→ 每帧 `workspace_focus.focus()` 把焦点从 picker 抢回根 → `on_key_down` 永不触发。修:`overlay_focused` 加 `remote_dir_picker.is_some()`。附:`disable_ime` 也补 picker/split/layout/palette(无 `EntityInputHandler` 的导航浮层须关 IME,免活动 CJK IME 把导航键当 `VK_PROCESSKEY` 吞掉)。

### Fixed(2026-06-08:TnE-12 — 自绘路径查找 parity:跳转/高亮/中文输入/候选框)
- **查找跳转**:`find_next` 在 `el_render` 下走新增 `el_center_row`(命中行滚到视口纵向居中)并 pin `last_follow_cursor`,防 render 顶的 `el_follow_caret` 把它边缘弹走(旧实现只调 `uniform_list::scroll_to_item`,自绘路径无效 → 不跳)。
- **命中高亮(突出显示)**:`file_element` 实时算 `all_matches(query)`,`paint_file_preview` 加 `matches` 参数,逐行画底色(与当前命中的选区 `accent` 异色)。此前自绘只画选区(当前一处),无「全部命中」高亮。**底色不醒目复修**:从 `cola(accent_alt,0.20)` 纯填充升为 `gpui::quad`(`accent_alt 0.38` 填充 + `0.85` 1px 描边 + 2px 圆角),在密集语法着色行上也清晰可辨。
- **查找横向跟随**:命中落在需横滚才可见的长行位置时不跳过去——`find_next` 在 `el_center_row`(纵向居中)外新增 `el_reveal_col`(命中列超出视口时把 `hscroll_px` 横向居中到该列),纵横双轴都跟随。
- **查找框输入中文**:根因——查找开时不注册 IME handler、且 `on_key` 对可打印键 `stop_propagation`(gpui 跳过 `translate_message`,微软拼音无法合成,同编辑器旧坑)。修:`find_key` 只接管 Esc/Enter/Tab/Backspace 返回 `handled`,**可打印键放行**给 IME handler;`handle_input` 改 editing 即注册(不再 gate `!find_open`);`replace_text_in_range` 按 `find_open` 把提交/合成文本路由进 `find_query`/`replace_query`(`find_input`),查找条 active 字段回显 `ime_marked` preedit、正文不重复画。`cargo test -p tn-ui --lib` 140 测全绿。真机待:拼音打查找 + 高亮 + Enter 居中跳转。
- **IME 候选框贴查找框(候选框 bounds parity)**:查找开时 IME 合成文本进的是查找框,但候选框此前定位在代码区光标(中文搜索时飘到正文)。新增 `find_field_bounds: Rc<Cell<Option<Bounds>>>`,find_bar 激活字段输入框挂占位 `canvas` 每帧写入窗口坐标;`bounds_for_range` 在 `find_open` 时据此把候选框对齐到框左下缘,否则回退原代码区光标定位。
- **「下线旧编辑 renderer」并入 TnE-13**:旧 `uniform_list` 路径仍被 Diff tab 共用、且是 CLAUDE.md 明列的 `TN_QL_LEGACY=1` 紧急逃生口;此刻删除会同砸逃生口与 Diff。故旧编辑分支保留为 env 门控逃生口,整路 `uniform_list` 下线随 TnE-13(Diff 自绘)一并做。

### Changed(2026-06-08:TnE-10 收尾 + TnE-11 自绘编辑器,默认翻转)
- **自绘 File 预览/编辑器现为默认**:`el_render` 默认开(`new()` 看 `TN_QL_LEGACY` 未设);`TN_QL_LEGACY=1` 强制回退旧 `uniform_list`(紧急逃生口,Diff 仍用旧路)。File 预览的选区/复制/CJK 命中/横滚已真机签收(owner)。
- **TnE-11 编辑态自绘**:`file_element` 同时承载预览 + 编辑——编辑态行源走 `edit` 镜像(借用、不每帧深拷);`paint_file_preview` 加 `editing/caret/ime`:画瞬时反相块 caret(foreground 底 + chrome_bg 字)、选区底色、IME preedit(composing 串覆盖绘制 + accent 下划线);editing 时 `window.handle_input` 注册输入处理器(中文合成/WM_CHAR);新增 `el_follow_caret`(cursor 变才跟随的去抖,纵 `el_scroll_y` + 横 `follow_h_offset`)。输入 transaction 经 `Document`。`cargo build --workspace` + `cargo test -p tn-ui --lib` 140 测全绿。真机待验:连打/中文 IME/选区/查找滚动/保存;问题回退 `TN_QL_LEGACY=1`。完整 IME/鼠标/滚动 parity 收尾 = TnE-12。

### Added(2026-06-08:TnE-10 自绘预览选区/复制/CJK 命中 + 横滚轮)
- **自绘 File 预览的只读选区 / 复制 / CJK 命中**(`TN_QL_ELEMENT=1`):`file_element` 加鼠标 down/move/up——行号由 y 反推(含纵向滚动、clamp)、列经 `caret_col_at_x`(点击)/`hover_char_at_x`(拖选,含光标字符语义)+ `hscroll_px` 偏移 + CJK 双宽,`pressed_button` 兜底结束拖选;`paint_file_preview` 加 `sel` 画 `cola(accent,0.22)` 选区底色。复用既有 `place_cursor`/`copy`/`select_all`,`Ctrl+C`/`Ctrl+A` 即生效。另补 Shift+滚轮 / 触控板横向滚动 + **横滚条 thumb 可拖拽**(底部 14px 命中条 + `h_scroll_thumb`/`h_offset_from_drag`,点 thumb 抓取 / 点轨道跳转)。`cargo test -p tn-ui --lib` 140 测全绿。真机已验选区/复制/CJK 命中(2026-06-08 owner);旧 `uniform_list` File 路径暂留默认 fallback,真机确认 parity 后再下线。

### Added(2026-06-08:TnE-09 只读自绘 File 预览,env 门控)
- **`TN_QL_ELEMENT=1` 自绘只读 File 预览**:新增 `paint_file_preview` + `QuickLook::file_element`,File 只读预览可改走 GPUI `canvas` 自绘(行号右对齐 / 语法着色文本 / 横滚 thumb),用 `editor::{geometry,prepaint}` 模型按 1/2 列网格逐段 `shape_line`+`paint`(ASCII 连排、CJK 单字 2 列步进)复刻固定单元格防漂移;纵向 `el_scroll_y` 滚轮驱动 + clamp,横向复用 `hscroll_px`,文本经 `with_content_mask` 裁到 gutter 右侧。**默认关**——不设 env 即旧 `uniform_list` 路径(一键回退)。`cargo test -p tn-ui --lib` 140 测全绿、默认路径零改动;真机肉眼对照待 owner(选区/复制是 TnE-10)。

### Fixed(2026-06-08:真机验出的 Quick Look 编辑两 bug)
- **退出编辑回预览显示旧内容**(TnE-06 真机):预览从 `file_data` 渲染,而编辑只改 `Document`/编辑缓冲,escape 退出编辑时没把缓冲镜像回 `file_data` → 预览停在编辑前的旧文本(要重开文件才刷新)。新增 `QuickLook::sync_preview_from_edit`:escape 退出编辑时(仅 dirty 时)把编辑缓冲写回 `file_data` 并清按行的高亮缓存;**不写盘、不动 dirty**(保存冲突/dirty-close 守卫仍见未保存改动)。
- **OS 关窗绕过 dirty-close 守卫**(TnE-03 真机):标题栏 ✕ / Alt+F4 走 `window_control_area(Close)` 由 OS 直接关窗,从不经 `request_quit` → 有未保存 Quick Look 编辑时静默丢失(只在用 ✕/Alt+F4 关窗时复现,故「偶尔」)。在 workspace reveal 块注册 `window.on_window_should_close`:`quick_look_open` 时返回 `quick_look.request_quit()`(干净放行 / 脏阻止并弹保存提示,选保存或放弃后经 `QuitConfirmed` → `cx.quit()`)。app 内 关浮层/切文件/切 Diff/菜单退出/Ctrl+Shift+Q 早已守卫。

### Added(2026-06-08:TnE-13 Diff 装饰 + hunk 跳转模型)
- **`tn-ui::editor::diff` 装饰/导航模型**:新增 `DiffRowKind{HunkHeader|Addition|Deletion|Context|Meta}`(`gutter()`/`is_content()`)、`classify_diff_line`(`+++`/`---` 文件头先于 `+`/`-` 内容判定,meta 涵盖 git 头/mode/rename/二进制/no-newline)、hunk 跳转 `hunk_header_rows`/`next_hunk`/`prev_hunk`。纯 headless,8 个单测;`cargo test -p tn-ui --lib` 140 测全绿。只读 Diff renderer 接入(按 DiffRowKind 上色,复用 prepaint)+ 真机对照留待真机轮;不做 accept/reject。

### Added(2026-06-08:TnE-17 [editor] 配置 + motion policy)
- **`tn-config [editor]` 段 + 降级策略模型**:新增 `EditorAnimations{Off|Subtle|Full}`(默认 `subtle`)、`Editor{animations}`、`EffectiveMotion{Instant|Subtle|Full}` 与 `Editor::effective_motion(reduced_motion,high_load)`——`off`/OS 减少动态效果/高渲染负载一律降级为 `Instant`(光标瞬时反相块=TnE-12 基线),否则 `subtle`/`full` 透传。默认 `config/config.toml` 加 `[editor] animations` 段。3 个单测;`cargo test --workspace --lib` 全绿(tn-config 42)。不绘动画、不改命中(动画效果是 TnE-18)。

### Added(2026-06-08:TnE-15 LineLayout 软换行 headless 模型)
- **`tn-editor::line_layout` logical→visual 映射**:新增 `WrapMode{None | Word{width_cols}}`、`VisualLine`、`LineLayout::build`(贪心词换行:空格优先 / 硬断 / 超宽单字独占 / CJK=2 列 / 空行保一条)+ `logical_to_visual`(换行边界归下一行首)/`visual_to_logical`/`range_segments`(选区·查找 TextRange→跨 visual·跨 logical 的局部列段)/`hit_test`(visual 行内 CJK 命中)。纯 headless 模型,9 个单测;`cargo test --workspace --lib` 全绿(tn-editor 27)。不接 GPUI renderer、不改代码文件默认横滚(软换行接入 File/Edit 是 TnE-16)。

### Added(2026-06-08:TnE-09 只读 prepaint 渲染模型)
- **`tn-ui::editor::prepaint` 只读渲染模型**:新增 `editor/prepaint.rs`,产出只读 `EditorElement::prepaint` 所需的完整布局纯函数——`visible_row_indices`(按文档 clamp + 底部 +1 行)、`row_top`、`gutter_label`、`content_origin_x`、`prepaint_readonly`→`ReadOnlyPrepaint{content_w,max_off,h_offset(clamp),rows,thumb,content_x}`,忠实复刻 File renderer 的 content 宽至少撑满视口 / thumb `>8px` 可见门 / h_offset clamp,字段对齐 gpui `ShapedLine::paint` 以便 paint 层薄包装。5 个 headless 单测;`cargo test -p tn-ui --lib` 137 测全绿。GPUI `paint` + `TN_QL_ELEMENT` 门控接入 File tab + 真机对照留待真机轮(自绘渲染须真机肉眼验,headless 不盲写 paint)。

### Added(2026-06-08:TnE-08 编辑器只读几何/布局模型)
- **`tn-ui::editor` 几何模块骨架**:新增 `crates/tn-ui/src/editor/`(`mod.rs` + `geometry.rs`),把 Quick Look `uniform_list` renderer 内联的几何复刻成**纯函数 + 数据结构**(`Metrics`、`disp_width`/`prefix_cols`、`caret_x`、`content_width`、`max_h_offset`、`hover_char_at_x`/`caret_col_at_x`、`visible_rows`/`row_out_of_view`、`follow_h_offset`、`h_scroll_thumb`/`h_offset_from_drag`、`caret_abs_x`),为后续 `EditorElement`(TnE-09+)的 layout/prepaint 提供 Quick Look / Editor Pane / Diff Review 共享的可测模型。无 GPUI 依赖、未接入任何 render 路径(脚手架 `#![allow(dead_code)]`),8 个 headless 单测覆盖 CJK 列宽/caret x/content 宽/取整命中/可见窗/caret-follow/thumb↔drag 反函数;Quick Look 默认渲染与旧 `uniform_list` 不变。

### Performance(2026-06-08:TnE-07 编辑核心增量化守卫)
- **去除每键整 buffer 深拷,锁死「每键 O(1)」不变量**:复核确认增量机制随 TnE-05/06 已落地——`tn-editor::Document` 的 undo/redo 用 `EditTransaction` + 行区间 `EditSnapshot`(`capture_line_span` 只拷受影响行,不存整 buffer),连续打字按 `start_row` `coalesce` 成一条仍行有界的记录、移动光标即断开;Quick Look 薄壳 `sync_lines` 仅按 `last_transaction` 的行区间 `splice` 镜像,不再每键 `to_vec()` 整 buffer(仅开文件那一次兜底)。本轮补回归守卫把不变量钉死:`tn-editor` 新增 `continuous_typing_keeps_undo_records_line_bounded`(4000 行连打 500 字 + 换行,undo 栈合并为 1 条且 before/after 行数 < 8,移动光标后第二条记录仍有界),与既有 `undo_history_does_not_store_full_buffer_for_single_line_edit`、`tn-ui::edit_state_updates_line_mirror_without_replacing_whole_buffer`(`Rc::ptr_eq` 证镜像未被整 Vec 重建)合围。真机 4000 行连打手感待肉眼验证。

### Still TODO
- **SSH/SFTP 真机端到端回归(剩余)**:hunk 按钮真改远端 working tree、`git apply` 拒绝补丁的失败文案、并发/超时;远端目录 picker hunk 头随长行水平滚动(极长行需常驻浮起按钮)。远端文件树/「打开文件夹」/picker 键鼠导航 **已真机确认可用(2026-06-07)**。

## [Unreleased] — 面板解耦:per-pane 工作区上下文(2026-06)

让每个终端窗格拥有自己的「工作区上下文」,文件树状态不再被全局单例串台;「打开文件夹」只影响当前焦点 pane。

### Added
- **per-pane 文件树状态(展开态 + 选中文件)**:`ExplorerSnapshot`(`crates/tn-ui/src/explorer.rs`)+ `ExplorerView::snapshot()`/`switch_pane()`;Workspace 按 `PaneId` 存 `explorer_states` 快照、`explorer_pane` 记当前展示的 pane。焦点在分屏 pane 间切换时保存旧 pane、恢复新 pane 的展开/选中,各 pane 文件树互不串台;同 pane 内 `cd` 仍走 `follow_root`(保留子目录展开态)。快照在保存时惰性裁掉已关闭 pane,无需逐 `remove` 钩子。纯函数 `snapshot_under_root` 把恢复过滤到新 root 内(headless 单测覆盖)。

### Changed
- **「打开文件夹」收敛到焦点 pane**:`cd_panes_to_root`(广播给所有非 agent pane)→ `cd_pane_to_root(id, …)`(单 pane);`menu_open_folder` 只 `cd` + `set_rail_root` 当前焦点 pane,其它 pane 保持各自目录,agent pane 永不被 `cd`。SSH pane 跳过本机 picker,打开当前远端 root 的应用内 SFTP 浏览并只向该 pane 发送远端 `cd`。

> Agent 身份/用量环/「本次改动」/git watcher 早已 per-pane(在 `TerminalView` 上),本轮只验证不回归。逐项见 [docs/架构蓝图.md](docs/架构蓝图.md);坑 + 操作见 [CLAUDE.md](CLAUDE.md)。

## [Unreleased] — Agent Host 平台化(2026-06)

把「Claude/Codex 特判」重构为**对具体 agent 零知识的 Agent Host 平台**,分 P0–P6 落地(每阶段独立编译 + 测试绿)。

### Added
- **`tn-agent` 平台 crate(headless)**:`AgentId`(开放字符串身份,替代闭合 `AgentKind`)· `AgentDescriptor` + `AgentCapabilities` + `AgentRuntimeKind`(身份/能力插槽/运行位置)· `AgentEvent` + `AgentStatus`(UI 唯一输入契约)· `AgentAdapter` trait + `GenericAdapter`(有身份无遥测)· `AgentRegistry`(按 id/命令解析,空 = 纯 shell 宿主;`register_manifest` 注册 config agent)· `AiUsage` + pricing(从 tn-ai 上移)。
- **config `[[agents]]` manifest**:用户写 TOML 即可让新 agent 进启动器/头/能力插槽(`AgentManifest` → `AgentDescriptor::from_manifest`,无遥测);`runtime_support` 声明 PTY / structured / http / websocket / remote-daemon 支持,`allow_network = true` 只进入“需用户确认”策略而非静默放行;`[agents.<id>]` 主题色表 + `[general.billing]` 按 id billing 覆盖(`accent_for`/`billing_for`)。
- **`LaunchSpec::runtime()` + 非 PTY runtime 契约**:从 ssh/file_namespace 派生 `AgentRuntimeKind`(PTY 家族),并开放 `RemoteDaemon`/`Http`/`WebSocket`/`Structured`;runtime 与 `FileNamespace` 严格分离。
- **外部 agent 实时事件 adapter**:`ExternalEventAdapter` 接 JSONL → `AgentEvent`, `ExternalProcessAdapter` 可拉起 stdio 子进程并从 stdout JSONL 实时入队,stderr 转 `ErrorReported`;UI 只对 `has_realtime_events()` 的 adapter 启动轻量事件 poller。
- **配置可达的 sidecar 遥测 + 网络确认(最后一公里)**:`[[agents]] sidecar = "cmd"`(`tn_config::AgentManifest.sidecar`)→ `AgentDescriptor.realtime_command`;`AgentDescriptor::sidecar_launch()` 是默认拒绝网络的单点决策(`SidecarLaunch::{None,SpawnNow,Confirm}`)。launched agent pane 据此**每 pane** 拉起自己的 `ExternalProcessAdapter`(`from_descriptor`)+ 事件 poller:本地 stdio sidecar 直接起、**networked sidecar 弹确认卡**(`SidecarConfirm`,拒绝/允许并连接)。adapter 归 `TerminalView.realtime_adapter` 持有,`clear_agent`/drop 即杀子进程。这让**配置声明的 agent 也能有真实用量/transcript/权限**,无需内置 adapter —— 兑现「配置可达 + 网络确认」的最后一公里。诚实:应用内编辑器仍只产 generic(不收 sidecar),sidecar 是 config 高级项。
- **AgentEvent 高级渲染槽**:`StatusChanged` / `ModelChanged` / `TranscriptAppended` / `PermissionRequested` / `ErrorReported` 进入同一个 `reduce_agent_event` 漏斗,agent 头显示状态 / 权限 / 错误 / transcript 摘要 chip;`CwdChanged` 只更新目标 pane 的上下文和活动栏 cwd。
- **守卫测试** `agent_host::guard::ui_has_no_closed_agent_enum`:扫 `tn-ui/src` 锁死 UI 零闭合枚举。

### Changed
- **tn-ai → 内置 Claude/Codex `AgentAdapter`**(平台两个种子 provider,可移除):薄包装 `claude.rs`/`codex.rs` 解析;`detect.rs` 泛化为 `resolve_pane_session(&dyn AgentAdapter)`(launch 后 stale→fresh,第三个 agent 无需新 match)。`builtin_registry()` 保留但**默认 app 不再注册**(出厂无内置 agent)。
- **tn-ui 全面去 `AgentKind`**:身份走 `AgentId` + `AgentRegistry`(gpui Global);per-pane 缓存 `agent_accent`/`label`/`short`/`manages_cursor`/`caps` 经 `resolve_agent_view` 在 agent 变更时解析;`force_hide_cursor` → 描述符 `manages_own_cursor`;header 用量环 gate `caps.usage`、活动栏 gate `caps.git_diff`;状态栏按 `AgentId` 聚合(无固定 Claude/Codex 槽);用量轮询和外部实时事件都经 `AgentEvent` 归约器入账(内置日志 adapter 仍不新增热路径)。

### Removed
- **闭合 `AgentKind` 枚举 + 其 kind-dispatch API**(`resolve_session`/`session_mtimes`/`parse_session`/`update_session`/`detect_subscription`/`agent_kind_for_command` 等)全删——agent 身份与解析全部走 `AgentId` + adapter。
- **`tn-ai` 对 Claude/Codex 原始解析器的 public re-export**:外部只拿 `ClaudeAdapter` / `CodexAdapter` / `builtin_registry`,具体 JSONL 解析函数继续留在 `tn-ai` 内部给 adapter 和单测使用。

## [Unreleased] — 应用内 Agent 编辑器(2026-06)

把「加 agent」从手编 `config.toml` `[[agents]]` 升级为应用内现代交互(用户反馈:编辑配置太极客)。建立在 Agent Host 平台之上 —— 编辑器只产出 config 数据,平台零改动。

### Added
- **欢迎页 launchpad「+ 添加 Agent」磁贴 + 居中玻璃浮层编辑器**(`workspace::render_agent_form`):收集 名称 / 命令 / 颜色(`tn_config::ACCENT_SWATCHES` 预设)/「由 Agent 自绘光标(Ink TUI)」开关 + 实时磁贴预览;`Tab` 切字段 · `Enter` 保存 · `Esc` 取消 · 点外关闭。名称字段支持中文(IME,复用 `EntityInputHandler` 多路复用,与 SSH 重命名同源),命令字段 ASCII(IME 关)。
- **自定义磁贴 hover ✎/✕**(`welcome::agent_tile_actions`):编辑预填表单回写、删除抹掉 `[[agents]]`+`[[profiles]]`(`EditAgentRequested`/`DeleteAgentRequested`/`AddAgentRequested` 事件回 workspace)。
- **保存即生效(无需重启)**:`workspace::reload_agents` 重读 config → 重建 `AgentRegistry` global → 刷新 `launch_profiles` → 重建 welcome(`subscribe_welcome` 复用订阅),新磁贴立即出现。
- **tn-config 持久化**:`append_agent[_to]` / `remove_agent[_from]`(块级追加/删除,保注释,泛化的 `block_ranges`)+ `agents_toml_fragment` + `ACCENT_SWATCHES` 颜色预设。
- **id 派生**:`slugify`(名称→命令首词,deduped)生成稳定 `AgentId`;命令首词进 `aliases` → 「shell 里敲它自动切 Agent 态」即时可用。诚实:config-only agent = generic(无用量遥测,需内置/外部 adapter)。
- **claude/codex 命令的配置 agent 自动获得真实用量环(2026-06)**:`agent_host::build_registry`(startup 与 `reload_agents` 共用)对每个 manifest 先试 `tn_ai::builtin_adapter_for_manifest` —— 命令/别名命中 claude/codex → 用内置日志解析器 + **用户自己的颜色/名称/Ink**(`ClaudeAdapter/CodexAdapter::with_descriptor`,`usage` 强制开),否则 generic。**用户「+ 添加 Agent」填命令 `claude` 即自动出用量环,不需要 sidecar、不需要懂遥测**。平台 `tn-agent` 仍零 agent 知识,只 `tn-ai` 认名字。
- **进阶项也在 GUI(2026-06)**:编辑器加「**高级 · 用量遥测**」(文案明示「不懂就留空」)—— `AgentField::Sidecar` 文本字段 + 「联网 sidecar」开关。留空 = generic;设了 sidecar(开发者用:自备吐 JSON 用量的伴随程序)→ `capabilities=["usage"]` + 写 manifest `sidecar`,勾联网 → **只设 `allow_network=true`**(启动走确认卡)。这样进阶遥测也纯图形可配,不必手编 config.toml。
- **设计真源**:[`design/panels/04-overlays.html`](design/panels/04-overlays.html) 新增编辑器原型(含高级遥测区)。

### Fixed
- **联网 sidecar 把 agent 开成 shell**:GUI「联网 sidecar」曾写成 agent 的 `runtime_support=["remote_daemon"]`(非 PTY)→ PTY 启动器「不支持 LocalPty 即拒绝」守卫把 `claude` 这种命令型 agent 当非 PTY 拒了 → 回退 pwsh shell。修:sidecar 的网络属性只走 `allow_network`(`AgentDescriptor::sidecar_launch` 改为只看 `network_policy`,与 `runtime_support` 解耦);命令型 agent 的 `runtime_support` 永远是 PTY。旧版存的 agent 重新保存一次即修。
- **一个 agent 显示成两张磁贴**:旧版默认 config 出厂带 claude/codex profile;用户经编辑器声明同名 `[[agents]]` 后,`is_removed_builtin_agent_profile`(只隐藏「无 manifest 的内置遗留」)对它失效 → 遗留 profile + 新增 profile = 两张。修:`discover_profiles` 增 `dedup_agent_profiles` —— 按 agent id 去重,**保留最新保存的一条**(非 agent 的 shell/WSL/SSH 不去重)。即便 config 仍有遗留条目也只显示一张。
- **欢迎页「打开文件夹」失效**:欢迎 launchpad 的焦点「pane」是 `WELCOME_DUMMY`(`PaneId::MAX`,无 `LaunchSpec`),`open_folder_should_use_native_picker(None)` 旧返回 `false` → 走 SSH 式「应用内浏览」分支并提前 return,**原生文件夹选择器从不弹出**。修:无 spec(=欢迎页)默认用原生选择器 → 选目录即重定 explorer root → 欢迎页磁贴启动的 agent/shell 继承该 root 为 cwd(无同级 pane 可继承时回退 explorer root),实现「选目录 → 点磁贴在该目录起 agent」。

### 后续(未做)
- Agent Protocol / JSON-RPC 的完整请求-响应语义、HTTP/WebSocket 网络客户端和 tool-call/checkpoint Inspector。当前已落地的是 stdio JSONL 事件 adapter + 网络 runtime 安全契约。
- Agent 编辑器:命令参数/cwd 字段 · sidecar 命令的带引号路径(当前 `split_whitespace`)· 编辑器内中文搜索 · 在 Quick Terminal/分屏启动器也暴露「+ 添加」。
