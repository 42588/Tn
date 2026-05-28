# UI-CATALOG.md — Tn 界面 / 功能面板分类清单

把 Tn 的**每个界面 / 功能面板**摊开成一张表:它的**设计原型(HTML)→ gpui 还原(Rust)→ 真实实现(crate)** 三者一一对应,方便对照、改样、查值。

> **图形界面必走的链**(详见 [../CLAUDE.md](../CLAUDE.md)「图形界面 / 样式开发」):
> 设计真源 [`design/mockup.html`](../design/mockup.html) → 查确切值 [`CSS_TO_GPUI.md`](CSS_TO_GPUI.md) §16 →
> 写 gpui 用 `col/cola` token(禁硬编码主题色)→ 三道 headless 守卫(`cargo test -p tn-ui`)→
> 真机肉眼比对(下方 Rust 还原 example)。

## 资源结构

| 角色 | 位置 | 说明 |
|---|---|---|
| **设计真源(全窗)** | [`design/mockup.html`](../design/mockup.html) | 完整窗口合成原型;**token_drift 守卫读它的 `:root`**,勿动其内联 CSS |
| **共享样式** | [`design/calm-glass.css`](../design/calm-glass.css) | 令牌 + 组件类,`design/panels/*.html` 全部 `<link>` 它(镜像 mockup `:root`) |
| **分类原型(HTML)** | [`design/panels/`](../design/panels/) | 按 5 类拆分的高保真原型;入口 [`index.html`](../design/panels/index.html) |
| **gpui 还原(全窗)** | [`test/mockup_replica.rs`](test/mockup_replica.rs) | `cargo run -p tn-ui --example mockup_replica` |
| **gpui 还原(浮层/状态)** | [`test/panels_replica.rs`](test/panels_replica.rs) | `cargo run -p tn-ui --example panels_replica` |
| **译法字典 + 数值** | [`CSS_TO_GPUI.md`](CSS_TO_GPUI.md) | §1–§15 = HOW,§16(自动生成)= 每组件权威数值 |
| **视觉决策 / 取舍** | [`UX-DESIGN.md`](UX-DESIGN.md) §6 | Calm Glass 令牌、§6.3 gpui 落地取舍 |

## 分类清单(原型 ↔ Rust ↔ 实现)

### ① 窗口外壳(window chrome)
| 面板 | HTML 原型 | gpui 还原 | 真实实现 |
|---|---|---|---|
| 标题栏(品牌 + Tab 行 + 窗控) | [01](../design/panels/01-window-chrome.html) `.titlebar` | `mockup_replica::titlebar` | [`workspace.rs`](../crates/tn-ui/src/workspace.rs) `render` → titlebar |
| Tab(活动/非活动 + agent 顶条 + cwd 徽章) | [01](../design/panels/01-window-chrome.html) `.tab` | `mockup_replica::tab` | [`workspace.rs`](../crates/tn-ui/src/workspace.rs) tabs 段 |
| 窗口控制(min/max/close) | [01](../design/panels/01-window-chrome.html) `.wctl .b` | `mockup_replica::wctl_button` | [`workspace.rs`](../crates/tn-ui/src/workspace.rs) `ctl_btn` |
| 状态栏(分支/会话/ctx/文件/编码/主题) | [01](../design/panels/01-window-chrome.html) `.status` | `panels_replica::status_bar` · `mockup_replica::status_bar` | [`workspace.rs`](../crates/tn-ui/src/workspace.rs) `render_status_bar` |

### ② 工作区窗格(panes)
| 面板 | HTML 原型 | gpui 还原 | 真实实现 |
|---|---|---|---|
| Agent 面板(头 + 工具流 + 气泡) | [02](../design/panels/02-workspace-panes.html) `.agenthead/.agentbody` | `mockup_replica::agent_pane` | [`terminal_view/header.rs`](../crates/tn-ui/src/terminal_view/header.rs) `render_agent_header` + [`mod.rs`](../crates/tn-ui/src/terminal_view/mod.rs) |
| 用量环(灰轨 + agent 色弧 + % 标) | [02](../design/panels/02-workspace-panes.html) `.ring/.usage` | `mockup_replica::usage_pill` | [`header.rs`](../crates/tn-ui/src/terminal_view/header.rs) `usage_ring` + [`assets.rs`](../crates/tn-ui/src/assets.rs) ring |
| Shell 面板(cwd 头 + 终端正文) | [02](../design/panels/02-workspace-panes.html) `.phead/.body` | `mockup_replica::shell_pane` | [`header.rs`](../crates/tn-ui/src/terminal_view/header.rs) `render_shell_header` + [`mod.rs`](../crates/tn-ui/src/terminal_view/mod.rs) |
| 分屏容器(n-ary 平铺 + 分隔线) | [`mockup.html`](../design/mockup.html) `.work/.col` | `mockup_replica::workspace` | [`workspace.rs`](../crates/tn-ui/src/workspace.rs) `render_node` |

### ③ 侧栏(side panels)
| 面板 | HTML 原型 | gpui 还原 | 真实实现 |
|---|---|---|---|
| 资源管理器(文件树 + git M/U) | [03](../design/panels/03-side-panels.html) `.sidebar/.tree/.tnode` | `mockup_replica::sidebar_pane` | [`explorer.rs`](../crates/tn-ui/src/explorer.rs) `render` / `render_row` |
| 文件/Diff 查看器(切换 + 行号 + 增删) | [03](../design/panels/03-side-panels.html) `.viewer/.vh/.code` | `mockup_replica::viewer_pane` | [`viewer.rs`](../crates/tn-ui/src/viewer.rs) `render` / `render_diff` |

### ④ 浮层 / 启动器(overlays)
| 面板 | HTML 原型 | gpui 还原 | 真实实现 |
|---|---|---|---|
| 命令面板(scrim + 输入 + 结果行) | [04](../design/panels/04-overlays.html) `.palette/.prow` | `panels_replica::command_palette` | [`workspace.rs`](../crates/tn-ui/src/workspace.rs) `render_palette` |
| Quick Terminal 启动器(磁贴) | [04](../design/panels/04-overlays.html) `.quick/.launcher/.tile` | `panels_replica::quick_launcher` | [`quick_terminal.rs`](../crates/tn-ui/src/quick_terminal.rs) |

### ⑤ 状态屏(states)
| 面板 | HTML 原型 | gpui 还原 | 真实实现 |
|---|---|---|---|
| Block 卡(运行/成功/失败) | [05](../design/panels/05-states.html) `.block` | `panels_replica::block_cards` · `mockup_replica` block | [`block_view.rs`](../crates/tn-ui/src/block_view.rs) `bar_base` / `exit_chip` |
| 欢迎 / 空状态(新会话磁贴) | [05](../design/panels/05-states.html) `.welcome` | `panels_replica::welcome` | ⏳ **未实现(后置)** — 见 [BLUEPRINT.md](BLUEPRINT.md) 打磨项 |

## 改样 / 加面板的步骤
1. 改 HTML 原型(`design/panels/*.html` 或全窗 `design/mockup.html`),共享样式改 `design/calm-glass.css`;改了 mockup/主题跑 `TN_GEN_SPEC=1 cargo test -p tn-ui spec_gen` 刷新 §16。
2. 在对应 `crates/tn-ui/src/*` 用 `col/cola` token 落 gpui(查 [CSS_TO_GPUI.md](CSS_TO_GPUI.md) §16 抄数值,别估)。
3. `cargo test -p tn-ui` 三道守卫(token_drift / no_hardcoded_theme_colors / spec_gen)把关。
4. 真机比:`cargo run -p tn-ui --example mockup_replica`(全窗)/ `--example panels_replica`(浮层/状态),与浏览器开的 HTML 并排看。
