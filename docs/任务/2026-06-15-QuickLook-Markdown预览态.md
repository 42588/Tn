# QuickLook Markdown 预览态(默认渲染 · Enter 进编辑 · Esc 回预览)

- 日期:2026-06-15
- 范围:`crates/tn-ui/src/quick_look.rs`、`crates/tn-ui/Cargo.toml`
- 状态:已实现 · 待真机

## 缘起

QuickLook 打开 `.md` 文件时,过去和普通文本一样显示「带行号的原始源码」。诉求:
**Markdown 默认进入「预览态」直接显示渲染后的排版,按 Enter 切到「编辑态」改源码,Esc 回预览。**

## 设计决策(经确认)

| 决策点 | 选择 | 说明 |
| ------ | ---- | ---- |
| 切换方式 | Enter→编辑,Esc→预览 | 两态来回切。Enter/Esc 二者**均已存在**于 `on_key`(预览态 Enter→`enter_edit`、编辑态 Esc→`editing=false`+`sync_preview_from_edit`),无需新造状态机。 |
| 适用范围 | Markdown 渲染 + 代码高亮 | `.md/.markdown/...` 走渲染预览;代码文件原本就有只读语法着色预览,不动。 |
| 渲染引擎 | **GPUI 原生**(pulldown-cmark 解析 → `div`/`StyledText`/`TextRun`) | **不引 WebView**:整进程是 GPUI 终端 app,且契约「原型必须 GPUI 可还原」。HTML 只配做静态设计稿。 |

## 实现

- 依赖:`pulldown-cmark 0.12`(纯 Rust CommonMark,`default-features = false`)。
- 解析:`Parser::new_ext`,开启表格 / 删除线 / 任务列表 / 脚注扩展。
- 渲染(`quick_look.rs` 末尾「Markdown 预览渲染」段):事件流 → GPUI 元素。
  - 行内:`md_inline` 用样式栈层叠 **粗体 / 斜体 / 删除线 / 链接(INFO 蓝下划线)/ 行内码(mono+L2 底)**,累成
    `StyledText` + `Vec<TextRun>` 以获得正确换行。
  - 块级:`md_blocks` 递归(靠「落到本层的第一个 `End` 即收尾本容器」分层,版本无关):
    标题(字号阶梯 + h1/h2 底部发丝边)、段落、**代码块(复用文件预览的 `highlight()` 着色器)**、
    有序/无序列表(任务列表 ☑/☐)、引用(左 2px 竖条)、表格、分隔线、图片占位(🖼 + alt)。
  - 色板严守磷光契约:正文走文字阶梯 T0/T2,链接用 INFO 蓝,**不滥用磷光 PH**;结构靠发丝边/海拔。
- 接线:`Render::render` body 链新增分支
  `!editing && tab==File && Text && is_markdown_path()` → `markdown_view()`;
  容器 `overflow_y_scroll` 纵向滚动,背景同预览面 L3(`CODE_BG`)。
- 编辑/回切:Enter 进编辑走原自绘编辑器(`file_element`);Esc 回预览,`sync_preview_from_edit`
  把未保存改动镜像进 `file_data`,预览即时反映改后内容。

## 验证

- `cargo test -p tn-ui markdown` 绿:`markdown_path_detection`、`markdown_code_fence_collects_lines`、
  既有 `markdown_file_uses_visual_soft_wrap_while_code_keeps_horizontal_scroll`。
- `cargo check --workspace` 通过(唯一 warning 为既有的 `local_dir_picker::open_selected`,无关)。
- **待真机**:① 各 md 元素排版观感(标题层级 / 列表缩进 / 代码块 / 表格);② 大文件滚动;
  ③ Enter↔Esc 来回切与未保存镜像;④ CJK 正文换行。
