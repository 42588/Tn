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

use gpui::{div, prelude::*, px, rgba, Context, FocusHandle, MouseButton, SharedString};
use tn_config::Loaded;

use crate::style::{col, cola, icon, HOVER, INSET, RIM};

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
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(col(c))
        .bg(cola(c, 0.16))
        .child(SharedString::from(letter.to_string()))
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

    /// Run `git status --porcelain` in the root and map each changed path
    /// (forward-slash, relative) to a one-letter tag: M(odified) / U(ntracked)
    /// / A(dded) / D(eleted) / R(enamed).
    fn compute_git_status(root: &Path) -> HashMap<String, char> {
        let mut map = HashMap::new();
        let out = match Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("status")
            .arg("--porcelain")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                // git missing / not spawnable: log once, then stay silent
                // (待优化清单 §8.2). Not-a-repo just yields empty output.
                static WARN: std::sync::Once = std::sync::Once::new();
                WARN.call_once(|| tracing::warn!(error = %e, "git unavailable; explorer status marks off"));
                return map;
            }
        };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
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

    fn on_row_click(&mut self, path: PathBuf, is_dir: bool, cx: &mut Context<Self>) {
        if is_dir {
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.rebuild();
        } else {
            self.selected = Some(path.clone());
            cx.emit(OpenFile(path));
        }
        cx.notify();
    }

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> gpui::Div {
        let ui = &self.config.theme.ui;
        let t = &self.config.theme;
        let is_sel = self.selected.as_deref() == Some(row.path.as_path());
        let indent = 10.0 + row.depth as f32 * 14.0;
        let path = row.path.clone();
        let is_dir = row.is_dir;

        let mut r = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .h(px(25.))
            .pr_2()
            .pl(px(indent))
            .rounded(px(7.))
            .text_size(px(12.5))
            .when(is_sel, |d| d.bg(rgba(HOVER)))
            .when(!is_sel, |d| d.hover(|s| s.bg(rgba(INSET))))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| this.on_row_click(path.clone(), is_dir, cx)),
            );

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
                .text_color(col(if row.is_dir || is_sel { ui.foreground } else { ui.muted }))
                .when(row.is_dir, |d| d.font_weight(gpui::FontWeight::MEDIUM))
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
            .gap_2()
            .h(px(30.))
            .px_3()
            .flex_none()
            .text_size(px(11.5))
            .text_color(col(ui.muted))
            .child(icon("explorer", 14., ui.accent))
            .child(div().child("Explorer · "))
            .child(
                div()
                    .text_color(col(ui.accent))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(root_name)),
            );

        // Precompute rows to avoid borrowing `self` inside the children closure.
        let rows: Vec<gpui::Div> = (0..self.rows.len())
            .map(|i| {
                let row = &self.rows[i];
                self.render_row(
                    &Row {
                        path: row.path.clone(),
                        name: row.name.clone(),
                        depth: row.depth,
                        is_dir: row.is_dir,
                        expanded: row.expanded,
                    },
                    cx,
                )
            })
            .collect();

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .min_h(px(0.))
            .overflow_hidden()
            .rounded(px(14.))
            .border_1()
            .border_color(rgba(RIM))
            .bg(rgba(0x1f233566)) // frosted panel (surface_1 @ ~0.4)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .px_1()
                    .pb_1()
                    .flex()
                    .flex_col()
                    .children(rows),
            )
    }
}
