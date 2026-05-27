//! Workspace: multiple tabs, each an n-ary pane tree of [`TerminalView`]s.
//!
//! Splitting uses an n-ary container tree (not a binary tree): splitting along
//! the same axis as the focused pane's parent inserts an aligned sibling;
//! splitting along the other axis nests a new container. This matches the
//! flexible-tiling model in docs/UX-DESIGN.md. Divider-drag and drag-dock are
//! later refinements; this cut gives tabs + keyboard splits + click-to-focus.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    actions, div, hsla, linear_color_stop, linear_gradient, point, prelude::*, px, relative, rgb,
    rgba, AnyElement, App, AppContext, AsyncApp, BoxShadow, Context, Div, Entity, FocusHandle,
    KeyBinding, KeyDownEvent, MouseButton, Rgba, SharedString, WeakEntity, Window, WindowControlArea,
};
use tn_config::Loaded;

use crate::explorer::{ExplorerView, OpenFile};
use crate::terminal_view::{LaunchSpec, TerminalView, UsageUpdated};
use crate::viewer::ViewerView;

type PaneId = u64;

/// Convert a `tn-config` chrome color to a GPUI color.
fn col(c: tn_config::Color) -> Rgba {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
}

/// A chrome color with explicit alpha. Calm Glass surfaces are translucent so
/// the acrylic-blurred backdrop (the window material) shows through, instead of
/// being filled with an opaque color. See docs/UX-DESIGN §6.1.
fn cola(c: tn_config::Color, a: f32) -> Rgba {
    Rgba { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a }
}

// Calm Glass white-on-glass overlay tokens (alpha-only — depth from layered
// translucency + a top mirror highlight, never from glow). docs/UX-DESIGN §6.1.
const RIM: u32 = 0xffffff12; // glass edge (~white .07) — replaces hard borders
const SHEEN: u32 = 0xffffff1a; // top 1px mirror highlight (~white .10)
const INSET: u32 = 0xffffff0a; // header / inset card overlay (~white .04)
const HOVER: u32 = 0xffffff14; // chip / hover (~white .08)
/// UI sans-serif font for chrome (tabs / headers / status / numbers) — the
/// mockup pairs this with the mono terminal/code font. "Segoe UI Variable" /
/// "Segoe UI" ship on Windows 10/11. docs/UX-DESIGN §6.1.
pub(crate) const UI_SANS: &str = "Segoe UI";

// Calm Glass corner radii (px): window 16, panel 14, card 11. docs/UX-DESIGN §6.1.
const R_WINDOW: f32 = 16.0;
const R_PANEL: f32 = 14.0;
const R_CARD: f32 = 11.0;

/// A soft, contained drop shadow (depth without glow — Calm Glass). A negative
/// spread keeps it tucked under the element rather than blooming outward.
fn soft_shadow(y: f32, blur: f32, spread: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: hsla(0., 0., 0., alpha),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}

/// Attach box shadows to a div (gpui 0.2.2 has no fluent `.shadow_*` helper).
fn shadowed(mut d: Div, shadows: Vec<BoxShadow>) -> Div {
    d.style().box_shadow = Some(shadows);
    d
}

/// A Calm Glass line icon, sized square and tinted `color`. (gpui paints an SVG
/// only when a text color is set, so the tint is always explicit — see
/// `assets.rs`.)
fn icon(name: &str, size: f32, color: tn_config::Color) -> gpui::Svg {
    gpui::svg()
        .path(crate::assets::icon_path(name))
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(col(color))
}

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

/// Short cwd for the tab badge: the last two path components (`proj/tn`).
fn short_cwd(p: &str) -> String {
    let p = p.trim().replace('\\', "/");
    let parts: Vec<&str> = p.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    match parts.len() {
        0 => p,
        1 => parts[0].to_string(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// The current git branch of the app's cwd, if it's a repo (for the status bar).
fn git_branch() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .arg("branch")
        .arg("--show-current")
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Profiles launchable now (carry a command = shell / agent) that match the
/// query (case-insensitive substring on the name). WSL/SSH (no command) are M2.
fn launchable_matches<'a>(
    profiles: &'a [tn_config::Profile],
    query: &str,
) -> Vec<&'a tn_config::Profile> {
    let q = query.to_ascii_lowercase();
    profiles
        .iter()
        .filter(|p| p.command.is_some())
        .filter(|p| q.is_empty() || p.name.to_ascii_lowercase().contains(&q))
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Row, // children side by side (vertical dividers)
    Col, // children stacked (horizontal dividers)
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
        ToggleViewer
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
        KeyBinding::new("ctrl-shift-j", ToggleViewer, ctx),
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
        "toggle_viewer" | "viewer" => KeyBinding::new(keys, ToggleViewer, ctx),
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
    config: Arc<Loaded>,
    /// File explorer sidebar (left column) + whether it's shown.
    explorer: Entity<ExplorerView>,
    explorer_open: bool,
    /// File/diff viewer (right column) + whether it's shown (auto-opens on
    /// clicking a file in the explorer).
    viewer: Entity<ViewerView>,
    viewer_open: bool,
    /// Current git branch of the app cwd (status bar), resolved at startup.
    branch: Option<String>,
    /// Command palette (Ctrl+Shift+P) state.
    palette_open: bool,
    palette_query: String,
    palette_sel: usize,
    palette_focus: FocusHandle,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let explorer = cx.new(|cx| ExplorerView::new(cx, config.clone()));
        let viewer = cx.new(|cx| ViewerView::new(cx, config.clone()));
        // Clicking a file in the explorer opens it in the viewer (auto-showing it).
        cx.subscribe(&explorer, |ws, _explorer, ev: &OpenFile, cx| {
            let path = ev.0.clone();
            ws.viewer.update(cx, |v, _| v.open(path));
            ws.viewer_open = true;
            cx.notify();
        })
        .detach();
        let mut ws = Self {
            tabs: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_id: 0,
            focused_init: false,
            config,
            explorer,
            explorer_open: true,
            viewer,
            viewer_open: false,
            branch: git_branch(),
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            palette_focus: cx.focus_handle(),
        };
        let id = ws.spawn_pane(cx);
        ws.tabs.push(Tab {
            root: Node::Leaf(id),
            focused: id,
        });
        if std::env::var("TN_DEMO").is_ok() {
            Self::spawn_demo(cx);
        }
        ws
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

    /// Resize the focused pane by adjusting its weight along `axis`.
    fn resize_focused(&mut self, axis: Axis, delta: f32, cx: &mut Context<Self>) {
        let active = self.active;
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
            self.palette_focus.focus(window);
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

    /// Show/hide the file/diff viewer (Ctrl+Shift+J).
    fn toggle_viewer(&mut self, _: &ToggleViewer, _window: &mut Window, cx: &mut Context<Self>) {
        self.viewer_open = !self.viewer_open;
        cx.notify();
    }

    /// Refocus the active tab's focused pane (after the palette closes).
    fn refocus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fid = self.tabs[self.active].focused;
        self.focus_pane(fid, window, cx);
    }

    fn palette_match_count(&self) -> usize {
        launchable_matches(&self.config.config.profiles, &self.palette_query).len()
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
            let matches = launchable_matches(&self.config.config.profiles, &self.palette_query);
            matches
                .get(self.palette_sel)
                .and_then(|p| LaunchSpec::from_profile(p))
        };
        let Some(spec) = spec else { return };
        self.palette_open = false;
        let id = self.spawn_pane_with(cx, spec);
        self.tabs.push(Tab {
            root: Node::Leaf(id),
            focused: id,
        });
        self.active = self.tabs.len() - 1;
        self.focus_pane(id, window, cx);
    }

    /// Launch the profile at `idx` (mouse click on a palette row).
    fn launch_index(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_sel = idx;
        self.launch_selected(window, cx);
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("ACTION new_tab");
        let id = self.spawn_pane(cx);
        self.tabs.push(Tab {
            root: Node::Leaf(id),
            focused: id,
        });
        self.active = self.tabs.len() - 1;
        self.focus_pane(id, window, cx);
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
        let mut leaves = Vec::new();
        collect_leaves(&self.tabs[i].root, &mut leaves);
        for id in leaves {
            self.panes.remove(&id); // drop the view → drop LocalPty → kill child
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            let id = self.spawn_pane(cx);
            self.tabs.push(Tab {
                root: Node::Leaf(id),
                focused: id,
            });
            self.active = 0;
            self.focus_pane(id, window, cx);
        } else {
            self.active = self.active.min(self.tabs.len() - 1);
            let fid = self.tabs[self.active].focused;
            self.focus_pane(fid, window, cx);
        }
        cx.notify();
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("ACTION split {}", if axis == Axis::Row { "right" } else { "down" });
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

    /// Root window-surface fill: translucent glass over the acrylic backdrop, or
    /// an opaque fill when the theme requests a `solid` window.
    fn window_glass(&self) -> Rgba {
        let ui = &self.config.theme.ui;
        match ui.window.backdrop {
            tn_config::Backdrop::Solid => col(ui.chrome_bg),
            _ => cola(ui.chrome_bg, 0.72), // window-glass token (~.72)
        }
    }

    fn render_node(&self, node: &Node, focused: PaneId, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf(id) => {
                let id = *id;
                let view = self.panes.get(&id).expect("pane exists").clone();
                let is_focused = id == focused;
                let theme = &self.config.theme;
                // Focused pane: a faint warm rim + a lift (deeper shadow) so the
                // eye finds it instantly — no glow. Others: a plain glass rim,
                // sitting flat. (docs/UX-DESIGN §6.2 active-split focus.)
                let rim = if is_focused {
                    cola(theme.agents.claude, 0.45)
                } else {
                    rgba(RIM)
                };
                let pane = div()
                    .size_full()
                    .border_1()
                    .rounded(px(R_PANEL))
                    .overflow_hidden()
                    .p_1()
                    .bg(cola(theme.terminal.background, 0.96)) // readable, faintly glassy
                    .border_color(rim)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            this.focus_pane(id, window, cx);
                        }),
                    )
                    .child(view);
                if is_focused {
                    shadowed(pane, vec![soft_shadow(16.0, 48.0, -28.0, 0.55)]).into_any_element()
                } else {
                    pane.into_any_element()
                }
            }
            Node::Split {
                axis,
                kids,
                weights,
            } => {
                let row = *axis == Axis::Row;
                let sum: f32 = weights.iter().sum::<f32>().max(1.0);
                let mut container = div()
                    .size_full()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .overflow_hidden()
                    .flex();
                container = if row {
                    container.flex_row()
                } else {
                    container.flex_col()
                };
                for (kid, w) in kids.iter().zip(weights.iter()) {
                    let frac = w / sum;
                    // min_w/min_h 0 + overflow_hidden: without these a flex child's
                    // default `min-size: auto` lets a too-tall pane inflate past its
                    // `relative` share and spill out of the window.
                    let mut wrap = div().flex_none().min_w(px(0.)).min_h(px(0.)).overflow_hidden();
                    wrap = if row {
                        wrap.h_full().w(relative(frac))
                    } else {
                        wrap.w_full().h(relative(frac))
                    };
                    container = container.child(wrap.child(self.render_node(kid, focused, cx)));
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
                .px_3()
                .h(px(18.))
                .children(children)
        };
        let sep = || div().w(px(1.)).h(px(13.)).flex_none().bg(rgba(0xffffff14));
        let num = |s: String| -> AnyElement {
            div().text_color(col(ui.foreground)).child(SharedString::from(s)).into_any_element()
        };

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(30.))
            .px_2()
            .border_t(px(1.))
            .border_color(rgba(SHEEN)) // top mirror edge catches the light
            .bg(cola(ui.chrome_bg, 0.55)) // glass over the acrylic backdrop
            .text_size(px(11.))
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

        // right cluster: viewer file·lang, encoding, theme
        if let Some((name, lang)) = self.viewer.read(cx).status() {
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
        let matches = launchable_matches(&self.config.config.profiles, &self.palette_query);
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
            .map(|(i, tab)| {
                let pane = self.panes.get(&tab.focused);
                let title = pane
                    .and_then(|v| v.read(cx).title())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| truncate_label(s.trim(), 24));
                let agent = pane.and_then(|v| v.read(cx).agent());
                let cwd = pane.and_then(|v| v.read(cx).cwd());
                (
                    title.unwrap_or_else(|| format!("Term {}", i + 1)),
                    tab.root.leaf_count(),
                    agent,
                    cwd,
                )
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
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded(px(R_CARD))
                    .text_size(px(12.0))
                    // Active tab = a glass pill (inset + rim + sheen) with a thin
                    // agent-color accent bar at the top. Inactive sits flat and
                    // lifts a touch on hover. No glow.
                    .when(is_active, |d| {
                        shadowed(
                            d.bg(cola(ui.tab_active_bg, 0.85))
                                .border_1()
                                .border_color(rgba(RIM))
                                .text_color(col(ui.foreground)),
                            vec![soft_shadow(2.0, 10.0, -4.0, 0.35)],
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(1.))
                                .left(px(11.))
                                .right(px(11.))
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
                    // a plain shell. See docs/UX-DESIGN §6.2 tab agent accent.
                    .child(if agent.is_some() {
                        icon("spark", 13., dot)
                    } else {
                        icon("term", 13., ui.accent)
                    })
                    .child(label)
                    // cwd path badge on the active tab (mockup's "~/proj/tn").
                    .when(is_active && cwd.is_some(), |d| {
                        d.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(col(ui.muted))
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
                    .w(px(28.))
                    .h(px(28.))
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
            .gap_2()
            .pl_1()
            .pr_2()
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .w(px(22.))
                    .h(px(22.))
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
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(col(ui.foreground))
                    .child("Tn"),
            );

        // Window controls: the OS performs the action from the marked region
        // (HTMINBUTTON / HTMAXBUTTON / HTCLOSE) — no click handler needed.
        let ctl_btn = |name: &'static str, area: WindowControlArea, danger: bool| {
            div()
                .w(px(34.))
                .h(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .hover(move |s| s.bg(if danger { rgba(0xF7768E33) } else { rgba(HOVER) }))
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
            .gap_2()
            .h(px(46.))
            .pl_3()
            .pr_2()
            .border_b(px(1.))
            .border_color(rgba(RIM)) // glass edge under the titlebar
            .child(brand)
            .child(tabs)
            // A flexible draggable spacer fills the gap between tabs and controls.
            .child(div().flex_1().h_full().window_control_area(WindowControlArea::Drag))
            .child(controls);

        let body = div()
            .flex_1()
            .min_h(px(0.)) // let the flex child be bounded by the window, not its content
            .overflow_hidden()
            .p_1()
            .flex()
            .flex_row()
            .gap_2()
            // File explorer sidebar (left column), toggled by Ctrl+Shift+B.
            .when(self.explorer_open, |d| {
                d.child(
                    div()
                        .w(px(214.))
                        .flex_none()
                        .min_h(px(0.))
                        .overflow_hidden()
                        .child(self.explorer.clone()),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(self.render_node(&self.tabs[active].root, focused, cx)),
            )
            // File/diff viewer (right column): auto-opens on file click,
            // toggle with Ctrl+Shift+J.
            .when(self.viewer_open, |d| {
                d.child(
                    div()
                        .w(px(420.))
                        .flex_none()
                        .min_h(px(0.))
                        .overflow_hidden()
                        .child(self.viewer.clone()),
                )
            });

        let palette = self.render_palette(cx);

        div()
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
            .on_action(cx.listener(Self::toggle_viewer))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .rounded(px(R_WINDOW)) // rounded window corners (Calm Glass)
            .bg(self.window_glass()) // translucent over the acrylic backdrop
            .text_color(col(ui.foreground))
            .font_family(UI_SANS) // UI sans for chrome; panes set mono themselves
            .child(titlebar)
            .child(body)
            .child(self.render_status_bar(cx))
            .when_some(palette, |d, p| d.child(p))
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
}
