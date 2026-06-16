# QuickLook 内存与 GPU 占用优化

## 任务背景
用户反馈在来回切换文件、关闭快速预览编辑器（QuickLook）后，以及关闭主窗口后，Tn 终端的内存与 GPU 占用并没有下降，出现资源无法回收、GPU 显存暴涨的现象。

## 根因分析
1. **GPU 纹理资源未卸载**：
   在 GPUI 中，图片及 PDF 页面通过 `gpui::img(img_source)` 渲染时，会触发图片解码并将其作为 `RenderImage` 缓存到 `SpriteAtlas`（GPU 显存中的纹理集）。
   旧的 `QuickLook::close` 和 `reset_for_open` 仅调用了 `img.clone().remove_asset(cx)`，只会将底层的图片加载 `Task` 从 CPU 侧的 `App::loading_assets` 全局哈希表中移除，但**从未调用 `cx.drop_image(render_image, window)`**，导致 GPU 显存中的纹理从未被释放，切换多次或预览多页 PDF 后 GPU 显存爆满。
2. **CPU 全局缓存泄露**：
   `App` 的生命周期是全局的（因为有关闭窗口仅隐藏至系统托盘的设计，进程依然存活）。
   每当 QuickLook 加载图片或 PDF 时，加载任务被全局存在 `App::loading_assets`。
   如果用户直接关闭主窗口而不按 Esc 键关闭 QuickLook，`QuickLook` 实体会被释放，但由于没有实现 `Drop` 或是 `on_release` 钩子，它所加载的这些全局 CPU 任务和 GPU 纹理均不会被回收，造成“幽灵”内存泄露。

## 解决方案
1. **定义辅助函数 `evict_image_asset`**：
   通过公共的 `img.clone().get_render_image(window, cx)` 方法安全获取已加载的 `Arc<RenderImage>`。
   若获取到对应的 `RenderImage`，调用 `cx.drop_image(render_image, Some(window))` 清理 GPU 纹理映射。
   最后，调用 `img.clone().remove_asset(cx)` 清理 CPU 侧 `loading_assets` 全局任务缓存。
2. **改造 QuickLook 的缓存清理逻辑**：
   重构 `close()`、`reset_for_open()` 以及异步加载 stale 任务时的 `img.remove_asset(cx)` 逻辑，使其均调用 `evict_image_asset`。
3. **实现 `on_release` 钩子以防止关闭窗口内存泄露**：
   在 `QuickLook::new` 中，使用 `cx.on_release` 注册一个实体释放回调，在 `QuickLook` 被 Drop 释放前（如直接关闭主窗口时），调用 `evict_assets_internal` 将当前仍在持有的图片与 PDF 的全部加载缓存及纹理一次性清理干净。

## 验证结果
1. 运行 `cargo check` 确保编译通过。
2. 运行 `cargo test -p tn-ui`，全量测试 208 个用例全部成功通过，无任何 Regression：
   ```
   test result: ok. 208 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
   ```
