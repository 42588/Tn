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
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, uniform_list, AnyElement,
    AsyncApp, Context, FocusHandle, KeyDownEvent, MouseButton, ScrollStrategy, SharedString,
    UniformListScrollHandle, WeakEntity, Window,
};
use tn_config::Loaded;
use tn_pty::remote_fs::{RemoteFileService, RemoteId, RemotePath, SftpFileService};

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
/// to a single chip. Assumes the caller ran git with `core.quotePath=false`, so
/// non-ASCII paths arrive as raw UTF-8 (the `\`→`/` normalization below would
/// otherwise corrupt octal-escaped CJK paths into unmatchable keys).
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
fn selection_under_root(
    selected: &Option<ExplorerPath>,
    root: &ExplorerRoot,
) -> Option<ExplorerPath> {
    selected.clone().filter(|p| p.is_under_root(root))
}

/// Filter a restored pane snapshot to stay inside `root_path`: keep expanded
/// entries under the root (or its direct ancestors, so the path down to the root
/// re-opens) and a selection only while it still points inside. Pure so the
/// pane-switch restore logic is headless-testable without a gpui `Context`.
fn snapshot_under_root(
    snap: ExplorerSnapshot,
    root: &ExplorerRoot,
) -> (HashSet<ExplorerPath>, Option<ExplorerPath>) {
    let expanded = snap
        .expanded
        .into_iter()
        .filter(|p| p.is_under_root(root) || p.is_ancestor_of_root(root))
        .collect();
    let selected = selection_under_root(&snap.selected, root);
    (expanded, selected)
}

fn explorer_file_for_path(
    root: &ExplorerRoot,
    path: &ExplorerPath,
    size: Option<u64>,
) -> Option<ExplorerFile> {
    match path {
        ExplorerPath::Local(path) => Some(ExplorerFile::Local(path.clone())),
        ExplorerPath::Remote(id) => match &root.fs {
            ExplorerFs::Ssh { cfg } => Some(ExplorerFile::Remote {
                cfg: cfg.clone(),
                id: id.clone(),
                size,
            }),
            _ => None,
        },
    }
}

/// Directories that are noise in a source tree — never listed.
const IGNORED: &[&str] = &[".git", "target", "node_modules", ".idea", ".vs"];
/// Cap the visible tree so a huge repo can't blow up a render pass.
const MAX_ROWS: usize = 400;
/// Fixed row height so `uniform_list` can measure once and assume the rest.
const TREE_ROW_H: f32 = 26.0; // §16 .tnode height 26

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ExplorerPath {
    Local(PathBuf),
    Remote(RemoteId),
}

impl ExplorerPath {
    fn local_path(&self) -> Option<&Path> {
        match self {
            Self::Local(path) => Some(path.as_path()),
            Self::Remote(_) => None,
        }
    }

    fn is_under_root(&self, root: &ExplorerRoot) -> bool {
        match (self, &root.fs) {
            (Self::Local(path), _) => root.path().is_some_and(|r| path.starts_with(r)),
            (Self::Remote(id), ExplorerFs::Ssh { cfg }) => {
                id.user == cfg.user
                    && id.host == cfg.host
                    && id.port == cfg.port
                    && root
                        .remote_path
                        .as_ref()
                        .is_some_and(|r| remote_path_under(&id.path, r))
            }
            _ => false,
        }
    }

    fn is_ancestor_of_root(&self, root: &ExplorerRoot) -> bool {
        match (self, &root.fs) {
            (Self::Local(path), _) => root.path().is_some_and(|r| r.starts_with(path)),
            (Self::Remote(id), ExplorerFs::Ssh { cfg }) => {
                id.user == cfg.user
                    && id.host == cfg.host
                    && id.port == cfg.port
                    && root
                        .remote_path
                        .as_ref()
                        .is_some_and(|r| remote_path_under(r, &id.path))
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplorerFile {
    Local(PathBuf),
    Remote {
        cfg: tn_pty::SshConfig,
        id: RemoteId,
        size: Option<u64>,
    },
}

/// Emitted when a file row is clicked, so the workspace can open it in the viewer.
pub struct OpenFile(pub ExplorerFile);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExplorerFs {
    Host,
    Wsl { distro: String },
    Ssh { cfg: tn_pty::SshConfig },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerRoot {
    fs: ExplorerFs,
    path: Option<PathBuf>,
    remote_path: Option<RemotePath>,
    display_path: String,
}

impl ExplorerRoot {
    pub fn host(path: PathBuf) -> Self {
        let display_path = path.to_string_lossy().to_string();
        Self {
            fs: ExplorerFs::Host,
            path: Some(path),
            remote_path: None,
            display_path,
        }
    }

    pub fn wsl(distro: String, linux_path: String, unc_path: PathBuf) -> Self {
        let linux_path = if linux_path == "/" {
            "/".to_string()
        } else {
            linux_path.trim_end_matches('/').to_string()
        };
        let display_path = format!("{distro}:{linux_path}");
        Self {
            fs: ExplorerFs::Wsl { distro },
            path: Some(unc_path),
            remote_path: None,
            display_path,
        }
    }

    pub fn ssh(cfg: tn_pty::SshConfig, remote_path: impl Into<String>) -> Self {
        let remote_path = RemotePath::new(remote_path);
        let display_path = format!(
            "{}@{}:{}:{}",
            cfg.user,
            cfg.host,
            cfg.port,
            remote_path.as_str()
        );
        Self {
            fs: ExplorerFs::Ssh { cfg },
            path: None,
            remote_path: Some(remote_path),
            display_path,
        }
    }

    pub fn from_accessible_path(path: PathBuf) -> Self {
        if let Some((distro, linux_path)) = parse_wsl_unc(&path) {
            Self::wsl(distro, linux_path, path)
        } else {
            Self::host(path)
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn path_buf(&self) -> Option<PathBuf> {
        self.path.clone()
    }

    pub fn remote_path(&self) -> Option<&RemotePath> {
        self.remote_path.as_ref()
    }

    pub fn ssh_config(&self) -> Option<&tn_pty::SshConfig> {
        match &self.fs {
            ExplorerFs::Ssh { cfg } => Some(cfg),
            _ => None,
        }
    }

    pub fn is_browsable(&self) -> bool {
        self.path.is_some() || self.remote_path.is_some()
    }

    pub fn path_for_namespace(&self, ns: &crate::terminal_view::FileNamespace) -> Option<String> {
        match (&self.fs, ns) {
            // Local Host namespace expects Windows path / UNC path
            (_, crate::terminal_view::FileNamespace::Host) => {
                self.path.as_ref().map(|p| p.to_string_lossy().to_string())
            }
            // WSL namespace expects Linux path
            (
                ExplorerFs::Wsl {
                    distro: root_distro,
                },
                crate::terminal_view::FileNamespace::Wsl {
                    distro: pane_distro,
                },
            ) => {
                if pane_distro.as_ref().map_or(true, |d| d == root_distro) {
                    if let Some(path) = &self.path {
                        if let Some((_, linux_path)) = parse_wsl_unc(path) {
                            return Some(linux_path);
                        }
                    }
                }
                None
            }
            // Host Windows path to WSL Linux path: C:\Users -> /mnt/c/Users
            (ExplorerFs::Host, crate::terminal_view::FileNamespace::Wsl { .. }) => self
                .path
                .as_ref()
                .and_then(|p| windows_drive_to_wsl_mount(p)),
            (ExplorerFs::Ssh { .. }, crate::terminal_view::FileNamespace::Ssh) => {
                self.remote_path.as_ref().map(|p| p.as_str().to_string())
            }
            _ => None,
        }
    }

    fn supports_git_status(&self) -> bool {
        matches!(self.fs, ExplorerFs::Host)
    }

    /// For a WSL root: `(distro, linux_path)` parsed from its `\\wsl$\<distro>\…`
    /// UNC path. `None` for Host/SSH roots. Drives the in-app WSL directory picker.
    pub fn wsl_parts(&self) -> Option<(String, String)> {
        match &self.fs {
            ExplorerFs::Wsl { distro } => self
                .path
                .as_ref()
                .and_then(|p| parse_wsl_unc(p))
                .map(|(_, linux)| (distro.clone(), linux))
                .or_else(|| Some((distro.clone(), "/".to_string()))),
            _ => None,
        }
    }

    fn same_fs(&self, other: &Self) -> bool {
        self.fs == other.fs
    }

    fn header_label(&self) -> String {
        match &self.fs {
            ExplorerFs::Host => self
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| self.display_path.clone()),
            ExplorerFs::Wsl { .. } => self.display_path.clone(),
            ExplorerFs::Ssh { .. } => self.display_path.clone(),
        }
    }
}

fn remote_path_under(path: &RemotePath, root: &RemotePath) -> bool {
    let p = path.as_str().trim_end_matches('/');
    let r = root.as_str().trim_end_matches('/');
    if r == "." {
        return true;
    }
    if r == "/" {
        return p == "/" || p.starts_with('/');
    }
    p == r || p.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'))
}

fn parse_wsl_unc(path: &Path) -> Option<(String, String)> {
    let s = path.to_string_lossy().replace('/', "\\");
    let rest = s
        .strip_prefix(r"\\wsl$\")
        .or_else(|| s.strip_prefix(r"\\wsl.localhost\"))?;
    let mut parts = rest.split('\\').filter(|p| !p.is_empty());
    let distro = parts.next()?.to_string();
    let linux_tail: Vec<&str> = parts.collect();
    let linux_path = if linux_tail.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", linux_tail.join("/"))
    };
    Some((distro, linux_path))
}

fn windows_drive_to_wsl_mount(path: &Path) -> Option<String> {
    let s = path.to_string_lossy().replace('\\', "/");
    let b = s.as_bytes();
    if s.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
        let drive = (b[0] as char).to_ascii_lowercase();
        Some(format!("/mnt/{}{}", drive, &s[2..]))
    } else {
        None
    }
}

/// A per-pane snapshot of the explorer's *view* state (not the root — that is
/// derived from the pane's live cwd). The workspace stashes one of these per
/// `PaneId` so switching focus between split panes restores each pane's own tree
/// expansion + selection instead of carrying a single global state across panes.
/// Scroll is intentionally *not* captured: the tree rebuilds asynchronously on a
/// different root, so a raw pixel offset would point at the wrong rows; restoring
/// the selection (and re-scrolling to it) is the meaningful, robust behavior.
#[derive(Clone, Default)]
pub struct ExplorerSnapshot {
    expanded: HashSet<ExplorerPath>,
    selected: Option<ExplorerPath>,
}

/// One rendered tree row (a directory or a file at some depth).
#[derive(Clone)]
struct Row {
    path: ExplorerPath,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
    size: Option<u64>,
}

pub struct ExplorerView {
    config: Arc<Loaded>,
    root: ExplorerRoot,
    expanded: HashSet<ExplorerPath>,
    selected: Option<ExplorerPath>,
    rows: Vec<Row>,
    remote_fs: Arc<dyn RemoteFileService>,
    read_error: Option<String>,
    rebuilding: bool,
    /// `git status --porcelain` tags, keyed by forward-slash path relative to
    /// the root (`crates/tn-ui/src/x.rs` → 'M'). Refreshed asynchronously.
    git_status: HashMap<String, char>,
    /// Set true when the tree is rebuilt; the next render will spawn a background
    /// task to refresh git status without blocking the UI thread.
    git_stale: bool,
    /// Keeps the background git task alive until completion.
    _git_task: Option<gpui::Task<()>>,
    rebuild_rev: u64,
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
            .text_color(if is_sel || row.is_dir {
                col(ui.foreground)
            } else {
                col(ui.muted)
            })
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
        let root = ExplorerRoot::host(root);
        let mut me = Self {
            config,
            root: root.clone(),
            expanded: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            remote_fs: SftpFileService::shared(),
            read_error: None,
            rebuilding: false,
            git_status: HashMap::new(),
            git_stale: true,
            _git_task: None,
            rebuild_rev: 0,
            scroll_handle: UniformListScrollHandle::default(),
            focus_handle: cx.focus_handle(),
            _change_watcher: None,
        };
        me._change_watcher = root
            .path()
            .and_then(|path| Self::spawn_change_watcher(path, cx));
        me.rebuild(cx);
        me
    }

    fn spawn_change_watcher(
        root: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> Option<notify::RecommendedWatcher> {
        use futures::future::{select, Either};
        use futures::StreamExt;
        use notify::Watcher;
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                // 噪声目录(.git 每次 git op 都抖、build/dep 巨大且无关)不触发重建;
                // 与 agent rail watcher 共用 gitutil::is_noise_path(审查⑨ 去重)。
                if ev.paths.iter().any(|p| gitutil::is_noise_path(p)) {
                    return;
                }
                let _ = tx.unbounded_send(());
            }
        })
        .ok()?;
        if watcher
            .watch(root, notify::RecursiveMode::Recursive)
            .is_err()
        {
            return None;
        }
        let exec = cx.background_executor().clone();
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                // Trailing-edge debounce(同 rail watcher,审查⑨):收到事件后持续吸收、每个新
                // 事件重置静默计时,静默 500ms 才 rebuild 一次。单次文件操作 ~500ms 后即刷;
                // 长构建的持续事件流被不断推后 → 只在停下后扫一次目录(旧固定窗口每 500ms 扫
                // 一次)。is_noise_path 已挡构建产物,源码区无持续事件流,无需 max-wait 上限。
                while rx.next().await.is_some() {
                    loop {
                        match select(
                            rx.next(),
                            std::pin::pin!(exec.timer(Duration::from_millis(500))),
                        )
                        .await
                        {
                            Either::Left((Some(_), _)) => continue,
                            Either::Left((None, _)) => return,
                            Either::Right(((), _)) => break,
                        }
                    }
                    if this.update(cx, |this, cx| this.rebuild(cx)).is_err() {
                        return;
                    }
                }
            },
        )
        .detach();
        Some(watcher)
    }

    /// The tree's focus handle — the workspace returns focus here after Quick Look
    /// closes (you opened the file from the list, so Esc goes back to the list).
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Re-root the tree at `root` (app menu「打开文件夹」): reset expansion +
    /// selection, then rebuild from the new folder (refreshing git status for it).
    pub fn set_browser_root(&mut self, root: ExplorerRoot, cx: &mut Context<Self>) {
        if !root.is_browsable() {
            return;
        }
        let watcher_root = root.path_buf();
        self.root = root.clone();
        self.expanded.clear();
        self.selected = None;
        self.read_error = None;
        self.git_status.clear();
        self._change_watcher = watcher_root
            .as_deref()
            .and_then(|path| Self::spawn_change_watcher(path, cx));
        self.rebuild(cx);
    }

    /// Re-root the tree to follow a shell `cd` (render-driven, not the explicit
    /// 「打开文件夹」). Unlike [`set_root`](Self::set_root), this **keeps the
    /// expansion state** for the direct ancestry: `expanded` holds absolute paths,
    /// so entries under the new root stay open and the tree does not collapse when
    /// you `cd` into a subdirectory. When backing out, direct ancestors remain
    /// expanded, though distant siblings are pruned to prevent memory leaks (待优化清单 §7).
    /// The selection is kept only while it still points inside the new root.
    pub fn follow_root(&mut self, root: ExplorerRoot, cx: &mut Context<Self>) {
        if !root.is_browsable() {
            return;
        }
        if self.root == root {
            return;
        }
        let old = self.root.clone();
        let watcher_root = root.path_buf();
        self.root = root.clone();
        if old.same_fs(&root) {
            let snap = ExplorerSnapshot {
                expanded: std::mem::take(&mut self.expanded),
                selected: self.selected.take(),
            };
            let (expanded, selected) = snapshot_under_root(snap, &root);
            self.expanded = expanded;
            self.selected = selected;
        } else {
            self.expanded.clear();
            self.selected = None;
        }
        self._change_watcher = watcher_root
            .as_deref()
            .and_then(|path| Self::spawn_change_watcher(path, cx));
        self.git_status.clear();
        self.read_error = None;
        self.rebuild(cx);
    }

    /// Capture this pane's current view state (expansion + selection) so the
    /// workspace can restore it when focus returns to the same pane.
    pub fn snapshot(&self) -> ExplorerSnapshot {
        ExplorerSnapshot {
            expanded: self.expanded.clone(),
            selected: self.selected.clone(),
        }
    }

    /// Switch the tree to a *different pane* (focus moved between split panes).
    /// Unlike [`follow_root`](Self::follow_root) — which keeps expansion across a
    /// same-pane `cd` — this restores the target pane's own saved view state, or
    /// starts clean when the pane has none yet (first time it gets focus). The
    /// root comes from the pane's live cwd; expansion/selection are filtered to
    /// stay inside that root so a stale snapshot can't point outside the tree.
    pub fn switch_pane(
        &mut self,
        root: ExplorerRoot,
        snap: Option<ExplorerSnapshot>,
        cx: &mut Context<Self>,
    ) {
        if !root.is_browsable() {
            return;
        }
        let watcher_root = root.path_buf();
        self.root = root.clone();
        match snap {
            Some(snap) => {
                let (expanded, selected) = snapshot_under_root(snap, &root);
                self.expanded = expanded;
                self.selected = selected;
            }
            // No saved state: start clean.
            None => {
                self.expanded.clear();
                self.selected = None;
            }
        }
        self._change_watcher = watcher_root
            .as_deref()
            .and_then(|path| Self::spawn_change_watcher(path, cx));
        self.git_status.clear();
        self.read_error = None;
        self.rebuild(cx);
    }

    /// The current tree root — the single source of truth for the working
    /// directory. Pane launch cwd and activity-rail git directory both read
    /// this so they stay in sync with the explorer.
    pub fn root(&self) -> ExplorerRoot {
        self.root.clone()
    }

    pub fn root_path(&self) -> Option<PathBuf> {
        self.root.path_buf()
    }

    /// Run `git status --porcelain` in the root and map each changed path
    /// (forward-slash, relative) to a one-letter tag: M(odified) / U(ntracked)
    /// / A(dded) / D(eleted) / R(enamed).
    /// Uses the bounded git helper (off-thread + timeout) so a slow / locked git
    /// never freezes the caller; propagates tags upward so parent directories also
    /// show an aggregated git indicator.
    fn compute_git_status(root: &Path) -> HashMap<String, char> {
        let mut map = HashMap::new();
        // `-c core.quotePath=false` makes git emit raw UTF-8 paths instead of quoting
        // + octal-escaping non-ASCII (e.g. ` M "docs/\344\274\230..."`). Without it,
        // `parse_porcelain`'s `\`→`/` step mangled those escapes, so CJK-named files
        // never matched a tree row's real path — only their ASCII ancestor dir got an
        // aggregated tag (symptom: 文件夹有 M、中文文件无标识). Same flag as `changes_for`.
        let out = match gitutil::capture_bounded(
            root,
            &["-c", "core.quotePath=false", "status", "--porcelain"],
            Duration::from_millis(1500),
        ) {
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

    fn include_entry_name(name: &str, show_dotfiles: bool) -> bool {
        !IGNORED.contains(&name) && (show_dotfiles || !name.starts_with('.'))
    }

    /// Read `dir`'s entries, drop ignored entries, and sort directories first
    /// then files, each alphabetically (case-insensitive). Host roots keep hiding
    /// dotfiles; WSL roots show them because Linux home/root dirs often contain
    /// only dotfiles such as `.bashrc`, `.profile`, or `.ssh`.
    fn read_dir_sorted(
        dir: &Path,
        show_dotfiles: bool,
    ) -> std::io::Result<Vec<(PathBuf, String, bool)>> {
        let entries = std::fs::read_dir(dir)?;
        let mut out: Vec<(PathBuf, String, bool)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if !Self::include_entry_name(&name, show_dotfiles) {
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
        Ok(out)
    }

    fn walk(
        dir: &Path,
        depth: usize,
        expanded: &HashSet<ExplorerPath>,
        show_dotfiles: bool,
        out: &mut Vec<Row>,
    ) -> std::io::Result<()> {
        for (path, name, is_dir) in Self::read_dir_sorted(dir, show_dotfiles)? {
            if out.len() >= MAX_ROWS {
                return Ok(());
            }
            let explorer_path = ExplorerPath::Local(path.clone());
            let is_expanded = is_dir && expanded.contains(&explorer_path);
            let size = if is_dir {
                None
            } else {
                std::fs::metadata(&path).ok().map(|m| m.len())
            };
            out.push(Row {
                path: explorer_path,
                name,
                depth,
                is_dir,
                expanded: is_expanded,
                size,
            });
            if is_expanded {
                let _ = Self::walk(&path, depth + 1, expanded, show_dotfiles, out);
            }
        }
        Ok(())
    }

    fn walk_remote(
        remote_fs: &dyn RemoteFileService,
        cfg: &tn_pty::SshConfig,
        dir: &RemotePath,
        depth: usize,
        expanded: &HashSet<ExplorerPath>,
        out: &mut Vec<Row>,
    ) -> anyhow::Result<()> {
        let mut entries: Vec<_> = remote_fs
            .list_dir(cfg, dir)?
            .into_iter()
            .filter(|e| Self::include_entry_name(&e.name, true))
            .collect();
        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
        });
        for entry in entries {
            if out.len() >= MAX_ROWS {
                return Ok(());
            }
            let explorer_path = ExplorerPath::Remote(entry.id.clone());
            let is_expanded = entry.is_dir && expanded.contains(&explorer_path);
            let child_path = entry.id.path.clone();
            out.push(Row {
                path: explorer_path,
                name: entry.name,
                depth,
                is_dir: entry.is_dir,
                expanded: is_expanded,
                size: entry.size,
            });
            if is_expanded {
                let _ = Self::walk_remote(remote_fs, cfg, &child_path, depth + 1, expanded, out);
            }
        }
        Ok(())
    }

    /// Rebuild the cached row list from the filesystem + current expansion.
    /// Runs off-thread to prevent blocking the UI on huge projects or slow disks.
    /// Git status is refreshed asynchronously on the next render cycle.
    pub fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rebuild_rev = self.rebuild_rev.wrapping_add(1);
        let rev = self.rebuild_rev;
        let expected_root = self.root.clone();
        let expanded = self.expanded.clone();
        let supports_git = self.root.supports_git_status();
        let show_dotfiles = matches!(self.root.fs, ExplorerFs::Wsl { .. });
        let local_root = self.root.path_buf();
        let remote_root = match (&self.root.fs, self.root.remote_path()) {
            (ExplorerFs::Ssh { cfg }, Some(path)) => Some((cfg.clone(), path.clone())),
            _ => None,
        };
        let remote_fs = self.remote_fs.clone();
        self.rebuilding = true;
        self.read_error = None;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let (rows, read_error) = cx
                .background_executor()
                .spawn(async move {
                    let (tx, rx) = futures::channel::oneshot::channel();
                    std::thread::spawn(move || {
                        let mut out = Vec::new();
                        let read_error = if let Some(root) = local_root {
                            Self::walk(&root, 0, &expanded, show_dotfiles, &mut out)
                                .err()
                                .map(|e| e.to_string())
                        } else if let Some((cfg, root)) = remote_root {
                            Self::walk_remote(
                                remote_fs.as_ref(),
                                &cfg,
                                &root,
                                0,
                                &expanded,
                                &mut out,
                            )
                            .err()
                            .map(|e| e.to_string())
                        } else {
                            Some("No browsable path for this namespace.".to_string())
                        };
                        let _ = tx.send((out, read_error));
                    });
                    rx.await.unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.rebuild_rev != rev || this.root != expected_root {
                    return;
                }
                this.rows = rows;
                this.read_error = read_error;
                this.rebuilding = false;
                this.git_stale = supports_git;
                if !supports_git {
                    this.git_status.clear();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Kick off an async git-status refresh. Safe to call from any context;
    /// only one task runs at a time (the flag is cleared immediately).
    fn start_git_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.root.supports_git_status() {
            self.git_status.clear();
            return;
        }
        let Some(root) = self.root.path_buf() else {
            self.git_status.clear();
            return;
        };
        let exec = cx.background_executor().clone();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // compute_git_status 内部走 gitutil::capture_bounded,会**同步阻塞调用线程**
            // 直到 git 返回(最坏 1.5s)。必须在后台线程跑,否则阻塞 GPUI 前台(审查⑦: 原先
            // 直接在 cx.spawn 前台同步调用,大仓库 / .git 被锁时卡 UI,与 quick_look 老坑同
            // 源)。同 rebuild / refresh_changes:丢一次性 OS 线程 + oneshot 回传,前台只 await。
            let status = exec
                .spawn(async move {
                    let (tx, rx) = futures::channel::oneshot::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(Self::compute_git_status(&root));
                    });
                    rx.await.unwrap_or_default()
                })
                .await;
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

    fn on_row_click(
        &mut self,
        path: ExplorerPath,
        is_dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            let size = self
                .rows
                .iter()
                .find(|row| row.path == path)
                .and_then(|row| row.size);
            if let Some(file) = explorer_file_for_path(&self.root, &path, size) {
                self.selected = Some(path);
                cx.emit(OpenFile(file));
            }
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
                    } else if let Some(file) = self
                        .rows
                        .iter()
                        .find(|row| row.path == path)
                        .and_then(|row| explorer_file_for_path(&self.root, &row.path, row.size))
                    {
                        cx.emit(OpenFile(file)); // → workspace opens Quick Look
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
            None => {
                if delta >= 0 {
                    0
                } else {
                    self.rows.len() - 1
                }
            }
        };
        self.selected = Some(self.rows[next].path.clone());
        self.scroll_to_selected();
    }

    /// Select the next/prev **file** row (skipping directories) and return its path
    /// — Quick Look's `↑↓ 换文件` live-follow (driven from the focused overlay).
    /// `None` when there is no further file in that direction (selection unchanged).
    pub fn select_adjacent_file(
        &mut self,
        delta: i32,
        cx: &mut Context<Self>,
    ) -> Option<ExplorerFile> {
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
                let row = self.rows[i as usize].clone();
                let file = explorer_file_for_path(&self.root, &row.path, row.size)?;
                self.selected = Some(row.path);
                self.scroll_to_selected();
                cx.notify();
                return Some(file);
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
        let root_name = self.root.header_label();

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
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(col(ui.accent))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(root_name)),
            );

        // Prepare data for the 'static uniform_list closure (Rc/Arc clones are cheap).
        let tree_rows: std::rc::Rc<Vec<Row>> = std::rc::Rc::new(self.rows.clone());
        let tree_config = self.config.clone(); // Arc
        let tree_root: std::rc::Rc<Option<PathBuf>> = std::rc::Rc::new(self.root.path_buf());
        let tree_view_root: std::rc::Rc<ExplorerRoot> = std::rc::Rc::new(self.root.clone());
        let tree_git: std::rc::Rc<HashMap<String, char>> =
            std::rc::Rc::new(self.git_status.clone());
        let tree_sel: std::rc::Rc<Option<ExplorerPath>> = std::rc::Rc::new(self.selected.clone());
        let tree_entity = cx.entity().downgrade();
        let empty_text = if let Some(err) = &self.read_error {
            Some(format!("Cannot read folder: {err}"))
        } else if self.rebuilding {
            Some("Loading folder...".to_string())
        } else if self.rows.is_empty() {
            Some(match self.root.fs {
                ExplorerFs::Host => "No visible files in this folder.".to_string(),
                ExplorerFs::Wsl { .. } => "This WSL folder is empty.".to_string(),
                ExplorerFs::Ssh { .. } => "This remote folder is empty.".to_string(),
            })
        } else {
            None
        };
        let tree_content: AnyElement = if let Some(text) = empty_text {
            div()
                .flex_1()
                .min_h(px(0.))
                .p(px(12.))
                .text_size(px(12.))
                .text_color(col(ui.muted))
                .child(SharedString::from(text))
                .into_any_element()
        } else {
            uniform_list(
                "explorer-tree",
                self.rows.len(),
                move |range, _window, _cx| {
                    range
                        .map(|i| {
                            let row = &tree_rows[i];
                            let indent = 10.0 + row.depth as f32 * 16.0;
                            let is_sel = tree_sel.as_ref().as_ref() == Some(&row.path);
                            let key = row.path.local_path().and_then(|path| {
                                tree_root
                                    .as_deref()
                                    .and_then(|root| path.strip_prefix(root).ok())
                                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                            });
                            let git_tag = key.as_ref().and_then(|k| tree_git.get(k)).map(|&tag| {
                                let c = match tag {
                                    'U' | 'A' => tree_config.theme.ansi.green,
                                    'D' => tree_config.theme.ansi.red,
                                    _ => tree_config.theme.ansi.yellow,
                                };
                                (tag, c)
                            });
                            let path = row.path.clone();
                            let is_dir = row.is_dir;
                            let size = row.size;
                            let entity = tree_entity.clone();
                            let view_root = tree_view_root.clone();
                            tree_row(
                                &tree_config.theme.ui,
                                &tree_config.theme,
                                row,
                                indent,
                                is_sel,
                                git_tag,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_ev, _w, app| {
                                    app.stop_propagation();
                                    let path = path.clone();
                                    let view_root = view_root.clone();
                                    let _ = entity.update(app, move |this, cx| {
                                        if is_dir {
                                            if !this.expanded.remove(&path) {
                                                this.expanded.insert(path.clone());
                                            }
                                            this.rebuild(cx);
                                        } else {
                                            let file =
                                                explorer_file_for_path(&view_root, &path, size);
                                            this.selected = Some(path);
                                            if let Some(file) = file {
                                                cx.emit(OpenFile(file));
                                            }
                                        }
                                        cx.notify();
                                    });
                                },
                            )
                        })
                        .collect::<Vec<_>>()
                },
            )
            .flex_1()
            .min_h(px(0.))
            .p(px(6.)) // mockup .tree padding 6
            .track_scroll(self.scroll_handle.clone())
            .into_any_element()
        };
        // Inner content, rounded 1px tighter so the gradient-border ring shows
        // (see style::glass_pane); g1 glass + specular + header + tree.
        let is_focused = self.focus_handle.is_focused(window);
        let inner = div()
            .track_focus(&self.focus_handle)
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)),
            )
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
            .child(tree_content);
        // mockup .pane::before 竖向渐变描边 + 浮起投影(与终端面板一致;explorer 恒非焦点)
        glass_pane(inner, false, ui.accent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(path: &str) -> ExplorerPath {
        ExplorerPath::Local(PathBuf::from(path))
    }

    fn host_root(path: &str) -> ExplorerRoot {
        ExplorerRoot::host(PathBuf::from(path))
    }

    fn ssh_cfg() -> tn_pty::SshConfig {
        tn_pty::SshConfig {
            host: "example.com".into(),
            port: 2222,
            user: "alice".into(),
            key_path: None,
            password: None,
        }
    }

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
    fn porcelain_matches_utf8_paths_when_quotepath_off() {
        // With core.quotePath=false git emits raw UTF-8 (no quotes / octal escapes),
        // so the parsed key equals the tree row's real path. Regression for: CJK-named
        // files (优化日志.md, 未修复.md …) showed no git tag while their ASCII ancestor
        // dir (docs) did — octal-escaped quoted paths produced unmatchable keys after
        // the `\`→`/` step (symptom: 文件夹有 M、中文文件无标识).
        let m = parse_porcelain(" M docs/优化日志.md\n?? 新增模块.md\n");
        assert_eq!(
            m.get("docs/优化日志.md"),
            Some(&'M'),
            "CJK file path matches its real key"
        );
        assert_eq!(m.get("新增模块.md"), Some(&'U'));
        // (Ancestor-dir aggregation lives in `compute_git_status`, not here.)
    }

    #[test]
    fn selection_kept_only_under_new_root() {
        // `cd` into a subdir: a selection inside the new root survives (so the
        // highlight follows you down); one outside is dropped (it'd point at a
        // now-invisible path). None stays None.
        let root = host_root("D:/proj/crates");
        let inside = Some(local("D:/proj/crates/tn-ui/src.rs"));
        assert_eq!(selection_under_root(&inside, &root), inside);
        let outside = Some(local("D:/proj/docs/x.md"));
        assert_eq!(selection_under_root(&outside, &root), None);
        assert_eq!(selection_under_root(&None, &root), None);
        // The root itself counts as under-root (component-wise starts_with).
        let at_root = Some(local("D:/proj/crates"));
        assert_eq!(selection_under_root(&at_root, &root), at_root);
    }

    #[test]
    fn snapshot_restore_filters_to_new_root() {
        // Restoring a pane's saved tree state when focus returns: expanded dirs
        // under the new root survive (and ancestors of the root, so the path down
        // re-opens), entries outside are pruned, and the selection is kept only
        // while it points inside the root (面板解耦 per-pane state).
        let root = host_root("D:/proj/crates");
        let snap = ExplorerSnapshot {
            expanded: HashSet::from([
                local("D:/proj/crates/tn-ui"), // under root → keep
                local("D:/proj"),              // ancestor of root → keep
                local("D:/other/x"),           // unrelated → drop
            ]),
            selected: Some(local("D:/proj/crates/tn-ui/src.rs")),
        };
        let (expanded, selected) = snapshot_under_root(snap, &root);
        assert!(expanded.contains(&local("D:/proj/crates/tn-ui")));
        assert!(expanded.contains(&local("D:/proj")));
        assert!(!expanded.contains(&local("D:/other/x")));
        assert_eq!(expanded.len(), 2);
        assert_eq!(selected, Some(local("D:/proj/crates/tn-ui/src.rs")));
    }

    #[test]
    fn snapshot_restore_drops_out_of_root_selection() {
        // A selection saved while the pane was elsewhere must not leak into a
        // different root (it'd highlight an invisible row).
        let root = host_root("D:/proj/crates");
        let snap = ExplorerSnapshot {
            expanded: HashSet::new(),
            selected: Some(local("D:/proj/docs/readme.md")),
        };
        let (expanded, selected) = snapshot_under_root(snap, &root);
        assert!(expanded.is_empty());
        assert_eq!(selected, None, "selection outside new root is dropped");
    }

    #[test]
    fn explorer_root_detects_wsl_unc() {
        let root = ExplorerRoot::from_accessible_path(PathBuf::from(r"\\wsl$\Ubuntu\home\me"));
        assert_eq!(
            root,
            ExplorerRoot::wsl(
                "Ubuntu".to_string(),
                "/home/me".to_string(),
                PathBuf::from(r"\\wsl$\Ubuntu\home\me")
            )
        );
        assert!(matches!(root.fs, ExplorerFs::Wsl { .. }));
        assert!(!root.supports_git_status());
    }

    #[test]
    fn porcelain_skips_blank_and_short_lines() {
        // Empty output (clean repo / not-a-repo) and malformed short lines yield
        // nothing instead of panicking on the `[..2]` / `[3..]` slices.
        assert!(parse_porcelain("").is_empty());
        assert!(
            parse_porcelain("\n\nx\n M\n").is_empty(),
            "lines < 4 chars skipped"
        );
    }

    #[test]
    fn entry_filter_shows_dotfiles_for_wsl_only() {
        assert!(!ExplorerView::include_entry_name(".bashrc", false));
        assert!(ExplorerView::include_entry_name(".bashrc", true));
        assert!(!ExplorerView::include_entry_name(".git", true));
        assert!(!ExplorerView::include_entry_name("target", true));
        assert!(ExplorerView::include_entry_name("src", false));
    }

    #[test]
    fn path_for_namespace_translation() {
        use crate::terminal_view::FileNamespace;

        // 1. Host root
        let host_root = ExplorerRoot::host(PathBuf::from(r"D:\coder\Tn"));
        assert_eq!(
            host_root.path_for_namespace(&FileNamespace::Host),
            Some(r"D:\coder\Tn".to_string())
        );
        assert_eq!(
            host_root.path_for_namespace(&FileNamespace::Wsl {
                distro: Some("Ubuntu".to_string())
            }),
            Some("/mnt/d/coder/Tn".to_string())
        );
        let unc_root = ExplorerRoot::host(PathBuf::from(r"\\server\share"));
        assert_eq!(
            unc_root.path_for_namespace(&FileNamespace::Wsl {
                distro: Some("Ubuntu".to_string())
            }),
            None,
            "only drive-letter Windows paths have a reliable /mnt/<drive> WSL mapping"
        );

        // 2. WSL root
        let wsl_root = ExplorerRoot::wsl(
            "Ubuntu".to_string(),
            "/home/me".to_string(),
            PathBuf::from(r"\\wsl$\Ubuntu\home\me"),
        );
        assert_eq!(
            wsl_root.path_for_namespace(&FileNamespace::Wsl {
                distro: Some("Ubuntu".to_string())
            }),
            Some("/home/me".to_string())
        );
        assert_eq!(
            wsl_root.path_for_namespace(&FileNamespace::Wsl {
                distro: Some("Debian".to_string())
            }),
            None
        );
        assert_eq!(
            wsl_root.path_for_namespace(&FileNamespace::Host),
            Some(r"\\wsl$\Ubuntu\home\me".to_string())
        );

        // SSH intentionally has no ExplorerRoot mapping until a remote filesystem
        // backend exists; host/WSL roots must still never fake such a mapping.
        assert_eq!(host_root.path_for_namespace(&FileNamespace::Ssh), None);
        assert_eq!(wsl_root.path_for_namespace(&FileNamespace::Ssh), None);

        let ssh_root = ExplorerRoot::ssh(ssh_cfg(), "/home/alice/project");
        assert_eq!(ssh_root.path_buf(), None);
        assert_eq!(
            ssh_root.path_for_namespace(&FileNamespace::Ssh),
            Some("/home/alice/project".to_string())
        );
        assert_eq!(ssh_root.path_for_namespace(&FileNamespace::Host), None);
    }

    #[derive(Default)]
    struct FakeRemoteFs {
        entries: HashMap<String, Vec<tn_pty::remote_fs::RemoteDirEntry>>,
    }

    impl tn_pty::remote_fs::RemoteFileService for FakeRemoteFs {
        fn list_dir(
            &self,
            _cfg: &tn_pty::SshConfig,
            path: &tn_pty::remote_fs::RemotePath,
        ) -> anyhow::Result<Vec<tn_pty::remote_fs::RemoteDirEntry>> {
            Ok(self.entries.get(path.as_str()).cloned().unwrap_or_default())
        }

        fn read_file(
            &self,
            _cfg: &tn_pty::SshConfig,
            _path: &tn_pty::remote_fs::RemotePath,
            _max_bytes: u64,
        ) -> anyhow::Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    fn remote_entry(
        cfg: &tn_pty::SshConfig,
        path: &str,
        name: &str,
        is_dir: bool,
        size: Option<u64>,
    ) -> tn_pty::remote_fs::RemoteDirEntry {
        tn_pty::remote_fs::RemoteDirEntry {
            id: tn_pty::remote_fs::RemoteId::new(cfg, path),
            name: name.into(),
            is_dir,
            size,
            permissions: None,
            mtime: None,
        }
    }

    #[test]
    fn remote_walk_builds_rows_and_emits_remote_file_handles() {
        let cfg = ssh_cfg();
        let root = ExplorerRoot::ssh(cfg.clone(), "/home/alice/project");
        let mut fs = FakeRemoteFs::default();
        fs.entries.insert(
            "/home/alice/project".into(),
            vec![
                remote_entry(&cfg, "/home/alice/project/src", "src", true, None),
                remote_entry(
                    &cfg,
                    "/home/alice/project/main.rs",
                    "main.rs",
                    false,
                    Some(42),
                ),
                remote_entry(&cfg, "/home/alice/project/.git", ".git", true, None),
            ],
        );
        fs.entries.insert(
            "/home/alice/project/src".into(),
            vec![remote_entry(
                &cfg,
                "/home/alice/project/src/lib.rs",
                "lib.rs",
                false,
                Some(7),
            )],
        );

        let expanded = HashSet::from([ExplorerPath::Remote(tn_pty::remote_fs::RemoteId::new(
            &cfg,
            "/home/alice/project/src",
        ))]);
        let mut rows = Vec::new();
        ExplorerView::walk_remote(
            &fs,
            &cfg,
            root.remote_path().unwrap(),
            0,
            &expanded,
            &mut rows,
        )
        .unwrap();

        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["src", "lib.rs", "main.rs"]
        );
        let main = rows.iter().find(|r| r.name == "main.rs").unwrap();
        assert_eq!(main.size, Some(42));
        assert!(matches!(
            explorer_file_for_path(&root, &main.path, main.size),
            Some(ExplorerFile::Remote { id, size: Some(42), .. })
                if id.path.as_str() == "/home/alice/project/main.rs"
        ));
    }
}
