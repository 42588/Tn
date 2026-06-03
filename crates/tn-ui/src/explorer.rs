//! File explorer sidebar (M4 chrome) — the mockup's left column.
//!
//! A read-only directory tree rooted at the app's cwd: folders expand/collapse,
//! files select. It is a Calm Glass panel like the terminal panes, but it is
//! *chrome* (a fixed left column the workspace toggles), not a node in the
//! split tree. The tree is cached and only rebuilt on expand/collapse/refresh,
//! so an idle explorer does no filesystem work per frame.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, uniform_list, AsyncApp, Context,
    FocusHandle, KeyDownEvent, MouseButton, ScrollStrategy, SharedString, UniformListScrollHandle,
    WeakEntity, Window,
};
use tn_config::Loaded;

use crate::gitutil;
use crate::style::{col, cola, glass_pane, icon, pane_fill, INSET, R_PANEL};

/// A small git-status tag chip (e.g. `M` yellow, `U` green).
fn git_tag(letter: char, c: tn_config::Color) -> gpui::Div {
    div()
        .flex_none()
        .w(px(15.))
        .h(px(15.))
        .rounded(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(9.))
        .font_weight(gpui::FontWeight(800.)) // §16 .tag weight 800
        .text_color(col(c))
        .bg(cola(c, 0.15)) // mockup .tag bg = 色 @ .15
        .child(SharedString::from(letter.to_string()))
}

/// Parse `git status --porcelain` output into a map of forward-slash, relative
/// path → one-letter tag (`U`ntracked / `A`dded / `D`eleted / `R`enamed /
/// `M`odified). Pure (no IO) so it's unit-testable (待优化清单 §7.4); the priority
/// order matches how a combined index+worktree status (`MM`, `AM`, …) collapses
/// to a single chip.
fn parse_porcelain(stdout: &str) -> HashMap<String, char> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let xy = &line[..2];
        let mut path = line[3..].to_string();
        if let Some(i) = path.find(" -> ") {
            path = path[i + 4..].to_string(); // rename: keep the new name
        }
        let key = path.trim().trim_matches('"').replace('\\', "/");
        let tag = if xy.contains('?') {
            'U'
        } else if xy.contains('A') {
            'A'
        } else if xy.contains('D') {
            'D'
        } else if xy.contains('R') {
            'R'
        } else if xy.contains('M') {
            'M'
        } else {
            continue;
        };
        map.insert(key, tag);
    }
    map
}

/// Carry a selection across a `cd`-driven re-root: keep it only while it still
/// points inside the new root, else drop it (a highlight on a now-invisible path
/// is meaningless). Pure (component-wise `starts_with`, separator-agnostic on
/// Windows) so the [`ExplorerView::follow_root`] rule is unit-testable headless.
fn selection_under_root(selected: &Option<PathBuf>, root: &Path) -> Option<PathBuf> {
    selected.clone().filter(|p| p.starts_with(root))
}

/// Directories that are noise in a source tree — never listed.
const IGNORED: &[&str] = &[".git", "target", "node_modules", ".idea", ".vs"];
/// Cap the visible tree so a huge repo can't blow up a render pass.
const MAX_ROWS: usize = 400;
/// Fixed row height so `uniform_list` can measure once and assume the rest.
const TREE_ROW_H: f32 = 26.0; // §16 .tnode height 26

/// Emitted when a file row is clicked, so the workspace can open it in the viewer.
pub struct OpenFile(pub PathBuf);

/// One rendered tree row (a directory or a file at some depth).
#[derive(Clone)]
struct Row {
    path: PathBuf,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
}

pub struct ExplorerView {
    config: Arc<Loaded>,
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    rows: Vec<Row>,
    /// `git status --porcelain` tags, keyed by forward-slash path relative to
    /// the root (`crates/tn-ui/src/x.rs` → 'M'). Refreshed asynchronously.
    git_status: HashMap<String, char>,
    /// Set true when the tree is rebuilt; the next render will spawn a background
    /// task to refresh git status without blocking the UI thread.
    git_stale: bool,
    /// Keeps the background git task alive until completion.
    _git_task: Option<gpui::Task<()>>,
    scroll_handle: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _change_watcher: Option<notify::RecommendedWatcher>,
}

/// One tree row (`.tnode`): indent guide + chevron + icon + name + optional git
/// tag. Pure rendering — free fn so the `'static` [`uniform_list`] closure can
/// call it without borrowing the view.
fn tree_row(
    ui: &tn_config::UiColors,
    t: &tn_config::Theme,
    row: &Row,
    indent: f32,
    is_sel: bool,
    maybe_tag: Option<(char, tn_config::Color)>,
) -> gpui::Div {
    let mut r = div()
        .flex()
        .flex_row()
        .items_center()
        .relative()
        .gap(px(7.)) // §16 .tnode gap 7
        .h(px(TREE_ROW_H))
        .pr_2()
        .pl(px(indent))
        .rounded(px(8.)) // §16 .tnode radius 8
        .text_size(px(12.5))
        // mockup .tnode.active bg = 白渐变 .075→.025
        .when(is_sel, |d| {
            d.bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0xffffff13), 0.), // .075 → 19 = 0x13
                linear_color_stop(rgba(0xffffff06), 1.), // .025 → 6 = 0x06
            ))
        })
        .when(!is_sel, |d| d.hover(|s| s.bg(rgba(INSET))));

    // chevron (directories) or spacer (files)
    if row.is_dir {
        let chev = if row.expanded { "chev-d" } else { "chev-r" };
        r = r.child(icon(chev, 13., ui.muted));
    } else {
        r = r.child(div().w(px(13.)).flex_none());
    }

    // type icon
    let (glyph, glyph_color) = if row.is_dir {
        ("folder", ui.accent)
    } else if is_sel {
        ("file", t.agents.claude)
    } else {
        ("file", ui.muted)
    };
    r = r.child(icon(glyph, 14., glyph_color)).child(
        div()
            .flex_1()
            .overflow_hidden()
            .text_ellipsis()
            .text_color(if is_sel || row.is_dir { col(ui.foreground) } else { col(ui.muted) })
            .when(row.is_dir, |d| d.font_weight(gpui::FontWeight(540.)))
            .child(SharedString::from(row.name.clone())),
    );

    // git-status tag (files + directories)
    if let Some((tag, c)) = maybe_tag {
        r = r.child(git_tag(tag, c));
    }
    r
}

impl ExplorerView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let root = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let mut me = Self {
            config,
            root: root.clone(),
            expanded: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            git_status: HashMap::new(),
            git_stale: true,
            _git_task: None,
            scroll_handle: UniformListScrollHandle::default(),
            focus_handle: cx.focus_handle(),
            _change_watcher: None,
        };
        me._change_watcher = Self::spawn_change_watcher(&root, cx);
        me.rebuild(cx);
        me
    }

    fn spawn_change_watcher(root: &std::path::Path, cx: &mut Context<Self>) -> Option<notify::RecommendedWatcher> {
        use notify::Watcher;
        use futures::StreamExt;
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if ev.paths.iter().any(|p| {
                    p.components().any(|c| {
                        matches!(
                            c.as_os_str().to_str(),
                            Some(".git" | "target" | "node_modules" | ".cargo" | "dist" | ".next")
                        )
                    })
                }) {
                    return;
                }
                let _ = tx.unbounded_send(());
            }
        }).ok()?;
        if watcher.watch(root, notify::RecursiveMode::Recursive).is_err() {
            return None;
        }
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            while rx.next().await.is_some() {
                exec.timer(std::time::Duration::from_millis(500)).await;
                while rx.try_recv().is_ok() {}
                let _ = this.update(cx, |this, cx| this.rebuild(cx));
            }
        }).detach();
        Some(watcher)
    }

    /// The tree's focus handle — the workspace returns focus here after Quick Look
    /// closes (you opened the file from the list, so Esc goes back to the list).
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Re-root the tree at `root` (app menu「打开文件夹」): reset expansion +
    /// selection, then rebuild from the new folder (refreshing git status for it).
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.root = root.clone();
        self.expanded.clear();
        self.selected = None;
        self._change_watcher = Self::spawn_change_watcher(&root, cx);
        self.rebuild(cx);
    }

    /// Re-root the tree to follow a shell `cd` (render-driven, not the explicit
    /// 「打开文件夹」). Unlike [`set_root`](Self::set_root), this **keeps the
    /// expansion state** for the direct ancestry: `expanded` holds absolute paths,
    /// so entries under the new root stay open and the tree does not collapse when
    /// you `cd` into a subdirectory. When backing out, direct ancestors remain
    /// expanded, though distant siblings are pruned to prevent memory leaks (待优化清单 §7).
    /// The selection is kept only while it still points inside the new root.
    pub fn follow_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root == root {
            return;
        }
        self.root = root.clone();
        self.expanded.retain(|p| p.starts_with(&root) || root.starts_with(p));
        self._change_watcher = Self::spawn_change_watcher(&root, cx);
        self.selected = selection_under_root(&self.selected, &root);
        self.rebuild(cx);
    }

    /// The current tree root — the single source of truth for the working
    /// directory. Pane launch cwd and activity-rail git directory both read
    /// this so they stay in sync with the explorer.
    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    /// Run `git status --porcelain` in the root and map each changed path
    /// (forward-slash, relative) to a one-letter tag: M(odified) / U(ntracked)
    /// / A(dded) / D(eleted) / R(enamed).
    /// Uses the bounded git helper (off-thread + timeout) so a slow / locked git
    /// never freezes the caller; propagates tags upward so parent directories also
    /// show an aggregated git indicator.
    fn compute_git_status(root: &Path) -> HashMap<String, char> {
        let mut map = HashMap::new();
        let out = match gitutil::capture_bounded(root, &["status", "--porcelain"], Duration::from_millis(1500)) {
            Some(s) => s,
            None => return map,
        };
        map.extend(parse_porcelain(&out));
        // Propagate tags upward: for each entry, walk up to every ancestor and
        // keep the highest-priority tag (M > A > D > U > R). One pass, O(files × depth).
        for (path, &tag) in map.clone().iter() {
            let rank = Self::tag_rank(tag);
            let mut parent = path.clone();
            while let Some(pos) = parent.rfind('/') {
                parent.truncate(pos);
                map.entry(parent.clone())
                    .and_modify(|t| {
                        if rank > Self::tag_rank(*t) {
                            *t = Self::rank_to_tag(rank);
                        }
                    })
                    .or_insert(Self::rank_to_tag(rank));
            }
        }
        map
    }

    fn tag_rank(t: char) -> u32 {
        match t {
            'M' => 5,
            'A' => 4,
            'D' => 3,
            'U' => 2,
            'R' => 1,
            _ => 0,
        }
    }

    fn rank_to_tag(r: u32) -> char {
        match r {
            5 => 'M',
            4 => 'A',
            3 => 'D',
            2 => 'U',
            1 => 'R',
            _ => 'M',
        }
    }

    /// Read `dir`'s entries, drop hidden/ignored, and sort directories first
    /// then files, each alphabetically (case-insensitive).
    fn read_dir_sorted(dir: &Path) -> Vec<(PathBuf, String, bool)> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<(PathBuf, String, bool)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || IGNORED.contains(&name.as_str()) {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some((e.path(), name, is_dir))
            })
            .collect();
        out.sort_by(|a, b| {
            b.2.cmp(&a.2) // dirs (true) before files (false)
                .then_with(|| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()))
        });
        out
    }

    fn walk(dir: &Path, depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Row>) {
        for (path, name, is_dir) in Self::read_dir_sorted(dir) {
            if out.len() >= MAX_ROWS {
                return;
            }
            let is_expanded = is_dir && expanded.contains(&path);
            out.push(Row { path: path.clone(), name, depth, is_dir, expanded: is_expanded });
            if is_expanded {
                Self::walk(&path, depth + 1, expanded, out);
            }
        }
    }

    /// Rebuild the cached row list from the filesystem + current expansion.
    /// Runs off-thread to prevent blocking the UI on huge projects or slow disks.
    /// Git status is refreshed asynchronously on the next render cycle.
    pub fn rebuild(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let expanded = self.expanded.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let rows = cx.background_executor().spawn(async move {
                let mut out = Vec::new();
                Self::walk(&root, 0, &expanded, &mut out);
                out
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.rows = rows;
                this.git_stale = true;
                cx.notify();
            });
        }).detach();
    }

    /// Kick off an async git-status refresh. Safe to call from any context;
    /// only one task runs at a time (the flag is cleared immediately).
    fn start_git_refresh(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let status = Self::compute_git_status(&root);
            this.update(cx, |this, cx| {
                this.git_status = status;
                cx.notify();
            })
            .ok();
        });
        self._git_task = Some(task);
    }

    /// Signal that the file tree may be stale — trigger a full rebuild + git
    /// refresh on the next render cycle.
    pub fn mark_stale(&mut self) {
        self.git_stale = true;
    }

    fn on_row_click(&mut self, path: PathBuf, is_dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        if is_dir {
            // Keep the tree focused so ↑↓ / Space keep working after expanding.
            self.focus_handle.focus(window);
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.rebuild(cx);
        } else {
            // Opening a FILE: do NOT focus the tree — the Quick Look overlay grabs
            // focus (its `needs_focus`) so its own keys (↑↓ 换文件 / Esc 关 / Enter
            // 编辑) work. Focusing the tree here would steal focus from the opening
            // overlay → its `Esc` never fires (踩过的坑).
            self.selected = Some(path.clone());
            cx.emit(OpenFile(path));
        }
        cx.notify();
    }

    /// Keyboard nav while the tree is focused (preview-state entry point): ↑↓ move
    /// the selection, Space/Enter open the file in Quick Look (or toggle a dir).
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return; // leave Ctrl+Shift+B etc. to the workspace
        }
        match ev.keystroke.key.as_str() {
            "up" => {
                self.move_selection(-1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" => {
                self.move_selection(1);
                cx.stop_propagation();
                cx.notify();
            }
            "space" | "enter" => {
                if let Some(path) = self.selected.clone() {
                    cx.stop_propagation();
                    let is_dir = self.rows.iter().any(|r| r.path == path && r.is_dir);
                    if is_dir {
                        self.on_row_click(path, true, window, cx); // toggle expand
                    } else {
                        cx.emit(OpenFile(path)); // → workspace opens Quick Look
                    }
                }
            }
            _ => {}
        }
    }

    /// Move the highlight by `delta` rows (clamped). Tree-local nav; does not open
    /// anything (opening is Space/Enter).
    fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self
            .selected
            .as_ref()
            .and_then(|p| self.rows.iter().position(|r| &r.path == p));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, self.rows.len() as i32 - 1) as usize,
            None => if delta >= 0 { 0 } else { self.rows.len() - 1 },
        };
        self.selected = Some(self.rows[next].path.clone());
        self.scroll_to_selected();
    }

    /// Select the next/prev **file** row (skipping directories) and return its path
    /// — Quick Look's `↑↓ 换文件` live-follow (driven from the focused overlay).
    /// `None` when there is no further file in that direction (selection unchanged).
    pub fn select_adjacent_file(&mut self, delta: i32, cx: &mut Context<Self>) -> Option<PathBuf> {
        if self.rows.is_empty() {
            return None;
        }
        let start = self
            .selected
            .as_ref()
            .and_then(|p| self.rows.iter().position(|r| &r.path == p))
            .map(|i| i as i32)
            .unwrap_or(-1);
        let step = if delta >= 0 { 1 } else { -1 };
        let mut i = start;
        loop {
            i += step;
            if i < 0 || i as usize >= self.rows.len() {
                return None;
            }
            if !self.rows[i as usize].is_dir {
                let p = self.rows[i as usize].path.clone();
                self.selected = Some(p.clone());
                self.scroll_to_selected();
                cx.notify();
                return Some(p);
            }
        }
    }

    /// After changing the selection, scroll the virtualised list so the newly-
    /// selected row is visible.
    fn scroll_to_selected(&self) {
        if let Some(ref p) = self.selected {
            if let Some(idx) = self.rows.iter().position(|r| &r.path == p) {
                self.scroll_handle.scroll_to_item(idx, ScrollStrategy::Top);
            }
        }
    }
}

impl gpui::EventEmitter<OpenFile> for ExplorerView {}

impl Render for ExplorerView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        // If the file tree was rebuilt (or first shown), kick off async git refresh.
        if self.git_stale {
            self.git_stale = false;
            self.start_git_refresh(cx);
        }

        let ui = &self.config.theme.ui;
        let root_name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".into());

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.)) // §16 .phead gap 9
            .h(px(36.)) // §16 .phead height 36
            .px(px(13.)) // §16 .phead padding 0 13
            .flex_none()
            .text_size(px(11.5))
            .font_weight(gpui::FontWeight(560.)) // §16 .phead weight 560
            .text_color(col(ui.muted))
            .child(icon("explorer", 14., ui.accent))
            .child(div().child("Explorer · "))
            .child(
                div()
                    .text_color(col(ui.accent))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(root_name)),
            );

        // Prepare data for the 'static uniform_list closure (Rc/Arc clones are cheap).
        let tree_rows: std::rc::Rc<Vec<Row>> = std::rc::Rc::new(self.rows.clone());
        let tree_config = self.config.clone(); // Arc
        let tree_root: std::rc::Rc<PathBuf> = std::rc::Rc::new(self.root.clone());
        let tree_git: std::rc::Rc<HashMap<String, char>> = std::rc::Rc::new(self.git_status.clone());
        let tree_sel: std::rc::Rc<Option<PathBuf>> = std::rc::Rc::new(self.selected.clone());
        let tree_entity = cx.entity().downgrade();

        // Inner content, rounded 1px tighter so the gradient-border ring shows
        // (see style::glass_pane); g1 glass + specular + header + tree.
        let is_focused = self.focus_handle.is_focused(window);
        let inner = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .size_full()
            .relative() // anchor the absolute specular layer
            .flex()
            .flex_col()
            .min_h(px(0.))
            .overflow_hidden()
            .rounded(px(R_PANEL - 1.))
            // mockup .sidebar 是 .pane:g1 玻璃(baked opaque,防 glass_pane 渐变边透底)
            .bg(pane_fill(ui.chrome_bg))
            .child(crate::style::specular_wash(is_focused, ui.accent))
            .child(header)
            .child(
                uniform_list("explorer-tree", self.rows.len(), move |range, _window, _cx| {
                    range
                        .map(|i| {
                            let row = &tree_rows[i];
                            let indent = 10.0 + row.depth as f32 * 16.0;
                            let is_sel =
                                tree_sel.as_ref().as_ref() == Some(&row.path);
                            let key = row
                                .path
                                .strip_prefix(tree_root.as_ref())
                                .ok()
                                .map(|p| p.to_string_lossy().replace('\\', "/"));
                            let git_tag =
                                key.as_ref()
                                    .and_then(|k| tree_git.get(k))
                                    .map(|&tag| {
                                        let c = match tag {
                                            'U' | 'A' => tree_config.theme.ansi.green,
                                            'D' => tree_config.theme.ansi.red,
                                            _ => tree_config.theme.ansi.yellow,
                                        };
                                        (tag, c)
                                    });
                            let path = row.path.clone();
                            let is_dir = row.is_dir;
                            let entity = tree_entity.clone();
                            tree_row(
                                &tree_config.theme.ui,
                                &tree_config.theme,
                                row,
                                indent,
                                is_sel,
                                git_tag,
                            )
                            .on_mouse_down(MouseButton::Left, move |_ev, _w, app| {
                                app.stop_propagation();
                                let path = path.clone();
                                let _ = entity.update(app, move |this, cx| {
                                    if is_dir {
                                        if !this.expanded.remove(&path) {
                                            this.expanded.insert(path.clone());
                                        }
                                        this.rebuild(cx);
                                    } else {
                                        let p = path.clone();
                                        this.selected = Some(path);
                                        cx.emit(OpenFile(p));
                                    }
                                    cx.notify();
                                });
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .flex_1()
                .min_h(px(0.))
                .p(px(6.)) // mockup .tree padding 6
                .track_scroll(self.scroll_handle.clone()),
            );
        // mockup .pane::before 竖向渐变描边 + 浮起投影(与终端面板一致;explorer 恒非焦点)
        glass_pane(inner, false, ui.accent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_tags_each_status_kind() {
        // A representative `git status --porcelain` block exercising every tag,
        // the worktree/index combinations, renames, quoted spaces and backslashes.
        // `concat!` (not `\`-line-continuation, which strips the leading space
        // that " M" / " D" worktree statuses depend on).
        let out = concat!(
            " M crates/tn-ui/src/explorer.rs\n",
            "?? new_file.txt\n",
            "A  staged_add.rs\n",
            " D removed.rs\n",
            "R  old\\name.rs -> src/new_name.rs\n",
            "MM both_sides.rs\n",
            "AM added_then_modified.rs\n",
            "?? \"with space.txt\"\n",
        );
        let m = parse_porcelain(out);
        assert_eq!(m.get("crates/tn-ui/src/explorer.rs"), Some(&'M'));
        assert_eq!(m.get("new_file.txt"), Some(&'U'), "?? -> untracked");
        assert_eq!(m.get("staged_add.rs"), Some(&'A'));
        assert_eq!(m.get("removed.rs"), Some(&'D'));
        // Rename keeps the NEW name, backslash normalized to forward slash.
        assert_eq!(m.get("src/new_name.rs"), Some(&'R'));
        assert_eq!(m.get("both_sides.rs"), Some(&'M'), "MM collapses to M");
        assert_eq!(m.get("added_then_modified.rs"), Some(&'A'), "A wins over M");
        // A quoted path (git quotes names with spaces) is unquoted.
        assert_eq!(m.get("with space.txt"), Some(&'U'));
        assert_eq!(m.len(), 8);
    }

    #[test]
    fn selection_kept_only_under_new_root() {
        // `cd` into a subdir: a selection inside the new root survives (so the
        // highlight follows you down); one outside is dropped (it'd point at a
        // now-invisible path). None stays None.
        let root = PathBuf::from("D:/proj/crates");
        let inside = Some(PathBuf::from("D:/proj/crates/tn-ui/src.rs"));
        assert_eq!(selection_under_root(&inside, &root), inside);
        let outside = Some(PathBuf::from("D:/proj/docs/x.md"));
        assert_eq!(selection_under_root(&outside, &root), None);
        assert_eq!(selection_under_root(&None, &root), None);
        // The root itself counts as under-root (component-wise starts_with).
        let at_root = Some(PathBuf::from("D:/proj/crates"));
        assert_eq!(selection_under_root(&at_root, &root), at_root);
    }

    #[test]
    fn porcelain_skips_blank_and_short_lines() {
        // Empty output (clean repo / not-a-repo) and malformed short lines yield
        // nothing instead of panicking on the `[..2]` / `[3..]` slices.
        assert!(parse_porcelain("").is_empty());
        assert!(parse_porcelain("\n\nx\n M\n").is_empty(), "lines < 4 chars skipped");
    }
}
