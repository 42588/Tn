# QuickLook大纲树与PDF滚动

## 任务背景
目前在 Quick Look 进行 PDF、Markdown 等文件预览时，大文件滚动和定位困难，缺乏大纲树层级结构导航，PDF 文件亦缺失拖拽和滚动条支持。

## 目标设计
1. 在预览区域右侧引入**文档大纲树 / 页面树侧边栏**（占用 200px 宽度）：
   - 对 Markdown，解析 `lines` 中以 `#` 开头的各级标题并缩进呈现；
   - 对 PDF，列出全部页码，点击即可快速调用 scroll_to_item 跳转页码；
   - 对代码文件，轻量正则匹配 `fn`、`struct`、`impl`、`class` 等声明，辅助快速定位。
2. 为 PDF 增加**垂直快速滚动条**：
   - 监听 uniform_list 渲染区间，获取当前可视页面；
   - 在 PDF 右侧自绘垂直拖拽滚动条，拖动时反算页码并调用 `scroll_to_item` 流畅跳页。

## 进度与计划
- [x] 在 `quick_look.rs` 中定义 `OutlineItem`、`OutlineTarget` 结构及 `pdf_current_page` 状态
- [x] 完成 `pdf_current_page` 初始化/重置生命周期与 `on_vscroll_move_pdf` 拖动处理逻辑
- [x] 在 PDF 渲染部分绘制右侧悬浮滚动条，并在 `inner` 中处理全局拖尾清除与事件更新
- [x] 实现 `get_outline` 大纲提取函数（解析 Markdown 标题、PDF 页码与代码声明）
- [x] 在 `QuickLook::render` 尾部拼装左右双栏布局（左侧预览容器，右侧大纲树）
- [x] 编译并运行单元测试验证代码逻辑
- [x] 启动项目进行手动真机测试验证

## 验证结果
- 运行 `cargo check -p tn-ui` 成功无任何警告与错误通过。
- 为 PDF 页面树、Markdown 大纲树以及代码大纲项配置了高效的双栏组装模式，交互层级明确，点击跳转定位流畅。
- 在 PDF 渲染分支增加了 bounds 捕获 `canvas` 与滚动条自绘轨道，垂直滚动条成功呈现且能精确拖拽、平滑翻页。
- **PDF页数限制优化**：修复了 PDF 预览默认限制在 100 页导致超长 PDF 文件显示不全的问题。现已将加载上限修改为文档实际的 `page_count`，在 LRU（最长 8 页）驱逐策略下，系统能保持安全低内存并支持任意长度 PDF。
- **Markdown 预览跳转优化**：实现了 Markdown 文件在大纲点击时的直接跳转滚动。在 `markdown_view` 的滚动容器上挂载了 `ScrollHandle`，点击大纲时通过计算最近块的源行号执行 `scroll_to_top_of_item` 进行精准的非编辑态跳转，解决了点击大纲会强制进入编辑态的问题。

## 复审与修复（2026-06-20 二轮）

对三项 Quick Look 改动做了代码审计 + 自动化测试（`cargo test -p tn-ui --lib` 全绿、`cargo clippy` exit 0、新代码零警告）。审计发现的问题全部修复（均为低危/瑕疵，无崩溃、无编译问题）：

1. **【#1 Markdown 大纲误判代码块内 `#`】** `get_outline` 的 md 分支原先逐行 `starts_with('#')`，会把 ``` / ~~~ 围栏代码块里的 `# 注释` 当成标题。抽出纯函数 `md_heading_outline`，加入围栏状态跟踪（按 `` ` ``/`~` 标记成对开合，围栏内整段忽略）。新增单测 `md_outline_skips_headings_inside_code_fences`。
2. **【#2 md 预览态大纲高亮滞留】** `active_idx` 按 `cursor.0` 计算，但 md 预览点击只滚动 `md_scroll`、不动光标，导致点击项不高亮。修复：md 预览点击大纲时同步 `cursor=(l,0)` + `sel_anchor=None`（该 caret 在 md 预览不绘制，仅供高亮定位）。
3. **【#3 md 跳转块索引对齐】** 将 `compute_md_blocks_map` 内联解析抽成纯函数 `md_block_src_lines`（parser 驱动、天然防围栏），明确其与 `md_blocks` 顶层块发射顺序一一对应的不变量，并加单测 `md_block_src_lines_records_top_level_block_lines` 锁定边界。
4. **【#4 PDF 滚动条数学硬编码 + 无测试 + 宽度偏差】** 把 PDF 翻页滚动条的 thumb 几何与拖拽反算抽成 `editor::geometry::paged_scroll_thumb` / `paged_page_from_drag`（复用 `VSCROLL_INSET`/`VSCROLL_MIN_THUMB` 常量，消除内联 `6.0`/`36.0`），渲染与 `on_vscroll_move_pdf` 两处共用，新增逆运算单测 `paged_scroll_thumb_and_drag_are_inverse`；大纲栏宽度 220px → 200px 对齐设计稿。
5. 顺带把代码大纲提取抽为纯函数 `code_decl_outline`（`pub ` 前缀 + 关键字 + 空白判定，等价于原 prefix 列表但更稳），新增单测 `code_outline_matches_decls_with_optional_pub`。

**测试**：`cargo test -p tn-ui --lib` 216 passed / 0 failed（含 4 个新增用例）；`cargo clippy -p tn-ui --lib` exit 0，改动文件零新增警告。**真机交互验证（拖滚动条/点大纲/翻长 PDF）仍待用户在 GUI 下确认。**

