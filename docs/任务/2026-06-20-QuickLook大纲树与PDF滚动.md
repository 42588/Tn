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

