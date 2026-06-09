# Quick Look 外部保存刷新修复

## 目标

修复外部编辑器保存当前 Quick Look 正在查看或编辑的本地文件后,Quick Look 不会及时刷新内容的问题。

## 根因

内置编辑器保存会主动发 `QuickLookEvent::FileSaved`,workspace 随即刷新 agent rail 和 explorer。外部编辑器保存虽然能被 explorer 的目录 watcher 看到,但该 watcher 只在 `ExplorerView` 内部触发 `rebuild`,没有把“当前打开文件可能发生外部变化”通知给 Quick Look。因此文件树可刷新,Quick Look 内容仍停留在旧快照。

## 改动

- Explorer 的目录 watcher 在 debounce 后发出 `ExplorerChanged` 事件。
- Workspace 订阅 `ExplorerChanged`,通知 Quick Look 检查当前打开的本地文件。
- Quick Look 新增 `refresh_after_external_change`:
  - 仅处理本地文件,远端文件仍由远端保存/刷新路径管理。
  - 若当前处于预览态且没有未保存编辑,磁盘 guard 变化时自动重新打开当前文件。
  - 若当前处于编辑态,外部保存不自动 reload、不退出编辑、不立即弹冲突;后续 `Ctrl+S` 仍通过保存 guard 提示冲突。
  - 若当前处于预览态但已有 dirty 镜像,只显示保存冲突,不覆盖编辑缓冲。
- 新增 `ExternalReloadDecision` 和 `external_reload_decision_refreshes_clean_file_but_preserves_dirty_edit` 回归测试。

## 验证

- RED: `cargo test -p tn-ui --lib external_reload_decision_refreshes_clean_file_but_preserves_dirty_edit` 初始失败,确认 clean 外部保存未触发 reload 的策略缺口。
- GREEN: 同一测试通过。
- 回归: `cargo test -p tn-ui --lib` 通过,158 passed,0 failed。

## 后续

真机复验:
- Quick Look 预览一个本地文本文件,用外部编辑器保存该文件,约 500ms debounce 后内容应自动更新。
- Quick Look 编辑态下用外部编辑器保存同一文件,内置编辑器应保持编辑态且不自动同步;随后 `Ctrl+S` 时应出现保存冲突条,不会静默覆盖内置编辑器缓冲。
