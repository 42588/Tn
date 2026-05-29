# Changelog — Tn 终端

本文件记录 Tn  各里程碑的变更,遵循 [Keep a Changelog](https://keepachangelog.com/) 风格。
版本对应开发蓝图([docs/架构蓝图.md](docs/架构蓝图.md) §8)的里程碑。日期格式 `YYYY-MM-DD`。

> Tn 是 **Windows 优先、Rust、GPU 加速**的终端,为 vibe coding 设计:托管 Claude Code /
> Codex 等 AI CLI,灵活平铺,原生 WSL + SSH。技术栈:GPUI(DX11 + DirectWrite)·
> alacritty_terminal(VT 引擎)· portable-pty(ConPTY)· russh(SSH,M2)。许可证 GPL-3.0-or-later。

**当前状态(2026-05):M0–M5 全部落地**(执行顺序 M0→M1→M3→M4→M5→M2)。M1 已 tag 为 `[0.1.0]`;
M3/M4/M5/M2-WSL 在 `main` 上以单次提交落地(下方各 `[Unreleased]` 段,**新里程碑在上**),尚未打新 tag。
**唯一未完成:M2 的 SSH**——已编译 + headless 单测,owner 决定暂停(parked),等有远程登录需求再做端到端。

---

## [Unreleased] — 启动卡聚合(WSL 一卡 / SSH 占位)+ 失焦重锚 + 速览相对宽(2026-05-30)

> 四个启动页(欢迎页 / Quick Terminal / 命令面板 / 新会话分屏)统一:发现的 WSL 发行版折成**一张 WSL 卡**
> (点开钻取选发行版、只有 1 个直接起),末尾一张 **SSH 占位卡**(后端 parked,点了 no-op),**agents
> (Claude/Codex)排在最前**。共享聚合层 `welcome::launch_entries` / `launch_rows`(headless 单测)。

### 新增 (Added)
- **启动卡聚合 + WSL 钻取 + SSH 占位**(四个启动页统一):`launch_entries`(profiles → 卡:agents 优先 → shell →
  WSL 折一张 → SSH 占位)+ `launch_rows`(命令面板/分屏用的扁平可搜索行,按当前层过滤;WSL 卡在打发行版名时也不消失)。
  **欢迎页 / Quick Terminal 两行排版**(agents 上、PowerShell/WSL/SSH 下);**命令面板 / 新会话**列表(agents 在前 +
  WSL 钻取 + SSH 占位)。点 WSL 卡 → 发行版二级选择器(`‹` 标题/Esc 返回),**只有 1 个发行版直接启动**。新增图标 `chev-l`。

### 修复 (Fixed)
- **失焦后唤不出命令面板**(三层):焦点锚点 `track_focus` 放**窗口根**(整窗,点哪儿都重锚)+ 标题栏拖拽条 `.occlude()`
  (保住 NC 拖窗)+ `on_focus_out` 在焦点掉成 `None` 时立即重锚(覆盖关浮层/程序性失焦等非点击失焦)。详见踩坑。
- **命令面板焦点 / 光标位置**:浮层聚焦改「开着且未聚焦每帧重抢」(幂等、不循环;一次性首帧偶尔没落上 → 键漏给底层
  shell);输入光标移到**插入点**(空时在最前,不再飘在占位文字「搜索…」之后)。
- **Quick Terminal 启动器底部提示被裁**:`card_height` 按中文行高(比 ASCII 高)调宽,两行排版下余量由 `flex_1` 吸收。
- **文件速览/编辑器最大化失衡**:浮层右边距从绝对 `right(64)+max_w(880)` 改**相对** `right(relative(0.07))` →
  随窗口同比例缩放,默认观感不变,放大时不再卡死 880 缩在左边。

### 未完成 (TODO)
- **命令面板中文搜索**:面板还没接 IME `EntityInputHandler`(同终端「打中文」那套:合成态 + 提交进 query + canvas
  注册 `handle_input` + `on_palette_key` 放行可打印键),目前只能搜英文。parked 待做。

## [Unreleased] — 中文输入根治(VK_PROCESSKEY 路由)+ 打字光标平滑滑动 / 字符淡化(2026-05-29)

> 前几轮在 `on_key` 的「放行集」里反复横跳(空格放行、退格放行又改回编码)其实是**打地鼠**:gpui 0.2.2 走旧
> IMM32、判定合成只认 `GCS_COMPSTR`(微软拼音从不发),`is_composing` 恒 false → 每键先进 `on_key`,我们只能
> **猜**该编码还是放行,合成期的退格/回车/方向必然猜错。**WT 走 TSF**(IME 在键到应用前认领合成键)故无此问题。
> 本轮找到 OS 明示「此键属 IME」的信号 `VK_PROCESSKEY`,从根上路由,终结打地鼠。

### 修复 (Fixed)
- **中文合成期的退格/回车/方向键不再被终端抢走(`platform::install_ime_keyfix`)**:在 gpui wndproc 之前**子类化
  窗口**(`SetWindowSubclass`),凡 `WM_KEYDOWN` 且 `wParam == VK_PROCESSKEY`(IME 正在处理此键)就替它
  `TranslateMessage`(驱动 IME 合成/提交 → `replace_text` 写 PTY)并消费掉,gpui 永不会误编码它;IME 不要的键
  以真实虚拟键到达、原样透传给 `on_key` 照常编码。**IME 自己逐键决定要不要**,IME 无关、无需 fork gpui。主窗 +
  Quick Terminal 各装一次。诊断日志:命中 info `routed VK_PROCESSKEY`,每键 raw vk 在 `RUST_LOG=tn::ime=debug`。

### 新增 (Added)
- **打字/删除时光标平滑滑动(待优化清单 §3.1)**:光标块向目标格 ~90ms `ease_out_cubic` 缓动滑动,不再瞬移。
  **只对同行小幅移动**(打字/删除/本地导航)滑动;换行/清屏/远跳/首帧贴位,免「甩」过半屏。字形即时出现、只有
  光标块追上去 → 输入流畅。`CURSOR_GLIDE_MS` 可调感。
- **字符淡入/淡出(待优化清单 §3.1)**:逐帧 diff 可见网格,新字 ~75ms 淡入(bg 盖层 alpha 1→0 显影)、删字
  淡出(旧字残影 alpha 1→0)。**纯 overlay 实现(不拆 run)** + **一帧变化超 24 格(批量粘贴/清屏/滚动/程序
  输出)直接贴位不淡化** → 守住 perf 红线(无逐格 div 爆量)。仅聚焦窗格跑。`CHAR_FADE_MS` 可调感。
- `spawn_cursor_glide` / `spawn_cell_fade` 逐帧驱动复刻 `spawn_bell_fade`;`rows_to_cells`(宽字符→格)与
  `ease_out_cubic` 纯函数 headless 单测(tn-ui 58 测)。

## [Unreleased] — IME 退格删拼音 + 光标贴合中文(2026-05-29)

> `tn.log` 实证:微软拼音**从不发合成串(GCS_COMPSTR)**——`marked_text_range` 恒返回 `None`、`replace_and_mark`
> 从不触发,只在提交时发结果。故 gpui 的 `is_composing` 恒 false、**从不**把按键短路给 IME → 每个键都进 `on_key`。
> 这意味着:**所有 IME 合成期要用、且有 WM_CHAR 回退的键,都必须由我们放行**(不能 encode+stop)。

### 修复 (Fixed)
- **退格在合成时删拼音(终端 + 编辑器)**:`backspace` 改为放行 → 合成时 IME 吃掉它删拼音(不产生 WM_CHAR、不碰
  终端);非合成时经 WM_CHAR `0x08` 到 `replace_text_in_range`,**终端重映射为 `0x7f`(DEL,与 encode_key 一致)、
  编辑器调 `backspace()` 删除**。此前 backspace 被编码送 PTY,合成时删的是终端字符。
- **中文后光标贴合(消除累积间距)**:`tn-core::row_runs` 把宽字符拆成**独立 run**(每个 2 列盒),而非并入相邻
  run。原先整段中文是一个被强制定宽到 `列数×cell_width` 的 run,而 CJK 回退字体字形步进 < 2×cell_width → 多出的
  宽度全堆在 run 末尾 → 光标落在文字很远的右侧。逐格独立盒把这点细微差摊到字间、每格贴齐网格 → 光标紧跟文字。
  headless 单测更新。

### 已知限制 (Known limitations)
- 合成期间 **回车 / Esc / 方向键 / Tab / 翻页**仍走终端(不进 IME):方向键等无 WM_CHAR 回退,放行会破坏非合成态的
  终端导航;回车/Esc 对终端(运行命令 / vim)太关键、LNM/转义风险高。核心流程(拼音→退格纠错→空格/数字提交)完整。
- CJK 与 2×cell_width 的字形步进细微差表现为字间微距;要完全贴合需指定等宽 CJK 字体(`[font].fallback` 待接)。

## [Unreleased] — IME 提交键(空格)修复:中文候选词可按空格上屏(2026-05-29)

### 修复 (Fixed)
- **IME 按空格提交候选词(终端 + 编辑器)**:`on_key` 的「纯文本键放行」条件原是 `key.chars().count()==1`,但
  **空格的 `key` 是 `"space"`(命名键,非单字符)**,于是被 encode+`stop_propagation` → gpui 判定 keydown 已处理、
  **跳过 `translate_message` → IME 收不到空格 → 无法提交候选词**,只打出一个空格(数字/标点是单字符已放行,故只有空格
  这类命名键中招——正是「很多输入法自带方式都这样」)。**修**:把 `"space"` 也纳入放行(终端 defer;编辑器移除
  `"space"` 分支让其落到 `_ => 不处理`)→ 空格流向 IME 提交;非合成态则经 WM_CHAR 正常写一个空格。
- 加 `tn::ime` 定向日志(on_key 放行/编码 + `replace_and_mark`/`replace_text` + `marked_text_range`),便于核查
  IME 合成态跟踪是否还需放行更多键(回退/回车)。

## [Unreleased] — 中文渲染(宽字符对齐)+ IME preedit 内联 + 反相光标(2026-05-29)

> IME 输入通了之后,中文**显示**仍不对:每个汉字后有半角空隙、字符错位。根因 = **宽字符(CJK,占 2 格)
> 的 spacer 占位格被当普通空格渲染** + 网格按等宽 'm' 格宽 flex 排版、CJK 回退字体步进不符。

### 修复 (Fixed)
- **中文(宽字符)渲染对齐**:`tn-core::row_runs` 跳过 `WIDE_CHAR_SPACER`/`LEADING_WIDE_CHAR_SPACER` 占位格
  (消除每个汉字后的半角空隙),并给 `CellRun` 加 `cols`(列跨度:宽字符=2、余=1);`terminal_view` 渲染时每个
  run 框**显式定宽 `cols × cell_width` + `flex_none` + `overflow_hidden`**,强制贴齐单元格网格——即使 CJK 用
  回退字体(CaskaydiaCove 无 CJK 字形)、字形步进 ≠ 格宽,行内/行间也不再漂移。headless 单测覆盖。
- **IME 合成 preedit 内联显示**:合成中的拼音现在显示在**光标处**(实心底盖住后字 + accent 下划线 = 「正在输入」),
  不必盯着浮动候选窗盲打,体验接近原生输入法。

### 调整 (Changed)
- **光标改为反相块**:聚焦时实心块(光标色)+ **把光标处字符以背景色重绘在块上** = 锐利清晰,替换原来 0.85
  半透明叠层(字在底下发糊);失焦时细描边(更克制)。
- (未做)CJK 回退**字体**仍走系统默认(DirectWrite 自动回退);`[font].fallback` 接入留作可选(gpui Windows 支持
  自定义回退链,但无现成链式 API,需改 Style)。

## [Unreleased] — IME 真正修好 + 编辑器焦点穿透 + 保存即刷新(2026-05-29)

> 上一轮接了 `EntityInputHandler` 但中文仍打不出——**真因**:`on_key` 对可打印键 `stop_propagation`,
> gpui 据此判定 keydown 已处理、**跳过 `translate_message`**,IME 合成永远启动不了(gpui 官方
> `examples/input.rs` 佐证:控制键走 `on_action`、**可打印文本完全不在 key_down 处理**,全交输入处理器)。

### 修复 (Fixed)
- **中文输入真正可用(终端 + 编辑器)**:`on_key` 改为**不消费纯文本键**(单字符 `key`、无 Ctrl/Alt/Win)
  → 放它走 `translate_message`:英文经 WM_CHAR、中文经 WM_IME_COMPOSITION,统一进 `replace_text_in_range`
  (终端写 PTY / 编辑器 `type_char` 插入)。命名/带修饰键(回车/方向/Ctrl-*等)仍 encode + `stop_propagation`;
  合成进行中 gpui 自动把键交给 IME。编辑器输入处理器仅在「编辑态且未开查找栏」注册(否则会把文本误插入缓冲而非查找框)。
- **编辑器焦点漏到底层 shell / 面板穿透**:① 代码行点击 `app.stop_propagation()`;② 面板根 `on_mouse_down`
  兜底吞掉未被子元素处理的点击(点面板保持焦点在此、不穿透);③ workspace 加**正文区 click-away scrim**
  (覆盖终端区、**不盖文件树/标题栏/状态栏**)——点裸终端不再 `focus_pane` 偷走焦点,而是干净关闭浮层
  (`ql_refocus` 把焦点还给树/当前窗格);点文件树仍能换预览。
- **编辑器保存即刷新 git**:Quick Look `save()` 成功后发 `QuickLookEvent::FileSaved`,workspace 订阅后**同步**
  刷新所有 agent 窗格的「本次改动」(`refresh_changes`,非 agent 自动跳过)——绕过文件监听的覆盖/防抖/多 cwd 坑。

## [Unreleased] — IME / 中文输入:终端 + 编辑器接 gpui 输入处理器(2026-05-29)

> **重大修复**:此前**整个 app 无法输入中文**(终端所有窗格 + Quick Look 编辑器)——根因是从未接
> gpui 的 `EntityInputHandler`。gpui 只经输入处理器投递 IME 合成文本(拼音→中文),没接它时只有
> WM_KEYDOWN 的 ASCII `key_char` 能到 `encode_key`,中文永远丢失。

### 修复 (Fixed)
- **终端窗格可输入中文**:`TerminalView` 实现 `EntityInputHandler` + 在 canvas paint 阶段 `window.handle_input`
  注册(仅聚焦时生效)。终端无可编辑文档,故 IME 文本模型 = **合成中的 preedit**(`ime_marked`);**提交的文本
  (中文 / 任意)直接写入 PTY**。`on_key` 现在对**已处理的键 `stop_propagation`**——让 gpui 把 WM_KEYDOWN 标记为
  handled、跳过 `translate_message`,**不再生成重复的 WM_CHAR**(否则接了处理器后每个 ASCII 键会双输入);
  英文仍走 `on_key`/`encode_key`,中文经 WM_IME_COMPOSITION → `replace_text_in_range`,两条路天然隔离。
  `bounds_for_range` 把候选窗定位到光标格。
- **Quick Look 编辑器可输入中文**:`QuickLook` 同款 `EntityInputHandler`,**仅编辑态**注册;IME 提交经 `type_char`
  在光标处插入(支持多字 / 选区替换 / 撤销)。候选窗列精确(gutter + 列×字宽)、行近似到代码区竖直中心
  (编辑时光标滚动居中,`uniform_list` 滚动偏移生产态不可读,见坑)。

### 内部 (Internal)
- `CODE_GUTTER` 提为模块常量(鼠标命中 + IME 光标 bounds 共用,防漂移)。

## [Unreleased] — 活动栏:变化即刷新 + shell 内敲命令起 agent 自动切态(2026-05-29)

### 新增 (Added)
- **活动栏「本次改动」变化即刷新**(`notify` 文件监听):新增 `spawn_change_watcher` 递归监听 agent pane 的 cwd,
  过滤噪声目录(`.git`/`target`/`node_modules`/`dist`/`.next`——`.git` 每次 git 操作都抖、构建目录巨大且与
  `git diff` 无关)+ **450ms 防抖**(一次保存/构建会触发多文件事件,合并成一次 diff)→ 触发 `refresh_changes`
  (后台有界 `git diff HEAD`)。**git 改动从「会话 mtime 门控」解耦** → **agent 改文件、用户手动改都即时刷新**;
  idle 时监听器阻塞在事件队列、零唤醒(保 idle 零开销)。监听器存在 view 上,**丢弃即停**(agent 退出/pane 关闭)。
- **shell 内敲 `claude`/`codex` 自动切 agent 态**(`sync_shell_agent`):此前 `agent` **只认 launch intent**——在普通
  shell 里**敲命令**起 agent 不会切到 agent 头/活动栏卡片。现在 repaint loop 读 shell 集成的**当前运行命令**
  (`BlockModel::current().command`,OSC 633),**首个 token** 命中 claude/codex(用 `tn_ai::agent_kind_for_command`,
  只判程序名故 `cd claude-proj`/`cat codex.md` 不误触)→ 翻成 agent 态(起用量轮询 + 活动栏监听 + 重标签),
  命令块结束即还原。**诚实**:用户真敲了这命令(非脆弱的进程树轮询 / 会话新鲜度猜测,后者会误标同目录的 dev agent)。

### 修复 / 调整 (Changed)
- `spawn_usage_poller` 还原为**仅用量**(上一版把 git 塞进轮询、绑会话 mtime;现 git 改由文件监听驱动,更及时)。
- `clear_agent` 统一收口:agent 退出(launcher 的退出哨兵 / shell agent 的命令块结束)时,一并清 usage / 活动栏数据 /
  **停文件监听**,干净回落普通 shell。区分 `agent_from_shell`:launcher agent 靠 `AGENT_EXIT_SENTINEL` 清,
  shell agent 靠命令块结束清。

## [Unreleased] — 工作区窗格重建:活动栏接真实 git + 正文内边距(2026-05-29)

> 原型 [②工作区窗格](design/panels/02-workspace-panes.html) 端口收尾:agent 面板右侧**活动栏**从占位示例
> 改为**真实数据**,正文补上 mockup 的 `.body` 内边距。守住「**不伪造思考态**」原则——状态行只显诚实信息。

### 新增 (Added)
- **`crate::gitutil` 共享有界 git 模块**:把 quick_look 的 `git_capture_bounded` 提为 `capture_bounded`(单一真源,
  线程 + `recv_timeout` 超时 + `CREATE_NO_WINDOW`,**绝不在 UI 线程跑**),新增 `parse_numstat` / `parse_preview` /
  `changes_for`(`git diff HEAD --numstat --relative`)/ `diff_preview`,**6 个 headless 单测**。quick_look 改用之。
- **活动栏「本次改动」接真实 git diff**:`io::spawn_usage_poller` 在**后台线程**(非 UI)按会话 mtime 变化(=agent 有
  新活动)跑 `git diff HEAD` 拿真实改动文件 + 首文件迷你 diff,存 `TerminalView.rail_files/rail_preview/rail_root`,
  发 `UsageUpdated` 重绘。**数据来自 git,不解析终端正文**;idle agent 零开销(沿用 mtime 守卫)。
- **活动栏卡片可点 → Quick Look**:点改动卡发 `OpenInQuickLook(abs_path)`,workspace 订阅后用 `QuickLook::open_diff`
  在 Diff tab 弹速览——`.ahint`「点卡片 = 速览全 diff」**现已诚实**(不再是空头提示)。

### 修复 / 调整 (Changed)
- **诚实状态行(不伪造「运行中」)**:原型 `.astat` 的「运行中 · Update · 1m12s」是**实时运行态**,但 agent 思考/运行态
  PTY 不可观测(CLAUDE.md 硬原则,**不伪造**)→ 状态行改为诚实的 git 摘要:agent 色点 + 「N 个文件改动 / 工作区干净」+
  右侧「+X −Y」(全来自 git)。空状态显「agent 改动会实时显示在这里」,不再写死示例卡。
- **正文补 `.body` 内边距 `11px 15px`**(mockup):此前终端正文直贴面板内缘、与头部文字不对齐;现网格 / 光标 / 鼠标命中 /
  cols-rows 适配**统一按 `BODY_PAD_X/Y` 偏移**(全相对 `content_bounds`,`bw>1` 守卫故 headless 不受影响)。
  `TN_AUTOQUIT` 验证网格仍收敛、正文内容正常;像素内边距真机肉眼验。

## [Unreleased] — 焦点跟踪修复(分屏基准 + Quick Look 返回)(2026-05-29)

### 修复 (Fixed)
- **`新会话(⌃⇧N)` 分屏不以当前窗格为基准(`⌃⇧E/D` 直接分屏却正常)**:`新会话` 弹的启动器**浮层会抢焦点**,而
  `split_session` 在浮层关闭后才读 `tabs[active].focused` —— 这中间 `focused` 可能已被 `render` 的焦点同步改写 →
  分屏落到错的窗格。`⌃⇧E/D` 因为是**当场同步分屏**(焦点还在窗格上读 `focused`)所以一直对。**修**:`new_session`
  触发的**那一刻**(浮层尚未抢焦点)就把分屏目标**快照**进 `split_target`,`split_session` 优先用快照而非事后的
  `focused`——与 `⌃⇧E/D` 读取时机对齐。另加**兜底**:若快照目标已不在活动树中(失效/dummy),回退到第一个叶子并
  `warn`,避免新窗格变成不可见孤儿。诊断:`FOCUSDBG`/`split_session` tracing 打 `active`/`focused_field`/`gpui_focused`,
  真机复现时可从 `tn.log` 核对焦点是否漂移。**`tn.log` 已实锤**:一次「窗格 1 上开新会话」的会话里,启动器浮层期间
  gpui 焦点掉到了**窗格 0**(`gpui_focused=[0]`)、`focused_field` 被同步逻辑从 1 改写成 0,而快照把 `target` 钉回 1。
- **根因加固 —— 覆盖层持焦点时冻结 `focused`(`render` 焦点同步跳过)**:上条暴露的病根是「抢焦点的覆盖层
  (命令面板 / 新会话启动器 / 布局管理器 / Quick Look)开着时,gpui 可能把焦点瞬时甩到第一个叶子,`render` 的焦点
  同步据此改写 `focused`」。这些覆盖层开着时用户本就不会点窗格 → **同步直接跳过、冻结 `focused`**,从源头杜绝漂移
  (快照是「保险带」、这道守卫是「背带」)。
- **`焦点描边 / 新会话分屏基准` 不跟随点击的窗格**:`tabs[active].focused` 只在 `focus_pane`(点窗格外壳)时更新,
  但点击进**终端正文**时 gpui 已把焦点切到该终端(`track_focus`),`focused` 却没跟上 → 焦点描边停在旧窗格、
  「新会话」分屏也以旧窗格为基准。**修**:在 `render` 里把 `tabs[active].focused` **同步成真正持有 gpui 焦点的那个
  窗格**(遍历活动标签叶子查 `is_focused`)——焦点描边与「新会话」分屏基准都跟随你**当前所在的窗格**(覆盖层/文件树
  持焦点时保留上一个窗格,不乱跳)。
- **Quick Look `Esc` 退出后焦点不回文件列表**:之前关闭浮层把焦点丢给某个**终端窗格**(`refocus_active`),但你是
  从**文件列表**打开文件来看的,理应退回文件列表。**修**:`refocus_after_quick_look` —— explorer 开着就把焦点还给
  **文件树**(可继续 `↑↓` 浏览),否则才回当前窗格(`ExplorerView::focus_handle()` 暴露给 workspace)。

## [Unreleased] — 布局保存/加载/删除(7 槽)(2026-05-29)

> app 菜单的「在资源管理器中显示」改成「布局…」:把**当前标签的分屏结构 + 各窗格启动器**存进槽位,日后召回。
> 运行中的会话无法序列化,加载是按结构**重新拉起启动器**(Claude/Codex/pwsh/WSL),不恢复会话内容。

### 新增 (Added)
- **布局模块**([layout.rs](crates/tn-ui/src/layout.rs)):`LayoutNode`(镜像 `Node` 的可序列化树,叶 = `LayoutPane`
  启动器)+ `Layouts`(7 槽,JSON 持久化到 `%APPDATA%\Tn\layouts.json`)+ `LayoutPane ↔ LaunchSpec` 转换
  (SSH 不持久化,M2 parked)。纯逻辑 headless 单测(spec 往返 / JSON 往返 / 窗格计数)。
- **布局管理器**([workspace.rs](crates/tn-ui/src/workspace.rs) `render_layout_manager`):app 菜单「布局…」弹 7 槽
  浮层,每槽:**保存**(把当前标签分屏存入/覆盖)· **加载**(按该布局**替换当前标签**——杀掉旧窗格、重新拉起)·
  **删除**(清空)。`Esc` 关闭。`tab_to_layout` / `spawn_layout` 在 `Node` ↔ `LayoutNode` 间转换;`pane_specs`
  提供每窗格启动器。owner 定:布局 = 当前标签结构,加载替换本标签。
- `serde` / `serde_json` 加入 `tn-ui`(布局持久化)。单测 48 → 50。

## [Unreleased] — app 菜单各项接真实行为(2026-05-29)

> 按 owner 重新定义 app 菜单各项的行为(原先多为"打开/显示"类占位,现接成真实功能)。

### 变更 (Changed)
- **打开文件夹…**:文件夹选择器 → 文件树重定根(`explorer::set_root`)**+ 把所有「纯本地 shell」窗格 `cd` 进该目录**
  (`cd_shells_to`:遍历窗格,按 `pane_specs` 跳过 Claude/Codex/WSL/SSH——它们改不了宿主 cwd;cmd 用 `cd /d`、pwsh 用 `cd`)。
- **设置**:改为在**我们自己的 Quick Look 编辑器**里打开 `config.toml`(`QuickLook::open_for_edit`,`Ctrl+S` 存盘),
  不再丢给系统默认程序。
- **重载配置**:改为**还原默认(panic button)**——把磁盘上的 `config.toml` + `themes/tn-dark.toml` **覆盖为内置默认**
  再重载,用于从手改坏的配置里恢复(`reset_config`,**破坏性**:丢弃用户改动)。**菜单项不再标 `⌃⇧R`**——那个快捷键
  仍是**非破坏性**的热重载(读取你当前的 config),与这个"重置"区分。
- **主题**:暂时只有一个选项 = 当前默认主题(显示「主题 · Tn Dark」,点击 no-op);多主题时再做真正的选择器。
- **文件浏览器**:维持原样(只开/关文件列表窗格)。
- 新增 `Workspace::pane_specs`(每个活动窗格的 `LaunchSpec`),供「打开文件夹」判断哪些是可 `cd` 的纯 shell(后续布局复用)。

### 后续 (Next)
- **「布局」**(替换「在资源管理器中显示」)—— ✅ 已实现,见上「布局保存/加载/删除」条。

## [Unreleased] — 新会话=分屏启动器 + Quick Look 焦点修复(2026-05-29)

### 修复 (Fixed)
- **Quick Look 打开文件后 `Esc` 无法退出**:`explorer::on_row_click` 打开文件前会 `focus_handle.focus()`
  把焦点抢到**文件树**,而浮层随后在 render 里抢焦点的请求被这次抢占盖过 → 浮层始终拿不到键盘焦点,其
  `Esc`/`↑↓` 处理永不触发(树又不处理 `Esc`,故"按 Esc 没反应")。**修**:打开**文件**时不再聚焦树(只
  在展开**目录**时聚焦,保 `↑↓` nav),让浮层经 `needs_focus` 稳拿焦点。

### 变更 (Changed)
- **app 菜单「新会话」改为分屏启动器**(原先与「新标签」都最终"开个会话",感觉重复)。现在二者职责清晰:
  - **新标签(⌃⇧T)**=新建标签 + 欢迎启动页(可视磁贴选 profile)——逻辑不变。
  - **新会话…(⌃⇧N)**=`render_split_launcher` 浮层:**① 选分屏方向**(←↑↓→ 十字,方向键 / 点击)→
    **② 选启动器**(profile 列表,↑↓ / Enter / 点击)→ 在焦点窗格的该方向**分屏**打开新会话(欢迎标签则填入本标签)。
  - 后端:`Node::split` 加 `before` 参数(左/上 = 插在前;右/下 = 插在后)+ `SplitDir` 枚举 + `Workspace::split_session`。
    新增 `NewSession` 动作 + `Ctrl+Shift+N` 绑定。命令面板(⌃⇧P,新标签内启动)保持不变。
- 单测 +1(`split_before_inserts_left_or_after_inserts_right`);47 → 48。

## [Unreleased] — 原型同步轨道:app 菜单 popup(2026-05-29)

> [`design/panels/01`](design/panels/01-window-chrome.html) 的「点 Tn logo 弹下拉」端口进 gpui。
> **至此原型同步轨道 ①–⑤ 全部端口完成**,余下仅「真实数据接入」类后续(活动栏 git/JSONL、欢迎最近目录)。

### 新增 (Added)
- **app 菜单 popup**([workspace.rs](crates/tn-ui/src/workspace.rs) `render_app_menu`):点 Tn 品牌弹 `.appmenu` 下拉
  (248px、`pane_fill` 暗玻璃 + rim + 深投影 + specular,1:1 复刻 mockup `.appmenu`/`.mi`/`.sep`)。品牌**去掉
  `WindowControlArea::Drag` 改可点**(拖窗改靠标签条 spacer)、caret 开态变亮、**全窗 scrim 点外即关**。
  **11 项全接真实动作**(不留空壳):新会话→命令面板 · 新标签 · 打开文件夹→`prompt_for_paths` 选目录后
  `explorer::set_root` 重定文件树根 · 在资源管理器中显示→`cx.reveal_path(cwd)` · 文件浏览器→toggle ·
  设置→`cx.open_with_system(config_path)` · 主题→`cx.reveal_path(themes_dir)` · 重载配置 · 关于→`open_with_system(README)` ·
  退出→`cx.quit()`。加 `Quit` 动作 + `Ctrl+Shift+Q` 绑定。
- **7 个菜单图标**([assets.rs](crates/tn-ui/src/assets.rs)):external / sidebar / sliders / moon / refresh / info / power
  (照搬 mockup `<symbol>` 路径)。
- **`ExplorerView::set_root`**([explorer.rs](crates/tn-ui/src/explorer.rs)):重置展开/选中并以新目录重建树(「打开文件夹」用)。

---

## [Unreleased] — 原型同步轨道:Quick Look 速览浮层(2026-05-29)

> [`design/panels/03`](design/panels/03-side-panels.html) 的速览编辑端口进 gpui:**砍掉常驻右侧查看器列**,
> 改成点文件树弹**贴树右缘、浮于终端、不占分屏**的玻璃浮层(`viewer.rs` → `quick_look.rs`)。

### 新增 (Added)
- **Quick Look 速览浮层**([quick_look.rs](crates/tn-ui/src/quick_look.rs) `QuickLook`,原 `viewer.rs` `ViewerView`):
  绝对定位浮层,锚到 explorer 右缘(关则锚工作区左缘),浮在终端之上、**不占分屏树**。结构 1:1 复刻
  mockup `.quicklook`:`.vh` 头(file 图标 + dir/name 路径 + 已改动 badge + Diff/File 点切 pill)· `.code` 正文
  (行号槽 38px + 标记列 14px + 语法/增删着色)· `.qlfoot` 键帽提示条 · 左缘 accent `.seam` 指向选中文件。
- **浮层玻璃助手**([style.rs](crates/tn-ui/src/style.rs)):`quicklook_fill`(mockup `.quicklook` 暗玻璃 baked
  **opaque** —— 浮终端正文上须压住后字,无 backdrop-blur 半透会漏出尖锐文字)· `quicklook_frame`(冷能量渐变
  描边,1px-padding reveal,同 `glass_pane`)· `quicklook_shadows`(比常驻面板更深的浮起投影,硬 1px 暗线换 3px 软晕避接缝)。
- **接线**([workspace.rs](crates/tn-ui/src/workspace.rs)):`viewer`/`viewer_open` → `quick_look`/`quick_look_open`;
  动作 `ToggleViewer` → `ToggleQuickLook`(键位 `Ctrl+Shift+J` 不变);点文件树打开、再按收起(浮层不抢焦点 →
  动作在 Workspace 层稳派发);仅在装了文件时渲染。砍掉旧「查看器」常驻列 + 其 `✕` 关闭条。

### 修复 / 优化 (Fixed)
- **大文件卡死整窗**:代码区原把全部行(上限 500)逐行 + 逐 token 成 div **一次性布局**,几百行就让
  gpui 的 Taffy 布局爆量、每次重渲卡顿冻结整窗。改用 **`uniform_list` 虚拟化**(只测首行高 + **只渲可见 ~30 行**、
  其余惰性,带滚动)→ 文件再大也只渲可见行;`file_lines`/`diff` 存 `Rc` 供 `'static` 闭包零成本捕获,行构建抽成
  自由函数(`file_row`/`diff_row`)。读入上限提到 4000(仅约束一次性读取,渲染量与文件大小无关)。
- **浮层太大**:四边留白加大(top 70 / bottom 60 / right 64 / left 锚 explorer 右缘)+ `max_w 880` 宽屏封顶 →
  从近铺满改成**浮起的卡片**(原型那种比例),贴树左缘锚定不被拉过宽。

### 交互 (Added)
- **Quick Look 全套键盘交互**(原型 03 的速览编辑模型):
  - **文件树键盘 nav**([explorer.rs](crates/tn-ui/src/explorer.rs)):focus-on-click + `↑↓` 移选中 +
    `Space`/`Enter` 开 Quick Look(目录则展开);`select_adjacent_file` 供浮层换文件。
  - **预览态**(焦点在浮层):`↑↓` 换文件实时跟随(发 `QuickLookEvent::Nav` → workspace 调
    `select_adjacent_file` 移树选中 + `open`)· `⇥` 切 Diff/File · `Esc`/`Space` 收起(发 `Close` →
    workspace 焦点还终端)· 开浮层自动抢焦点(`needs_focus` 在 render 里聚焦)。
  - **编辑态 = 自绘小编辑器**:`Enter` 进编辑;打字插入(读 `key_char`,多字节安全)/ Backspace / Delete /
    Enter 拆行 / Tab→空格 / 方向键·Home·End·PgUp·Dn 移光标 / **`Ctrl+S` 写盘**(写后刷新 File+Diff)/
    `Esc` 回预览。光标在虚拟化行内按 col 切 `[前][caret][后]` 渲染;`ROW_H` 固定行高保证 `uniform_list`
    一致 + caret 对齐。缓冲逻辑抽成纯 `op_*` 自由函数,**headless 单测 6 条**(多字节/拆并行/列钳)。
- 单测 33 → **39**。

### 编辑器增量 (Added)
- **Quick Look 自绘编辑器补全全套增量**(原 deferred 项全部落地):
  - **选区**:`Shift`+方向/Home/End/PgUp·Dn 扩选;无 Shift 移动折叠到近/远端;`Ctrl+A` 全选。
  - **复制/剪切/粘贴**:`Ctrl+C/X/V`(无选区时整行;粘贴多行 `op_insert_multiline`;经 gpui clipboard)。
  - **撤销/重做**:`Ctrl+Z` / `Ctrl+Y` / `Ctrl+Shift+Z`,(buffer,cursor) 快照栈(`Rc` 廉价)+ **连续打字合并成一步**(coalesce)。
  - **鼠标点位**:点击置光标、`Shift`-点击扩选;`char_w` 经 `text_system().advance` 量一次,code 区 canvas 捕获 bounds 映射 x→列(行号 `i` 已知,免滚动偏移)。
  - **编辑态语法高亮 + 选区底色 + caret 三合一**:`edit_row` 按字符展开 tint、按 (tint,选中) 分组成 run、caret 切 run 内联插入。
  - **查找/替换**:`Ctrl+F` 查找 / `Ctrl+H` 替换条;`Enter`/`Shift+Enter` 下/上个匹配(选中 + 滚入视区,环绕)· `Tab` 切查找/替换框 · `Ctrl+Enter` 全部替换 · `Esc` 关;输入由 `on_key` 的 `find_key` 捕获。
- 纯逻辑(`op_delete_range`/`op_insert_multiline`/`selected_text`/`all_matches`/`replace_all_in`/`find_in_chars`)抽成自由函数,**新增 5 条 headless 单测**(单测 39 → 44)。

### 修复 (Fixed)
- **编辑态空格键无反应**:`Space` 原走 `key_char` 分支插入,但 gpui on Windows 对空格键常**不填 `key_char`**(终端输入也是按 `key=="space"` 名处理),导致空格被静默丢弃。改为编辑态显式 `"space" => type_char(" ")`。
- **Quick Look 冻结(同步 `git diff` 卡死 UI 线程)**:`open()` 原**每次文件树点击 / 预览 `↑↓` 换文件**都同步跑
  `git diff`(`.output()` 阻塞调用线程),大仓库 / `.git/index.lock` 占用 / 杀软扫描 git 时**整窗冻死**。定位:
  5-agent workflow + 日志(冻结点在**未埋点**的预览→`Nav`→`open`→git 暗路,故零 `SLOW` 告警、日志在编辑态 `escape`
  切回预览后戛止)。**修法两层**:① **惰性 diff**(新 `diff_dirty`:`open`/`save` 只标脏,仅切到 **Diff tab** 时
  `ensure_diff` 跑 git → git 离开导航/打开/保存热路径)· ② **`git_capture_bounded`**(`.output()` 丢一次性线程 +
  `mpsc::recv_timeout(1.5s)` 超时即放弃 + `#[cfg(windows)] CREATE_NO_WINDOW` 防控制台闪;**别用 `try_wait` 轮询读
  piped stdout**——大 diff 撑爆管道缓冲会死锁)。`explorer::compute_git_status`(`git status`)同加 `CREATE_NO_WINDOW`。
  `compute_diff` 的解析拆成纯 `parse_diff` + headless 单测(单测 44 → 45)。
- **Quick Look 真·冻结根因:`highlight()` 在 `①` 类字符上死循环 → OOM**(切到含 `①` 的 HTML 必冻;CLAUDE.md 无 `①`
  故正常)。`①`(U+2460)`is_alphanumeric()==true` 但 `is_alphabetic()==false`、`is_ascii_digit()==false`,故词分支(查
  `is_alphabetic`)和数字分支(查 `is_ascii_digit`)都不收它,落到标点分支;标点分支的 `while` 在 `is_alphanumeric()` 处
  `break`,于是 `j==i` 不前进 → `i` 永不推进 → 无限 push 空 token → `out` 撑爆内存 OOM(`rust_oom`,Windows 报 `0xc0000409`;
  交互式下表现为 CPU 打满、整窗冻、用户 Ctrl+C)。**修**:标点分支末尾保证前进——`if j == i { j = i + 1 }`(把这个字符当 1
  字符 Plain token 吃掉)。加回归单测 `highlight_terminates_on_alphanumeric_nonword_chars`(`①②③½Ⅷ⑩㊀` 等;单测 46 → 47)。
  **定位法**:加 `TN_QL_BENCH=<file>` 探针(开机即在真窗口里把该文件弹进 Quick Look、2.5s 后自退;paint 挂则进程挂)→
  `cargo run` 复现出 `0xc0000409`,`RUST_BACKTRACE` 顶帧 `rust_oom ← grow_one ← highlight:146` 直指死循环。探针保留(env 门控)。
- **paint 成本加固(顺带,非根因)**:`coalesce_spans` 按 tint 合并相邻 token(标记行 ~30 span → 个位数)+ 封顶 `MAX_SPANS(48)` +
  长行(>2000B)整行单 span;`file_row`/`edit_row` 共用。减少 `div`/整形 run 数,降低密集行 paint 负担(单测含合并/不丢内容/封顶)。
- 临时埋点(`render START`/逐键 `IN`/`open`·`nav` 计时/`TN_QL_PLAIN`·`NOBODY`)在定位后已清;保留 `compute_diff` 超时 `warn` + `TN_QL_BENCH`。

### 待接 (Deferred)
- **键盘两态 + 编辑**:prototype 的 `Space` 开 / `↑↓` 换文件实时跟随 / `Enter` 进编辑态 / 方向键归编辑器 /
  `Ctrl+S` 保存 —— 需 explorer 键盘焦点 + 可编辑文本缓冲,**编辑写盘有风险**,按视觉先行轨道延后(同活动栏先例)。

## [Unreleased] — 原型同步轨道:欢迎 launchpad(2026-05-29)

> [`design/panels/05`](design/panels/05-states.html) 的欢迎页端口进 gpui:默认新标签/首标签 = 启动磁贴 + 快捷键提示。

### 新增 (Added)
- **欢迎 launchpad**([welcome.rs](crates/tn-ui/src/welcome.rs) `WelcomeView`):wmark + 「开一个新会话」+ **启动磁贴**
  (发现的 profile:Claude 珊瑚 / Codex 青绿 / pwsh 蓝 / WSL 紫,spark/term 图标)+ **快捷键提示**。是与终端面板
  同款的玻璃面板。点磁贴 `LaunchRequested(index)` → 在**当前标签**启动该 profile(welcome → pane)。
- **新标签 = 欢迎页**:`Tab` 加 `welcome` 态;**+ / `Ctrl+Shift+T`** 与正常启动的首标签都开 launchpad(`TN_AUTOQUIT`/
  `TN_DEMO` 下首标签仍开 pwsh,保 headless 自测)。welcome 标签的 split/resize 空操作,标签名「欢迎」。

### 修复 (Fixed)
- **关欢迎标签导致 abort**:welcome 标签的 dummy `root`/`focused` 原用 `0`,与**首个真实 pane id(0)** 撞 →
  关欢迎标签时 `collect_leaves` 误删 pane 0 → 首标签下一帧 `panes.get(0).expect` 在 GPUI 非 unwinding 回调里
  panic → 进程 abort。改 dummy id 为 `PaneId::MAX`(永不与真实 pane 撞)+ welcome 标签跳过 pane 回收。
- **启动时先闪一个透明窗口**:主窗口原 `show:true` 立即显示,但 DX swapchain 首帧未呈现 → 透明/空白闪一下才出
  界面。改 `show:false` 开窗,`Workspace` 首帧 `render` 后用 spawned 任务(等 ~40ms 首帧呈现)调 `platform::show`
  揭示(读 HWND 在 render 内安全,`ShowWindow` 须在 update 借用**外**调,否则重入窗口过程,见坑)。`TN_AUTOQUIT`
  下不揭示(保 headless)。

### 待接 (Deferred)
- **「最近」目录列表**:需 recent-sessions 数据源(claude/codex 会话 cwd + mtime;Claude 工程目录名编码有损),
  单独成项,不伪造。

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
