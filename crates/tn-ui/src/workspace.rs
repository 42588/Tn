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
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Rgba, SharedString, WeakEntity,
    Window, WindowControlArea,
};
use tn_config::Loaded;

use crate::explorer::{ExplorerView, OpenFile};
use crate::perf::PerfStats;
use crate::quick_look::QuickLook;
use crate::terminal_view::{LaunchSpec, TerminalView, UsageUpdated};
use crate::welcome::{LaunchRequested, WelcomeView};

type PaneId = u64;

// Calm Glass tokens + helpers (col/cola/soft_shadow/shadowed/icon/UI_SANS/radii)
// now live in `crate::style` — single source of truth (待优化清单 §4.1).
use crate::style::{
    col, cola, glass_pane, icon, pane_fill, shadowed, soft_shadow, specular_top, DIVIDER, HOVER,
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
        Err(e) => {
            static WARN: std::sync::Once = std::sync::Once::new();
            WARN.call_once(|| tracing::warn!(error = %e, "git unavailable; status bar branch disabled"));
            return None;
        }
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
            accent: Some(tn_config::Color::new(0x7A, 0xA2, 0xF7)), // a soft blue for WSL
            glyph: None,
        });
    }
    profiles
}

/// Launchable profiles matching the query (case-insensitive substring on name).
fn launchable_matches<'a>(
    profiles: &'a [tn_config::Profile],
    query: &str,
) -> Vec<&'a tn_config::Profile> {
    let q = query.to_ascii_lowercase();
    profiles
        .iter()
        .filter(|p| is_launchable(p))
        .filter(|p| q.is_empty() || p.name.to_ascii_lowercase().contains(&q))
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Row, // children side by side (vertical dividers)
    Col, // children stacked (horizontal dividers)
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
    fn split(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let old = Node::Leaf(*id);
                *self = Node::Split {
                    axis,
                    kids: vec![old, Node::Leaf(new)],
                    weights: vec![1.0, 1.0],
                };
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
                        kids.insert(pos + 1, Node::Leaf(new));
                        weights.insert(pos + 1, 1.0);
                        return true;
                    }
                }
                kids.iter_mut().any(|k| k.split(target, new, axis))
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
        ToggleQuickLook
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
        match binding_for(&kb.keys, command) {
            Some(b) => binds.push(b),
            None => tracing::warn!(keys = %kb.keys, id = %kb.id, "unknown keybinding action; skipped"),
        }
    }
    cx.bind_keys(binds);
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    panes: HashMap<PaneId, Entity<TerminalView>>,
    next_id: PaneId,
    focused_init: bool,
    /// The main window opens hidden; revealed after the first frame paints (avoids
    /// the pre-paint transparent flash). Tracks the one-shot reveal.
    revealed: bool,
    config: Arc<Loaded>,
    /// File explorer sidebar (left column) + whether it's shown.
    explorer: Entity<ExplorerView>,
    explorer_open: bool,
    /// Quick Look 速览浮层(贴树右缘、浮于终端之上)+ whether it's shown
    /// (auto-opens on clicking a file in the explorer; only rendered when it
    /// actually has a file loaded).
    quick_look: Entity<QuickLook>,
    quick_look_open: bool,
    /// Welcome launchpad shown as a new tab's content (until a tile is clicked).
    /// One shared entity (stateless chrome); its `LaunchRequested` launches into
    /// the active tab.
    welcome: Entity<WelcomeView>,
    /// Current git branch of the app cwd (status bar), resolved at startup.
    branch: Option<String>,
    /// Command palette (Ctrl+Shift+P) state.
    palette_open: bool,
    palette_query: String,
    palette_sel: usize,
    palette_focus: FocusHandle,
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
        // Clicking a file in the explorer pops the Quick Look overlay for it.
        cx.subscribe(&explorer, |ws, _explorer, ev: &OpenFile, cx| {
            let path = ev.0.clone();
            ws.quick_look.update(cx, |v, _| v.open(path));
            ws.quick_look_open = true;
            cx.notify();
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
            next_id: 0,
            focused_init: false,
            revealed: false,
            config,
            explorer,
            explorer_open: true,
            quick_look,
            quick_look_open: false,
            welcome,
            branch: git_branch(),
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            palette_focus: cx.focus_handle(),
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
    }

    /// Mouse-up: commit the divider move — recompute the two adjacent weights
    /// from the drag delta and apply once (a single resize, like keyboard resize).
    fn on_divider_up(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.divider_drag.take() else { return };
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
        self.tabs[active].root.split(target, new_id, axis);
        self.tabs[active].focused = new_id;
        cx.notify();
    }

    fn spawn_pane(&mut self, cx: &mut Context<Self>) -> PaneId {
        self.spawn_pane_with(cx, LaunchSpec::pwsh())
    }

    fn spawn_pane_with(&mut self, cx: &mut Context<Self>, launch: LaunchSpec) -> PaneId {
        let config = self.config.clone();
        let view = cx.new(|cx| TerminalView::new(cx, config, launch));
        // Repaint the status bar when this pane's usage changes (only on change,
        // not on every terminal frame — that's why TerminalView emits an event
        // rather than relying on plain `notify`).
        cx.subscribe(&view, |_ws, _view, _ev: &UsageUpdated, cx| cx.notify())
            .detach();
        let id = self.next_id;
        self.next_id += 1;
        self.panes.insert(id, view);
        id
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

    /// Show/hide the Quick Look overlay (Ctrl+Shift+J).
    fn toggle_quick_look(&mut self, _: &ToggleQuickLook, _window: &mut Window, cx: &mut Context<Self>) {
        self.quick_look_open = !self.quick_look_open;
        cx.notify();
    }

    /// Refocus the active tab's focused pane (after the palette closes).
    fn refocus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fid = self.tabs[self.active].focused;
        self.focus_pane(fid, window, cx);
    }

    fn palette_match_count(&self) -> usize {
        launchable_matches(&self.launch_profiles, &self.palette_query).len()
    }

    /// Palette keystrokes: type to filter, ↑↓ to select, Enter launches, Esc closes.
    fn on_palette_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        match ev.keystroke.key.as_str() {
            "escape" => {
                self.palette_open = false;
                self.refocus_active(window, cx);
                cx.notify();
            }
            "enter" => self.launch_selected(window, cx),
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

    /// Launch the selected profile in a new tab, then close the palette.
    fn launch_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = {
            let matches = launchable_matches(&self.launch_profiles, &self.palette_query);
            matches
                .get(self.palette_sel)
                .and_then(|p| LaunchSpec::from_profile(p))
        };
        let Some(spec) = spec else { return };
        self.palette_open = false;
        let id = self.spawn_pane_with(cx, spec);
        self.tabs.push(Tab::panes(Node::Leaf(id), id));
        self.active = self.tabs.len() - 1;
        self.focus_pane(id, window, cx);
    }

    /// Launch the profile at `idx` (mouse click on a palette row).
    fn launch_index(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_sel = idx;
        self.launch_selected(window, cx);
    }

    fn new_tab(&mut self, _: &NewTab, _window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("ACTION new_tab");
        // A new tab opens on the welcome launchpad (no pane yet) — a tile click
        // there launches the chosen session into this tab.
        self.tabs.push(Tab::welcome());
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("ACTION next_tab");
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
        tracing::info!("ACTION split {}", if axis == Axis::Row { "right" } else { "down" });
        if self.tabs[self.active].welcome {
            return; // nothing to split on the welcome launchpad
        }
        let new_id = self.spawn_pane(cx);
        let active = self.active;
        let target = self.tabs[active].focused;
        self.tabs[active].root.split(target, new_id, axis);
        self.focus_pane(new_id, window, cx);
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Row, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Col, window, cx);
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("ACTION close_pane");
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
        tracing::info!("ACTION next_pane");
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
        tracing::info!(theme = %loaded.theme.name, "reloaded config");
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
                    // mockup .pane specular 柔光洗(折射,无 glow);先于 view 添加 → 画在
                    // 内容之下,经半透明 header 透出。
                    .child(specular_top())
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

    /// The command-palette overlay (M4), or `None` when closed: a dim scrim +
    /// a centered Calm Glass panel (query line + launchable profile rows).
    fn render_palette(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.palette_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let matches = launchable_matches(&self.launch_profiles, &self.palette_query);
        let sel = self.palette_sel.min(matches.len().saturating_sub(1));

        let query_line = div().px_3().py_2().text_size(px(13.)).child(if self.palette_query.is_empty() {
            div()
                .text_color(col(ui.muted))
                .child(SharedString::from("启动会话 / 搜索…   ↑↓ 选择 · Enter 启动 · Esc 关闭"))
        } else {
            div()
                .text_color(col(ui.foreground))
                .child(SharedString::from(self.palette_query.clone()))
        });

        let rows = matches.iter().enumerate().map(|(i, p)| {
            let is_sel = i == sel;
            let dot = p.accent.unwrap_or(t.agents.claude);
            let hint = p.command.clone().unwrap_or_default();
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
                    cx.listener(move |this, _e, w, cx| this.launch_index(i, w, cx)),
                )
                .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(col(dot)))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(col(ui.foreground))
                        .child(SharedString::from(p.name.clone())),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(col(ui.muted))
                        .child(SharedString::from(hint)),
                )
        });

        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(540.))
                .rounded(px(R_WINDOW))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(RIM))
                .bg(cola(ui.palette_bg, 0.86)) // frosted panel over the scrim + acrylic
                .child(div().h(px(1.)).bg(rgba(SHEEN))) // top mirror highlight
                .child(query_line)
                .child(div().h(px(1.)).bg(rgba(RIM)))
                .child(div().flex().flex_col().p_1().gap_1().children(rows)),
            vec![soft_shadow(24.0, 64.0, -36.0, 0.6)], // floats above the workspace
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(0x0a0b11cc))
                .track_focus(&self.palette_focus)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                    this.on_palette_key(ev, w, cx)
                }))
                .child(div().h(px(110.))) // top spacer (centers the panel below the tab bar)
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
        // Reveal the window once, after this first frame is built (the window was
        // opened hidden). Reading the HWND here is read-only (safe); the actual
        // `ShowWindow` runs in a spawned task *outside* this update borrow — calling
        // it synchronously inside a gpui callback re-enters the window proc (踩过的坑).
        // A short timer lets the first DX frame present, so no transparent flash.
        if !self.revealed {
            self.revealed = true;
            if std::env::var("TN_AUTOQUIT").is_err() {
                if let Some(h) = crate::platform::hwnd_of(window) {
                    let exec = cx.background_executor().clone();
                    cx.spawn(async move |_this, _cx| {
                        exec.timer(std::time::Duration::from_millis(40)).await;
                        crate::platform::show(h, true);
                    })
                    .detach();
                }
            }
        }
        // Focus the palette overlay here (its track_focus element exists this
        // frame), so ↑↓/Enter/Esc reach it instead of the terminal underneath.
        if self.palette_open && self.palette_needs_focus {
            self.palette_needs_focus = false;
            self.palette_focus.focus(window);
        }

        // Time the chrome build (待优化清单 §2.2) when TN_PERF is on. Panes are
        // embedded as entities, so this fires only on the workspace's own
        // notifies (usage/tab/split/focus/palette), not per terminal frame.
        let perf_t0 = self.perf.enabled().then(Instant::now);

        let active = self.active;
        let focused = self.tabs[active].focused;
        let ui = &self.config.theme.ui;

        // Each tab labels itself with its focused pane's OSC title, falling back
        // to "Term N", and carries that pane's agent for an identity dot.
        // Precomputed so the click closures below own `cx` freely.
        let tab_info: Vec<(String, usize, Option<tn_ai::AgentKind>, Option<String>)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(_i, tab)| {
                if tab.welcome {
                    return ("欢迎".to_string(), 1, None, None); // launchpad tab
                }
                let pane = self.panes.get(&tab.focused);
                let label = pane
                    .map(|v| truncate_label(&v.read(cx).tab_label(), 24))
                    .unwrap_or_else(|| "shell".into());
                let agent = pane.and_then(|v| v.read(cx).agent());
                let cwd = pane.and_then(|v| v.read(cx).cwd());
                (label, tab.root.leaf_count(), agent, cwd)
            })
            .collect();

        let tabs = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .children(tab_info.into_iter().enumerate().map(|(i, (label, panes, agent, cwd))| {
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
                    // cwd path badge on the active tab (mockup's "~/proj/tn").
                    .when(is_active && cwd.is_some(), |d| {
                        d.child(
                            // mockup .tab .badge: 11px · faint #474E72 · mono · weight 400
                            div()
                                .text_size(px(11.))
                                .font_family("Cascadia Code")
                                .font_weight(gpui::FontWeight(400.))
                                .text_color(gpui::rgb(0x474E72)) // --faint(无主题 token)
                                .child(SharedString::from(short_cwd(cwd.as_deref().unwrap_or("")))),
                        )
                    })
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
        let brand = div()
            .flex()
            .items_center()
            .gap(px(9.)) // mockup .brand gap 9
            .pl_1()
            .pr_2()
            .window_control_area(WindowControlArea::Drag)
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
                // mockup .brand .caret 13×13 · muted · opacity .55(视觉先行;点击展开 app 菜单 = 后续 ④)
                crate::assets::icon("chev-d", 13.).text_color(cola(ui.muted, 0.55)),
            );

        // Window controls: the OS performs the action from the marked region
        // (HTMINBUTTON / HTMAXBUTTON / HTCLOSE) — no click handler needed.
        // mockup .wctl .b.close:hover bg = 红 @ 0.22(原硬编码 0x33=0.2)
        let danger_bg = cola(self.config.theme.ansi.red, 0.22);
        let ctl_btn = |name: &'static str, area: WindowControlArea, danger: bool| {
            div()
                .w(px(35.)) // mockup .wctl .b 35×30
                .h(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(9.)) // mockup .b radius 9
                .hover(move |s| s.bg(if danger { danger_bg } else { rgba(INSET) })) // mockup hover = g2(.04)
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
            .child(div().flex_1().h_full().window_control_area(WindowControlArea::Drag))
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
            .when(self.explorer_open, |d| {
                d.child(
                    // mockup .sidebar:flex 0 0 224px —— 干净面板,无外层「资源管理器」标签栏。
                    // No overflow_hidden: the explorer pane clips its own content
                    // (+ min_h 0 bounds it), so the column passes the pane's drop
                    // shadow through to float in the gap. (See render_node.)
                    div()
                        .w(px(224.))
                        .flex_none()
                        .min_h(px(0.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .child(self.explorer.clone()),
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
            let left = if self.explorer_open { 244. } else { 40. };
            div()
                .absolute()
                // 留足四边边距 = 浮起的「卡片」而非铺满浮层;`max_w` 在宽屏下封顶,
                // 贴树左缘锚定不被拉得过宽(原型那种比例)。
                .top(px(70.)) // 标题栏 46 之下,留白
                .bottom(px(60.)) // 状态栏 30 之上,留白
                .left(px(left)) // explorer 右缘(12 + 224 + 8)/ 工作区左缘
                .right(px(64.))
                .max_w(px(880.))
                .child(self.quick_look.clone())
        });

        let palette = self.render_palette(cx);

        let root = div()
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
            .when_some(palette, |d, p| d.child(p));

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
