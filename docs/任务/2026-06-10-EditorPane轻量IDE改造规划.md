# EditorPane 轻量 IDE 改造规划

## 定位

记录 2026-06-10 对 Quick Look / Editor Pane 体验问题的复盘、外部设计研究、学术/HCI 原则整理和新增模块方案落地。本任务只产出规划文档和 TODO 登记,不执行源码改造。

## 背景

用户指出当前 Editor Pane 与 Quick Look 数据互通不足,外部变更和保存不能及时进入同一体验,打字没有手感,缺少可信光标,更像“存在而已”。同时用户希望 Markdown 在 Quick Look 中默认能像文档一样阅读,并希望 Editor Pane 最终成为具备特色、适合终端工作流的轻量 IDE。

## 具体内容

- [x] 使用 Web-Rooter 检查环境并检索现代编辑器/IDE 资料。
- [x] 派遣并行研究智能体,分别调研现代编辑器设计和 HCI / 开发者体验原则。
- [x] 阅读官方或权威资料:Zed、VS Code、Cursor、Obsidian、Typora、Kakoune、Helix、NN/g、web.dev。
- [x] 使用 academic-research-suite 的 deep-research/synthesis 思路综合证据,区分证据、推断和 Tn 设计含义。
- [x] 新增模块方案文档:[Tn 轻量 IDE 编辑器](../新增模块/Tn轻量IDE编辑器.md)。
- [x] 更新 [新增模块索引](../新增模块索引.md)。
- [x] 将本任务加入 [TODO](../../TODO.md) 当前任务。
- [x] 完成 Phase 0:明确现有 Editor Pane 只是实验性轻量编辑宿主,不是完整轻量 IDE。
- [x] 同步产品体验与架构文档中的边界、不可承诺范围和命名门槛。

## 验证 / 状态

- 本任务为文档规划,未执行源码实现。
- Web-Rooter `wr doctor` 通过:13/13。
- 外部搜索中部分通用搜索超时或低相关,已改用官方页面和权威资料直接抓取。
- 方案结论:Editor Pane 有必要,但只有在成为“终端旁轻量 IDE 工作区”时才有价值;优先级应是可信编辑底座、Markdown 阅读/Live Preview、终端往返、工作集和非打断式 AI。
- Phase 0 已完成边界澄清:当前 Editor Pane 只能称为实验性轻量编辑宿主;Phase 1 前不称可信长驻编辑器,Phase 3/4 前不称轻量 IDE 已实现。
- 2026-06-10:按用户要求暂停并移入 [TODO](../../TODO.md) 待办队列,等待下一次指令后再进入 Phase 1;不得直接把本文视为已完成实现。

## 下一步

- 当前暂停,等待用户下一次指令。
- 恢复后下一步应进入 Phase 1:可信编辑底座。
- Phase 1 先拆 Quick Look / Editor Pane 文档会话语义、保存 guard、光标/selection/IME/输入手感。
- 不建议先做 AI 或复杂 IDE 面板;当前最大风险是编辑器不可信。

## 反向链接

- 当前队列:[TODO](../../TODO.md)
- 新增模块:[Tn 轻量 IDE 编辑器](../新增模块/Tn轻量IDE编辑器.md)
- 架构:[编辑器与快速预览](../架构/编辑器与快速预览.md)
- 产品体验:[快速预览与编辑](../产品体验/快速预览与编辑.md)
