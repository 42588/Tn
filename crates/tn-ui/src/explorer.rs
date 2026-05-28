//! File explorer sidebar (M4 chrome) — the mockup's left column.
//!
//! A read-only directory tree rooted at the app's cwd: folders expand/collapse,
//! files select. It is a Calm Glass panel like the terminal panes, but it is
//! *chrome* (a fixed left column the workspace toggles), not a node in the
//! split tree. The tree is cached and only rebuilt on expand/collapse/refresh,
//! so an idle explorer does no filesystem work per frame.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, Context, FocusHandle,
    KeyDownEvent, MouseButton, SharedString, Window,
};
use tn_config::Loaded;

use crate::style::{col, cola, glass_pane, icon, pane_fill, specular_top, INSET, R_PANEL};

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

/// Directories that are noise in a source tree — never listed.
const IGNORED: &[&str] = &[".git", "target", "node_modules", ".idea", ".vs"];
/// Cap the visible tree so a huge repo can't blow up a render pass.
const MAX_ROWS: usize = 400;

/// Emitted when a file row is clicked, so the workspace can open it in the viewer.
pub struct OpenFile(pub PathBuf);

/// One rendered tree row (a directory or a file at some depth).
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
    /// the root (`crates/tn-ui/src/x.rs` → 'M'). Refreshed on rebuild.
    git_status: HashMap<String, char>,
    focus_handle: FocusHandle,
}

impl ExplorerView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut me = Self {
            config,
            root,
            expanded: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            git_status: HashMap::new(),
            focus_handle: cx.focus_handle(),
        };
        me.rebuild();
        me
    }

    /// The tree's focus handle — the workspace returns focus here after Quick Look
    /// closes (you opened the file from the list, so Esc goes back to the list).
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Re-root the tree at `root` (app menu「打开文件夹」): reset expansion +
    /// selection, then rebuild from the new folder (refreshing git status for it).
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.root = root;
        self.expanded.clear();
        self.selected = None;
        self.rebuild();
        cx.notify();
    }

    /// Run `git status --porcelain` in the root and map each changed path
    /// (forward-slash, relative) to a one-letter tag: M(odified) / U(ntracked)
    /// / A(dded) / D(eleted) / R(enamed).
    fn compute_git_status(root: &Path) -> HashMap<String, char> {
        let mut map = HashMap::new();
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(root).arg("status").arg("--porcelain");
        // No console flash when spawned from the GUI process (see tn-pty::wsl).
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                // git missing / not spawnable: log once, then stay silent
                // (待优化清单 §8.2). Not-a-repo just yields empty output.
                static WARN: std::sync::Once = std::sync::Once::new();
                WARN.call_once(|| tracing::warn!(error = %e, "git unavailable; explorer status marks off"));
                return map;
            }
        };
        map.extend(parse_porcelain(&String::from_utf8_lossy(&out.stdout)));
        map
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

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        for (path, name, is_dir) in Self::read_dir_sorted(dir) {
            if out.len() >= MAX_ROWS {
                return;
            }
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(Row { path: path.clone(), name, depth, is_dir, expanded });
            if expanded {
                self.walk(&path, depth + 1, out);
            }
        }
    }

    /// Rebuild the cached row list from the filesystem + current expansion,
    /// refreshing the git-status tags.
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.walk(&root, 0, &mut rows);
        self.rows = rows;
        self.git_status = Self::compute_git_status(&self.root);
    }

    fn on_row_click(&mut self, path: PathBuf, is_dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        if is_dir {
            // Keep the tree focused so ↑↓ / Space keep working after expanding.
            self.focus_handle.focus(window);
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.rebuild();
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
                cx.notify();
                return Some(p);
            }
        }
    }

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> gpui::Div {
        let ui = &self.config.theme.ui;
        let t = &self.config.theme;
        let is_sel = self.selected.as_deref() == Some(row.path.as_path());
        let indent = 10.0 + row.depth as f32 * 16.0; // mockup .tnode padding 10 + margin-left 16/级
        let path = row.path.clone();
        let is_dir = row.is_dir;

        let mut r = div()
            .flex()
            .flex_row()
            .items_center()
            .relative() // anchor the indent guide line
            .gap(px(7.)) // §16 .tnode gap 7
            .h(px(26.)) // §16 .tnode height 26
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
            .when(!is_sel, |d| d.hover(|s| s.bg(rgba(INSET))))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, w, cx| this.on_row_click(path.clone(), is_dir, w, cx)),
            );

        // Indent guide (mockup .tnode[class*="ind"]::before): a 1px vertical line
        // 8px left of the content, full row height (flush rows → continuous tree
        // guides). 白 .05 overlay。
        if row.depth > 0 {
            r = r.child(
                div()
                    .absolute()
                    .left(px(indent - 8.0))
                    .top(px(0.))
                    .bottom(px(0.))
                    .w(px(1.))
                    // mockup .tnode::before 是白 .05,但真机无 backdrop-blur 衬托会看不见 →
                    // 提到 .12 才读得出引导线(白叠加,round(.12×255)=31)
                    .bg(rgba(0xffffff1f)),
            );
        }

        // chevron (directories only; files get a matching-width spacer)
        if row.is_dir {
            let chev = if row.expanded { "chev-d" } else { "chev-r" };
            r = r.child(icon(chev, 13., ui.muted));
        } else {
            r = r.child(div().w(px(13.)).flex_none());
        }
        // type icon: folder (accent) / file (muted, or claude when selected)
        let (glyph, glyph_color) = if row.is_dir {
            ("folder", ui.accent)
        } else if is_sel {
            ("file", t.agents.claude)
        } else {
            ("file", ui.muted)
        };
        let mut r = r.child(icon(glyph, 14., glyph_color)).child(
            div()
                // mockup: .tnode 文件 = fg-dim;.tnode.dir = fg(亮)、weight 540;active → fg。
                // (#A6AFD4 = fg-dim,无主题 token → 字面量)
                .text_color(if is_sel || is_dir { col(ui.foreground) } else { gpui::rgb(0xA6AFD4) })
                .when(is_dir, |d| d.font_weight(gpui::FontWeight(540.)))
                .child(SharedString::from(row.name.clone())),
        );
        // git-status tag (files only), right-aligned.
        if !row.is_dir {
            let key = row
                .path
                .strip_prefix(&self.root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"));
            if let Some(&tag) = key.as_ref().and_then(|k| self.git_status.get(k)) {
                let c = match tag {
                    'U' | 'A' => t.ansi.green,
                    'D' => t.ansi.red,
                    _ => t.ansi.yellow,
                };
                r = r.child(div().flex_1()).child(git_tag(tag, c));
            }
        }
        r
    }
}

impl gpui::EventEmitter<OpenFile> for ExplorerView {}

impl Render for ExplorerView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        // Precompute the row Divs (collecting ends the `self.rows` borrow before
        // the `.children()` closure). `render_row` takes `&Row`, so we pass the
        // cached row by reference — no per-row clone (待优化清单 §2.5).
        let rows: Vec<gpui::Div> =
            (0..self.rows.len()).map(|i| self.render_row(&self.rows[i], cx)).collect();

        // Inner content, rounded 1px tighter so the gradient-border ring shows
        // (see style::glass_pane); g1 glass + specular + header + tree.
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
            .child(specular_top())
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .p(px(6.)) // mockup .tree padding 6
                    .flex()
                    .flex_col()
                    .children(rows),
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
    fn porcelain_skips_blank_and_short_lines() {
        // Empty output (clean repo / not-a-repo) and malformed short lines yield
        // nothing instead of panicking on the `[..2]` / `[3..]` slices.
        assert!(parse_porcelain("").is_empty());
        assert!(parse_porcelain("\n\nx\n M\n").is_empty(), "lines < 4 chars skipped");
    }
}
