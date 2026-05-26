# Tn 体验设计:分屏 · 多会话 · 一键 AI · 用量 · 颜值

> 本文给出 Tn 面向用户的核心体验设计方案:**原生分屏、多终端会话与窗口管理、一键启动 Claude/Codex、按 AI 工具获取用量信息,以及统一的视觉/颜值设计语言**。
> 标记同 [BLUEPRINT.md](BLUEPRINT.md):「✅ 现状」/「🧭 规划」。本文均为 🧭 规划,落地映射见末节。

设计原则(贯穿全文):
1. **对用户极友好**:零配置可用、操作可发现(有可视入口,不靠背快捷键)、键鼠皆顺手、状态始终可见。
2. **会话是一等公民**:一个窗口里多 Tab、每 Tab 任意分屏、每个分屏跑一个会话(shell / WSL / SSH / **Claude / Codex**)。
3. **AI 原生**:一键起 agent 会话,实时看到它在想什么、花了多少 token/钱、上下文用了多少。
4. **颜值优先**:Windows 11 原生质感(Mica/圆角)、活动焦点清晰、动效克制流畅、深浅主题一致。

---

## 1. 数据模型:窗口 / 标签 / 分屏 / 会话

清晰的四层层级。分屏采用 **n-ary 容器树 + 标签组**(VS Code / Zed 式)——比二叉树自由得多:一行/一列可放任意多个分屏、同一容器内分隔线对齐共享、拖拽即可重排,能拼出任意规整矩形布局且自动整齐。

```
Window(OS 窗口)
└─ tabs: Vec<Tab>,  active_tab
   Tab
   ├─ root: PaneNode          # 分屏树
   ├─ focused: PaneId
   └─ zoomed: Option<PaneId>  # 临时最大化某屏
      PaneNode =
        ├─ Leaf(Pane)                                   # 叶 = 一个会话
        ├─ Container { axis: Row|Col, children: Vec<Child> }  # 一行/一列放 N 个,权重可调,分隔线对齐共享
        └─ Stack { panes: Vec<PaneId>, active }          # 同一格里多会话叠成"标签组"(拖到中心形成)
           Child { node: PaneNode, weight: f32 }         # 权重之和归一,决定各子占比
           Pane(叶) { id, content }                      # content = 会话 或 查看器
              content = Session(终端会话) | Viewer(文件/Diff 查看器, 只读)
              Session                       # 真正的"会话",与渲染解耦
              ├─ kind: SessionKind          # Shell{profile} | Wsl{distro} | Ssh{host} | Agent{AgentKind}
              ├─ terminal: tn_core::Terminal
              ├─ pty: Box<dyn PtyBackend>
              ├─ title / cwd / exit_status / activity(idle/running/bell)
              └─ usage: Option<AiUsage>     # 仅 agent 会话,由 UsageProvider 填充
```

```rust
pub struct SessionId(u64);   pub struct PaneId(u64);   pub struct TabId(u64);

pub enum SessionKind {
    Shell { profile: String },          // pwsh / cmd / ...
    Wsl   { distro: String },
    Ssh   { host: String },
    Agent { kind: AgentKind, cwd: PathBuf, argv: Vec<String> },  // ClaudeCode | Codex
}

pub enum PaneNode {
    Leaf(Pane),
    Container { axis: Axis, children: Vec<Child> },  // N 路平铺
    Stack { panes: Vec<PaneId>, active: usize },     // 标签组:同位多会话
}
pub struct Child { pub node: PaneNode, pub weight: f32 } // 权重决定占比(归一化)
pub enum Axis { Row, Col }  // Row=横向并排(竖分隔线);Col=纵向堆叠(横分隔线)

pub struct Pane { pub id: PaneId, pub content: PaneContent }
pub enum PaneContent { Session(SessionId), Viewer(Viewer) }  // 分屏可承载会话 或 查看器
pub enum Viewer { File(FileView), Diff(DiffView) }           // 只读文件 / Diff
```

为什么不是二叉树:二叉树每次只能二分,**相邻非兄弟的分隔线无法对齐拖动**,也拼不出"一行三等分"这类布局。n-ary `Container` 让**同一容器内的分隔线天然对齐、可整体拖动**;`Stack` 让同一格位叠放多个会话成标签组(把一个分屏拖到另一个的**中心**即形成)。

要点:**Session 与 Pane 解耦** —— 会话不绑定在某个分屏上,可被移动到别的 Tab/分屏/容器、可"分离/重新附着",为"拖拽重排""会话管理器"打基础。Tab 标题、Tab/分屏的图标与状态都从 Session 派生。

---

## 2. 原生分屏(panes)

**交互(全部既有可视入口、又有快捷键,默认键位对齐 Windows Terminal,可在 config 改):**

| 操作 | 默认键 | 可视入口 |
|---|---|---|
| 左右分屏(竖分隔线) | `Ctrl+Shift+D` | Tab 右键菜单 / 分屏按钮 |
| 上下分屏(横分隔线) | `Ctrl+Shift+E` | 同上 |
| 焦点移动(方向) | `Alt+←↑→↓` | 点击任意分屏 |
| 调整大小 | `Ctrl+Alt+←↑→↓` | **拖动分隔线** |
| 最大化/还原当前屏(zoom) | `Ctrl+Shift+Z` | 双击分隔线 / 分屏头按钮 |
| 关闭当前屏 | `Ctrl+Shift+W` | 分屏头 × |
| 均分 | `Ctrl+Shift+=` | 菜单 |
| 拖拽重排 / 停靠 | **拖分屏头到目标上/下/左/右/中心** | 拖拽 + 落点高亮 |
| 广播输入(同时打字到多屏) | `Ctrl+Shift+I` | 状态栏开关 |

**拖拽停靠(drag-dock)语义** —— 自由布局的核心:拖起一个分屏头,移到目标分屏上方时显示**落点高亮区**(四边 + 中心五个区域):
- 落 **上/下/左/右** → 在目标处插入/扩展对应方向的 `Container`(如已是同向 Container 则插入为新 `Child`,分隔线自动对齐)。
- 落 **中心** → 与目标合成 `Stack`(标签组,同位多会话切换)。
- 拖到 **Tab 栏** → 移成新 Tab;拖出窗口 → 新窗口(后期)。

**实现要点:**
- **分隔线**是容器内相邻两 `Child` 间的可拖 handle;拖动 → 调整这两个 `Child.weight` → 重布局 → 各 Session `Terminal::resize` + `PtyBackend::resize`(列在前,见 [REFERENCES.md](REFERENCES.md) §三)。同容器内分隔线对齐、可整体拖。
- **关闭叶子**:从父 `Container.children` 移除;若容器只剩 1 个 Child → 坍缩;`Stack` 只剩 1 个 → 退化为 `Leaf`;焦点转移到最近兄弟。
- **方向焦点**:按几何位置找目标方向最近的叶子(WezTerm 算法),跨容器有效。
- **zoom**:`Tab.zoomed = Some(pane)`,渲染时只画该屏全幅,退出还原树;不改树结构。
- **布局持久化**:Tab 的分屏树(Container/Stack/weight)+ 各会话 kind/cwd 序列化(serde),支持**会话恢复**(见 §4)。
- **布局预设**:常用布局一键套用(如"主屏 + 右侧 AI 屏 + 底部日志屏")。
- **每个分屏可选 1 行 pane header**(默认活动屏显示、可全局关):cwd 面包屑 + 会话类型图标 + 状态 chip +(agent 屏)用量微读数。

### 2.1 文件树 + 文件/Diff 查看器(viewer pane)

分屏不止能放终端会话,还能放**只读查看器**(`PaneContent::Viewer`)——这是 vibe coding 的关键拼图:**实时看到 AI 正在改什么**。保持"轻量查看,不是 IDE"的边界(只读、不做编辑器)。

- **文件树(Explorer)**:一个窄侧栏 pane,浏览当前 cwd;点文件 → 在查看器里打开;显示 git 状态标记(`M` 改动 / `U` 新增);**高亮 agent 正在编辑的文件**。可整体开关。
- **文件查看器(FileView)**:只读、语法高亮(`syntect` 或 tree-sitter),行号、面包屑路径、跳转。
- **Diff 查看器(DiffView)**:展示某次改动的 diff(红 `-` / 绿 `+`)。来源:agent 工具调用事件(opt-in 桥)、或对文件做前后快照 / 读 git。**"Claude 改了 element.rs" → 右侧直接看 diff**,审阅 AI 改动不用切窗口。
- **打开方式**:点文件树条目;点终端输出里的文件路径 / OSC 8 超链接;点 agent 活动行("Editing X" → 打开该文件/диff)。新查看器默认在右侧开一个分屏(可配)。
- **默认布局("vibe coding"工作区)**:`Explorer(窄) | Claude(大)+ 小 shell(底) | Diff 查看器(右)`。shell 默认较小(平时用不上大 shell),AI 与文件查看占主舞台。见原型 [`design/mockup.html`](../design/mockup.html)。

---

## 3. 会话启动器:一键 Claude / Codex / shell

**两个入口,都很显眼:**
1. **Tab 栏的 `+` 下拉**(像浏览器新标签的 ▾):列出所有会话类型,点一下即开新 Tab;按住 `Alt` 点则是"在当前分屏旁开"。
2. **命令面板**(`Ctrl+Shift+P` / `Ctrl+K`):模糊搜索 + 大块 **agent 快启磁贴**(Claude/Codex,带上次项目目录)。

启动器条目来自 **Profile**(配置即数据,见 §6 与 [BLUEPRINT.md](BLUEPRINT.md) §4.4):

```toml
[[profiles]] name="pwsh"   kind="shell" command="pwsh.exe"
[[profiles]] name="Ubuntu" kind="wsl"   distro="Ubuntu"
[[profiles]] name="devbox" kind="ssh"   host="10.0.0.5" user="me"
[[profiles]] name="Claude" kind="agent" agent="claude" command="claude"  cwd="$PROJECT" accent="#F0916D" glyph="✻"
[[profiles]] name="Codex"  kind="agent" agent="codex"  command="codex"   cwd="$PROJECT" accent="#73DACA" glyph="✾"
```

**一键 AI 会话的语义**(关键友好点):
- 选 "Claude Code (here)" → 在**当前 cwd** 起;"Claude Code (pick dir)" → 弹目录选择。因为是我们主动拉起(**spawn intent**),会话的身份/cwd/argv 完全已知 → 用量关联与状态识别最可靠(见 §5、[BLUEPRINT.md](BLUEPRINT.md) §4.6)。
- 起 agent 默认**在右侧开一个新分屏**(可配),让"代码/命令在左、AI 在右"成为顺手布局;也可整 Tab。
- agent 会话用其**强调色 + 字形**标识(Claude 橙 ✻ / Codex 绿 ✾),在 Tab、分屏头、会话管理器里一眼可辨。
- 记住每个 agent 的"最近项目目录"做快启磁贴。

---

## 4. 窗口与会话管理

**Tab 栏(顶部):** 圆角 pill 标签;每 Tab 显示会话类型图标 + 标题(派生自 cwd/进程)+ agent 强调色条;**活动时的状态指示**(运行中转圈、bell 闪、agent "Thinking…" 微光);hover 出 × 关闭;可拖拽重排;末尾 `+` 启动器。多分屏的 Tab 显示分屏角标。

**会话管理器(可呼出的侧栏 / 覆盖面板,`Ctrl+Shift+O`):** 跨所有 Tab 列出全部会话,按**项目/cwd 分组**(workspace 概念);可搜索、跳转、重命名、关闭、把会话拖到别的 Tab/分屏;agent 会话直接显示实时用量摘要。这是"多会话"规模化后的导航中枢。

**会话恢复(persistence):** 退出时序列化窗口/Tab/分屏树 + 各会话(kind/cwd/profile);重启可**还原布局**(shell 重新拉起;agent 提示"重开 Claude @ /repo");可配为自动或询问。

**广播输入:** 选中多个分屏,键入同时发往所有(批量操作多机/多会话)。

**多窗口:** 支持多个 OS 窗口;会话可在窗口间拖拽迁移(后期)。

---

## 5. AI 用量信息(按工具获取)

> 目标:在不依赖 AI 工具"配合"的前提下,实时展示**模型、上下文占用(context window)、输入/输出/缓存 token、估算花费、限额窗口**,并能看 会话/当日/当月 汇总。**上下文占用与 token 用量并列为两个一等读数**(对 vibe coding,上下文满了 = agent 变笨,比花费更该盯)。

### 5.1 数据来源阶梯(可靠优先)

| 阶梯 | Claude Code | Codex | 可靠性 |
|---|---|---|---|
| **本地会话 JSONL(主源)** | `~/.claude/projects/<proj>/<session>.jsonl`,每行 `message.usage`:`input_tokens / output_tokens / cache_creation_input_tokens / cache_read_input_tokens` + `model` | `$CODEX_HOME/sessions/**/rollout-*.jsonl` 的 `token_count` 事件(2025-09+ 版本) | **高**(无需配合,`ccusage` 同源) |
| **opt-in 桥(实时更丰富)** | Claude **hooks** 在 tool_use/stop 发我们的私有 `OSC 1737;tn;<json>`;或 OTel 指标 | Codex 通知/JSON-RPC | 高(需用户开启) |
| **TUI 内** `/cost`、`/status`、`/statusline` | 仅 TUI 内,不在线上 | 同 | 不可抓 |
| **窗口标题** | 状态词(Thinking/Working/Ready) | 同 | 低(仅状态) |

**默认走主源**:监听对应工具的会话目录(`notify` / `ReadDirectoryChangesW`),增量解析新行 → 累加 token → 按内置定价表估算花费。**关联**:因 agent 由我们 spawn(已知 cwd),Claude 可由 cwd 推出 project 目录名并取最新 session 文件;Codex 取启动后最新的 rollout 文件。

### 5.2 架构:可插拔 UsageProvider

```rust
pub struct AiUsage {
    pub model: String,
    pub input: u64, pub output: u64, pub cache_create: u64, pub cache_read: u64,
    pub context_used: u32, pub context_max: u32,   // 当前上下文大小(最新一轮总输入)/ 模型窗口 → %
    pub cost_usd: f64,                              // 由 pricing 表估算
    pub window: Option<RateWindow>,                 // 5 小时/订阅限额窗口(Claude Max/Codex plan)
    pub session: Aggregate, pub today: Aggregate, pub month: Aggregate,
    pub updated_at: Instant,
}

pub trait UsageProvider: Send {
    fn kind(&self) -> AgentKind;
    /// 监听该工具的本地会话产物,增量推送某会话的用量更新。
    fn watch(&self, session: &AgentSession) -> mpsc::Receiver<AiUsage>;
}
// 实现:ClaudeUsageProvider(读 ~/.claude/projects)、CodexUsageProvider(读 $CODEX_HOME/sessions)、
//       BridgeUsageProvider(OSC 1737 / hooks)。定价:内置可更新的 pricing 表(LiteLLM 风格)。
```

### 5.3 上下文用量(context window)怎么算

**上下文占用 = 当前轮的总输入 token(`input + cache_read + cache_creation`)/ 模型上下文窗口**。这正是 Claude `/context`、Codex `/statusline` 显示的"上下文已用/剩余",我们从同一份 JSONL 的**最新一轮 usage** 直接算出,无需工具配合。
- 模型窗口大小来自**内置 `model → context_window` 表**(如 Claude Sonnet 200K / 1M 变体、各 GPT/Codex 型号),随模型更新维护。
- `context_used` 取**最新一轮的总输入**(= 当前上下文实际大小),区别于 `session/today/month` 的**累计** token。
- **为什么重要**:上下文越满,agent 越健忘、越易跑偏 —— 对 vibe coding 这是关键健康指标。接近阈值(如 >80%)变色告警,并提示 `/compact` 或开新会话。

### 5.4 展示(三层,信息密度递增)

1. **分屏头 / Tab 上的微读数**(agent 屏常驻):`✻ Sonnet · ◗ 42% · $0.31` —— **上下文用一个环形进度**(绿→黄→红随占用升高),旁附 `已用K/窗口K`(如 `84K/200K`);token 累计与花费并列。
2. **AI 状态栏 / 侧面板**:列出所有活跃 agent 会话,每个一行:模型 + **上下文环** + token(in/out/cache)+ 花费 + "Thinking…"动画;限额窗口剩余(如 "5h 窗口剩 38%")。上下文接近满时整行高亮告警。
3. **用量面板/命令**(`AI: Usage`):会话/当日/当月 的 token 与花费明细,按模型/token 类型分解(对标 ccusage);上下文占用走势(本会话随轮次的曲线)。

**注意/诚实**:token/费用/上下文**都不在终端字节流里**,靠解析本地会话文件;`/cost`、`/context` 等是 TUI 内不可抓;费用是**按定价表估算**(订阅制下更应理解为"等价 API 成本/限额占用");上下文%受模型窗口表准确性影响。文件监听只读、防抖,不影响性能与隐私(数据本就在本机)。

---

## 6. 颜值 / 视觉设计语言

**总基调**:现代、克制、深色优先,Windows 11 原生质感;信息清晰、焦点明确、动效流畅不喧宾夺主。

> **默认主题 = `Tn Dark`**(Tokyo Night 调校):定义见 [`config/themes/tn-dark.toml`](../config/themes/tn-dark.toml);**高保真原型见 [`design/mockup.html`](../design/mockup.html)**(浏览器打开),渲染图 `design/mockup.png`。原型展示了默认 "vibe coding" 布局:**文件树 Explorer | Claude 大屏 + 小 shell | Diff 查看器**(Codex 等为一键添加,不占默认屏),以及上下文环/用量读数、Warp block、活动屏焦点光,和**有质感的背景**(彩色 mesh 渐变 + 细颗粒 grain + 边缘 vignette + 窗口玻璃高光/Mica)。

### 6.1 设计令牌(主题里集中定义,见 tn-config)
- **颜色**:背景分层 `surface.0/1/2`(窗口/面板/卡片)、`fg`、`muted`、`border`;`accent`(品牌强调);**agent 强调色**(由主题定义,见 [`config/themes/tn-dark.toml`](../config/themes/tn-dark.toml) `[agents]`,源自品牌色按 Tokyo Night 调校)Claude `#F0916D` / Codex `#73DACA`;语义色 success/warn/error;16 色 ANSI 调色板。深/浅两套,跟随系统。
- **间距**:4 的倍数刻度(4/8/12/16/24)。**圆角**:面板 8、卡片/标签 6–10。**阴影/高度**:活动元素轻投影。
- **字体**:等宽正文(Nerd Font,连字开关)+ CJK/emoji 回退;UI 用系统 UI 字体。
- **动效**:时长 120/200ms,缓动 ease-out;遵守系统"减少动态效果"。

### 6.2 关键界面
- **窗口质感(Windows 11)**:Mica/Acrylic 背景(DWM `DWMWA_SYSTEMBACKDROP_TYPE`)、圆角(`DWMWA_WINDOW_CORNER_PREFERENCE`)、深浅标题栏;可选整体透明度 + 背景模糊。
- **活动分屏焦点**:活动屏一圈细强调色描边 + 略提亮;非活动屏轻微压暗 —— 多屏时一眼知道焦点在哪。
- **Tab 栏**:pill 标签 + 类型图标 + agent 强调色条;运行/思考态有微光/转圈;`+` 启动器下拉精致。
- **分屏头(可选、超薄)**:cwd 面包屑 + 图标 + 状态 chip +(agent)用量微读数。
- **命令面板 / 启动器**:居中、背景模糊、模糊搜索;agent 快启用**大磁贴**(图标 + 名称 + 上次目录)。
- **空状态**:新会话欢迎屏,大按钮直达 pwsh / WSL / Claude / Codex。
- **微交互**:分屏开合 grow/shrink、Tab 切换淡入、Quick Terminal 滑入(见 [BLUEPRINT.md](BLUEPRINT.md) M5)、焦点描边过渡、agent "Thinking" 呼吸光。
- **无障碍**:对比度达标、可调字号、尊重 reduced-motion、焦点可视。
- **组件**:复用 `gpui-component`(面板/输入/列表/弹层)加速一致性与质感。

---

## 7. 落地映射(crate / 里程碑)

| 能力 | 主要 crate | 里程碑 |
|---|---|---|
| 会话/Tab/分屏 数据模型 + n-ary 容器树 + 拖拽停靠 | tn-ui(+ tn-core 的 Session) | **M1** |
| 原生分屏交互(分隔线/焦点/zoom/关闭/拖拽) | tn-ui | **M1** |
| 文件树 + 文件/Diff 查看器(viewer pane,只读) | tn-ui(+ tn-ai 提供 diff 源) | **M3**(查看器)/ **M4**(跟随 agent 编辑) |
| 每格颜色 + 自定义 `TerminalElement` + 焦点描边 | tn-ui | **M1** |
| Profile + 启动器(`+` 下拉 / 面板)+ shell/WSL/SSH | tn-config + tn-ui | **M1**(基础)/ **M2**(WSL/SSH) |
| 一键 Claude/Codex agent 会话 + 强调色标识 | tn-ai + tn-ui | **M4** |
| AI 用量(UsageProvider:Claude/Codex 文件解析 + 展示) | tn-ai(+ tn-ui 展示) | **M4**(基础可在 M1 后起步) |
| 会话管理器 + 广播 + 布局/会话持久化 | tn-ui | **M1→M2** 增量 |
| 视觉设计令牌 + Mica/圆角 + 动效 + 组件化打磨 | tn-config(令牌)+ tn-ui | **M1** 起,**M4** 集中打磨颜值 |
| Quick Terminal(幽灵模式) | tn-ui | **M5** |

> 对路线图的影响:**M1 的"Tab + 分屏"扩展为本文的完整会话/分屏模型与启动器**;**M4 增加"AI 用量"与 agent 会话强调色/状态**;**颜值设计语言从 M1 起贯穿、M4 集中打磨**。详见 [BLUEPRINT.md](BLUEPRINT.md) 路线图。

---

### 参考来源
- Claude Code 用量数据:本地 `~/.claude/projects/**/*.jsonl`(input/output/cache token + model);ccusage 同源。
- Codex 用量数据:`$CODEX_HOME/sessions/**/rollout-*.jsonl` 的 `token_count` 事件(2025-09+);`/status`、`/statusline` 为 TUI 内。
- 分屏/会话交互参考 WezTerm、tmux、Windows Terminal;窗口质感参考 Windows 11 DWM(Mica/圆角)。
