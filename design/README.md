# Tn 设计原件 · 磷光 PHOSPHOR

本目录是 Tn 当前唯一权威的界面设计原件。设计语言为「磷光 Phosphor」:**结构给秩序,磷光给生命** —— 为 GPUI 精准还原而设计,取代已证伪的 Calm Glass 玻璃方案。

## 查看方式

用浏览器直接打开 [panels/index.html](panels/index.html)(总览宣言:四原则、海拔阶梯、渲染契约、全部分页入口)。

| 图纸 | 主题 | 实现入口 |
|---|---|---|
| [panels/01-window-chrome.html](panels/01-window-chrome.html) | 窗体外壳:标题栏 / Tab 身份棒 / 窗控 / App Menu / 状态栏 | `crates/tn-ui/src/workspace.rs` `style.rs` |
| [panels/02-workspace-panes.html](panels/02-workspace-panes.html) | 工作区:Explorer / Agent 板面 / Shell 板面 / 活动栏 / 块条 | `crates/tn-ui/src/terminal_view/` `explorer.rs` |
| [panels/03-quicklook-editor.html](panels/03-quicklook-editor.html) | **特色① QuickLook 编辑器**:速览 / 编辑 / Diff / CJK 固定单元格 | `crates/tn-ui/src/quick_look.rs` `tn-editor` |
| [panels/04-ghost-terminal.html](panels/04-ghost-terminal.html) | **特色② 幽灵终端**:召唤线 / 顶垂浮窗 / 残影 / 会话常驻 | `crates/tn-ui/src/quick_terminal.rs` |
| [panels/05-pet-system.html](panels/05-pet-system.html) | **特色③ 宠物系统**:栖位 / 品种架 / 状态机 / 互动 | `docs/宠物/` + 终端 overlay |
| [panels/06-overlays.html](panels/06-overlays.html) | 浮层:命令面板 / 分屏启动器 / SSH 信任 / 确认件 | `crates/tn-ui/src/workspace.rs` |
| [panels/07-states.html](panels/07-states.html) | 状态:欢迎页 / 空板面 / 命令块三态 / SSH 过程态 | `crates/tn-ui/src/welcome.rs` `block_view.rs` |

共享样式(即 token 单源)在 [phosphor.css](phosphor.css),文件头写明渲染契约全文。

真机截图回归差异汇总见 [原型与真机截图差异总结](原型与真机截图差异总结.md),其中已合并宏观总结和逐页像素级复审。

## 渲染契约(为什么这套能被 Rust 还原)

原型 CSS 只使用 GPUI 已验证的能力,一一对应:

- 大面积一律不透明纯色(海拔 L0–L4)→ `Div::bg`;杜绝大面渐变色带。
- 平铺板面零投影,深度 = 2px 接缝 + 1px 发丝边 → 规避平铺投影渗血坑。
- `linear-gradient` 只出现在 ≤4px 条带(tab 身份棒 / 幽灵顶缘)→ `linear_gradient`。
- `box-shadow` 仅真浮层外投影 → `BoxShadow`(父级不裁剪)。
- scrim 为纯色压暗,无 backdrop blur;用量环 conic 仅示意,GPUI 用 path 弧线。
- 禁用:`backdrop-filter` / `filter` / inset shadow / `text-shadow` / 噪点 / `letter-spacing`。

## 验收口径

实现照各页 SPEC 表逐项落 token(集中在 `crates/tn-ui/src/style.rs`);真机 `target/ui-shots` 截图与图纸对照,核对三件事:海拔阶梯、接缝深度、磷光纪律。不引入样式守卫测试。

设计文档与决策记录见 [docs/设计索引.md](../docs/设计索引.md)。
