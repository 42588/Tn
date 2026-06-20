# QuickLook显示垂直滚动条与快捷滚动

## 任务背景
在文件预览（Quick Look）时，当文件或 Diff 内容超出视口高度时，无法拖拽垂直滚动条进行快速定位，也无法使用键盘 PageUp/PageDown/Home/End 键在文件内容中快速翻页和定位，影响预览体验。

## 目标设计
1. 增加垂直滚动条：
   - 包含自绘视觉 Thumb 细条（3px 宽，边缘偏移 5px）。
   - 交互响应区为 14px 宽的透明右侧热区，支持点击跳转及按下拖拽定位。
   - 复用原有的 `el_scroll_y` 物理偏移进行视图定位。
2. 增加快捷键导航：
   - 键盘 `PageUp` / `PageDown` 键在 `Tab::File` 下进行整屏上下翻页。
   - `Home` / `End` 键在 `Tab::File` 和 `Tab::Diff` 下快速定位到最上方和最下方。

## 进度与计划
- [x] 在 `geometry.rs` 中实现 `VScrollThumb` 及相关计算函数，并导出
- [x] 在 `quick_look.rs` 中添加 `vscroll_drag` 状态并完成生命周期重置
- [x] 在 `quick_look.rs` 的 `file_element` 和 `diff_element` 中增加右侧拖拽触发热区与移动处理逻辑
- [x] 在 `paint_file_preview` 和 `paint_diff_preview` 中绘制垂直滚动条 Thumb
- [x] 在 `on_key` 键盘事件中支持 PageUp / PageDown / Home / End 键快速滚动
- [x] 编写并运行单元测试以验证垂直滚动计算公式的正确性
- [x] 启动项目进行手动真机测试验证

## 验证结果
1. **单元测试验证**：在 `crates/tn-ui/src/editor/geometry.rs` 中新增的 `v_scroll_thumb_and_drag_are_inverse` 测试涵盖了所有滚动缩放计算和拖拽逆向反算。通过 `cargo test -p tn-ui --lib` 确认 211 个用例全数通过。
2. **构建与排版**：整个 workspace 的编译和 cargo check 均正常无 warnings/errors，滚动条与 Phosphor 设计语言在逻辑上契合良好。
