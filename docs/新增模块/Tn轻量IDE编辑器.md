# Tn 轻量 IDE 编辑器

## 定位

本文记录 Quick Look 与 Editor Pane 从“快速预览 + 基础编辑宿主”升级为 Tn 轻量 IDE 编辑器的产品方案、研究依据、分阶段边界和验收标准。本文是规划文档,不表示功能已经实现。

目标定位:Quick Look 负责快速阅读、预览和小改;Editor Pane 最终负责长驻编辑、终端旁并排工作、保存安全、错误跳转和轻量 IDE 能力。二者必须共享同一套文档语义,不能成为两套割裂的编辑器。

## 背景

当前 Editor Pane 已具备宿主形态、基础输入、状态栏和 dirty close prompt,但用户反馈指出它还没有形成可信编辑体验:

- Quick Look 与 Editor Pane 的数据语义不够互通,外部变更和保存状态不能自然连续。
- Editor Pane 输入手感弱,缺少稳定可见光标,不像 Quick Look 或终端那样可信。
- Markdown 在 Quick Look 中默认显示源码,不符合“快速看文档”的主要场景。
- 用户期望 Tn 能在终端中无缝完成编辑工作,而不是只把编辑器作为一个存在的 pane。

因此,Editor Pane 的必要性不来自“多一个编辑框”,而来自 Quick Look 做不到的长驻、并排、恢复上下文、终端往返和轻量代码智能。

## 当前边界

现有 Editor Pane 只能定义为实验性轻量编辑宿主。它可以承载 Quick Look 升级后的长驻 pane 形态、基础本地文本输入、状态栏和未保存关闭提示,但不能被包装为完整 IDE,也不能被视为已经达到 Quick Look 编辑体验的产品级一致性。

当前可承诺范围:

- 基础宿主:可从 Quick Look 显式打开为 Editor Pane,进入长驻编辑容器。
- 基础编辑:提供本地文本输入、撤销/重做、复制/粘贴、状态栏、本地保存和 dirty close prompt。
- 安全下限:关闭 pane、关闭 tab 或退出应用时,未保存内容不应静默丢失。

当前不可承诺范围:

- Quick Look 与 Editor Pane 的完整数据语义互通、同源保存状态机和跨入口一致刷新。
- Editor Pane 与 Quick Look/终端一致的真实光标、selection、IME caret、输入手感和动画降级策略。
- Markdown 默认渲染预览、Live Preview、Split Preview 和源码/预览位置映射。
- Quick Look 格式保持、编码/换行保持、外部变更 guard 与保存冲突 guard 的共用。
- 终端输出跳转、保存后验证、诊断/grep/diff hunk 工作集和非打断式 AI。

边界规则:

- Phase 1 完成前,文档和 UI 文案只能称其为“实验性轻量编辑宿主”或“Editor Pane 基础宿主”。
- Phase 1 通过验收后,才可称为“可信长驻编辑器”。
- Phase 3 到 Phase 4 的终端往返和工作集完成前,不应称为“Tn 轻量 IDE”已实现。

## 研究依据

### 现代编辑器启发

- Zed 把终端、任务、编辑器和 AI 都纳入同一个工作区。其 Terminal 文档强调内置终端与编辑器集成,Multibuffers 用一个可编辑工作集承载跨文件内容,Inline Assistant 使用当前选区或光标附近上下文做局部改写。参考:[Zed Terminal](https://zed.dev/docs/terminal)、[Zed Multibuffers](https://zed.dev/docs/multibuffers)、[Zed Inline Assistant](https://zed.dev/docs/ai/inline-assistant)。
- VS Code 的 Markdown 体验不是只显示源码,而是提供源码编辑、预览、侧边预览和同步滚动;其终端是编辑器工作区的一等成员。参考:[VS Code Markdown](https://code.visualstudio.com/docs/languages/markdown)、[VS Code Terminal Basics](https://code.visualstudio.com/docs/terminal/basics)。
- Obsidian 与 Typora 都证明 Markdown 的主要阅读场景应该优先减少语法干扰。Obsidian 提供 Reading、Live Preview、Source mode;Typora 以 Live Preview 减少源码和预览之间的切换。参考:[Obsidian Views and editing mode](https://obsidian.md/help/edit-and-read)、[Typora Quick Start](https://support.typora.io/Quick-Start/)。
- Kakoune 与 Helix 的启发不是照搬 modal 编辑,而是强调“选择先可见,动作后发生”。这对 Tn 的选区、复制、删除、AI 改写都很重要。参考:[Why Kakoune](https://kakoune.org/why-kakoune/why-kakoune.html)、[Helix Usage](https://docs.helix-editor.com/master/usage.html)。
- Cursor、Zed、VS Code Copilot 的共同趋势是把 AI 放在选区、行内、命令面板或任务面板中,而不是默认打断输入。AI 编辑必须可预览、可撤销、可取消。

### HCI 与开发者体验依据

- NN/g 的响应时间原则把 0.1s 作为即时反馈阈值,1s 作为不中断思维流的边界。web.dev RAIL 也把输入响应和帧预算作为交互性能核心。参考:[Response Time Limits](https://www.nngroup.com/articles/response-times-3-important-limits/)、[RAIL model](https://web.dev/articles/rail)。
- 渐进披露要求默认只暴露核心能力,高级能力按需出现。Tn 不能一开始塞满传统 IDE 控件。参考:[Progressive Disclosure](https://www.nngroup.com/articles/progressive-disclosure/)。
- 系统状态可见性要求用户始终知道 dirty、保存中、保存失败、外部已变更、冲突、只读和当前模式。参考:[Visibility of System Status](https://www.nngroup.com/articles/visibility-system-status/)。
- 直接操作理论强调对象持续可见、操作可逆、反馈即时。对 Tn 来说,Quick Look 升级到 Editor Pane 后,同一份内容、光标、选区、undo、dirty 和保存状态必须连续。

## 设计原则

### 1. 可信编辑底座优先

输入、删除、光标移动、选区、复制、IME 合成和保存安全是底座。任何 Markdown 渲染、动画、AI、诊断、文件监听都不能抢占输入链路。

要求:

- Editor Pane 必须拥有真实光标、稳定选区、准确 IME caret rect 和可复制文本。
- 动画只跟随打字/删除绘制,不参与真实布局、命中测试、选择、复制或 IME。
- 输入反馈目标应落在用户感知的即时范围内;高负载、大文件、IME 合成、拖选和选区态必须自动降级为 snap。

### 2. Quick Look 与 Editor Pane 共享文档语义

Quick Look 与 Editor Pane 不能各自维护互不可信的 buffer。二者应共享或通过同一会话层同步:

- 文本内容、cursor、selection、undo/redo、dirty。
- 文件来源、本地/远端身份、打开时 guard、当前磁盘版本、保存状态。
- 外部变更策略:预览态 clean 自动刷新;编辑态 dirty 不强制刷新,保存时走冲突 guard。
- 关闭和退出策略:任何未保存内容都必须可见提示,取消后状态不丢。

### 3. Markdown 先读后改

Quick Look 打开 Markdown 时默认应显示渲染预览,因为“快速看文档”的主任务是阅读。源码不消失,而是成为显式模式。

建议模式:

| 模式 | 目标 | 默认入口 |
|---|---|---|
| 渲染预览 | 快速阅读 `.md`、README、说明文档 | Quick Look 打开 Markdown |
| Source | 精确查看 Markdown 源码 | `Tab` 或模式切换 |
| Live Preview | 编辑时尽量减少语法噪音 | Quick Look 编辑态和 Editor Pane Markdown |
| Split Preview | 长文档或审校时源码/预览并排 | Editor Pane 按需展开 |

复杂 Markdown、frontmatter、表格、代码块、未识别语法必须保留 Source 兜底。

### 4. 终端往返是特色

Tn 的差异化不应是复制 VS Code,而是把“终端输出 -> 打开文件 -> 修改 -> 保存 -> 回到命令验证”压缩成一个闭环。

目标能力:

- 终端输出中的 `path:line:col` 可打开或聚焦 Editor Pane。
- Editor Pane 显示关联终端 cwd、文件来源和最近命令上下文。
- 保存后可提示运行上次相关命令,但不自动执行危险命令。
- 错误行、grep 结果、git diff hunk 可进入同一轻量工作集。

### 5. 渐进式轻量 IDE

默认界面只显示文件标题、模式、dirty、保存状态、光标位置和必要操作。高级能力通过命令面板、状态栏按钮或按需面板展开。

建议能力层:

1. 基础编辑:光标、选择、复制、粘贴、撤销、保存、查找。
2. 文档阅读:Markdown 渲染、outline、代码块阅读、源文切换。
3. 终端往返:错误跳转、保存后验证、cwd 上下文。
4. 工作集:相关文件、搜索结果、diff hunk、诊断结果组成当前任务集。
5. AI:选区动作、行内建议、diff 预览、命令面板触发,不默认抢焦点。

## 模块边界

### 文档会话层

从当前 `DocumentSession` 扩展为 Editor/Quick Look 共用的文档会话模型。该层应拥有:

- `Document` 与行镜像。
- selection、cursor、undo/redo、dirty。
- source identity:本地路径、远端文件、临时文档。
- text format:encoding、newline、final newline。
- file guard:打开时版本、当前磁盘版本、冲突状态。
- save state:clean、dirty、saving、failed、conflict、readonly。

### 编辑表面层

Quick Look 和 Editor Pane 应尽量复用同一自绘编辑表面,只在容器、尺寸、工具条和模式入口上不同。

- Quick Look:轻量、浮层、快速阅读和小改。
- Editor Pane:长驻、可分屏、可恢复、可承载终端往返和轻量 IDE。

### Markdown 渲染层

Markdown 预览应是独立渲染层,避免把 Markdown 解析塞进输入热路径。

要求:

- 预览渲染异步或增量化。
- 编辑态优先保证输入,预览可以延迟更新。
- 源码位置与预览块建立映射,支持滚动同步和点击跳回源码。

### 终端桥接层

终端桥接层负责把终端输出、cwd、命令状态和 Editor Pane 连接起来。

要求:

- 保守识别 `file:line:col`、编译错误和 grep 结果。
- 不把任意文本当命令执行。
- 所有运行命令动作都需要明确用户触发。

## 分阶段计划

### Phase 0: 明确边界

- [x] 把现有 Editor Pane 标记为实验性轻量编辑宿主,不把它包装成完整 IDE。
- [x] 文档中明确当前未完成:数据语义互通、光标/手感、Markdown 预览、保存冲突共用、终端往返。
- [x] 明确命名门槛:Phase 1 前不称可信长驻编辑器,Phase 3/4 前不称轻量 IDE 已实现。

### Phase 1: 可信编辑底座

- Quick Look 与 Editor Pane 共享同一文档会话语义。
- Editor Pane 接入 Quick Look 同源自绘编辑表面,补齐光标、selection、copy、IME、scroll、find。
- 保存接入 Quick Look 的格式保持和冲突 guard。
- 外部文件变更进入统一状态机。

验收:

- Quick Look 编辑后升级到 Editor Pane,内容、光标、undo、dirty 连续。
- Editor Pane 编辑保存后,Quick Look/文件树/agent 改动轨能及时刷新。
- 外部编辑器保存时,预览态自动刷新;编辑态保留本地修改并在保存时提示冲突。
- 连续输入、删除、IME、拖选、复制不受动画影响。

### Phase 2: Markdown 阅读与 Live Preview

- Quick Look 打开 `.md` 默认渲染预览。
- 提供 Source/Preview 切换。
- Editor Pane 中 Markdown 支持 Live Preview 或 Split Preview。
- 预览与源码建立滚动和位置映射。

验收:

- README、任务文档、普通 Markdown 默认可直接阅读。
- Source 模式可精确复制和编辑原文。
- 复杂 Markdown 无法稳定渲染时自动回退 Source。

### Phase 3: 终端无缝编辑闭环

- 终端输出路径可打开/聚焦 Editor Pane。
- Editor Pane 保存后可触发“运行上次相关命令”入口。
- 编译错误、grep、git diff hunk 可跳转到对应文件行。

验收:

- 用户从终端错误行进入文件,修改保存后回到终端验证,不需要离开 Tn。
- 误识别链接不会自动执行命令或破坏终端输入。

### Phase 4: 轻量 IDE 工作集

- 引入任务工作集:相关文件、搜索命中、诊断、diff hunk 可在一个 pane 中聚合。
- 支持 outline、symbols、diagnostics、Git diff 作为按需面板。
- 保持默认界面克制,高级面板只在用户打开时出现。

验收:

- 用户能围绕一个终端任务保留上下文,而不是在文件树中反复找文件。
- 关闭工作集前仍有 dirty 和冲突保护。

### Phase 5: 非打断式 AI

- 选区动作:解释、改写、生成测试、修复错误。
- 行内建议:ghost text 或 diff 预览,用户显式接受。
- Agent 任务:进入独立任务面板,不抢输入焦点。

验收:

- AI 输出默认不修改文件,必须经过 diff/accept。
- 用户正在打字、选择、IME 合成时 AI 不插入 UI 抢焦点。

## 非目标

- 不复制完整 VS Code / JetBrains IDE。
- 不先做插件生态。
- 不让 AI 常驻占据主编辑体验。
- 不用 Markdown WYSIWYG 牺牲 Source 控制权。
- 不在输入链路里做重解析、重渲染或同步 AI 请求。

## 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| Markdown 渲染阻塞输入 | 打字卡顿,用户不信任编辑器 | 渲染异步/增量,输入优先,大文档降级 |
| Quick Look / Editor Pane 状态分裂 | 保存结果不可预测 | 单一文档会话和统一保存状态机 |
| 模式过多 | `Esc`、方向键、输入含义混乱 | 状态栏清晰显示模式,快捷键保持稳定 |
| AI 打断输入 | 破坏终端工作流 | 选区/命令面板触发,diff 接受,不抢焦点 |
| 终端链接误触 | 错开文件或执行风险命令 | 只打开文件,命令必须显式确认 |

## 当前状态

- 实现状态:规划。
- 验证状态:不适用。
- Phase 0 边界文档已完成。
- 本文只定义改造方向、边界和验收标准,尚未执行代码实现。

## 反向链接

- 母文档:[新增模块索引](../新增模块索引.md)
- 当前任务:[2026-06-10-EditorPane轻量IDE改造规划](../任务/2026-06-10-EditorPane轻量IDE改造规划.md)
- 架构专题:[编辑器与快速预览](../架构/编辑器与快速预览.md)
- 产品体验:[快速预览与编辑](../产品体验/快速预览与编辑.md)
