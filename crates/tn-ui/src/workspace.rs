//! Workspace: multiple tabs, each an n-ary pane tree of [`TerminalView`]s.
//!
//! Splitting uses an n-ary container tree (not a binary tree): splitting along
//! the same axis as the focused pane's parent inserts an aligned sibling;
//! splitting along the other axis nests a new container. This matches the
//! flexible-tiling model in docs/产品设计.md. Divider-drag and drag-dock are
//! later refinements; this cut gives tabs + keyboard splits + click-to-focus.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    actions, canvas, div, linear_color_stop, linear_gradient, prelude::*, px, relative, rgba,
    AnyElement, App, AppContext, AsyncApp, Context, Entity, FocusHandle, KeyBinding, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Rgba, SharedString,
    Subscription, WeakEntity, Window, WindowControlArea,
};
use tn_config::Loaded;

use crate::explorer::{ExplorerView, OpenFile};
use crate::layout::{LayoutNode, LayoutPane, Layouts, SLOTS};
use crate::perf::PerfStats;
use crate::quick_look::{QuickLook, QuickLookEvent};
use crate::terminal_view::{FilesChanged, LaunchSpec, OpenInQuickLook, TerminalView, UsageUpdated};
use crate::welcome::{launch_rows, row_card, wsl_distros, LaunchRequested, LaunchRow, WelcomeView};

type PaneId = u64;

// Calm Glass tokens + helpers (col/cola/soft_shadow/shadowed/icon/UI_SANS/radii)
// now live in `crate::style` — single source of truth (待优化清单 §4.1).
use crate::style::{
    col, cola, glass_pane, icon, pane_fill, shadowed, soft_shadow, DIVIDER, HOVER,
    INSET, RIM, R_CARD, R_PANEL, R_WINDOW, SHEEN, UI_SANS,
};

/// Trim a tab title to `max` characters, appending an ellipsis when clipped.
fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    } else {
        s.to_string()
    }
}

/// Pretty model id for the status bar (`claude-opus-4-7` → `Opus 4.7`).
pub(crate) fn short_model(id: &str) -> String {
    let l = id.to_ascii_lowercase();
    let fam = if l.contains("opus") {
        "Opus"
    } else if l.contains("sonnet") {
        "Sonnet"
    } else if l.contains("haiku") {
        "Haiku"
    } else if l.contains("gpt") || l.contains("codex") {
        "GPT"
    } else {
        return id.to_string();
    };
    let ver: String = id
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .replace('-', ".");
    if ver.is_empty() {
        fam.to_string()
    } else {
        format!("{fam} {ver}")
    }
}

/// Humanize a token count (`444731` → `444K`, `1000000` → `1.0M`).
pub(crate) fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

/// Short cwd for the tab badge / shell header: last two path components (`proj/tn`).
pub(crate) fn short_cwd(p: &str) -> String {
    let p = p.trim().replace('\\', "/");
    let parts: Vec<&str> = p.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    match parts.len() {
        0 => p,
        1 => parts[0].to_string(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// The current git branch of the app's cwd, if it's a repo (for the status bar).
/// Returns `None` both when not in a repo (silent — expected) and when `git`
/// can't be spawned (logged once — likely not installed / PATH). (待优化清单 §8.2)
fn git_branch() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let out = match std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .arg("branch")
        .arg("--show-current")
        .output()
    {
        Ok(o) => o,
        Err(_) => return None, // git unavailable; status bar branch disabled
    };
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Whether a profile can be launched now: a command-bearing shell/agent, a WSL
/// distro (`wsl.exe` over the local ConPTY), or an SSH host (russh backend).
pub(crate) fn is_launchable(p: &tn_config::Profile) -> bool {
    use tn_config::ProfileKind;
    match p.kind {
        ProfileKind::Wsl => p.distro.as_deref().is_some_and(|d| !d.is_empty()),
        ProfileKind::Ssh => p.host.as_deref().is_some_and(|h| !h.is_empty()),
        _ => p.command.is_some(),
    }
}

/// The launcher's profiles: the configured `[[profiles]]` plus every installed
/// WSL distro not already covered by a config profile — so users get *all* their
/// distros without editing config (the default config ships only one). Shells
/// out to `wsl.exe` once (cache the result; don't call per render). Docker's
/// internal `docker-desktop*` distros are skipped (not interactive shells).
pub(crate) fn discover_profiles(config: &Loaded) -> Vec<tn_config::Profile> {
    let mut profiles = config.config.profiles.clone();
    let configured: std::collections::HashSet<String> = profiles
        .iter()
        .filter(|p| p.kind == tn_config::ProfileKind::Wsl)
        .filter_map(|p| p.distro.as_deref())
        .map(str::to_ascii_lowercase)
        .collect();
    for distro in tn_pty::wsl::list_distros() {
        let low = distro.to_ascii_lowercase();
        if low.starts_with("docker-desktop") || configured.contains(&low) {
            continue;
        }
        profiles.push(tn_config::Profile {
            name: distro.clone(),
            kind: tn_config::ProfileKind::Wsl,
            command: None,
            args: Vec::new(),
            cwd: None,
            distro: Some(distro),
            host: None,
            user: None,
            agent: None,
            // No explicit accent → `launch_tile_accent` derives the WSL identity
            // (violet, mockup `.dot`/`.tile.wsl` = --violet). (Was a hardcoded blue.)
            accent: None,
            glyph: None,
        });
    }
    profiles
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Axis {
    Row, // children side by side (vertical dividers)
    Col, // children stacked (horizontal dividers)
}

/// Where a `新会话` split places the new pane relative to the focused one.
#[derive(Clone, Copy, PartialEq)]
enum SplitDir {
    Left,
    Right,
    Up,
    Down,
}
impl SplitDir {
    fn axis(self) -> Axis {
        match self {
            SplitDir::Left | SplitDir::Right => Axis::Row,
            SplitDir::Up | SplitDir::Down => Axis::Col,
        }
    }
    /// Insert before the focused pane (left/up) vs after (right/down).
    fn before(self) -> bool {
        matches!(self, SplitDir::Left | SplitDir::Up)
    }
    /// `(icon, label)` for the direction tile.
    fn label(self) -> (&'static str, &'static str) {
        match self {
            SplitDir::Left => ("←", "左"),
            SplitDir::Right => ("→", "右"),
            SplitDir::Up => ("↑", "上"),
            SplitDir::Down => ("↓", "下"),
        }
    }
}

/// An in-progress divider drag (mouse). Identifies a split (by tree path in the
/// active tab) and the gap being dragged, plus the start state for an absolute
/// (drift-free) weight recompute on each mouse move.
struct DividerDrag {
    path: Vec<usize>,
    gap: usize, // seam between kids[gap] and kids[gap + 1]
    axis: Axis,
    start_weights: Vec<f32>,
    start_pos: f32, // mouse coord along the split axis at mouse-down
    cur_pos: f32,   // latest mouse coord (drives the live preview line)
}

/// An in-progress explorer sidebar width drag (mouse). Simpler than a split
/// divider — just tracks the horizontal delta and clamps between min/max.
struct ExplorerDrag {
    start_x: f32,
    start_width: f32,
}

/// A tab's layout: a tree whose leaves are panes.
enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        kids: Vec<Node>,
        weights: Vec<f32>,
    },
}

impl Node {
    fn leaf_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { kids, .. } => kids.iter().map(Node::leaf_count).sum(),
        }
    }

    /// Insert `new` next to leaf `target`, splitting along `axis`. Returns true
    /// if `target` was found.
    /// Split `target` along `axis`, inserting the `new` leaf `before` it (left/up)
    /// or after it (right/down).
    fn split(&mut self, target: PaneId, new: PaneId, axis: Axis, before: bool) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let old = Node::Leaf(*id);
                let kids = if before {
                    vec![Node::Leaf(new), old]
                } else {
                    vec![old, Node::Leaf(new)]
                };
                *self = Node::Split { axis, kids, weights: vec![1.0, 1.0] };
                true
            }
            Node::Leaf(_) => false,
            Node::Split {
                axis: sa,
                kids,
                weights,
            } => {
                // Same axis + target is a direct child -> aligned n-ary insert.
                if *sa == axis {
                    if let Some(pos) = kids
                        .iter()
                        .position(|k| matches!(k, Node::Leaf(id) if *id == target))
                    {
                        let at = if before { pos } else { pos + 1 };
                        kids.insert(at, Node::Leaf(new));
                        weights.insert(at, 1.0);
                        return true;
                    }
                }
                kids.iter_mut().any(|k| k.split(target, new, axis, before))
            }
        }
    }

    /// The node at `path` (a sequence of child indices from this node). `[]` is
    /// `self`. Used by divider-drag to address a specific split.
    fn at_path_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        match path.split_first() {
            None => Some(self),
            Some((i, rest)) => match self {
                Node::Split { kids, .. } => kids.get_mut(*i)?.at_path_mut(rest),
                Node::Leaf(_) => None,
            },
        }
    }

    /// Does this subtree contain leaf `target`?
    fn contains(&self, target: PaneId) -> bool {
        match self {
            Node::Leaf(id) => *id == target,
            Node::Split { kids, .. } => kids.iter().any(|k| k.contains(target)),
        }
    }

    /// Grow/shrink (by `delta`) the weight of the child — along the nearest
    /// `axis`-matching split — whose subtree holds `target`. Innermost match
    /// wins, so resize is local to the focused pane. Returns true if applied.
    fn resize(&mut self, target: PaneId, axis: Axis, delta: f32) -> bool {
        if let Node::Split { axis: sa, kids, weights } = self {
            // Recurse first: a deeper matching split is more local.
            for k in kids.iter_mut() {
                if k.resize(target, axis, delta) {
                    return true;
                }
            }
            if *sa == axis {
                if let Some(pos) = kids.iter().position(|k| k.contains(target)) {
                    weights[pos] = (weights[pos] + delta).clamp(0.1, 100.0);
                    return true;
                }
            }
        }
        false
    }
}

/// Remove `target`, collapsing single-child splits. Returns the new subtree
/// (None if it became empty).
fn prune(node: Node, target: PaneId) -> Option<Node> {
    match node {
        Node::Leaf(id) => (id != target).then_some(Node::Leaf(id)),
        Node::Split {
            axis,
            kids,
            weights,
        } => {
            let mut nk = Vec::new();
            let mut nw = Vec::new();
            for (k, w) in kids.into_iter().zip(weights) {
                if let Some(pk) = prune(k, target) {
                    nk.push(pk);
                    nw.push(w);
                }
            }
            match nk.len() {
                0 => None,
                1 => Some(nk.into_iter().next().unwrap()),
                _ => Some(Node::Split {
                    axis,
                    kids: nk,
                    weights: nw,
                }),
            }
        }
    }
}

fn first_leaf(node: &Node) -> PaneId {
    match node {
        Node::Leaf(id) => *id,
        Node::Split { kids, .. } => first_leaf(&kids[0]),
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { kids, .. } => kids.iter().for_each(|k| collect_leaves(k, out)),
    }
}

struct Tab {
    root: Node,
    focused: PaneId,
    /// A new tab opens on the welcome launchpad (no pane yet); clicking a launch
    /// tile spawns the pane and flips this off. While `true`, `root`/`focused`
    /// are unused dummies and the pane-tree actions (split/resize/close) no-op.
    welcome: bool,
}

/// Dummy pane id for a welcome tab's unused `root`/`focused`. Must never collide
/// with a real pane id (those start at 0 and increment), or closing a welcome tab
/// would `panes.remove` a real pane — so we use the top of the id space.
const WELCOME_DUMMY: PaneId = PaneId::MAX;

impl Tab {
    /// A fresh welcome-launchpad tab (no pane — dummy root/focused never touched).
    fn welcome() -> Self {
        Tab { root: Node::Leaf(WELCOME_DUMMY), focused: WELCOME_DUMMY, welcome: true }
    }
    /// A tab holding a (single, to start) pane tree.
    fn panes(root: Node, focused: PaneId) -> Self {
        Tab { root, focused, welcome: false }
    }
}

actions!(
    tn,
    [
        NewTab,
        SplitRight,
        SplitDown,
        ClosePane,
        NextPane,
        NextTab,
        ReloadConfig,
        GrowWidth,
        ShrinkWidth,
        GrowHeight,
        ShrinkHeight,
        TogglePalette,
        ToggleExplorer,
        ToggleQuickLook,
        NewSession,
        Quit
    ]
);

/// Weight step per resize keystroke (panes start at weight 1.0).
const RESIZE_STEP: f32 = 0.2;

/// The built-in default key bindings (the base; config `[[keybindings]]` layer
/// on top, so a custom config adds/rebinds without losing the rest).
fn default_bindings() -> Vec<KeyBinding> {
    let ctx = Some("Workspace");
    vec![
        KeyBinding::new("ctrl-shift-t", NewTab, ctx),
        KeyBinding::new("ctrl-shift-d", SplitRight, ctx),
        KeyBinding::new("ctrl-shift-e", SplitDown, ctx),
        KeyBinding::new("ctrl-shift-w", ClosePane, ctx),
        KeyBinding::new("ctrl-shift-]", NextPane, ctx),
        KeyBinding::new("ctrl-tab", NextTab, ctx),
        KeyBinding::new("ctrl-shift-r", ReloadConfig, ctx),
        KeyBinding::new("ctrl-shift-right", GrowWidth, ctx),
        KeyBinding::new("ctrl-shift-left", ShrinkWidth, ctx),
        KeyBinding::new("ctrl-shift-down", GrowHeight, ctx),
        KeyBinding::new("ctrl-shift-up", ShrinkHeight, ctx),
        KeyBinding::new("ctrl-shift-p", TogglePalette, ctx),
        KeyBinding::new("ctrl-shift-b", ToggleExplorer, ctx),
        KeyBinding::new("ctrl-shift-j", ToggleQuickLook, ctx),
        KeyBinding::new("ctrl-shift-n", NewSession, ctx),
        KeyBinding::new("ctrl-shift-q", Quit, ctx),
    ]
}

/// Build a binding for `keys` that triggers the action named `command`, or
/// `None` for an unknown action name.
fn binding_for(keys: &str, command: &str) -> Option<KeyBinding> {
    let ctx = Some("Workspace");
    Some(match command {
        "new_tab" => KeyBinding::new(keys, NewTab, ctx),
        "split_right" => KeyBinding::new(keys, SplitRight, ctx),
        "split_down" => KeyBinding::new(keys, SplitDown, ctx),
        "close_pane" => KeyBinding::new(keys, ClosePane, ctx),
        "next_pane" => KeyBinding::new(keys, NextPane, ctx),
        "next_tab" => KeyBinding::new(keys, NextTab, ctx),
        "reload_config" => KeyBinding::new(keys, ReloadConfig, ctx),
        "grow_width" => KeyBinding::new(keys, GrowWidth, ctx),
        "shrink_width" => KeyBinding::new(keys, ShrinkWidth, ctx),
        "grow_height" => KeyBinding::new(keys, GrowHeight, ctx),
        "shrink_height" => KeyBinding::new(keys, ShrinkHeight, ctx),
        "command_palette" | "toggle_palette" => KeyBinding::new(keys, TogglePalette, ctx),
        "toggle_explorer" | "explorer" => KeyBinding::new(keys, ToggleExplorer, ctx),
        "toggle_quick_look" | "quick_look" | "toggle_viewer" | "viewer" => {
            KeyBinding::new(keys, ToggleQuickLook, ctx)
        }
        "quit" => KeyBinding::new(keys, Quit, ctx),
        "new_session" => KeyBinding::new(keys, NewSession, ctx),
        _ => return None,
    })
}

/// Register workspace key bindings: built-in defaults plus any `[[keybindings]]`
/// from config (resolving each `id` through the `[[actions]]` table). Config
/// bindings are additive in M1 — they add or remap, but don't unbind defaults.
pub fn bind_keys(cx: &mut App, config: &Loaded) {
    let mut binds = default_bindings();
    let cmd_for_id: HashMap<&str, &str> = config
        .config
        .actions
        .iter()
        .map(|a| (a.id.as_str(), a.command.as_str()))
        .collect();
    for kb in &config.config.keybindings {
        let command = cmd_for_id.get(kb.id.as_str()).copied().unwrap_or(kb.id.as_str());
        if let Some(b) = binding_for(&kb.keys, command) {
            binds.push(b); // unknown action ids are silently skipped
        }
    }
    cx.bind_keys(binds);
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    panes: HashMap<PaneId, Entity<TerminalView>>,
    /// Each live pane's launch spec — lets `打开文件夹` know which panes are plain
    /// local shells (safe to `cd`) and (later) lets layouts re-spawn panes.
    pane_specs: HashMap<PaneId, LaunchSpec>,
    next_id: PaneId,
    focused_init: bool,
    /// Re-park focus when it's orphaned (dropped to `None`): `on_focus_out` on the
    /// root anchor fires whenever focus leaves *everything*, so we re-grab it and the
    /// `Workspace` shortcuts stay live (fixes "失焦后唤不出命令面板" for non-click
    /// orphans — overlay closes, programmatic blur — that the in-render parking missed).
    /// Registered once on first render; the `Subscription` lives as long as the view.
    focus_out_sub: Option<Subscription>,
    /// The main window opens hidden; revealed after the first frame paints (avoids
    /// the pre-paint transparent flash). Tracks the one-shot reveal.
    revealed: bool,
    config: Arc<Loaded>,
    /// File explorer sidebar (left column) + whether it's shown.
    explorer: Entity<ExplorerView>,
    explorer_open: bool,
    explorer_width: f32,
    /// Active explorer-width drag (mouse), if any.
    explorer_drag: Option<ExplorerDrag>,
    /// Quick Look 速览浮层(贴树右缘、浮于终端之上)+ whether it's shown
    /// (auto-opens on clicking a file in the explorer; only rendered when it
    /// actually has a file loaded).
    quick_look: Entity<QuickLook>,
    quick_look_open: bool,
    /// Return focus to the active pane next render (set when Quick Look closes via
    /// its own keyboard — the event callback has no `window` to focus with).
    ql_refocus_pane: bool,
    /// App menu (click the Tn brand) dropdown open state.
    app_menu_open: bool,
    /// Welcome launchpad shown as a new tab's content (until a tile is clicked).
    /// One shared entity (stateless chrome); its `LaunchRequested` launches into
    /// the active tab.
    welcome: Entity<WelcomeView>,
    /// Current git branch of the app cwd (status bar), resolved at startup.
    branch: Option<String>,
    /// Fallback focus anchor for the workspace. gpui dispatches keybindings by
    /// **focus**, not mouse position, and the action context lives on the workspace
    /// root. When focus is orphaned (e.g. a click landed on an empty chrome gap,
    /// blurring the pane), `render` parks focus here so `key_context("Workspace")`
    /// stays live and `Ctrl+Shift+P` (and the other shortcuts) keep working.
    ///
    /// This handle is `track_focus`'d onto the **window root**, so clicking *anywhere*
    /// re-anchors focus under the `Workspace` context (fixing "失去焦点后唤不出命令面板"
    /// when a click lands on the status bar / a closed overlay's spot). A `track_focus`
    /// element registers a focus-on-click listener that calls `prevent_default`, and the
    /// Windows NC-drag is suppressed when the titlebar mouse-down is default-prevented —
    /// which is why the titlebar drag spacer is `.occlude()`d (BlockMouse): its
    /// mouse-down never reaches the root's focus-on-click, so the drag survives (踩过的坑).
    workspace_focus: FocusHandle,
    /// Command palette (Ctrl+Shift+P) state.
    palette_open: bool,
    palette_query: String,
    palette_sel: usize,
    /// Within the palette, drilled into the WSL group's distros.
    palette_wsl: bool,
    palette_focus: FocusHandle,
    /// `新会话` split launcher (app menu): pick a split direction, then a profile,
    /// then open the session in a new split. `split_dir = None` = phase 1 (picking
    /// direction); `Some(dir)` = phase 2 (picking the profile). Distinct from the
    /// command palette (which opens a session in a new *tab*).
    split_launcher_open: bool,
    split_dir: Option<SplitDir>,
    split_sel: usize,
    /// Within phase 2, drilled into the WSL group's distros.
    split_wsl: bool,
    split_focus: FocusHandle,
    split_needs_focus: bool,
    /// Pane to split on, snapshotted when `新会话` is invoked (before the launcher
    /// overlay steals focus). `split_session` prefers this over the live `focused`
    /// field, which can drift while the overlay is up.
    split_target: Option<PaneId>,
    /// `布局` manager (app menu): 7 slots that save/load the active tab's pane
    /// structure + launchers (loading re-spawns; a live session isn't serialized).
    layouts: Layouts,
    layout_manager_open: bool,
    layout_focus: FocusHandle,
    layout_needs_focus: bool,
    /// Launchable profiles (config `[[profiles]]` + installed WSL distros),
    /// resolved once at startup (see [`discover_profiles`]).
    launch_profiles: Vec<tn_config::Profile>,
    /// Active divider drag (mouse), if any.
    divider_drag: Option<DividerDrag>,
    /// Each split container's extent (px along its axis), captured per render by
    /// a canvas, keyed by tree path — lets a divider drag map pixels → weight.
    split_extents: Rc<RefCell<HashMap<Vec<usize>, f32>>>,
    /// Focus the palette in the next render. Focusing in the toggle action (before
    /// the overlay is rendered) doesn't reliably land, so keys leaked to the
    /// terminal underneath; we focus it in render where the element exists.
    palette_needs_focus: bool,
    /// Opt-in render instrumentation (TN_PERF, 待优化清单 §2.2): how often the
    /// workspace chrome re-renders and how long it takes. Panes are embedded as
    /// entities, so terminal output frames don't trigger this — only the
    /// workspace's own notifies (usage updates, tab/split/focus, palette) do.
    perf: PerfStats,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let explorer = cx.new(|cx| ExplorerView::new(cx, config.clone()));
        let quick_look = cx.new(|cx| QuickLook::new(cx, config.clone()));
        // Clicking / Space-ing a file in the explorer pops the Quick Look overlay
        // for it (open() also flags the overlay to grab focus on its next render).
        cx.subscribe(&explorer, |ws, _explorer, ev: &OpenFile, cx| {
            let path = ev.0.clone();
            ws.quick_look.update(cx, |v, cx| {
                v.open(path);
                cx.notify();
            });
            ws.quick_look_open = true;
            cx.notify();
        })
        .detach();
        // Quick Look keyboard that needs the workspace: `↑↓` change file (drive the
        // tree's selection), `Esc`/`Space` close (give focus back to the terminal).
        cx.subscribe(&quick_look, |ws, _ql, ev: &QuickLookEvent, cx| {
            match ev {
                QuickLookEvent::Nav(delta) => {
                    let next = ws.explorer.update(cx, |e, cx| e.select_adjacent_file(*delta, cx));
                    if let Some(path) = next {
                        ws.quick_look.update(cx, |v, cx| {
                            v.open(path);
                            cx.notify();
                        });
                    }
                }
                QuickLookEvent::Close => {
                    ws.quick_look_open = false;
                    ws.ql_refocus_pane = true; // refocus the pane in next render
                    cx.notify();
                }
                QuickLookEvent::FileSaved(_path) => {
                    // Editor saved a file → refresh every agent pane's「本次改动」now
                    // (synchronous + deterministic; `refresh_changes` no-ops on plain
                    // shells and recomputes git only for panes whose cwd covers it).
                    for view in ws.panes.values() {
                        view.update(cx, |v, cx| v.refresh_changes(cx));
                    }
                    // Also mark the explorer stale so git tags refresh.
                    ws.explorer.update(cx, |explorer, _cx| explorer.mark_stale());
                    cx.notify();
                }
            }
        })
        .detach();
        // Resolve launchable profiles once (config + installed WSL distros).
        let launch_profiles = discover_profiles(&config);
        // Welcome launchpad (new-tab default): clicking a tile launches that
        // profile into the active tab (welcome → panes).
        let welcome = cx.new(|cx| WelcomeView::new(cx, config.clone(), launch_profiles.clone()));
        cx.subscribe(&welcome, |ws, _welcome, ev: &LaunchRequested, cx| {
            ws.launch_in_active_tab(ev.0, cx);
            cx.notify();
        })
        .detach();
        let mut ws = Self {
            tabs: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            pane_specs: HashMap::new(),
            next_id: 0,
            focused_init: false,
            focus_out_sub: None,
            revealed: false,
            config,
            explorer,
            explorer_open: true,
            explorer_width: 224.0,
            explorer_drag: None,
            quick_look,
            quick_look_open: false,
            ql_refocus_pane: false,
            app_menu_open: false,
            welcome,
            branch: git_branch(),
            workspace_focus: cx.focus_handle(),
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            palette_wsl: false,
            palette_focus: cx.focus_handle(),
            split_launcher_open: false,
            split_target: None,
            split_dir: None,
            split_sel: 0,
            split_wsl: false,
            split_focus: cx.focus_handle(),
            split_needs_focus: false,
            layouts: Layouts::load(),
            layout_manager_open: false,
            layout_focus: cx.focus_handle(),
            layout_needs_focus: false,
            launch_profiles,
            divider_drag: None,
            split_extents: Rc::new(RefCell::new(HashMap::new())),
            palette_needs_focus: false,
            perf: PerfStats::new("workspace.render"),
        };
        // First tab: the welcome launchpad on a normal launch. But the headless
        // self-test (TN_AUTOQUIT) + scripted demo (TN_DEMO) drive the *first pane*
        // (TerminalView::new spawns the self-test), so under those a pwsh pane is
        // spawned instead — else there's no pane and the test never runs/quits.
        if std::env::var("TN_AUTOQUIT").is_ok() || std::env::var("TN_DEMO").is_ok() {
            let id = ws.spawn_pane(cx);
            ws.tabs.push(Tab::panes(Node::Leaf(id), id));
        } else {
            ws.tabs.push(Tab::welcome());
        }
        if std::env::var("TN_DEMO").is_ok() {
            Self::spawn_demo(cx);
        }
        // DEBUG(freeze bench): TN_QL_BENCH=<path> opens that file in Quick Look on a
        // real window, then quits after 2.5s. If gpui paint of the overlay hangs,
        // the process hangs (the quit timer can't run on the frozen main thread) —
        // so a `cargo run` with this env reproduces the freeze headlessly-ish here.
        if let Ok(p) = std::env::var("TN_QL_BENCH") {
            let path = std::path::PathBuf::from(&p);
            ws.quick_look.update(cx, |v, _| v.open(path));
            ws.quick_look_open = true;
            tracing::info!(target: "tn::quicklook", path = %p, "bench: opened, will quit in 2.5s");
            let exec = cx.background_executor().clone();
            cx.spawn(async move |_this, cx: &mut AsyncApp| {
                exec.timer(Duration::from_millis(2500)).await;
                tracing::info!(target: "tn::quicklook", "bench: 2.5s elapsed, quitting (paint did NOT hang)");
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        }
        ws
    }

    /// Launch the profile at `index` (welcome tile click) into the active tab,
    /// turning a welcome tab into a pane tree.
    fn launch_in_active_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let spec = self
            .launch_profiles
            .get(index)
            .and_then(LaunchSpec::from_profile)
            .unwrap_or_else(LaunchSpec::pwsh);
        let id = self.spawn_pane_with(cx, spec);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.root = Node::Leaf(id);
            tab.focused = id;
            tab.welcome = false;
        }
    }

    /// Scripted feature demo (`TN_DEMO=1`): steps through prompt → colored output
    /// → split right → split down → scrollback, holding each state ~5s, then
    /// quits. Lets a human watch the M1 features without driving them by hand.
    fn spawn_demo(cx: &mut Context<Self>) {
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let step = Duration::from_secs(5);
            let color = b"$e=[char]27; Write-Host \"$e[1;31m RED $e[32m GREEN $e[33m YELLOW $e[34m BLUE $e[35m MAGENTA $e[36m CYAN $e[0m\"\r";

            // 1) prompt + banner.
            let _ = this.update(cx, |ws, cx| {
                ws.send_to_focused(b"echo '== Tn M1 demo (5s/state): prompt + Tn Dark =='\r", cx)
            });
            exec.timer(step).await;
            // 2) colored ANSI output.
            let _ = this.update(cx, |ws, cx| ws.send_to_focused(color, cx));
            exec.timer(step).await;
            // 3) fill the scrollback with 40 lines.
            let _ = this.update(cx, |ws, cx| {
                ws.send_to_focused(b"1..40 | ForEach-Object { \"scrollback line $_\" }\r", cx)
            });
            exec.timer(step).await;
            // 4) scroll UP into history (verify scrollback rendering).
            let _ = this.update(cx, |ws, cx| ws.demo_on_focused(cx, |tv, cx| tv.demo_scroll(14, cx)));
            exec.timer(step).await;
            // 5) highlight a selection region (verify selection rendering).
            let _ = this.update(cx, |ws, cx| ws.demo_on_focused(cx, |tv, cx| tv.demo_select(cx)));
            exec.timer(step).await;
            // 6) clear selection + back to bottom, then split right.
            let _ = this.update(cx, |ws, cx| {
                ws.demo_on_focused(cx, |tv, cx| tv.demo_reset_view(cx));
                ws.demo_split(Axis::Row, cx);
                ws.send_to_focused(b"echo '== split right :: Ctrl+Shift+D =='\r", cx);
            });
            exec.timer(step).await;
            // 7) split down (three panes).
            let _ = this.update(cx, |ws, cx| {
                ws.demo_split(Axis::Col, cx);
                ws.send_to_focused(b"echo '== split down :: Ctrl+Shift+E =='\r", cx);
            });
            exec.timer(step).await;
            // 8) resize the focused (bottom) pane taller (verify weight resize).
            let _ = this.update(cx, |ws, cx| {
                ws.send_to_focused(b"echo '== resize taller :: Ctrl+Shift+Down =='\r", cx);
                ws.resize_focused(Axis::Col, 1.2, cx);
            });
            exec.timer(step).await;

            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// Run `f` against the active tab's focused pane (demo helper).
    fn demo_on_focused(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TerminalView, &mut Context<TerminalView>),
    ) {
        let fid = self.tabs[self.active].focused;
        if let Some(view) = self.panes.get(&fid).cloned() {
            view.update(cx, |tv, cx| f(tv, cx));
        }
    }

    /// Mouse-move while a divider is held: **only** track the cursor for the
    /// preview line — weights are NOT changed mid-drag. Resizing the panes live
    /// would resize each pane's PTY grid every frame, which makes ConPTY reprint
    /// (history scrolls out of view) and the layout jitter. So the actual resize
    /// is deferred to release (`on_divider_up`); during the drag a thin ghost line
    /// shows where the seam will land.
    fn on_divider_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.divider_drag.as_mut() {
            d.cur_pos = match d.axis {
                Axis::Row => f32::from(ev.position.x),
                Axis::Col => f32::from(ev.position.y),
            };
            cx.notify();
        }
        // Explorer sidebar width drag: live resize as the handle moves.
        if let Some(ref d) = self.explorer_drag {
            let dx = f32::from(ev.position.x) - d.start_x;
            self.explorer_width = (d.start_width + dx).clamp(150.0, 500.0);
            cx.notify();
        }
    }

    /// Mouse-up: commit the divider move — recompute the two adjacent weights
    /// from the drag delta and apply once (a single resize, like keyboard resize).
    fn on_divider_up(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.divider_drag.take() {
            cx.notify();
            let extent = self.split_extents.borrow().get(&d.path).copied().unwrap_or(0.0);
            if extent <= 1.0 {
                return;
            }
            let sum: f32 = d.start_weights.iter().sum::<f32>().max(1.0);
            let pair = d.start_weights[d.gap] + d.start_weights[d.gap + 1];
            let min = 0.08 * sum; // keep both sides usably wide
            if pair <= 2.0 * min {
                return; // too small to redistribute
            }
            // Pixel delta → weight units (weights are relative: 1px = sum/extent units).
            let dw = (d.cur_pos - d.start_pos) / extent * sum;
            let w0 = (d.start_weights[d.gap] + dw).clamp(min, pair - min);
            if let Some(Node::Split { weights, .. }) = self.tabs[self.active].root.at_path_mut(&d.path) {
                if d.gap + 1 < weights.len() {
                    weights[d.gap] = w0;
                    weights[d.gap + 1] = pair - w0;
                }
            }
        }
        // End explorer-width drag (the width is already set live; just clean up).
        if self.explorer_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Resize the focused pane by adjusting its weight along `axis`.
    fn resize_focused(&mut self, axis: Axis, delta: f32, cx: &mut Context<Self>) {
        let active = self.active;
        if self.tabs[active].welcome {
            return; // no panes to resize on the welcome launchpad
        }
        let target = self.tabs[active].focused;
        if self.tabs[active].root.resize(target, axis, delta) {
            cx.notify();
        }
    }

    fn grow_width(&mut self, _: &GrowWidth, _w: &mut Window, cx: &mut Context<Self>) {
        self.resize_focused(Axis::Row, RESIZE_STEP, cx);
    }
    fn shrink_width(&mut self, _: &ShrinkWidth, _w: &mut Window, cx: &mut Context<Self>) {
        self.resize_focused(Axis::Row, -RESIZE_STEP, cx);
    }
    fn grow_height(&mut self, _: &GrowHeight, _w: &mut Window, cx: &mut Context<Self>) {
        self.resize_focused(Axis::Col, RESIZE_STEP, cx);
    }
    fn shrink_height(&mut self, _: &ShrinkHeight, _w: &mut Window, cx: &mut Context<Self>) {
        self.resize_focused(Axis::Col, -RESIZE_STEP, cx);
    }

    /// Send raw bytes to the active tab's focused pane (demo driver).
    fn send_to_focused(&self, bytes: &[u8], cx: &mut Context<Self>) {
        let fid = self.tabs[self.active].focused;
        if let Some(view) = self.panes.get(&fid) {
            view.read(cx).send_bytes(bytes);
        }
    }

    /// Split the focused pane along `axis` without touching GPUI focus (demo).
    fn demo_split(&mut self, axis: Axis, cx: &mut Context<Self>) {
        let new_id = self.spawn_pane(cx);
        let active = self.active;
        let target = self.tabs[active].focused;
        self.tabs[active].root.split(target, new_id, axis, false);
        self.tabs[active].focused = new_id;
        cx.notify();
    }

    fn spawn_pane(&mut self, cx: &mut Context<Self>) -> PaneId {
        self.spawn_pane_with(cx, LaunchSpec::pwsh())
    }

    fn spawn_pane_with(&mut self, cx: &mut Context<Self>, mut launch: LaunchSpec) -> PaneId {
        // Use the active pane's cwd when splitting inside an existing tab, so the
        // new pane opens in the same directory as its sibling. Fall back to the
        // explorer root for the first pane (no sibling to inherit from).
        launch.cwd.get_or_insert_with(|| {
            self.panes
                .get(&self.tabs[self.active].focused)
                .and_then(|v| v.read(cx).effective_cwd().map(std::path::PathBuf::from))
                .unwrap_or_else(|| self.explorer.read(cx).root())
        });
        let config = self.config.clone();
        let view = cx.new(|cx| TerminalView::new(cx, config, launch.clone()));
        // Repaint the status bar when this pane's usage changes (only on change,
        // not on every terminal frame — that's why TerminalView emits an event
        // rather than relying on plain `notify`).
        cx.subscribe(&view, |_ws, _view, _ev: &UsageUpdated, cx| cx.notify())
            .detach();
        // File watcher fired → refresh explorer git tags too.
        cx.subscribe(&view, |ws, _view, _ev: &FilesChanged, cx| {
            ws.explorer.update(cx, |explorer, _cx| explorer.mark_stale());
        })
        .detach();
        // Agent activity-rail card click → open that file in Quick Look (Diff tab).
        // The rail emits an absolute path; Quick Look reads it + shows its git diff.
        cx.subscribe(&view, |ws, _view, ev: &OpenInQuickLook, cx| {
            let path = ev.0.clone();
            ws.quick_look.update(cx, |v, cx| {
                v.open_diff(path);
                cx.notify();
            });
            ws.quick_look_open = true;
            cx.notify();
        })
        .detach();
        let id = self.next_id;
        self.next_id += 1;
        self.panes.insert(id, view);
        self.pane_specs.insert(id, launch);
        id
    }

    /// Send `cd <dir>` to every **plain local shell** pane (`打开文件夹`). Agents
    /// (Claude/Codex), WSL and SSH panes are skipped — they don't take a host `cd`.
    fn cd_shells_to(&self, dir: &std::path::Path, cx: &Context<Self>) {
        let path = dir.to_string_lossy().to_string();
        for (id, view) in &self.panes {
            let Some(spec) = self.pane_specs.get(id) else { continue };
            let prog = spec.program.to_ascii_lowercase();
            let is_plain_shell = spec.agent.is_none()
                && spec.ssh.is_none()
                && (prog.contains("powershell") || prog.contains("pwsh") || prog.contains("cmd"));
            if !is_plain_shell {
                continue;
            }
            // cmd needs `/d` to switch drives; pwsh's `cd`(Set-Location) changes
            // drive on its own. Quote the path for spaces.
            let line = if prog.contains("cmd") {
                format!("cd /d \"{path}\"\r")
            } else {
                format!("cd \"{path}\"\r")
            };
            view.read(cx).send_bytes(line.as_bytes());
        }
    }

    fn focus_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.panes.get(&id) {
            view.read(cx).focus_handle().focus(window);
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focused = id;
        }
        cx.notify();
    }

    // ---- command palette (M4) ----

    /// Open/close the launcher (Ctrl+Shift+P). Open steals key focus; close
    /// returns it to the active pane.
    fn toggle_palette(&mut self, _: &TogglePalette, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = !self.palette_open;
        if self.palette_open {
            self.palette_query.clear();
            self.palette_sel = 0;
            self.palette_wsl = false;
            self.palette_needs_focus = true; // focus in render (see field doc)
        } else {
            self.refocus_active(window, cx);
        }
        cx.notify();
    }

    /// Show/hide the file explorer sidebar (Ctrl+Shift+B).
    fn toggle_explorer(&mut self, _: &ToggleExplorer, _window: &mut Window, cx: &mut Context<Self>) {
        self.explorer_open = !self.explorer_open;
        cx.notify();
    }

    /// Show/hide the Quick Look overlay (Ctrl+Shift+J). On close, return focus to
    /// the active pane (we have a `window` here, so focus directly).
    fn toggle_quick_look(&mut self, _: &ToggleQuickLook, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_look_open = !self.quick_look_open;
        if !self.quick_look_open {
            self.refocus_after_quick_look(window, cx);
        }
        cx.notify();
    }

    /// Refocus the active tab's focused pane (after the palette closes).
    fn refocus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fid = self.tabs[self.active].focused;
        self.focus_pane(fid, window, cx);
    }

    /// Where focus goes after Quick Look closes: back to the **file list** (you
    /// opened the file from there, so Esc returns to browsing it) when the explorer
    /// is open, otherwise the active pane.
    fn refocus_after_quick_look(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.explorer_open {
            self.explorer.read(cx).focus_handle().focus(window);
        } else {
            self.refocus_active(window, cx);
        }
    }

    /// The palette's current rows (aggregated + drill-resolved + query-filtered).
    fn palette_rows(&self) -> Vec<LaunchRow> {
        launch_rows(&self.launch_profiles, self.palette_wsl, &self.palette_query)
    }

    fn palette_match_count(&self) -> usize {
        self.palette_rows().len()
    }

    /// Palette keystrokes: type to filter, ↑↓ to select, Enter activates, Esc backs out
    /// of the WSL sub-list (or closes the palette).
    fn on_palette_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        match ev.keystroke.key.as_str() {
            "escape" => {
                if self.palette_wsl {
                    self.palette_wsl = false; // back to the root list
                    self.palette_query.clear();
                    self.palette_sel = 0;
                    cx.notify();
                } else {
                    self.palette_open = false;
                    self.refocus_active(window, cx);
                    cx.notify();
                }
            }
            "enter" => self.activate_palette_sel(window, cx),
            "backspace" => {
                self.palette_query.pop();
                self.palette_sel = 0;
                cx.notify();
            }
            "up" => {
                self.palette_sel = self.palette_sel.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                let n = self.palette_match_count();
                if n > 0 {
                    self.palette_sel = (self.palette_sel + 1).min(n - 1);
                }
                cx.notify();
            }
            "space" if !m.control && !m.alt => {
                self.palette_query.push(' ');
                self.palette_sel = 0;
                cx.notify();
            }
            k if k.chars().count() == 1 && !m.control && !m.alt => {
                self.palette_query.push_str(k);
                self.palette_sel = 0;
                cx.notify();
            }
            _ => {}
        }
    }

    /// Activate the selected palette row: launch a profile (new tab), drill into the WSL
    /// sub-list (or launch the lone distro), or no-op on the parked SSH placeholder.
    fn activate_palette_sel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.palette_rows();
        let Some(row) = rows.get(self.palette_sel) else { return };
        match *row {
            LaunchRow::Profile(i) => self.launch_profile_in_tab(i, window, cx),
            LaunchRow::DrillWsl => {
                let distros = wsl_distros(&self.launch_profiles);
                if distros.len() == 1 {
                    self.launch_profile_in_tab(distros[0], window, cx);
                } else {
                    self.palette_wsl = true;
                    self.palette_query.clear(); // filter the distros fresh
                    self.palette_sel = 0;
                    cx.notify();
                }
            }
            LaunchRow::SshSoon => {} // parked placeholder — no-op
        }
    }

    /// Launch the profile at `idx` in a new tab, then close the palette.
    fn launch_profile_in_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.launch_profiles.get(idx).and_then(LaunchSpec::from_profile) else {
            return;
        };
        self.palette_open = false;
        self.palette_wsl = false;
        let id = self.spawn_pane_with(cx, spec);
        self.tabs.push(Tab::panes(Node::Leaf(id), id));
        self.active = self.tabs.len() - 1;
        self.focus_pane(id, window, cx);
    }

    fn new_tab(&mut self, _: &NewTab, _window: &mut Window, cx: &mut Context<Self>) {
        // A new tab opens on the welcome launchpad (no pane yet) — a tile click
        // there launches the chosen session into this tab.
        self.tabs.push(Tab::welcome());
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
        let fid = self.tabs[self.active].focused;
        self.focus_pane(fid, window, cx);
    }

    fn activate_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            let fid = self.tabs[i].focused;
            self.focus_pane(fid, window, cx);
        }
    }

    /// Close tab `i` entirely, dropping all its panes (which kills their child
    /// processes via `LocalPty`'s Drop). Never leaves zero tabs — closing the
    /// last one spawns a fresh default pane. Driven by the tab's `×` button.
    fn close_tab_index(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        // A welcome tab has no real panes (dummy root) — don't collect/remove, or
        // we'd drop a real pane sharing the dummy id.
        if !self.tabs[i].welcome {
            let mut leaves = Vec::new();
            collect_leaves(&self.tabs[i].root, &mut leaves);
            for id in leaves {
                self.panes.remove(&id); // drop the view → drop LocalPty → kill child
            }
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            // Never zero tabs: closing the last one falls back to a welcome tab.
            self.tabs.push(Tab::welcome());
            self.active = 0;
        } else {
            self.active = self.active.min(self.tabs.len() - 1);
            let fid = self.tabs[self.active].focused;
            self.focus_pane(fid, window, cx);
        }
        cx.notify();
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs[self.active].welcome {
            return; // nothing to split on the welcome launchpad
        }
        let new_id = self.spawn_pane(cx);
        let active = self.active;
        let target = self.tabs[active].focused;
        self.tabs[active].root.split(target, new_id, axis, false);
        self.focus_pane(new_id, window, cx);
    }

    /// `新会话` split direction (app menu). Maps to a (`Axis`, before?) split.
    fn split_session(&mut self, dir: SplitDir, spec: LaunchSpec, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active;
        let new_id = self.spawn_pane_with(cx, spec);
        if self.tabs[active].welcome {
            // No pane to split on a welcome tab — fill the tab with the session.
            self.tabs[active] = Tab::panes(Node::Leaf(new_id), new_id);
        } else {
            // Prefer the target snapshotted at `新会话` invocation (before the
            // launcher overlay stole focus); fall back to the live `focused` field.
            let target = self.split_target.take().unwrap_or(self.tabs[active].focused);
            let ok = self.tabs[active].root.split(target, new_id, dir.axis(), dir.before());
            if !ok {
                // `target` wasn't in the active tree (stale/dummy id) — splitting it
                // would orphan the new pane. Anchor to the first real leaf instead.
                let fallback = first_leaf(&self.tabs[active].root);
                self.tabs[active].root.split(fallback, new_id, dir.axis(), dir.before());
            }
        }
        self.split_target = None;
        self.focus_pane(new_id, window, cx);
        cx.notify();
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Row, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Col, window, cx);
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active;
        let target = self.tabs[active].focused;
        if self.tabs[active].root.leaf_count() <= 1 {
            // Last pane in the tab: close the whole tab if more than one remains.
            if self.tabs.len() > 1 {
                self.panes.remove(&target);
                self.tabs.remove(active);
                self.active = active.min(self.tabs.len() - 1);
                let fid = self.tabs[self.active].focused;
                self.focus_pane(fid, window, cx);
            }
            return;
        }
        let root = std::mem::replace(&mut self.tabs[active].root, Node::Leaf(0));
        self.tabs[active].root = prune(root, target).expect("tree non-empty");
        self.panes.remove(&target);
        let fid = first_leaf(&self.tabs[active].root);
        self.focus_pane(fid, window, cx);
    }

    fn next_pane(&mut self, _: &NextPane, window: &mut Window, cx: &mut Context<Self>) {
        let mut leaves = Vec::new();
        collect_leaves(&self.tabs[self.active].root, &mut leaves);
        if leaves.len() <= 1 {
            return;
        }
        let cur = self.tabs[self.active].focused;
        let i = leaves.iter().position(|&x| x == cur).unwrap_or(0);
        let next = leaves[(i + 1) % leaves.len()];
        self.focus_pane(next, window, cx);
    }

    /// Re-read config from disk and re-apply theme colors live. Palette + chrome
    /// update on existing panes; font/scrollback take effect on new panes only.
    fn reload_config(&mut self, _: &ReloadConfig, _window: &mut Window, cx: &mut Context<Self>) {
        let loaded = Arc::new(tn_config::load());
        let palette = crate::terminal_view::palette_from(&loaded.theme);
        self.config = loaded;
        let views: Vec<_> = self.panes.values().cloned().collect();
        for view in views {
            view.update(cx, |v, cx| {
                v.apply_palette(palette);
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Root window-surface fill: a mostly-opaque dark glass over the acrylic
    /// backdrop (just a hint of blur shows through), or a fully opaque fill when
    /// the theme requests a `solid` window. Kept high-alpha so a bright desktop
    /// doesn't bleed through the chrome — the layered translucency that reads as
    /// "glass" lives in the inner panels, not the window backdrop.
    fn window_glass(&self) -> Rgba {
        let ui = &self.config.theme.ui;
        match ui.window.backdrop {
            // Only explicit acrylic is see-through; mica/solid are opaque so the
            // desktop never bleeds through the chrome (see lib.rs window_background).
            tn_config::Backdrop::Acrylic => cola(ui.chrome_bg, 0.92),
            _ => col(ui.chrome_bg),
        }
    }

    fn render_node(
        &self,
        node: &Node,
        focused: PaneId,
        cx: &mut Context<Self>,
        path: Vec<usize>,
    ) -> AnyElement {
        match node {
            Node::Leaf(id) => {
                let id = *id;
                let view = self.panes.get(&id).expect("pane exists").clone();
                let is_focused = id == focused;
                // Inner content: g1 glass, rounded ONE px tighter than the outer so
                // the 1px gradient-border ring shows through (see `glass_pane`).
                // The TerminalView fills it + rounds its own corners to match; gpui
                // clips rectangularly so inner surfaces round themselves.
                let inner = div()
                    .size_full()
                    .relative() // anchor the absolute specular layer
                    .rounded(px(R_PANEL - 1.))
                    .overflow_hidden()
                    .bg(pane_fill(self.config.theme.ui.chrome_bg))
                    .child(view);
                // mockup .pane::before 竖向渐变描边(顶冷白承光 → 底 accent 回光,跟圆角)
                // + .pane 浮起投影栈;focused 边更亮、浮得更高(NO 暖橙、NO glow)。
                glass_pane(inner, is_focused, self.config.theme.ui.accent)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            this.focus_pane(id, window, cx);
                        }),
                    )
                    .into_any_element()
            }
            Node::Split {
                axis,
                kids,
                weights,
            } => {
                let row = *axis == Axis::Row;
                let ax = *axis;
                let sum: f32 = weights.iter().sum::<f32>().max(1.0);
                // `.relative()` so the absolutely-positioned dividers + the
                // extent-capture canvas position within this container.
                // No overflow_hidden here: each leaf pane clips its OWN content
                // (+ min_w/min_h 0 below bounds it), so dropping the clip lets the
                // panes' drop shadows bleed past the split seam and "float" — only
                // the window edge (body) clips. Re-adding it re-clips the shadows.
                let mut container = div()
                    .relative()
                    .size_full()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .flex();
                container = if row {
                    container.flex_row()
                } else {
                    container.flex_col()
                };
                let last = kids.len().saturating_sub(1);
                for (i, (kid, w)) in kids.iter().zip(weights.iter()).enumerate() {
                    let frac = w / sum;
                    // min_w/min_h 0 bounds the flex child (prevents the taffy
                    // min-size:auto overflow); the pane clips its own content, so
                    // we DON'T clip here — that would eat the pane's drop shadow.
                    let mut wrap = div().flex_none().min_w(px(0.)).min_h(px(0.));
                    wrap = if row {
                        wrap.h_full().w(relative(frac))
                    } else {
                        wrap.w_full().h(relative(frac))
                    };
                    // 11px gap between split panes (mockup .col gap): pad the inner
                    // side(s) of each wrap so panes pull back ~5.5px from the seam
                    // (the divider handle then sits centered in the gap). Padding is
                    // INSIDE relative(frac) → no overflow, and the wraps still tile
                    // exactly, so the divider seams (relative(cum)) stay accurate.
                    let g = px(5.5);
                    wrap = if row {
                        wrap.when(i > 0, |w| w.pl(g)).when(i < last, |w| w.pr(g))
                    } else {
                        wrap.when(i > 0, |w| w.pt(g)).when(i < last, |w| w.pb(g))
                    };
                    let mut child_path = path.clone();
                    child_path.push(i);
                    container = container.child(wrap.child(self.render_node(kid, focused, cx, child_path)));
                }
                // Capture this split's pixel extent (along its axis) so a divider
                // drag can map pixels → weight (canvas overlays, no mouse handler
                // → transparent to hit-testing).
                let extents = self.split_extents.clone();
                let cap_path = path.clone();
                container = container.child(
                    canvas(
                        move |bounds, _w, _cx| {
                            let ext = if row {
                                f32::from(bounds.size.width)
                            } else {
                                f32::from(bounds.size.height)
                            };
                            extents.borrow_mut().insert(cap_path.clone(), ext);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                );
                // Draggable divider handles at each interior seam: an invisible
                // 8px hit strip that only tints faintly on hover (no persistent
                // line — the panes' own rims already delineate them). Added last
                // so they sit on top of the panes + canvas.
                let accent = self.config.theme.agents.claude;
                let mut cum = 0.0_f32;
                for gap in 0..kids.len().saturating_sub(1) {
                    cum += weights[gap] / sum;
                    let dpath = path.clone();
                    let start_weights = weights.clone();
                    let mut handle = div().absolute();
                    handle = if row {
                        handle.top(px(0.)).bottom(px(0.)).left(relative(cum)).w(px(8.))
                    } else {
                        handle.left(px(0.)).right(px(0.)).top(relative(cum)).h(px(8.))
                    };
                    container = container.child(
                        handle
                            .hover(|s| s.bg(cola(accent, 0.16)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                                    let pos = if row {
                                        f32::from(ev.position.x)
                                    } else {
                                        f32::from(ev.position.y)
                                    };
                                    this.divider_drag = Some(DividerDrag {
                                        path: dpath.clone(),
                                        gap,
                                        axis: ax,
                                        start_weights: start_weights.clone(),
                                        start_pos: pos,
                                        cur_pos: pos,
                                    });
                                    cx.stop_propagation(); // don't focus a pane
                                    cx.notify();
                                }),
                            ),
                    );
                }
                // Live preview: a thin accent line at the seam's target position
                // while this split is being dragged (weights only commit on release).
                if let Some(d) = self.divider_drag.as_ref().filter(|d| d.path == path) {
                    let extent = self.split_extents.borrow().get(&path).copied().unwrap_or(0.0);
                    let seam: f32 = weights[..=d.gap].iter().sum::<f32>() / sum;
                    let delta = if extent > 1.0 { (d.cur_pos - d.start_pos) / extent } else { 0.0 };
                    let at = (seam + delta).clamp(0.02, 0.98);
                    let mut pv = div().absolute();
                    pv = if row {
                        pv.top(px(0.)).bottom(px(0.)).left(relative(at)).w(px(2.))
                    } else {
                        pv.left(px(0.)).right(px(0.)).top(relative(at)).h(px(2.))
                    };
                    container = container.child(pv.bg(cola(accent, 0.6)));
                }
                container.into_any_element()
            }
        }
    }

    /// Accent color for an agent (Claude coral / Codex teal / muted shell).
    fn agent_color(&self, agent: Option<tn_ai::AgentKind>) -> tn_config::Color {
        let t = &self.config.theme;
        match agent {
            Some(tn_ai::AgentKind::ClaudeCode) => t.agents.claude,
            Some(tn_ai::AgentKind::Codex) => t.agents.codex,
            None => t.ui.muted,
        }
    }

    /// Bottom status bar (M4) — the mockup's multi-segment readout: branch ·
    /// sessions · per-agent context % (Claude + Codex) · … · viewer file·lang ·
    /// encoding · theme. The per-agent ctx is aggregated across panes (one
    /// segment per agent kind present); detailed tokens/cost live in the pane's
    /// agent header (R2).
    fn render_status_bar(&self, cx: &Context<Self>) -> gpui::Div {
        let t = &self.config.theme;
        let ui = &t.ui;

        // Aggregate context % per agent kind across all panes.
        let mut claude_pct: Option<u32> = None;
        let mut codex_pct: Option<u32> = None;
        for v in self.panes.values() {
            let v = v.read(cx);
            if let (Some(a), Some(u)) = (v.agent(), v.usage()) {
                let pct = (u.context_frac() * 100.0).round() as u32;
                match a {
                    tn_ai::AgentKind::ClaudeCode => claude_pct.get_or_insert(pct),
                    tn_ai::AgentKind::Codex => codex_pct.get_or_insert(pct),
                };
            }
        }

        // A single segment (icon? + content), and a faint vertical divider.
        let seg = |children: Vec<AnyElement>| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(13.)) // §16 .seg2 padding 0 13
                .h(px(18.))
                .children(children)
        };
        let sep = || div().w(px(1.)).h(px(13.)).flex_none().bg(rgba(DIVIDER));
        let num = |s: String| -> AnyElement {
            // mockup .status .num:weight 640 · fg-dim #A6AFD4(无主题 token → 字面量)
            div()
                .font_weight(gpui::FontWeight(640.))
                .text_color(gpui::rgb(0xA6AFD4))
                .child(SharedString::from(s))
                .into_any_element()
        };

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(30.))
            .px(px(6.)) // §16 .status padding 0 6
            .border_t(px(1.))
            .border_color(rgba(SHEEN)) // box-shadow 0 1px 0 sheen inset → 顶部高光线
            // mockup .status bg:linear-gradient(180, transparent → black .2)
            .bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0x00000000), 0.),
                linear_color_stop(rgba(0x00000033), 1.), // black @ .2
            ))
            .text_size(px(11.))
            .font_weight(gpui::FontWeight(510.)) // §16 .status weight 510
            .text_color(col(ui.muted));

        // branch
        bar = bar.child(seg(vec![
            icon("branch", 13., ui.accent).into_any_element(),
            div().child(SharedString::from(self.branch.clone().unwrap_or_else(|| "—".into()))).into_any_element(),
        ]));
        // sessions (tab count)
        bar = bar.child(sep()).child(seg(vec![
            num(self.tabs.len().to_string()),
            div().child("sessions").into_any_element(),
        ]));
        // per-agent context readouts
        if let Some(p) = claude_pct {
            bar = bar.child(sep()).child(seg(vec![
                icon("spark", 12., t.agents.claude).into_any_element(),
                div().child("ctx").into_any_element(),
                num(format!("{p}%")),
            ]));
        }
        if let Some(p) = codex_pct {
            bar = bar.child(sep()).child(seg(vec![
                icon("spark", 12., t.agents.codex).into_any_element(),
                div().child("ctx").into_any_element(),
                num(format!("{p}%")),
            ]));
        }

        bar = bar.child(div().flex_1());

        // right cluster: quick look file·lang, encoding, theme
        if let Some((name, lang)) = self.quick_look.read(cx).status() {
            bar = bar.child(seg(vec![div()
                .text_color(col(ui.foreground))
                .child(SharedString::from(format!("{name} · {lang}")))
                .into_any_element()]));
            bar = bar.child(sep());
        }
        bar = bar.child(seg(vec![div().child("UTF-8").into_any_element()]));
        bar = bar.child(sep());
        bar = bar.child(seg(vec![div()
            .text_color(col(ui.accent))
            .child(SharedString::from(t.name.clone()))
            .into_any_element()]));
        bar
    }

    /// App menu dropdown (click the Tn brand), or `None` when closed. A click-away
    /// scrim + the `.appmenu` popup hugging the brand (mockup 01-window-chrome.html
    /// `.appmenu`): 会话 / 文件·工作区 / 设置 groups, each item wired to a real
    /// action (in-app actions or gpui OS helpers — see per-item closures).
    fn render_app_menu(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.app_menu_open {
            return None;
        }
        let ui = &self.config.theme.ui;
        let mono = SharedString::from(self.config.font().family.clone());
        let danger_hover = cola(self.config.theme.ansi.red, 0.16);

        // One `.mi` row: icon + label + optional keycap, with a click handler that
        // closes the menu then runs `act`. `danger` = red hover (退出).
        let mi = |icon_name: &'static str,
                  label: &'static str,
                  key: Option<&'static str>,
                  danger: bool,
                  act: Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>| {
            let hover_bg = if danger { danger_hover } else { rgba(HOVER) };
            let fg = if danger { col(self.config.theme.ansi.red) } else { gpui::rgb(0xA6AFD4) }; // .mi = fg-dim
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.)) // §16 .mi gap 10
                .h(px(32.)) // §16 .mi height 32
                .px(px(10.)) // §16 .mi padding 0 10
                .rounded(px(8.)) // §16 .mi radius 8
                .text_size(px(12.5))
                .text_color(fg)
                .hover(move |s| s.bg(hover_bg))
                .child(icon(icon_name, 15., ui.muted)) // §16 .mi .i 15 · muted
                .child(div().child(label))
                .when_some(key, |d, k| {
                    d.child(div().flex_1()).child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(10.))
                            .text_color(gpui::rgb(0x474E72)) // .mi .k = faint
                            .child(k),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, window, cx| {
                        this.app_menu_open = false;
                        act(this, window, cx);
                        cx.notify();
                    }),
                )
        };
        let sep = || div().h(px(1.)).mx(px(8.)).my(px(5.)).bg(rgba(0xffffff0f)); // §16 .sep 白 .06

        // mockup .appmenu: 248px, r-card, g1-ish opaque glass, rim border, deep shadow.
        let popup = shadowed(
            div()
                .absolute()
                .left(px(12.)) // mockup .appmenu left 12
                .top(px(46.)) // just below the 46px titlebar (mockup top 44)
                .w(px(248.)) // §16 .appmenu width 248
                .p(px(6.)) // §16 .appmenu padding 6
                .rounded(px(R_CARD))
                .border_1()
                .border_color(rgba(RIM))
                .bg(pane_fill(ui.chrome_bg)) // opaque deep glass (popup floats over content)
                .child(mi("spark", "新会话…", Some("⌃⇧N"), false, Box::new(|this, w, cx| this.new_session(&NewSession, w, cx))))
                .child(mi("plus", "新标签", Some("⌃⇧T"), false, Box::new(|this, w, cx| this.new_tab(&NewTab, w, cx))))
                .child(sep())
                .child(mi("folder", "打开文件夹…", None, false, Box::new(|this, _w, cx| this.menu_open_folder(cx))))
                .child(mi("max", "布局…", None, false, Box::new(|this, _w, cx| this.open_layout_manager(cx))))
                .child(mi("sidebar", "文件浏览器", Some("⌃⇧B"), false, Box::new(|this, w, cx| this.toggle_explorer(&ToggleExplorer, w, cx))))
                .child(sep())
                // 设置 → open config.toml in our own Quick Look editor (Ctrl+S to save).
                .child(mi("sliders", "设置", None, false, Box::new(|this, _w, cx| {
                    if let Some(p) = tn_config::config_path() {
                        this.quick_look.update(cx, |v, cx| {
                            v.open_for_edit(p);
                            cx.notify();
                        });
                        this.quick_look_open = true;
                    }
                })))
                // 主题 — only one theme for now (the default). A real picker comes
                // when there is more than one theme.
                .child(mi("moon", "主题 · Tn Dark", None, false, Box::new(|_t, _w, _cx| {})))
                // 重载配置 = panic button: reset config files to defaults + reload
                // (destructive). No ⌃⇧R keycap — that shortcut is the non-destructive
                // hot-reload (reads your edited config); this menu item RESETS.
                .child(mi("refresh", "重载配置", None, false, Box::new(|this, w, cx| this.reset_config(w, cx))))
                .child(sep())
                .child(mi("info", "关于 Tn", None, false, Box::new(|_t, _w, cx| {
                    if let Ok(p) = std::env::current_dir() {
                        let readme = p.join("README.md");
                        if readme.exists() { cx.open_with_system(&readme); }
                    }
                })))
                .child(mi("power", "退出", Some("⌃⇧Q"), true, Box::new(|_t, _w, cx| {
                    crate::platform::QUITTING.store(true, std::sync::atomic::Ordering::Release);
                    if let Some(th) = cx.try_global::<crate::TrayHwnd>() {
                        crate::platform::remove_tray_icon(th.0);
                    }
                    cx.quit();
                }))),
            vec![soft_shadow(30.0, 80.0, -24.0, 0.9)], // mockup .appmenu shadow
        );

        // Full-window click-away scrim (transparent): a click outside closes it.
        Some(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        this.app_menu_open = false;
                        cx.notify();
                    }),
                )
                .child(popup),
        )
    }

    /// App menu「打开文件夹」: native folder picker → re-root the explorer tree
    /// **and** `cd` every plain shell pane into the chosen folder.
    fn menu_open_folder(&mut self, cx: &mut Context<Self>) {
        let recv = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let explorer = self.explorer.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = recv.await {
                if let Some(p) = paths.into_iter().next() {
                    let _ = explorer.update(cx, |e, cx| e.set_root(p.clone(), cx));
                    let _ = this.update(cx, |ws, cx| {
                        ws.explorer_open = true;
                        ws.cd_shells_to(&p, cx);
                        for view in ws.panes.values() {
                            view.update(cx, |v, cx| v.set_rail_root(&p, cx));
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// `重载配置`(app menu, panic button): overwrite the on-disk `config.toml` +
    /// `themes/tn-dark.toml` with the built-in defaults, then reload — recovering
    /// from a broken hand-edited config. **Destructive**: discards user edits.
    fn reset_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = tn_config::config_path() {
            if let Err(e) = std::fs::write(&p, tn_config::DEFAULT_CONFIG_TOML) {
                tracing::error!(path = %p.display(), error = %e, "reset_config: write config.toml failed");
            }
        }
        if let Some(dir) = tn_config::themes_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let tp = dir.join("tn-dark.toml");
            if let Err(e) = std::fs::write(&tp, tn_config::TN_DARK_TOML) {
                tracing::error!(path = %tp.display(), error = %e, "reset_config: write theme failed");
            }
        }
        self.reload_config(&ReloadConfig, window, cx);
    }

    /// The command-palette overlay (M4), or `None` when closed: a dim scrim +
    /// a centered Calm Glass panel (query line + launchable profile rows).
    fn render_palette(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.palette_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let mono = SharedString::from(self.config.font().family.clone());
        let rows = self.palette_rows();
        let sel = self.palette_sel.min(rows.len().saturating_sub(1));

        // ── .pinput: leading icon (term, or a clickable ‹ back when drilled into WSL) +
        // query / placeholder + caret (mockup .pinput) ──
        let placeholder = if self.palette_wsl { "WSL 发行版 / 搜索…" } else { "启动会话 / 搜索…" };
        let lead = if self.palette_wsl {
            div()
                .rounded(px(6.))
                .hover(|s| s.bg(rgba(INSET)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        this.palette_wsl = false; // ‹ back to the root list
                        this.palette_query.clear();
                        this.palette_sel = 0;
                        cx.notify();
                    }),
                )
                .child(icon("chev-l", 16., ui.muted))
        } else {
            div().child(icon("term", 16., ui.muted)) // .pinput .i 16 muted
        };
        let input = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.)) // mockup .pinput gap 10
            .px(px(16.)) // mockup .pinput padding 13px 16px
            .py(px(13.))
            .text_size(px(14.)) // mockup .pinput font-size 14
            .child(lead)
            .child(
                // query + caret (AT the insertion point) + placeholder-when-empty, so the
                // caret sits where text goes (start when empty), not floating after the hint.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .when(!self.palette_query.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(col(ui.foreground))
                                .child(SharedString::from(self.palette_query.clone())),
                        )
                    })
                    .child(div().text_color(col(ui.muted)).child(SharedString::from("▏"))) // caret
                    .when(self.palette_query.is_empty(), |d| {
                        d.child(
                            div()
                                .ml(px(2.))
                                .text_color(col(ui.muted))
                                .child(SharedString::from(placeholder)),
                        )
                    }),
            );

        let row_divs = rows.iter().enumerate().map(|(i, row)| {
            let is_sel = i == sel;
            let card = row_card(t, &self.launch_profiles, row); // identity = tiles/.dot
            // Faint mono meta: a profile's command, or the WSL/SSH card's sub-label.
            let meta = match row {
                LaunchRow::Profile(pi) => self.launch_profiles[*pi].command.clone().unwrap_or_default(),
                _ => card.sub.clone(),
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.)) // mockup .prow gap 10
                .px(px(12.)) // mockup .prow padding 9px 12px
                .py(px(9.))
                .rounded(px(9.)) // mockup .prow radius 9
                .when(is_sel, |d| d.bg(rgba(HOVER))) // .prow.sel bg --g3
                .when(!is_sel, |d| d.hover(|s| s.bg(rgba(INSET))))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, w, cx| {
                        this.palette_sel = i;
                        this.activate_palette_sel(w, cx);
                    }),
                )
                .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(col(card.accent))) // .dot 7px
                .child(
                    div()
                        .text_size(px(13.)) // mockup .prow font-size 13
                        // .prow color = fg-dim(#A6AFD4, 无 token) → 选中 fg
                        .text_color(if is_sel { col(ui.foreground) } else { gpui::rgb(0xA6AFD4) })
                        .child(SharedString::from(card.name)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .font_family(mono.clone()) // mockup .meta font mono
                        .text_size(px(11.)) // .meta 11
                        .text_color(gpui::rgb(0x474E72)) // .meta faint(无 token)
                        .child(SharedString::from(meta)),
                )
        });

        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(560.)) // mockup .palette width 560
                .rounded(px(R_PANEL)) // mockup .palette radius --r-pane
                .overflow_hidden()
                .border_1()
                .border_color(rgba(RIM)) // mockup .palette border 1px --rim
                // mockup .palette bg:两停冷调渐变 #1F2335@.92 → #161826@.92(底停无 token)
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(cola(ui.palette_bg, 0.92), 0.),
                    linear_color_stop(rgba(0x161826eb), 1.),
                ))
                .child(input)
                .child(div().h(px(1.)).bg(rgba(0xffffff0f))) // .pinput border-bottom 白 .06
                .child(div().flex().flex_col().p(px(6.)).gap(px(2.)).children(row_divs)), // .plist padding 6
            vec![soft_shadow(40.0, 120.0, -30.0, 0.9)], // mockup .palette box-shadow
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(0x0a0b118c)) // mockup .scrim rgba(10,11,17,.55)(无 token)
                .track_focus(&self.palette_focus)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                    this.on_palette_key(ev, w, cx)
                }))
                .child(div().h(px(110.))) // top spacer (clears the title + tab bar)
                .child(panel),
        )
    }

    /// `新会话` (app menu / Ctrl+Shift+N): open the split launcher at phase 1.
    fn new_session(&mut self, _: &NewSession, _window: &mut Window, cx: &mut Context<Self>) {
        // Snapshot the split target NOW, while the pane the user is on still holds
        // focus — the launcher overlay is about to steal focus, so reading
        // `focused` later (in `split_session`) is fragile.
        self.split_target = Some(self.tabs[self.active].focused);
        self.split_launcher_open = true;
        self.split_dir = None;
        self.split_sel = 0;
        self.split_wsl = false;
        self.split_needs_focus = true;
        cx.notify();
    }

    fn close_split_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split_launcher_open = false;
        self.split_target = None;
        self.refocus_active(window, cx);
        cx.notify();
    }

    /// Split-launcher keyboard: phase 1 (arrow keys pick a split direction) → phase 2
    /// (↑↓ pick a profile, Enter splits + launches it there). Esc backs out / closes.
    fn on_split_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        cx.stop_propagation();
        match self.split_dir {
            None => {
                let dir = match key {
                    "left" => Some(SplitDir::Left),
                    "right" => Some(SplitDir::Right),
                    "up" => Some(SplitDir::Up),
                    "down" => Some(SplitDir::Down),
                    "escape" => return self.close_split_launcher(window, cx),
                    _ => None,
                };
                if let Some(d) = dir {
                    self.split_dir = Some(d);
                    self.split_sel = 0;
                    cx.notify();
                }
            }
            Some(dir) => {
                let n = self.split_rows().len();
                match key {
                    "up" => {
                        self.split_sel = self.split_sel.saturating_sub(1);
                        cx.notify();
                    }
                    "down" => {
                        if n > 0 {
                            self.split_sel = (self.split_sel + 1).min(n - 1);
                        }
                        cx.notify();
                    }
                    "enter" => self.activate_split_sel(dir, window, cx),
                    "escape" => {
                        if self.split_wsl {
                            self.split_wsl = false; // back to phase-2 root
                            self.split_sel = 0;
                        } else {
                            self.split_dir = None; // back to direction picking
                        }
                        cx.notify();
                    }
                    _ => {}
                }
            }
        }
    }

    /// Phase-2 rows: the aggregated launchers (profiles + WSL card + SSH placeholder),
    /// or — when drilled — the WSL distros. No query (split launcher is arrow-driven).
    fn split_rows(&self) -> Vec<LaunchRow> {
        launch_rows(&self.launch_profiles, self.split_wsl, "")
    }

    /// Activate the selected phase-2 row: split + launch a profile, drill into the WSL
    /// distros (or split the lone one), or no-op on the parked SSH placeholder.
    fn activate_split_sel(&mut self, dir: SplitDir, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.split_rows();
        let Some(row) = rows.get(self.split_sel) else { return };
        let launch = |this: &mut Self, idx: usize, window: &mut Window, cx: &mut Context<Self>| {
            if let Some(spec) = this.launch_profiles.get(idx).and_then(LaunchSpec::from_profile) {
                this.split_launcher_open = false;
                this.split_wsl = false;
                this.split_session(dir, spec, window, cx);
            }
        };
        match *row {
            LaunchRow::Profile(i) => launch(self, i, window, cx),
            LaunchRow::DrillWsl => {
                let distros = wsl_distros(&self.launch_profiles);
                if distros.len() == 1 {
                    launch(self, distros[0], window, cx);
                } else {
                    self.split_wsl = true;
                    self.split_sel = 0;
                    cx.notify();
                }
            }
            LaunchRow::SshSoon => {} // parked placeholder — no-op
        }
    }

    /// `新会话` split-launcher overlay (app menu), or `None` when closed. Phase 1 =
    /// a direction cross (←↑↓→ around the focused pane); phase 2 = the profile list.
    /// On launch it splits the focused pane in the chosen direction (vs the command
    /// palette, which opens in a new *tab*).
    fn render_split_launcher(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.split_launcher_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;

        let body = match self.split_dir {
            // ── phase 1: pick the split direction (click a tile / press an arrow) ──
            None => {
                let dir_tile = |d: SplitDir| {
                    let (arrow, label) = d.label();
                    div()
                        .w(px(74.))
                        .h(px(54.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(2.))
                        .rounded(px(R_CARD))
                        .bg(rgba(INSET))
                        .border_1()
                        .border_color(rgba(RIM))
                        .hover(|s| s.bg(rgba(HOVER)).border_color(cola(ui.accent, 0.5)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.split_dir = Some(d);
                                this.split_sel = 0;
                                cx.notify();
                            }),
                        )
                        .child(div().text_size(px(18.)).text_color(col(ui.accent)).child(arrow))
                        .child(div().text_size(px(11.)).text_color(col(ui.muted)).child(label))
                };
                let center = div()
                    .w(px(74.))
                    .h(px(54.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(R_CARD))
                    .bg(cola(ui.accent, 0.10))
                    .border_1()
                    .border_color(cola(ui.accent, 0.3))
                    .child(div().text_size(px(11.)).text_color(col(ui.muted)).child("当前"));
                let spacer = || div().w(px(74.));
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex().flex_row().gap(px(6.)).child(spacer()).child(dir_tile(SplitDir::Up)).child(spacer()))
                    .child(div().flex().flex_row().gap(px(6.)).child(dir_tile(SplitDir::Left)).child(center).child(dir_tile(SplitDir::Right)))
                    .child(div().flex().flex_row().gap(px(6.)).child(spacer()).child(dir_tile(SplitDir::Down)).child(spacer()))
            }
            // ── phase 2: pick the launcher (aggregated: profiles + WSL card + SSH;
            // drilling into WSL swaps in the distros) ──
            Some(dir) => {
                let rows = self.split_rows();
                let sel = self.split_sel.min(rows.len().saturating_sub(1));
                let row_divs = rows.iter().enumerate().map(|(i, row)| {
                    let is_sel = i == sel;
                    let card = row_card(t, &self.launch_profiles, row);
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .rounded(px(R_CARD))
                        .when(is_sel, |d| d.bg(rgba(HOVER)))
                        .when(!is_sel, |d| d.hover(|s| s.bg(rgba(INSET))))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, w, cx| {
                                this.split_sel = i;
                                this.activate_split_sel(dir, w, cx);
                            }),
                        )
                        .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(col(card.accent)))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(col(ui.foreground))
                                .child(SharedString::from(card.name)),
                        )
                });
                div().flex().flex_col().p_1().gap_1().children(row_divs)
            }
        };

        let (title, hint) = if self.split_wsl {
            ("新会话 · 选择 WSL 发行版", "↑↓ 选择 · Enter 启动 · Esc 返回")
        } else {
            match self.split_dir {
                None => ("新会话 · 选择分屏位置", "方向键 / 点击选择 · Esc 取消"),
                Some(d) => match d {
                    SplitDir::Left => ("新会话 · 左侧分屏 · 选择启动器", "↑↓ 选择 · Enter 启动 · Esc 返回"),
                    SplitDir::Right => ("新会话 · 右侧分屏 · 选择启动器", "↑↓ 选择 · Enter 启动 · Esc 返回"),
                    SplitDir::Up => ("新会话 · 上方分屏 · 选择启动器", "↑↓ 选择 · Enter 启动 · Esc 返回"),
                    SplitDir::Down => ("新会话 · 下方分屏 · 选择启动器", "↑↓ 选择 · Enter 启动 · Esc 返回"),
                },
            }
        };

        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(360.))
                .rounded(px(R_WINDOW))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(RIM))
                .bg(cola(ui.palette_bg, 0.86))
                .child(div().h(px(1.)).bg(rgba(SHEEN)))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(div().text_size(px(13.)).text_color(col(ui.foreground)).child(title))
                        .child(div().text_size(px(11.)).text_color(col(ui.muted)).child(hint)),
                )
                .child(div().h(px(1.)).bg(rgba(RIM)))
                .child(div().p_2().child(body)),
            vec![soft_shadow(24.0, 64.0, -36.0, 0.6)],
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(0x0a0b11cc))
                .track_focus(&self.split_focus)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_split_key(ev, w, cx)))
                .child(div().h(px(120.)))
                .child(panel),
        )
    }

    // ───────────────────────────── 布局 (layout slots) ─────────────────────────

    /// Serialize the active tab's pane tree into a layout (`None` if it has no real
    /// panes — e.g. a welcome tab).
    fn tab_to_layout(&self) -> Option<LayoutNode> {
        fn walk(node: &Node, specs: &HashMap<PaneId, LaunchSpec>) -> Option<LayoutNode> {
            match node {
                Node::Leaf(id) => specs.get(id).map(|s| LayoutNode::Pane(LayoutPane::from_spec(s))),
                Node::Split { axis, kids, weights } => {
                    let kids: Vec<_> = kids.iter().filter_map(|k| walk(k, specs)).collect();
                    if kids.is_empty() {
                        return None;
                    }
                    Some(LayoutNode::Split {
                        row: *axis == Axis::Row,
                        kids,
                        weights: weights.clone(),
                    })
                }
            }
        }
        if self.tabs[self.active].welcome {
            return None;
        }
        walk(&self.tabs[self.active].root, &self.pane_specs)
    }

    /// Spawn panes for `ln` and build the matching `Node` tree.
    fn spawn_layout(&mut self, ln: &LayoutNode, cx: &mut Context<Self>) -> Node {
        match ln {
            LayoutNode::Pane(p) => Node::Leaf(self.spawn_pane_with(cx, p.to_spec())),
            LayoutNode::Split { row, kids, weights } => {
                let kids: Vec<Node> = kids.iter().map(|k| self.spawn_layout(k, cx)).collect();
                let weights = if weights.len() == kids.len() {
                    weights.clone()
                } else {
                    vec![1.0; kids.len()]
                };
                Node::Split { axis: if *row { Axis::Row } else { Axis::Col }, kids, weights }
            }
        }
    }

    fn save_layout(&mut self, slot: usize, cx: &mut Context<Self>) {
        if let Some(layout) = self.tab_to_layout() {
            if let Some(s) = self.layouts.slots.get_mut(slot) {
                *s = Some(layout);
            }
            self.layouts.save();
            cx.notify();
        }
    }

    fn delete_layout(&mut self, slot: usize, cx: &mut Context<Self>) {
        if let Some(s) = self.layouts.slots.get_mut(slot) {
            *s = None;
            self.layouts.save();
            cx.notify();
        }
    }

    /// Load a slot into the **active tab** (owner's choice: replace this tab). Kills
    /// the tab's current panes and re-spawns the saved structure.
    fn load_layout(&mut self, slot: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layouts.slots.get(slot).cloned().flatten() else { return };
        let active = self.active;
        let mut old = Vec::new();
        if !self.tabs[active].welcome {
            collect_leaves(&self.tabs[active].root, &mut old);
        }
        let new_root = self.spawn_layout(&layout, cx);
        let first = first_leaf(&new_root);
        for id in old {
            self.panes.remove(&id); // drop view → kill child
            self.pane_specs.remove(&id);
        }
        self.tabs[active] = Tab::panes(new_root, first);
        self.layout_manager_open = false;
        self.focus_pane(first, window, cx);
        cx.notify();
    }

    /// `布局`(app menu): open the slot manager overlay.
    fn open_layout_manager(&mut self, cx: &mut Context<Self>) {
        self.layout_manager_open = true;
        self.layout_needs_focus = true;
        cx.notify();
    }

    /// The 7-slot layout manager overlay (save / load / delete), or `None` when closed.
    fn render_layout_manager(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.layout_manager_open {
            return None;
        }
        let ui = &self.config.theme.ui;
        let can_save = self.tab_to_layout().is_some(); // active tab has real panes

        // A small pill button (label + click action). `accent` = filled/primary look.
        let pill = |label: &'static str, accent: bool, act: Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>| {
            let (fg, bg) = if accent { (col(ui.accent), cola(ui.accent, 0.14)) } else { (gpui::rgb(0xA6AFD4), rgba(INSET)) };
            div()
                .px(px(9.))
                .py(px(3.))
                .rounded(px(7.))
                .text_size(px(11.))
                .text_color(fg)
                .bg(bg)
                .hover(|s| s.bg(rgba(HOVER)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _e, w, cx| act(this, w, cx)))
                .child(label)
        };

        let rows = (0..SLOTS).map(|i| {
            let filled = self.layouts.slots.get(i).and_then(|s| s.as_ref());
            let status = match filled {
                Some(l) => format!("{} 窗格", l.pane_count()),
                None => "空".to_string(),
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .h(px(34.))
                .px(px(10.))
                .rounded(px(8.))
                .hover(|s| s.bg(rgba(INSET)))
                .child(div().w(px(40.)).text_size(px(12.5)).text_color(col(ui.foreground)).child(SharedString::from(format!("槽 {}", i + 1))))
                .child(div().w(px(56.)).text_size(px(11.)).text_color(col(ui.muted)).child(SharedString::from(status)))
                .child(div().flex_1())
                .when(can_save, |d| d.child(pill("保存", true, Box::new(move |this, _w, cx| this.save_layout(i, cx)))))
                .when(filled.is_some(), |d| {
                    d.child(pill("加载", false, Box::new(move |this, w, cx| this.load_layout(i, w, cx))))
                        .child(pill("删除", false, Box::new(move |this, _w, cx| this.delete_layout(i, cx))))
                })
        });

        let hint = if can_save {
            "保存=把当前标签的分屏存入此槽 · 加载=按该布局替换当前标签 · Esc 关闭"
        } else {
            "当前标签无窗格可保存(欢迎页)· 加载=按布局替换当前标签 · Esc 关闭"
        };
        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(380.))
                .rounded(px(R_WINDOW))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(RIM))
                .bg(cola(ui.palette_bg, 0.86))
                .child(div().h(px(1.)).bg(rgba(SHEEN)))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(div().text_size(px(13.)).text_color(col(ui.foreground)).child("布局"))
                        .child(div().text_size(px(10.5)).text_color(col(ui.muted)).child(hint)),
                )
                .child(div().h(px(1.)).bg(rgba(RIM)))
                .child(div().p_1().flex().flex_col().gap(px(1.)).children(rows)),
            vec![soft_shadow(24.0, 64.0, -36.0, 0.6)],
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(0x0a0b11cc))
                .track_focus(&self.layout_focus)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                    if ev.keystroke.key == "escape" {
                        this.layout_manager_open = false;
                        this.refocus_active(w, cx);
                        cx.notify();
                    }
                }))
                .child(div().h(px(120.)))
                .child(panel),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus the initial pane once (no notify -> avoid a render loop).
        if !self.focused_init {
            self.focused_init = true;
            let fid = self.tabs[self.active].focused;
            if let Some(view) = self.panes.get(&fid) {
                view.read(cx).focus_handle().focus(window);
            }
        }
        // Re-park focus when it's orphaned. `workspace_focus` is `track_focus`'d on the
        // ROOT, so `on_focus_out` for it fires exactly when focus leaves *everything*
        // (drops to `None`) — any orphan (overlay close, programmatic blur, a click that
        // blurred without re-anchoring) — at which point we re-grab it so the
        // `key_context("Workspace")` shortcuts (Ctrl+Shift+P …) stay dispatchable. No
        // loop: re-focusing is a focus-IN, not a focus-out; guarded to the active window
        // so Alt-Tab away doesn't yank focus back. Registered once (needs `&mut Window`).
        if self.focus_out_sub.is_none() {
            let anchor = self.workspace_focus.clone();
            self.focus_out_sub = Some(window.on_focus_out(&self.workspace_focus, cx, move |_ev, window, _cx| {
                if window.is_window_active() {
                    anchor.focus(window);
                }
            }));
        }
        // Reveal the window once, after this first frame is built (the window was
        // opened hidden). Reading the HWND here is read-only (safe); the actual
        // `ShowWindow` runs in a spawned task *outside* this update borrow — calling
        // it synchronously inside a gpui callback re-enters the window proc (踩过的坑).
        // A short timer lets the first DX frame present, so no transparent flash.
        if !self.revealed {
            self.revealed = true;
            if std::env::var("TN_AUTOQUIT").is_err() {
                if let Some(h) = crate::platform::hwnd_of(window) {
                    // Route IME-owned keys (VK_PROCESSKEY) to the IME so 中文 composition
                    // edits/commits work (退格删拼音 / 回车提交 / 方向键翻候选). Safe to
                    // call inline — it doesn't re-enter the window proc (see platform.rs).
                    crate::platform::install_ime_keyfix(h);
                    // Set the taskbar / title-bar icon to the Tn brand mark (same icon
                    // used for the tray). WM_SETICON doesn't re-enter the window proc.
                    crate::platform::set_window_icon(h);
                    let exec = cx.background_executor().clone();
                    cx.spawn(async move |_this, _cx| {
                        exec.timer(std::time::Duration::from_millis(40)).await;
                        crate::platform::show(h, true);
                    })
                    .detach();
                }
            }
        }
        // Keep the open overlay focused so its keys (↑↓/Enter/Esc/typing) land on it,
        // not the terminal underneath. Re-asserting **every render while open** (not a
        // one-shot on open) is the fix for "焦点漏给底层 shell": the single `*_needs_focus`
        // grab sometimes failed to land on the first frame (踩过的坑). `focus()` is
        // idempotent — it early-returns when already focused — so this can't loop; the
        // `_needs_focus` flag is still consulted so the very first frame always grabs.
        if self.palette_open && (self.palette_needs_focus || !self.palette_focus.is_focused(window)) {
            self.palette_needs_focus = false;
            self.palette_focus.focus(window);
        }
        if self.split_launcher_open
            && (self.split_needs_focus || !self.split_focus.is_focused(window))
        {
            self.split_needs_focus = false;
            self.split_focus.focus(window);
        }
        if self.layout_manager_open
            && (self.layout_needs_focus || !self.layout_focus.is_focused(window))
        {
            self.layout_needs_focus = false;
            self.layout_focus.focus(window);
        }
        // Quick Look closed via its own keyboard (Esc/Space) — return focus to the
        // file list (or active pane) now (the event callback had no `window`).
        if self.ql_refocus_pane {
            self.ql_refocus_pane = false;
            self.refocus_after_quick_look(window, cx);
        }

        // Time the chrome build (待优化清单 §2.2) when TN_PERF is on. Panes are
        // embedded as entities, so this fires only on the workspace's own
        // notifies (usage/tab/split/focus/palette), not per terminal frame.
        let perf_t0 = self.perf.enabled().then(Instant::now);

        let active = self.active;
        // Make `focused` authoritative from **actual gpui focus**: if any pane in the
        // active tab currently holds keyboard focus, that's the selected pane. Fixes
        // the focus border + `新会话` split target tracking the pane you clicked into
        // (clicking a pane focuses its `track_focus` element; this reflects it back).
        //
        // BUT skip while a focus-stealing overlay is open: the user can't be clicking
        // panes then, and gpui can transiently drop the overlay's focus onto the first
        // leaf (observed in tn.log: a `新会话` launcher session where gpui focus fell to
        // pane 0 while the user was on pane 1) — letting that rewrite `focused` would
        // drift the split target. Freezing `focused` under overlays prevents the drift
        // at its source (the snapshot in `new_session` is the belt; this is suspenders).
        let overlay_focused = self.palette_open
            || self.split_launcher_open
            || self.layout_manager_open
            || self.quick_look_open;
        if !overlay_focused && !self.tabs[active].welcome {
            let mut leaves = Vec::new();
            collect_leaves(&self.tabs[active].root, &mut leaves);
            if let Some(id) = leaves.into_iter().find(|id| {
                self.panes.get(id).is_some_and(|v| v.read(cx).focus_handle().is_focused(window))
            }) {
                self.tabs[active].focused = id;
            } else {
                // No pane holds focus — e.g. the user clicked an empty chrome gap and
                // gpui dropped focus off the pane. Park focus on the workspace body so
                // its `key_context("Workspace")` stays live and `Ctrl+Shift+P` (and
                // friends) keep dispatching — gpui binds actions by focus, not mouse
                // position. Clicking a pane re-focuses it; the focus border stays on the
                // last-focused pane (we don't clear `focused`). BUT don't steal from the
                // explorer: it's also under the Workspace context, so its keyboard nav
                // already keeps shortcuts live — re-parking would break it (它也要焦点).
                let explorer_focused = self.explorer_open
                    && self.explorer.read(cx).focus_handle().is_focused(window);
                if !explorer_focused && !self.workspace_focus.is_focused(window) {
                    self.workspace_focus.focus(window);
                }
            }
        }
        let focused = self.tabs[active].focused;
        let ui = &self.config.theme.ui;

        // Explorer always follows the focused pane's effective cwd, so `cd`
        // (OSC 7) or switching focus to another split pane instantly redirects
        // the file list. Compare before calling follow_root so we only rebuild
        // when the path actually changed (never every frame). follow_root keeps
        // the expansion state, so `cd` into a subdir — or back out — doesn't
        // collapse the tree (子目录保留展开态). Skip welcome tabs (no panes).
        if !self.tabs[active].welcome {
            if let Some(cwd) = self
                .panes
                .get(&focused)
                .and_then(|v| v.read(cx).effective_cwd())
            {
                let new_root = std::path::PathBuf::from(&cwd);
                if self.explorer.read(cx).root() != new_root {
                    self.explorer.update(cx, |e, cx| e.follow_root(new_root, cx));
                }
            }
        }

        // Each tab labels itself with its focused pane's OSC title, falling back
        // to "Term N", and carries that pane's agent for an identity dot.
        // Precomputed so the click closures below own `cx` freely.
        let tab_info: Vec<(String, usize, Option<tn_ai::AgentKind>)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(_i, tab)| {
                if tab.welcome {
                    return ("欢迎".to_string(), 1, None); // launchpad tab
                }
                let pane = self.panes.get(&tab.focused);
                let label = pane
                    .map(|v| truncate_label(&v.read(cx).tab_label(), 24))
                    .unwrap_or_else(|| "shell".into());
                let agent = pane.and_then(|v| v.read(cx).agent());
                (label, tab.root.leaf_count(), agent)
            })
            .collect();

        let tabs = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .children(tab_info.into_iter().enumerate().map(|(i, (label, panes, agent))| {
                let is_active = i == active;
                let dot = self.agent_color(agent);
                // Accent bar/icon color: agent identity, or UI accent for shells.
                let accent_c = if agent.is_some() { dot } else { ui.accent };
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(7.)) // §16 .tab gap:7px(原 gap_2=8)
                    .h(px(34.)) // §16 .tab height:34px(原 py_1 无固定高)
                    .px(px(14.)) // §16 .tab padding:0 14px(原 px_3=12)
                    .rounded_t(px(R_CARD)) // §16 .tab radius:11 11 0 0(仅上,原四角)
                    .text_size(px(12.5)) // §16 .tab font-size:12.5px(原 12.0)
                    .font_weight(gpui::FontWeight(520.)) // §16 .tab font-weight:520(原未设)
                    // Active tab = a glass pill (inset + rim + sheen) with a thin
                    // agent-color accent bar at the top. Inactive sits flat and
                    // lifts a touch on hover. No glow.
                    .when(is_active, |d| {
                        // mockup .tab.active:白色微渐变 .055→.01 + agent 强调条。无 rim 边、无投影。
                        // (mockup 还有 0 1px 0 sheen inset 顶高光,按 owner 要求去掉。)
                        d.text_color(col(ui.foreground))
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(rgba(0xffffff0e), 0.), // .055 → round(.055×255)=14=0x0e
                                linear_color_stop(rgba(0xffffff03), 1.), // .01  → round(.01×255)=3=0x03
                            ))
                            // ::after 强调色条(agent 色),left/right 13,top 0,2px
                            .child(
                                div()
                                    .absolute()
                                    .top(px(0.))
                                    .left(px(13.))
                                    .right(px(13.))
                                    .h(px(2.))
                                    .rounded_full()
                                    .bg(col(accent_c)),
                            )
                    })
                    .when(!is_active, |d| {
                        d.border_1()
                            .border_color(rgba(0x00000000))
                            .text_color(col(ui.tab_inactive_fg))
                            .hover(|s| s.bg(rgba(INSET)))
                    })
                    // Type icon in agent identity color: spark for agents
                    // (Claude coral / Codex teal), terminal glyph (accent) for
                    // a plain shell. See docs/产品设计 §6.2 tab agent accent.
                    .child(if agent.is_some() {
                        icon("spark", 13., dot)
                    } else {
                        icon("term", 13., ui.accent)
                    })
                    .child(label)
                    .when(panes > 1, |d| {
                        d.child(
                            div()
                                .text_size(px(10.0))
                                .text_color(col(ui.muted))
                                .child(format!("\u{2317}{panes}")),
                        )
                    })
                    // Close button: kills the tab's process(es). stop_propagation
                    // so it closes the tab instead of just activating it.
                    .child(
                        div()
                            .ml_1()
                            .px_1()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(rgba(HOVER)))
                            .child(icon("close", 12., ui.muted))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab_index(i, window, cx);
                                }),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            this.activate_tab(i, window, cx);
                        }),
                    )
            }))
            .child(
                div()
                    .w(px(29.)) // §16 .newtab 29×29
                    .h(px(29.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(9.))
                    .hover(|s| s.bg(rgba(INSET)))
                    .child(icon("plus", 15., ui.muted))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, window, cx| this.new_tab(&NewTab, window, cx)),
                    ),
            );

        // Brand: a gradient-ish rounded mark + the product name. Marked as a
        // drag region (the OS moves the window from here via HTCAPTION).
        // Clicking the brand toggles the app menu (it's no longer a drag region —
        // the flexible spacer + tab strip empty space carry window dragging). The
        // caret brightens when open (mockup `.brand.open .caret { opacity: 1 }`).
        let menu_open = self.app_menu_open;
        let brand = div()
            .id("brand")
            .flex()
            .items_center()
            .gap(px(9.)) // mockup .brand gap 9
            .pl_1()
            .pr_2()
            .rounded(px(8.))
            .when(menu_open, |d| d.bg(rgba(INSET)))
            .hover(|s| s.bg(rgba(INSET)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.app_menu_open = !this.app_menu_open;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(21.)) // mockup .brand .mark 21×21
                    .h(px(21.))
                    .rounded(px(7.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(linear_gradient(
                        145.,
                        linear_color_stop(col(ui.accent), 0.),
                        linear_color_stop(col(ui.accent_alt), 1.),
                    ))
                    .child(icon("term", 13., ui.chrome_bg)),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(gpui::FontWeight(680.)) // mockup .name weight 680
                    .text_color(col(ui.foreground))
                    .child("Tn"),
            )
            .child(
                // mockup .brand .caret 13×13 · muted · opacity .55 → 1 when open
                crate::assets::icon("chev-d", 13.).text_color(cola(ui.muted, if menu_open { 1.0 } else { 0.55 })),
            );

        // Window controls: the OS performs the action from the marked region
        // (HTMINBUTTON / HTMAXBUTTON / HTCLOSE) — no click handler needed.
        // mockup .wctl .b.close:hover bg = 红 @ 0.22(原硬编码 0x33=0.2)
        let danger_bg = cola(self.config.theme.ansi.red, 0.22);
        // `.occlude()` (BlockMouse) prevents the root track_focus from intercepting
        // NC mouse-down events and calling prevent_default, which would swallow the
        // OS window command (same pattern as the drag spacer).
        let ctl_btn = |name: &'static str, area: WindowControlArea, danger: bool| {
            div()
                .w(px(35.)) // mockup .wctl .b 35×30
                .h(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(9.)) // mockup .b radius 9
                .hover(move |s| s.bg(if danger { danger_bg } else { rgba(INSET) })) // mockup hover = g2(.04)
                .occlude()
                .window_control_area(area)
                .child(icon(name, 13., ui.muted))
        };
        let controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .child(ctl_btn("min", WindowControlArea::Min, false))
            .child(ctl_btn("max", WindowControlArea::Max, false))
            .child(ctl_btn("close", WindowControlArea::Close, true));

        let titlebar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.)) // mockup .titlebar gap 14
            .h(px(46.))
            .pl(px(16.)) // mockup .titlebar padding-left 16
            .pr(px(10.)) // mockup .titlebar padding-right 10
            // No bottom border: tabs float on the glass; the body separates by
            // spacing, not a hard full-width divider (matches the mockup).
            .child(brand)
            .child(tabs)
            // A flexible draggable spacer fills the gap between tabs and controls.
            // `.occlude()` (BlockMouse) so its mouse-down never reaches the root's
            // focus-on-click (track_focus) — that calls `prevent_default`, which would
            // consume the NC click and kill the OS window drag (踩过的坑). The
            // window_control_area hit-test still sees the occluding hitbox → HTCAPTION.
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .occlude()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(controls);

        // Both side panels are clean panes (mockup .sidebar): no wrapper bars.
        // The explorer toggles via Ctrl+Shift+B; the file view is no longer a
        // docked column but a floating Quick Look overlay (built below) that pops
        // over the terminal hugging the tree's right edge.
        let body = div()
            .flex_1()
            .min_h(px(0.)) // let the flex child be bounded by the window, not its content
            // No overflow_hidden (mockup .work doesn't clip): panes clip their own
            // content + min_h 0 bounds them, so dropping it lets each pane's drop
            // shadow bleed into the gaps and through the translucent status bar —
            // the "float". The OS window is the only hard clip.
            // mockup .work:padding 5px 12px 11px · gap 11(原 p_1/gap_2 偏挤)
            .pt(px(5.))
            .px(px(12.))
            .pb(px(11.))
            .flex()
            .flex_row()
            .gap(px(11.))
            // File explorer sidebar (left column), toggled by Ctrl+Shift+B.
            // Width is adjustable by dragging the right edge (same look-and-feel
            // as split-pane dividers).
            .when(self.explorer_open, |d| {
                let accent = self.config.theme.agents.claude;
                let ew = self.explorer_width;
                d.child(
                    div()
                        .w(px(ew))
                        .flex_none()
                        .min_h(px(0.))
                        .flex()
                        .flex_col()
                        .relative()
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .child(self.explorer.clone()),
                        )
                        // Drag handle on the right edge; sits in the inter-column
                        // gap so it doesn't occlude the tree.
                        .child(
                            div()
                                .absolute()
                                .top(px(0.))
                                .bottom(px(0.))
                                .right(px(-4.)) // spill 4 px into the gap
                                .w(px(8.))
                                .cursor_col_resize()
                                .hover(|s| s.bg(cola(accent, 0.16)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                                        this.explorer_drag = Some(ExplorerDrag {
                                            start_x: f32::from(ev.position.x),
                                            start_width: ew,
                                        });
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                ),
                        ),
                )
            })
            .child(
                // No overflow_hidden: leaf panes clip their own content, so the
                // center column lets their shadows bleed into the surrounding gaps.
                // A welcome (new) tab shows the launchpad instead of a pane tree.
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .child(if self.tabs[active].welcome {
                        self.welcome.clone().into_any_element()
                    } else {
                        self.render_node(&self.tabs[active].root, focused, cx, Vec::new())
                    }),
            );

        // Quick Look 速览浮层:绝对定位浮在工作区之上,贴文件树右缘(explorer 开 → 锚到
        // 它右边;关 → 锚到工作区左缘),仅在装了文件时渲染。它**不占分屏**——飘在终端上,
        // Esc/再按 Ctrl+Shift+J 收起。放在 root 的 body/status 之后 = 画在它们之上。
        let quick_look = (self.quick_look_open && self.quick_look.read(cx).has_file()).then(|| {
            // Anchored to the explorer's right edge (body pad 12 + width + gap).
            let left = if self.explorer_open { self.explorer_width + 20. } else { 40. };
            // Click-away scrim over the **workspace body** (terminal area) — NOT the
            // explorer / titlebar / status bar. A click on the bare terminal used to
            // `focus_pane` and steal focus to the shell mid-edit (the「焦点漏到底层
            // shell / 面板穿透」bug); now it closes the overlay cleanly (`ql_refocus`
            // returns focus to the tree / active pane). Clicking the panel itself is
            // swallowed by its own root (see `quick_look.rs` inner `on_mouse_down`),
            // and the explorer stays clickable (scrim starts at its right edge) so
            // 点树里另一个文件仍能换预览。
            let scrim_left = if self.explorer_open { self.explorer_width + 14. } else { 0. };
            div()
                .absolute()
                .top(px(46.)) // below the titlebar
                .bottom(px(30.)) // above the status bar
                .left(px(scrim_left))
                .right(px(0.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|ws, _e, _w, cx| {
                        ws.quick_look_open = false;
                        ws.ql_refocus_pane = true;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        // 浮起「卡片」(原 top70/bottom60/left/right64,这里换算到 scrim 原点)。
                        .top(px(24.)) // 70 − 46
                        .bottom(px(30.)) // 60 − 30
                        .left(px(left - scrim_left))
                        // Relative right margin (was a fixed `right(64) + max_w(880)`). The
                        // absolute cap froze the width, so on a maximized window the panel
                        // stranded against the left with a big empty right (用户反馈失衡).
                        // ~7% of the body ≈ the default 64px gap, but now scales with the
                        // window so the viewer/editor keeps its proportion at any size.
                        .right(relative(0.07))
                        .child(self.quick_look.clone()),
                )
        });

        let palette = self.render_palette(cx);
        let split_launcher = self.render_split_launcher(cx);
        let layout_manager = self.render_layout_manager(cx);
        let app_menu = self.render_app_menu(cx);

        let root = div()
            // Full-window focus anchor: clicking anywhere (except the panes + the
            // occluded titlebar drag spacer, which handle their own focus) parks focus
            // here, so the `key_context("Workspace")` shortcuts (Ctrl+Shift+P …) always
            // dispatch even when no pane is focused. Safe on the root because the drag
            // spacer is `.occlude()`d — this focus-on-click can't swallow the NC drag.
            .track_focus(&self.workspace_focus)
            .key_context("Workspace")
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::next_pane))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::reload_config))
            .on_action(cx.listener(Self::grow_width))
            .on_action(cx.listener(Self::shrink_width))
            .on_action(cx.listener(Self::grow_height))
            .on_action(cx.listener(Self::shrink_height))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::toggle_explorer))
            .on_action(cx.listener(Self::toggle_quick_look))
            .on_action(cx.listener(Self::new_session))
            .on_action(cx.listener(|_this, _: &Quit, _w, cx| {
                    crate::platform::QUITTING.store(true, std::sync::atomic::Ordering::Release);
                    if let Some(th) = cx.try_global::<crate::TrayHwnd>() {
                        crate::platform::remove_tray_icon(th.0);
                    }
                    cx.quit();
                }))
            // Divider drag: the handle's mouse-down sets `divider_drag`; the move
            // (tracked at the root so it keeps working when the cursor leaves the
            // thin handle) recomputes weights; mouse-up anywhere ends it.
            .on_mouse_move(cx.listener(Self::on_divider_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_divider_up))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            // No rounding here: the fill spans the whole window rect and DWM
            // rounds the actual window corners. Rounding the fill more than DWM's
            // radius left a sliver of bare acrylic (blurred desktop) at the edge.
            // Flat window fill (no full-window gradient): a large translucent
            // gradient over the whole window banded badly at real (large) window
            // sizes — the mockup hides that with a feTurbulence noise dither + a
            // backdrop blur, neither of which we have. Flat reads cleaner here; the
            // depth lives in the inner panels' own gradients + shadows.
            .bg(self.window_glass()) // mostly-opaque dark glass over the acrylic backdrop
            .text_color(col(ui.foreground))
            .font_family(UI_SANS) // UI sans for chrome; panes set mono themselves
            .child(titlebar)
            .child(body)
            .child(self.render_status_bar(cx))
            .when_some(quick_look, |d, q| d.child(q))
            .when_some(palette, |d, p| d.child(p))
            .when_some(split_launcher, |d, s| d.child(s))
            .when_some(layout_manager, |d, l| d.child(l))
            .when_some(app_menu, |d, m| d.child(m));

        // No render-data cache here (tab labels/cwd live in the child panes and
        // change without signalling the workspace, so a cache would risk stale
        // labels for no real gain — the chrome build is cheap + infrequent). We
        // still instrument it so that's verifiable in the field.
        self.perf.record(false, perf_t0.map(|t| t.elapsed()));
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(axis: Axis, kids: Vec<Node>) -> Node {
        let weights = vec![1.0; kids.len()];
        Node::Split { axis, kids, weights }
    }

    #[test]
    fn resize_adjusts_matching_axis_only() {
        let mut n = split(Axis::Row, vec![Node::Leaf(0), Node::Leaf(1)]);
        assert!(n.resize(1, Axis::Row, 0.5));
        let Node::Split { weights, .. } = &n else { panic!() };
        assert_eq!(weights[1], 1.5);
        assert_eq!(weights[0], 1.0);
        // Wrong axis is a no-op.
        assert!(!n.resize(1, Axis::Col, 0.5));
    }

    #[test]
    fn resize_targets_innermost_split() {
        let inner = split(Axis::Row, vec![Node::Leaf(1), Node::Leaf(2)]);
        let mut n = split(Axis::Row, vec![Node::Leaf(0), inner]);
        assert!(n.resize(2, Axis::Row, 0.3));
        let Node::Split { weights, kids, .. } = &n else { panic!() };
        assert_eq!(weights, &vec![1.0, 1.0]); // outer untouched
        let Node::Split { weights: iw, .. } = &kids[1] else { panic!() };
        assert!((iw[1] - 1.3).abs() < 1e-6); // inner pane grew
    }

    #[test]
    fn split_before_inserts_left_or_after_inserts_right() {
        // `新会话` split direction: before=false (right/down) → new pane AFTER target;
        // before=true (left/up) → new pane BEFORE target.
        let mut n = Node::Leaf(0);
        assert!(n.split(0, 1, Axis::Row, false)); // split right
        let Node::Split { kids, .. } = &n else { panic!() };
        assert!(matches!((&kids[0], &kids[1]), (Node::Leaf(0), Node::Leaf(1))), "right → [0,1]");

        let mut n = Node::Leaf(0);
        assert!(n.split(0, 1, Axis::Row, true)); // split left
        let Node::Split { kids, .. } = &n else { panic!() };
        assert!(matches!((&kids[0], &kids[1]), (Node::Leaf(1), Node::Leaf(0))), "left → [1,0]");

        // Aligned n-ary insert respects before/after position.
        let mut n = split(Axis::Row, vec![Node::Leaf(0), Node::Leaf(1)]);
        assert!(n.split(1, 2, Axis::Row, true)); // insert 2 before pane 1
        let Node::Split { kids, .. } = &n else { panic!() };
        let ids: Vec<_> = kids.iter().map(|k| matches!(k, Node::Leaf(_)).then(|| if let Node::Leaf(i) = k { *i } else { 0 }).unwrap()).collect();
        assert_eq!(ids, vec![0, 2, 1], "before pane 1 → [0,2,1]");

        // SplitDir mapping
        assert_eq!(SplitDir::Right.axis(), Axis::Row);
        assert_eq!(SplitDir::Down.axis(), Axis::Col);
        assert!(SplitDir::Left.before() && SplitDir::Up.before());
        assert!(!SplitDir::Right.before() && !SplitDir::Down.before());
    }

    #[test]
    fn resize_clamps_to_minimum() {
        let mut n = split(Axis::Col, vec![Node::Leaf(0), Node::Leaf(1)]);
        for _ in 0..20 {
            n.resize(0, Axis::Col, -0.2);
        }
        let Node::Split { weights, .. } = &n else { panic!() };
        assert!(weights[0] >= 0.1 - 1e-6);
    }

    #[test]
    fn at_path_mut_navigates_to_nested_split() {
        // root[Row]: Leaf(0), inner[Col]: Leaf(1), Leaf(2)
        let inner = split(Axis::Col, vec![Node::Leaf(1), Node::Leaf(2)]);
        let mut n = split(Axis::Row, vec![Node::Leaf(0), inner]);
        // [] = root split (Row); [1] = the inner split (Col); [0] = a leaf.
        assert!(matches!(n.at_path_mut(&[]), Some(Node::Split { axis: Axis::Row, .. })));
        assert!(matches!(n.at_path_mut(&[1]), Some(Node::Split { axis: Axis::Col, .. })));
        assert!(matches!(n.at_path_mut(&[0]), Some(Node::Leaf(0))));
        // A divider drag sets the inner split's weights via this path.
        if let Some(Node::Split { weights, .. }) = n.at_path_mut(&[1]) {
            weights[0] = 2.0;
            weights[1] = 0.5;
        }
        let Node::Split { kids, .. } = &n else { panic!() };
        let Node::Split { weights: iw, .. } = &kids[1] else { panic!() };
        assert_eq!(iw, &vec![2.0, 0.5]);
        // Out-of-range / through-a-leaf paths are None.
        assert!(n.at_path_mut(&[9]).is_none());
        assert!(n.at_path_mut(&[0, 0]).is_none());
    }
}
