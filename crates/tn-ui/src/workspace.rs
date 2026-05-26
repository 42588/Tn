//! Workspace: multiple tabs, each an n-ary pane tree of [`TerminalView`]s.
//!
//! Splitting uses an n-ary container tree (not a binary tree): splitting along
//! the same axis as the focused pane's parent inserts an aligned sibling;
//! splitting along the other axis nests a new container. This matches the
//! flexible-tiling model in docs/UX-DESIGN.md. Divider-drag and drag-dock are
//! later refinements; this cut gives tabs + keyboard splits + click-to-focus.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{
    actions, div, prelude::*, px, relative, rgb, rgba, AnyElement, App, AppContext, AsyncApp,
    Context, Entity, FocusHandle, KeyBinding, KeyDownEvent, MouseButton, Rgba, SharedString,
    WeakEntity, Window,
};
use tn_config::Loaded;

use crate::terminal_view::{LaunchSpec, TerminalView};

type PaneId = u64;

/// Convert a `tn-config` chrome color to a GPUI color.
fn col(c: tn_config::Color) -> Rgba {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
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
fn short_model(id: &str) -> String {
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
fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
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
        TogglePalette
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
    /// Latest Claude usage for the app's project dir, polled off-thread (M4).
    ai_usage: Option<tn_ai::AiUsage>,
    /// Command palette (Ctrl+Shift+P) state.
    palette_open: bool,
    palette_query: String,
    palette_sel: usize,
    palette_focus: FocusHandle,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let mut ws = Self {
            tabs: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_id: 0,
            focused_init: false,
            config,
            ai_usage: None,
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
        Self::spawn_usage_poller(cx);
        if std::env::var("TN_DEMO").is_ok() {
            Self::spawn_demo(cx);
        }
        ws
    }

    /// Poll Claude usage for the app's project dir off the main thread, pushing
    /// an update only when the session file's mtime changes (so an idle session
    /// costs a cheap stat, not a re-parse). Reads the same JSONL `ccusage` does.
    fn spawn_usage_poller(cx: &mut Context<Self>) {
        let Some(cwd) = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
        else {
            return;
        };
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut last: Option<SystemTime> = None;
            loop {
                let cwd2 = cwd.clone();
                let prev = last;
                let res = exec
                    .spawn(async move {
                        let path = tn_ai::latest_session_file(&cwd2)?;
                        let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
                        if Some(mtime) == prev {
                            return None; // unchanged — skip the (heavy) re-parse
                        }
                        let text = std::fs::read_to_string(&path).ok()?;
                        Some((mtime, tn_ai::parse_claude_session(&text)?))
                    })
                    .await;
                if let Some((mtime, usage)) = res {
                    last = Some(mtime);
                    if this
                        .update(cx, |ws, cx| {
                            ws.ai_usage = Some(usage);
                            cx.notify();
                        })
                        .is_err()
                    {
                        break; // workspace dropped
                    }
                }
                exec.timer(Duration::from_secs(5)).await;
            }
        })
        .detach();
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

    fn render_node(&self, node: &Node, focused: PaneId, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf(id) => {
                let id = *id;
                let view = self.panes.get(&id).expect("pane exists").clone();
                let is_focused = id == focused;
                div()
                    .size_full()
                    .border_1()
                    .rounded_md()
                    .overflow_hidden()
                    .p_1()
                    .bg(col(self.config.theme.terminal.background))
                    .border_color(if is_focused {
                        col(self.config.theme.agents.claude) // active pane focus ring
                    } else {
                        col(self.config.theme.ui.border)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            this.focus_pane(id, window, cx);
                        }),
                    )
                    .child(view)
                    .into_any_element()
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

    /// Bottom status bar with the live Claude usage readout (M4): agent dot,
    /// model, a context-fill bar (green → yellow → red), %, tokens, cost.
    fn render_status_bar(&self) -> gpui::Div {
        let t = &self.config.theme;
        let ui = &t.ui;
        let bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(26.))
            .px_3()
            .bg(col(ui.chrome_bg))
            .text_size(px(11.))
            .text_color(col(ui.muted));
        match &self.ai_usage {
            Some(u) => {
                let frac = u.context_frac();
                let stripe = if frac >= 0.85 {
                    t.ansi.red
                } else if frac >= 0.6 {
                    t.ansi.yellow
                } else {
                    t.ansi.green
                };
                bar.child(div().w(px(6.)).h(px(6.)).rounded_full().bg(col(t.agents.claude)))
                    .child(
                        div()
                            .text_color(col(ui.foreground))
                            .child(SharedString::from(short_model(&u.model))),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .h(px(5.))
                            .rounded_full()
                            .bg(rgba(0xffffff1f))
                            .child(div().h_full().w(relative(frac)).rounded_full().bg(col(stripe))),
                    )
                    .child(SharedString::from(format!("{:.0}%", frac * 100.0)))
                    .child(div().text_color(col(ui.muted)).child(SharedString::from(format!(
                        "{} / {}",
                        human_tokens(u.context_used as u64),
                        human_tokens(u.context_max as u64)
                    ))))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(col(t.ansi.green))
                            .child(SharedString::from(format!("${:.2}", u.cost_usd))),
                    )
            }
            None => bar.child(SharedString::from("· 读取 Claude 用量…")),
        }
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
                .rounded_md()
                .when(is_sel, |d| d.bg(rgba(0xffffff14)))
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

        let panel = div()
            .flex()
            .flex_col()
            .w(px(540.))
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(rgba(0xffffff1f))
            .bg(rgba(0x1b1d2bf2))
            .child(query_line)
            .child(div().h(px(1.)).bg(rgba(0xffffff14)))
            .child(div().flex().flex_col().p_1().gap_1().children(rows));

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
        // to "Term N". Precomputed so the click closures below own `cx` freely.
        let tab_info: Vec<(String, usize)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let title = self
                    .panes
                    .get(&tab.focused)
                    .and_then(|v| v.read(cx).title())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| truncate_label(s.trim(), 28));
                (
                    title.unwrap_or_else(|| format!("Term {}", i + 1)),
                    tab.root.leaf_count(),
                )
            })
            .collect();

        let tab_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(34.0))
            .px_2()
            .gap_1()
            .bg(col(ui.chrome_bg))
            .children(tab_info.into_iter().enumerate().map(|(i, (label, panes))| {
                let is_active = i == active;
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_size(px(12.0))
                    .when(is_active, |d| {
                        d.bg(col(ui.tab_active_bg)).text_color(col(ui.foreground))
                    })
                    .when(!is_active, |d| d.text_color(col(ui.tab_inactive_fg)))
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
                            .text_size(px(13.0))
                            .text_color(col(ui.muted))
                            .hover(|s| s.text_color(col(ui.foreground)).bg(rgba(0xffffff1f)))
                            .child("\u{00d7}")
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
                    .px_2()
                    .text_size(px(16.0))
                    .text_color(col(ui.muted))
                    .child("+")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, window, cx| this.new_tab(&NewTab, window, cx)),
                    ),
            );

        let body = div()
            .flex_1()
            .min_h(px(0.)) // let the flex child be bounded by the window, not its content
            .overflow_hidden()
            .p_1()
            .child(self.render_node(&self.tabs[active].root, focused, cx));

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
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(col(ui.chrome_bg))
            .child(tab_bar)
            .child(body)
            .child(self.render_status_bar())
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
