# UI 渲染管线 — 物理隔离架构

> **对应源码**: `crates/tn-ui/src/terminal_view/mod.rs` · `crates/tn-ui/src/terminal_view/header.rs` · `crates/tn-ui/src/gitutil.rs`  
> **提交**: `fc9c03e refactor(ui): 活动栏状态机 — RailState 枚举 + 骨架屏 + 防过期丢弃`  
> **日期**: 2026-06-02

---

## 1. `crates/tn-ui/src/terminal_view/mod.rs` — RailState 枚举 + 代次计数 + refresh_changes

```rust
// ── 常量 ──

/// Activity rail (mockup `.arail` 本次改动): cap the changed-file cards (the narrow
/// rail shows a short stack).
pub(super) const RAIL_MAX_FILES: usize = 6;
/// Debounce for the working-tree change watcher: coalesce a burst of file events
/// (a save touches several files, a build churns many) into one `git diff` refresh.
const RAIL_WATCH_DEBOUNCE_MS: u64 = 450;

// ── 状态机 ──

/// Activity-rail「本次改动」state machine — keeps the UI render path a pure
/// read of an already-resolved state; no git/io inside `render()`. The enum
/// replaces ad-hoc `Vec` + `bool` flags so the render can distinguish between
/// "haven't run yet" (Idle), "background is computing" (Loading → skeleton),
/// and "data is ready" (Ready → real cards).
#[derive(Debug, Clone)]
pub enum RailState {
    /// No agent present (plain shell) → rail not rendered at all.
    Idle,
    /// Background git diff is in flight; UI draws a skeleton placeholder.
    Loading,
    /// Fresh data has arrived. `root` is the git working directory (paths in
    /// `files` are relative to it; used to resolve click→QuickLook absolute paths).
    Ready {
        files: Vec<crate::gitutil::FileChange>,
        root: std::path::PathBuf,
    },
}

// ── TerminalView 结构体（相关字段） ──

pub struct TerminalView {
    // ... 其他字段 ...
    pub(super) rail_state: RailState,
    /// Monotonic generation counter: incremented each time a background refresh
    /// is kicked off. The task captures the generation at spawn; on completion
    /// it is checked against `rail_generation` — stale results (from a previous
    /// refresh that finished after a newer one was already dispatched) are
    /// silently dropped. Wrapping on overflow (32-bit on 64-bit hosts → fine).
    rail_generation: usize,
    /// The directory the change watcher was started on (app cwd at launch, or
    /// the shell cwd for shell-typed agents). Used as a fallback in
    /// `refresh_changes` when the blocks model has no known cwd (launched
    /// agent panes carry no shell integration, so OSC 7 never fires).
    rail_cwd: Option<String>,
    // ...
}

// ── 初始化 ──

// (in TerminalView::new)
rail_state: RailState::Idle,
rail_generation: 0,
rail_cwd,

// ── 清空 ──

fn clear_agent(&mut self) {
    self.agent = None;
    self.agent_from_shell = false;
    self.usage = None;
    self.rail_state = RailState::Idle;
    self.rail_cwd = None;
    self.change_watcher = None;
}

// ── 核心: 后台刷新 + 防过期 ──

/// Refresh the activity-rail「本次改动」from real `git diff HEAD` in the pane's
/// cwd — off the UI thread, bounded. Triggered by the change watcher (变化即刷新)
/// and once on agent start. No-op once the agent is gone.
///
/// ## Stale-result prevention
/// Each call bumps `rail_generation`; the spawned task captures the generation at
/// dispatch time. When the task completes (potentially out of order — a slow git
/// run can finish AFTER a faster run that was dispatched later), the generation
/// is compared: stale results are silently dropped. This guarantees the UI never
/// regresses to an earlier diff snapshot.
pub(super) fn refresh_changes(&mut self, cx: &mut Context<Self>) {
    if self.agent.is_none() {
        return;
    }
    let Some(cwd) = self.cwd().or_else(|| self.rail_cwd.clone()) else { return };

    // Bump generation + switch to Loading → skeleton renders immediately.
    self.rail_generation = self.rail_generation.wrapping_add(1);
    let gen = self.rail_generation;
    self.rail_state = RailState::Loading;
    cx.notify();

    let exec = cx.background_executor().clone();
    cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
        // ── Background: expensive git ops (may block for >100ms) ──
        let (files, root) = exec
            .spawn(async move {
                let root = std::path::PathBuf::from(&cwd);
                let mut files = crate::gitutil::changes_for(&root);
                files.truncate(RAIL_MAX_FILES);
                (files, root)
            })
            .await;
        // ── Back on UI thread: only apply if still current ──
        let _ = this.update(cx, |v, cx| {
            if v.rail_generation != gen {
                // A newer refresh was dispatched while this one was in flight;
                // drop these stale results so the UI doesn't regress.
                return;
            }
            v.rail_state = RailState::Ready { files, root };
            cx.emit(UsageUpdated);
            cx.emit(FilesChanged);
            cx.notify(); // skeleton exits, real cards render
        });
    })
    .detach();
}

// ── 文件夹切换 ──

pub fn set_rail_root(&mut self, root: &std::path::Path, cx: &mut Context<Self>) {
    if self.agent.is_none() {
        return;
    }
    let cwd = root.to_string_lossy().to_string();
    self.rail_cwd = Some(cwd.clone());
    self.change_watcher = Self::spawn_change_watcher(cx, cwd);
    self.refresh_changes(cx);
}
```

---

## 2. `crates/tn-ui/src/terminal_view/header.rs` — 骨架屏 + Ready 渲染

```rust
//! Agent pane header UI (待优化清单 §6.2): the avatar + name/model + context
//! usage ring shown above Claude/Codex panes. Split out of `mod.rs` to keep the
//! render core lean; `impl super::TerminalView` so it can read the view's
//! private agent/usage/palette state. Only [`render_pane_header`] is called from
//! the parent (`render`); the rest are header-internal.

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, App, Context, Div, FontWeight,
    MouseButton, Overflow, SharedString, WeakEntity,
};
use tn_ai::AgentKind;
use tn_config::BillingMode;
use tn_core::Rgb;

use super::TerminalView;
use crate::style::{col, cola, icon, HOVER, INSET, R_CARD, UI_SANS};

impl TerminalView {
    /// This pane's identity accent: Claude coral / Codex teal, or the UI accent
    /// for a plain shell.
    fn agent_accent(&self) -> Rgb {
        match self.agent {
            Some(AgentKind::ClaudeCode) => self.claude_accent,
            Some(AgentKind::Codex) => self.codex_accent,
            None => self.ui_accent,
        }
    }

    // ... (render_agent_header, render_shell_header, arail_file — unchanged) ...

    /// agent 活动栏(mockup `.arail`):诚实状态行 + 「本次改动」真实 git diff 卡 + 提示。
    /// 数据 = `git diff HEAD`(pane cwd,后台有界 git 计算),**不解析终端正文**。
    /// **不伪造「运行中」实时态** → 状态行只显诚实的 git 摘要。
    /// 点卡片发 [`OpenInQuickLook`] 让 workspace 弹 Quick Look 看全 diff。
    ///
    /// ## Render-pure guarantee
    /// The render body performs **zero computation** — it only reads the pre-built
    /// `RailState` enum. All git I/O lives in `refresh_changes` on the background
    /// executor. `Loading` returns a skeleton immediately; the real cards appear
    /// when `Ready` arrives via channel delivery.
    pub(super) fn render_activity_rail(&self, cx: &mut Context<Self>) -> Div {
        let green = col(self.palette.ansi[2]);
        let red = col(self.palette.ansi[1]);

        // ── Build the chrome shell (status row + left border) once ──
        let rail_shell = |status: Div, body: Div| -> Div {
            div()
                .flex_none()
                .w(px(212.))
                .flex()
                .flex_col()
                .gap(px(11.))
                .pt(px(12.))
                .px(px(12.))
                .pb(px(14.))
                .min_h(px(0.))
                .overflow_hidden()
                .border_l(px(1.))
                .border_color(rgba(0xffffff0d))
                .font_family(UI_SANS)
                .child(status)
                .child(body)
        };

        // ── Status row (shared by all states) ──
        let build_status = |summary: &str, add: Option<u32>, del: Option<u32>| -> Div {
            let mut s = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.))
                .text_size(px(11.))
                .text_color(gpui::rgb(0xA6AFD4))
                .child(
                    div()
                        .w(px(7.))
                        .h(px(7.))
                        .rounded_full()
                        .flex_none()
                        .bg(col(self.agent_accent())),
                )
                .child(div().flex_1().child(SharedString::from(summary.to_string())));
            if let (Some(a), Some(d)) = (add, del) {
                if a > 0 || d > 0 {
                    s = s.child(
                        div()
                            .flex().flex_row().gap(px(5.)).flex_none()
                            .text_size(px(10.5)).font_weight(FontWeight(680.))
                            .child(div().text_color(green).child(SharedString::from(format!("+{a}"))))
                            .child(div().text_color(red).child(SharedString::from(format!("−{d}")))),
                    );
                }
            }
            s
        };

        match &self.rail_state {
            // ── Loading: skeleton placeholders ──
            super::RailState::Loading => {
                let status = build_status("正在分析改动…", None, None);
                let skeleton = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children((0..3).map(|_| {
                        div()
                            .w_full()
                            .h(px(32.))
                            .rounded(px(R_CARD))
                            .bg(rgba(INSET))
                    }));
                rail_shell(status, skeleton)
            }

            // ── Ready: real cards ──
            super::RailState::Ready { files, root } => {
                let total_add: u32 = files.iter().map(|f| f.add).sum();
                let total_del: u32 = files.iter().map(|f| f.del).sum();
                let summary = if files.is_empty() {
                    "工作区干净".to_string()
                } else {
                    format!("{} 个文件改动", files.len())
                };
                let has_files = !files.is_empty();
                let status =
                    build_status(&summary, has_files.then_some(total_add), has_files.then_some(total_del));

                if !has_files {
                    return rail_shell(status, div()
                        .text_size(px(10.5))
                        .text_color(col(self.ui_muted))
                        .pt(px(2.)).px(px(2.))
                        .child(SharedString::from("agent 改动会实时显示在这里")));
                }

                let mut scrollable = div()
                    .flex_1().min_h(px(0.)).flex().flex_col()
                    .gap(px(11.)).pb(px(14.)).overflow_hidden();
                scrollable.interactivity().base_style.overflow.y = Some(Overflow::Scroll);

                scrollable = scrollable.child(
                    div()
                        .text_size(px(10.)).font_weight(FontWeight(680.))
                        .text_color(col(self.ui_muted)).pt(px(2.)).px(px(2.))
                        .child(SharedString::from("本次改动")),
                );

                for (i, f) in files.iter().enumerate() {
                    let is_cur = i == 0;
                    let plus = format!("+{}", f.add);
                    let minus = (f.del > 0).then(|| format!("−{}", f.del));
                    let mut card = div()
                        .rounded(px(R_CARD)).py(px(8.)).px(px(10.))
                        .flex().flex_col().gap(px(6.));
                    card = if is_cur {
                        card.bg(cola(self.ui_accent, 0.06))
                            .border_1().border_color(cola(self.ui_accent, 0.22))
                    } else {
                        card.bg(rgba(INSET))
                    };
                    card = card.child(self.arail_file(f.name(), &plus, minus.as_deref()));
                    // root is from the Ready variant — always consistent with files
                    let abs = root.join(&f.path);
                    card = card.hover(|s| s.bg(rgba(HOVER))).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, _e, _w, cx| {
                            cx.emit(super::OpenInQuickLook(abs.clone()));
                        }),
                    );
                    scrollable = scrollable.child(card);
                }

                scrollable = scrollable.child(
                    div()
                        .text_size(px(10.)).text_color(gpui::rgb(0x474E72)).px(px(2.))
                        .child(SharedString::from("点卡片 = 速览全 diff")),
                );

                rail_shell(status, scrollable)
            }

            // ── Idle: shouldn't render (called only when agent is present) ──
            super::RailState::Idle => div(),
        }
    }

    /// Per-pane header — agent header for agents, else a shell `.phead`(cwd + chip).
    pub(super) fn render_pane_header(&self, weak: WeakEntity<Self>) -> Option<Div> {
        Some(match self.agent {
            Some(a) => self.render_agent_header(a, weak),
            None => self.render_shell_header(),
        })
    }
}
```

---

## 3. `crates/tn-ui/src/gitutil.rs` — 有界 git 调用(死代码已清除)

```rust
//! Shared, **bounded** git helpers: run git off the UI thread with a hard timeout
//! and no console flash, so a slow / `.git`-locked / AV-scanned git can never
//! freeze the window (踩过的坑: synchronous git on the UI thread froze the app).
//! Used by Quick Look (file diff) and the agent pane's activity rail (本次改动).
//!
//! Everything here is pure or a thin subprocess wrapper — the parsers are headless
//! unit-tested; the capture is `#[cfg(windows)]`-aware (`CREATE_NO_WINDOW`).

use std::path::Path;
use std::time::Duration;

/// Run `git <args>` in `root`, stdout captured, **bounded** to `timeout`, with **no
/// console flash**. `None` on timeout / spawn failure (caller treats that as "no
/// output"). The blocking `.output()` runs on a throwaway thread + `recv_timeout`,
/// so the caller blocks at most `timeout` and a stuck git can't hang anything —
/// **but never call this on the UI thread** (it blocks up to `timeout`); call it
/// from a background task. `.output()` drains stdout, avoiding the pipe-buffer
/// deadlock a `try_wait` loop would hit on big diffs.
pub(crate) fn capture_bounded(root: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let root = root.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(&root).args(&args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let _ = tx.send(out);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(s)) => Some(s),
        _ => None, // timeout or spawn error
    }
}

/// One changed file from `git diff --numstat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileChange {
    /// Path relative to the repo root (as git prints it).
    pub path: String,
    pub add: u32,
    pub del: u32,
}

impl FileChange {
    /// Display name = the path's final component (mockup `.afile .nm` shows the
    /// filename, e.g. `element.rs`).
    pub fn name(&self) -> &str {
        self.path
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.path)
    }
}

/// Parse `git diff --numstat` — one `add<TAB>del<TAB>path` per line (binary files
/// print `-<TAB>-<TAB>path`, counted as 0/0). Pure → headless unit-tested.
pub(crate) fn parse_numstat(text: &str) -> Vec<FileChange> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        out.push(FileChange {
            add: a.trim().parse().unwrap_or(0),
            del: d.trim().parse().unwrap_or(0),
            path: p.to_string(),
        });
    }
    out
}

/// Tracked changes vs `HEAD` in `root` (staged + unstaged), **bounded**. Blocking —
/// call from a background task. Empty when not a repo / no HEAD / no changes.
/// `--relative` makes the returned paths relative to `root` (not the repo toplevel),
/// so a caller can resolve a path back to an absolute one via `root.join(path)`.
pub(crate) fn changes_for(root: &Path) -> Vec<FileChange> {
    let out = capture_bounded(
        root,
        &["diff", "--no-color", "HEAD", "--numstat", "--relative"],
        Duration::from_millis(1200),
    );
    parse_numstat(out.as_deref().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_parses_counts_and_path() {
        let s = "3\t1\tcrates/tn-ui/src/element.rs\n1\t0\tlib.rs\n";
        let v = parse_numstat(s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], FileChange { path: "crates/tn-ui/src/element.rs".into(), add: 3, del: 1 });
        assert_eq!(v[0].name(), "element.rs");
        assert_eq!(v[1], FileChange { path: "lib.rs".into(), add: 1, del: 0 });
        assert_eq!(v[1].name(), "lib.rs");
    }

    #[test]
    fn numstat_treats_binary_dashes_as_zero_and_skips_blank() {
        let v = parse_numstat("-\t-\tassets/logo.png\n\n");
        assert_eq!(v, vec![FileChange { path: "assets/logo.png".into(), add: 0, del: 0 }]);
    }

    #[test]
    fn numstat_empty_is_empty() {
        assert!(parse_numstat("").is_empty());
    }

    #[test]
    fn name_handles_windows_separators() {
        let f = FileChange { path: r"crates\tn-ui\src\mod.rs".into(), add: 0, del: 0 };
        assert_eq!(f.name(), "mod.rs");
    }
}
```

---

## 4. `crates/tn-ui/src/quick_look.rs` — Quick Look 异步化

> **提交**: `40cbec4`  
> **日期**: 2026-06-02

### 4.1 LoadingState 枚举

```rust
/// QuickLook data-fetch state machine — render-pure: zero I/O inside `render()`.
/// Mirrors the activity rail's `RailState` pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoadingState {
    /// File content / binary peek is being read off-thread.
    Loading,
    /// Data has arrived — render real content (or the binary placeholder).
    Ready,
}
```

### 4.2 新增字段 (QuickLook struct)

```rust
pub struct QuickLook {
    // ... existing fields ...
    loading_state: LoadingState,   // Loading → skeleton, Ready → real content
    generation: usize,             // stale-result guard for file I/O
    edit_on_ready: bool,           // deferred edit: open_for_edit while Loading
    diff_loading: bool,            // independent loading track for git diff
    diff_generation: usize,        // stale-result guard for diff computation
}
```

### 4.3 open() — 异步文件读取 + 代次防乱序

```rust
pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    // Reset state, bump generation, switch to Loading → skeleton renders
    self.generation = self.generation.wrapping_add(1);
    let gen = self.generation;
    self.loading_state = LoadingState::Loading;
    self.edit_on_ready = false;
    cx.notify();

    let exec = cx.background_executor().clone();
    cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
        let res = exec.spawn(async move {
            // ── Background: fs::metadata + File::open + read_to_string ──
            let meta = std::fs::metadata(&path).ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            // binary detection (PEEK_SIZE null-byte check)
            // size > MAX_FILE_SIZE || is_binary → (Vec::new(), false, true, size)
            // else: read_to_string, lines().take(MAX_LINES)
            (lines, truncated, is_binary, size)
        }).await;

        let _ = this.update(cx, |v, cx| {
            if v.generation != gen { return; }  // stale → drop
            v.file_lines = Rc::new(res.0);
            v.file_truncated = res.1;
            v.file_binary = res.2;
            v.file_size = res.3;
            v.loading_state = LoadingState::Ready;
            if v.edit_on_ready { v.enter_edit(); v.edit_on_ready = false; }
            if v.tab == Tab::Diff { v.ensure_diff(cx); }
            cx.notify();
        });
    }).detach();
}
```

### 4.4 ensure_diff() — 异步 git diff + 独立代次

```rust
fn ensure_diff(&mut self, cx: &mut Context<Self>) {
    if !self.diff_dirty || self.diff_loading { return; }
    let Some(path) = self.path.clone() else { return };

    self.diff_generation = self.diff_generation.wrapping_add(1);
    let gen = self.diff_generation;
    self.diff_loading = true;
    cx.notify();

    let exec = cx.background_executor().clone();
    cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
        let diff_lines = exec.spawn(async move {
            // git diff --no-color HEAD -- <rel-path> (bounded 1.5s)
            let root = /* ... */;
            let text = crate::gitutil::capture_bounded(&root, &["diff", "--no-color", "--", &rel], Duration::from_millis(1500));
            parse_diff(text.as_deref().unwrap_or(""))
        }).await;

        let _ = this.update(cx, |v, cx| {
            if v.diff_generation == gen {
                v.diff = Rc::new(diff_lines);
                v.diff_dirty = false;
                v.diff_loading = false;
                cx.notify();
            }
        });
    }).detach();
}
```

### 4.5 render() 骨架屏

```rust
// Skeleton helper: short "code lines" of varying width
let code_skeleton = |n: usize, ws: &[f32]| {
    div()
        .flex_1().min_h(px(0.)).flex().flex_col().gap(px(6.))
        .pt(px(8.)).px(px(14.))
        .children((0..n).map(|i| {
            let w_px = ws[i % ws.len()];
            div().flex().flex_row().items_center().h(px(ROW_H))
                .child(div().w(px(38.)).mr(px(28.)))  // gutter
                .child(div().w(px(w_px)).h(px(10.)).rounded(px(3.)).bg(rgba(INSET)))
        }))
};

let body = if self.loading_state == LoadingState::Loading {
    code_skeleton(16, &[220., 130., 310., 180., 260., 140., 330., 160.])
} else if self.tab == Tab::Diff && self.diff_loading {
    code_skeleton(8, &[160., 280., 120., 200., 150., 310., 170.])
} else if self.file_binary {
    // ... binary placeholder ...
} else {
    // ... uniform_list with real content ...
};
```

### 4.6 调用点变更 (workspace.rs)

所有 QuickLook 公共方法现在需要 `cx: &mut Context<QuickLook>`:

```rust
// OpenFile event:                          v.open(path, cx)
// QuickLookEvent::Nav:                    v.open(path, cx)
// OpenInQuickLook (agent rail card):      v.open_diff(path, cx)
// App menu "设置":                     v.open_for_edit(p, cx)
```

### 4.7 移除的代码

- `fn compute_diff(&self, path: &PathBuf) -> Vec<DiffLine>` — 已内联到 `ensure_diff()` 的异步闭包中

### 4.8 架构总结

```
        open() / open_diff() / open_for_edit()
                     │
                     ▼
              generation++
              loading_state = Loading
              cx.notify()  ← 骨架屏立即渲染
                     │
                     ▼
         cx.background_executor().spawn()
              │                  │
         fs::metadata    fs::read_to_string
         File::open      (2MB cap)
         null-byte peek
              │                  │
              └──────┬───────────┘
                     ▼
              this.update(cx, ...)
              if gen == current:
                  file_lines = Rc::new(lines)
                  loading_state = Ready
                  cx.notify()  ← 真实内容渲染
              else:
                  return  ← 静默丢弃过期数据
```
