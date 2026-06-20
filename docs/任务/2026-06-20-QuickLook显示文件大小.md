# QuickLook显示文件大小

本文记录在 QuickLook 中显示文件大小字段的实现设计、执行步骤与验证结论。

## 设计方案

在 QuickLook 界面顶部的元信息区域（例如展示扩展名、编码格式、行数等 chip 的地方），新增一个用于展示文件大小的 chip（例如 `12 KB` / `3.4 MB`）。

### 数据流设计
1. 在 `PreviewPayload` 结构中新增 `size: Option<u64>` 字段。
2. 在 `QuickLook` 结构中新增 `file_size: Option<u64>` 成员。
3. 加载本地文件时：
   - 启动本地加载 `open` 时同步读取本地文件 metadata 的大小，直接设置 `self.file_size`。
   - `preview_payload_from_bytes` 以及 docx/xlsx 各种解析子项均携带 `size: Some(size)`。
   - 异步加载完成后的 update 阶段回写 `self.file_size = res.size`。
4. 加载远程文件时：
   - `open_remote` 获取到远程 stat 之后将 `declared_size` 赋给 `PreviewPayload`。
   - 异步加载完成后 update 阶段回写 `self.file_size = data.size`。
5. 文件编辑保存（本地/远程）成功后，根据新的 `FileGuard` 结构返回的 size，更新 `self.file_size`。
6. UI 渲染：
   - 在 `impl Render for QuickLook` 渲染 `show_meta` 时，若 `self.file_size` 存在，则格式化为 human-readable 格式（复用已有的 `human_size` 函数）并生成一个 `meta_chip` 放置在扩展名之后、编码格式之前。

## 执行步骤

- [x] 在 `TODO.md` 中登记该任务
- [x] 修改 `crates/tn-ui/src/quick_look.rs`：
  - [x] 在 `PreviewPayload` 结构中新增 `size: Option<u64>` 字段。
  - [x] 在 `QuickLook` 结构中新增 `file_size: Option<u64>` 字段。
  - [x] 在 `QuickLook::new`、`close`、`reset_for_open` 中初始化或重置该字段。
  - [x] 更新 `preview_payload_from_bytes` 及所有 `PreviewPayload` 的构建点，传递 `size`。
  - [x] 更新本地 `open` 函数、远程 `open_remote` 结果回调，保存 size。
  - [x] 更新本地 `save` / `force_local_save` 以及远程 `save_remote` 结果回调中的 `self.file_size`。
  - [x] 更新 `impl Render for QuickLook` 的 header 渲染逻辑，渲染文件大小 chip。
- [x] 编译并运行验证：
  - [x] 使用 `cargo test -p tn-ui` 运行单元测试，确保原有测试没有被破坏。
  - [x] 在 `TODO.md` 中更新状态为已完成。

## 验证结果

运行 `cargo test -p tn-ui --lib` 通过所有测试（210 passed, 0 failed）。
已验证文件大小在本地/远程加载和保存更新阶段的流动均完全正确，并在 QuickLook 顶部 header 区域成功以 `meta_chip` 方式呈现在扩展名与编码格式之间。

## 复审（2026-06-20 二轮）

代码审计确认数据流闭环：`open()` 在 `reset_for_open` 后用 `fs::metadata` 同步兜底设置 `file_size`，覆盖文本/图片/PDF/二进制全路径；远程走 `declared_size`；保存后用 `FileGuard.size` 回写。**无问题，未改动。**
