//! Workspace: multiple tabs, each an n-ary pane tree of [`TerminalView`]s.
//!
//! Splitting uses an n-ary container tree (not a binary tree): splitting along
//! the same axis as the focused pane's parent inserts an aligned sibling;
//! splitting along the other axis nests a new container. This matches the
//! flexible-tiling model in docs/产品体验索引.md. Divider-drag and drag-dock are
//! later refinements; this cut gives tabs + keyboard splits + click-to-focus.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    actions, canvas, div, linear_color_stop, linear_gradient, prelude::*, px, relative, rgb, rgba,
    uniform_list, AnyElement, App, AppContext, AsyncApp, Bounds, Context, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point, Rgba, ScrollStrategy,
    SharedString, Subscription, UTF16Selection, UniformListScrollHandle, WeakEntity, Window,
    WindowControlArea,
};
use tn_config::Loaded;

use crate::explorer::{
    default_host_root, ExplorerChanged, ExplorerFile, ExplorerRoot, ExplorerSnapshot, ExplorerView,
    OpenFile,
};
use crate::layout::{LayoutNode, LayoutPane, Layouts, SLOTS};
use crate::local_dir_picker::{
    read_local_dirs, windows_virtual_root, LocalDirAction, LocalDirFocus, LocalDirPicker,
    WorkdirRecents,
};
use crate::perf::PerfStats;
use crate::quick_look::{QuickLook, QuickLookEvent};
use crate::remote_dir_picker::{PickerEntry, PickerSource, RemoteDirPicker};
use crate::ssh_recents::{AuthBadge, SshRecents};
use crate::terminal_view::{
    is_host_process_path, CwdChanged, FileNamespace, FilesChanged, LaunchSpec, OpenInQuickLook,
    RailFileTarget, SshCloseRequested, SshConnected, SshRememberPassword, SshRetryRequested,
    TerminalView, UsageUpdated,
};
use crate::welcome::{launch_rows, row_card, wsl_distros, LaunchRequested, LaunchRow, WelcomeView};
use tn_agent::AgentId;
use tn_pty::remote_fs::{RemoteFileService, RemotePath, SftpFileService};

pub(crate) type PaneId = u64;

const AGENT_DIR_PANEL_H: f32 = 500.0;
const AGENT_DIR_RECENTS_H: f32 = 158.0;
const AGENT_DIR_LIST_H: f32 = 176.0;

// 磷光 Phosphor tokens + helpers(col/cola/plate/float_panel/focus_brackets/icon)
// live in `crate::style` — single source of truth(规范 docs/设计/磷光设计语言.md)。
use crate::style::{
    col, cola, icon, plate, shadow_float, shadowed, ERR_SOFT, H0, H1, H2, INFO, PH, PH_DIM, R_CARD,
    R_CHIP, R_PANEL, SCRIM, SEAM, STATUSBAR_H, T0, T1, T2, T3, TITLEBAR_H, UI_SANS,
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
    // 保留小数点:`gpt-5.5 xhigh` → `GPT 5.5`(曾丢成「GPT 55」,差异总结 2-7)。
    let ver: String = id
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
        .collect::<String>()
        .trim_matches(|c| c == '-' || c == '.')
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
    let parts: Vec<&str> = p
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match parts.len() {
        0 => p,
        1 => parts[0].to_string(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// The current git branch of the app's cwd, if it's a repo (for the status bar).
/// Returns `None` both when not in a repo (silent — expected) and when `git`
/// can't be spawned (logged once — likely not installed / PATH). See docs/修复与优化/智能体活动栏与正文显示.md.
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

fn is_removed_builtin_agent_profile(
    p: &tn_config::Profile,
    declared_agents: &std::collections::HashSet<String>,
) -> bool {
    if p.kind != tn_config::ProfileKind::Agent {
        return false;
    }
    let id = p
        .agent
        .as_deref()
        .or(p.command.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(id.as_str(), "claude" | "codex") && !declared_agents.contains(&id)
}

fn is_launchable_agent_profile(p: &tn_config::Profile) -> bool {
    (p.kind == tn_config::ProfileKind::Agent || p.agent.is_some())
        && p.command.as_deref().is_some_and(|c| !c.is_empty())
}

fn generic_agent_profile() -> tn_config::Profile {
    tn_config::Profile {
        name: "Agent".into(),
        kind: tn_config::ProfileKind::Agent,
        command: Some("agent".into()),
        args: Vec::new(),
        cwd: None,
        distro: None,
        host: None,
        user: None,
        agent: Some("agent".into()),
        accent: None,
        glyph: Some("spark".into()),
    }
}

/// Lowercase ascii-alnum slug (`"Gemini CLI"` → `"gemini-cli"`): non-alnum runs
/// collapse to one `-`, trimmed. Empty when the input has no ascii alnum (e.g. a
/// purely-CJK name) — callers fall back to another source. Used to derive a
/// stable `AgentId` from the agent editor's name/command.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// First non-empty slug among `candidates`, else `"agent"`.
fn first_nonempty_slug(candidates: &[&str]) -> String {
    candidates
        .iter()
        .map(|c| slugify(c))
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "agent".to_string())
}

/// `base`, or `base-2`/`base-3`/… if already taken — a unique agent id.
fn unique_agent_id(base: &str, existing: &std::collections::HashSet<String>) -> String {
    if !existing.contains(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !existing.contains(c))
        .expect("infinite range yields a free id")
}

/// A short display label (first whitespace word, capped) for the header `short`.
fn short_name(name: &str) -> String {
    name.split_whitespace()
        .next()
        .unwrap_or(name)
        .chars()
        .take(16)
        .collect()
}

/// Collapse duplicate **agent** tiles by resolved agent id — an agent shows once
/// in the launcher. Keeps the **last** occurrence (most-recently-saved wins), so a
/// freshly added/edited agent supersedes a stale leftover — e.g. a `claude`
/// profile written by an older default config that resurfaces once the user
/// declares a `claude` `[[agents]]` (the dup the user hit). Non-agent profiles
/// (shell / WSL / SSH) are never deduped — those can legitimately repeat.
fn dedup_agent_profiles(profiles: Vec<tn_config::Profile>) -> Vec<tn_config::Profile> {
    let key = |p: &tn_config::Profile| -> Option<String> {
        if p.kind != tn_config::ProfileKind::Agent {
            return None;
        }
        let k = p
            .agent
            .clone()
            .or_else(|| p.command.clone())
            .unwrap_or_default()
            .to_ascii_lowercase();
        (!k.is_empty()).then_some(k)
    };
    let mut last: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, p) in profiles.iter().enumerate() {
        if let Some(k) = key(p) {
            last.insert(k, i);
        }
    }
    profiles
        .into_iter()
        .enumerate()
        .filter(|(i, p)| key(p).map_or(true, |k| last.get(&k) == Some(i)))
        .map(|(_, p)| p)
        .collect()
}

/// The launcher's profiles: the configured `[[profiles]]` plus every installed
/// WSL distro not already covered by a config profile — so users get *all* their
/// distros without editing config (the default config ships only one). Shells
/// out to `wsl.exe` once (cache the result; don't call per render). Docker's
/// internal `docker-desktop*` distros are skipped (not interactive shells).
pub(crate) fn discover_profiles(config: &Loaded) -> Vec<tn_config::Profile> {
    let declared_agents: std::collections::HashSet<String> = config
        .config
        .agents
        .iter()
        .map(|a| a.id.to_ascii_lowercase())
        .collect();
    let removed_builtin_agent = config
        .config
        .profiles
        .iter()
        .any(|p| is_removed_builtin_agent_profile(p, &declared_agents));
    let mut profiles: Vec<_> = config
        .config
        .profiles
        .iter()
        .filter(|p| !is_removed_builtin_agent_profile(p, &declared_agents))
        .cloned()
        .collect();
    if removed_builtin_agent && !profiles.iter().any(is_launchable_agent_profile) {
        profiles.push(generic_agent_profile());
    }
    // One tile per agent: collapse duplicate agent profiles (e.g. a stale `claude`
    // from an older default config alongside a freshly added one). Latest wins.
    let mut profiles = dedup_agent_profiles(profiles);
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

fn wsl_unc_from_linux_cwd(distro: &str, cwd: &str) -> std::path::PathBuf {
    let rel = cwd.trim_start_matches('/').replace('/', "\\");
    let prefix = format!(r"\\wsl$\{}", distro);
    if rel.is_empty() {
        std::path::PathBuf::from(prefix)
    } else {
        std::path::PathBuf::from(format!(r"{prefix}\{rel}"))
    }
}

/// List a WSL directory for the in-app picker by reading the local
/// `\\wsl$\<distro>\…` UNC mapping of `linux_path` with `std::fs`. Child paths are
/// kept Linux-style (`linux_path/<name>`) so the picker navigates the same way the
/// SSH/SFTP source does. Runs on the background executor (blocking FS).
fn list_wsl_dir(distro: &str, linux_path: &RemotePath) -> anyhow::Result<Vec<PickerEntry>> {
    let unc = wsl_unc_from_linux_cwd(distro, linux_path.as_str());
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&unc)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(PickerEntry {
            path: linux_path.join(name.as_str()),
            name,
            is_dir,
        });
    }
    Ok(out)
}

fn explorer_root_for_pane(view: &TerminalView, spec: Option<&LaunchSpec>) -> Option<ExplorerRoot> {
    explorer_root_from_parts(
        view.file_namespace(),
        view.cwd(),
        view.effective_browsable_cwd(),
        spec.and_then(|s| s.ssh.clone()),
    )
}

fn explorer_root_from_parts(
    namespace: FileNamespace,
    cwd: Option<String>,
    host_browsable_cwd: Option<std::path::PathBuf>,
    ssh: Option<tn_pty::SshConfig>,
) -> Option<ExplorerRoot> {
    match namespace {
        FileNamespace::Host => host_browsable_cwd.map(ExplorerRoot::host),
        FileNamespace::Wsl {
            distro: Some(distro),
        } => {
            let linux_cwd = cwd.filter(|cwd| cwd.starts_with('/'))?;
            let unc = wsl_unc_from_linux_cwd(&distro, &linux_cwd);
            Some(ExplorerRoot::wsl(distro, linux_cwd, unc))
        }
        // Without a concrete WSL distro, the file explorer has no host-browsable
        // path to enumerate. Keep the previous tree instead of re-rooting empty.
        FileNamespace::Wsl { distro: None } => None,
        FileNamespace::Ssh => {
            let cfg = ssh?;
            let remote_cwd = cwd.filter(|cwd| cwd.starts_with('/'))?;
            Some(ExplorerRoot::ssh(cfg, remote_cwd))
        }
    }
}

fn open_folder_should_use_native_picker(spec: Option<&LaunchSpec>) -> bool {
    // No spec = the welcome launchpad (focused pane is the `WELCOME_DUMMY`, with no
    // live `LaunchSpec`): open the native folder picker so the chosen directory
    // becomes the explorer root — and thus the cwd for the next agent/shell tile.
    // SSH (SFTP) and WSL (\\wsl$ local UNC) panes use the in-app navigable picker
    // instead, so you can browse the Linux tree (with 上级/进入/确认) rather than the
    // Windows-rooted native dialog. Only Host panes / welcome use the native picker.
    match spec {
        None => true,
        Some(spec) => matches!(spec.file_namespace, FileNamespace::Host),
    }
}

fn workspace_overlay_freezes_pane_focus(
    palette_open: bool,
    split_launcher_open: bool,
    layout_manager_open: bool,
    quick_look_open: bool,
    ssh_prompt_open: bool,
    agent_form_open: bool,
    remote_dir_picker_open: bool,
    agent_dir_picker_open: bool,
) -> bool {
    palette_open
        || split_launcher_open
        || layout_manager_open
        || quick_look_open
        || ssh_prompt_open
        || agent_form_open
        || remote_dir_picker_open
        || agent_dir_picker_open
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteDirKeyAction {
    Cancel,
    MoveUp,
    MoveDown,
    Parent,
    EnterDirectory,
    Confirm,
    Reload,
    Ignore,
}

fn remote_dir_key_action(key: &str, control: bool, platform: bool) -> RemoteDirKeyAction {
    match key {
        "escape" => RemoteDirKeyAction::Cancel,
        "up" => RemoteDirKeyAction::MoveUp,
        "down" => RemoteDirKeyAction::MoveDown,
        "left" => RemoteDirKeyAction::Parent,
        "right" => RemoteDirKeyAction::EnterDirectory,
        "enter" => RemoteDirKeyAction::Confirm,
        "r" if control || platform => RemoteDirKeyAction::Reload,
        _ => RemoteDirKeyAction::Ignore,
    }
}

/// A root at the filesystem root `/` for an SSH/WSL pane, used when the pane's
/// live cwd isn't known yet so the in-app picker can still open (navigate from `/`).
fn fallback_remote_root(spec: Option<&LaunchSpec>) -> Option<ExplorerRoot> {
    let spec = spec?;
    match &spec.file_namespace {
        FileNamespace::Ssh => spec.ssh.clone().map(|cfg| ExplorerRoot::ssh(cfg, "/")),
        FileNamespace::Wsl {
            distro: Some(distro),
        } => Some(ExplorerRoot::wsl(
            distro.clone(),
            "/".to_string(),
            wsl_unc_from_linux_cwd(distro, "/"),
        )),
        _ => None,
    }
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
    /// 「右侧分屏」式整句(SHEET 06-B 方向读数/页脚 tag 用)。
    fn side_label(self) -> &'static str {
        match self {
            SplitDir::Left => "左侧分屏",
            SplitDir::Right => "右侧分屏",
            SplitDir::Up => "上方分屏",
            SplitDir::Down => "下方分屏",
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
                *self = Node::Split {
                    axis,
                    kids,
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
        if let Node::Split {
            axis: sa,
            kids,
            weights,
        } = self
        {
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
        Tab {
            root: Node::Leaf(WELCOME_DUMMY),
            focused: WELCOME_DUMMY,
            welcome: true,
        }
    }
    /// A tab holding a (single, to start) pane tree.
    fn panes(root: Node, focused: PaneId) -> Self {
        Tab {
            root,
            focused,
            welcome: false,
        }
    }
}

fn should_reset_explorer_for_welcome_tab(
    active_tab: &Tab,
    explorer_pane: Option<PaneId>,
    current_root: &ExplorerRoot,
    default_root: &ExplorerRoot,
) -> bool {
    if !active_tab.welcome || explorer_pane == Some(WELCOME_DUMMY) {
        return false;
    }
    explorer_pane.is_some() || current_root != default_root
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
        let command = cmd_for_id
            .get(kb.id.as_str())
            .copied()
            .unwrap_or(kb.id.as_str());
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
    /// App-internal SSH directory picker. Unlike the native host picker, this is
    /// backed by SFTP directory state and commits only after explicit confirm.
    remote_dir_picker: Option<RemoteDirPicker>,
    remote_dir_focus: FocusHandle,
    remote_dir_needs_focus: bool,
    remote_dir_scroll: UniformListScrollHandle,
    remote_fs: Arc<dyn RemoteFileService>,
    /// Local host workdir picker opened from welcome Agent tiles.
    agent_dir_picker: Option<LocalDirPicker>,
    agent_dir_focus: FocusHandle,
    agent_dir_needs_focus: bool,
    /// Per-pane explorer view state (expansion + selection). The single
    /// `explorer` entity renders one pane at a time; switching focus saves the
    /// outgoing pane's snapshot here and restores the incoming pane's, so each
    /// split pane keeps its own tree expansion + selected file (面板解耦). Pruned
    /// lazily on save (entries for closed panes dropped) — no per-`remove` hooks.
    explorer_states: HashMap<PaneId, ExplorerSnapshot>,
    /// Which pane the `explorer` is currently showing (None until first focus).
    explorer_pane: Option<PaneId>,
    /// Quick Look 速览浮层(贴树右缘、浮于终端之上)+ whether it's shown
    /// (auto-opens on clicking a file in the explorer; only rendered when it
    /// actually has a file loaded).
    quick_look: Entity<QuickLook>,
    quick_look_open: bool,
    /// Return focus to the active pane next render (set when Quick Look closes via
    /// its own keyboard — the event callback has no `window` to focus with).
    ql_refocus_pane: bool,
    ql_refocus_active_pane: bool,
    /// App menu (click the Tn brand) dropdown open state.
    app_menu_open: bool,
    /// Welcome launchpad shown as a new tab's content (until a tile is clicked).
    /// One shared entity (stateless chrome); its `LaunchRequested` launches into
    /// the active tab.
    welcome: Entity<WelcomeView>,
    /// 像素宠物 overlay(特色③)— 全局一只,状态栏上方栖位(SHEET 05)。
    pet: Entity<crate::pet::PetView>,
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
    /// `新会话` split launcher (app menu): **单浮层**(SHEET 06-B,用户定夺改回
    /// 原型)— 方向 4 格(⇥ 循环)与 profile 行同屏,↵ 按当前方向分屏启动。
    /// Distinct from the command palette (which opens a session in a new *tab*).
    split_launcher_open: bool,
    split_dir: SplitDir,
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
    /// Opt-in render instrumentation (TN_PERF, see docs/修复与优化/基础性能与审查勘误.md): how often the
    /// workspace chrome re-renders and how long it takes. Panes are embedded as
    /// entities, so terminal output frames don't trigger this — only the
    /// workspace's own notifies (usage updates, tab/split/focus, palette) do.
    perf: PerfStats,
    /// Tracks whether QuickLook was opened from an agent activity-rail card.
    /// `None`  → opened from the explorer (↑↓ nav uses `explorer.select_adjacent_file`).
    /// `Some(id)` → opened from pane `id`'s rail (↑↓ nav stays within that rail's
    ///              changed-file list using `TerminalView::rail_nav`).
    ql_rail_pane: Option<PaneId>,
    /// Index of the currently-previewed file within the rail's file list.  Only
    /// meaningful when `ql_rail_pane.is_some()`.
    ql_rail_idx: usize,
    ssh_prompt_open: bool,
    /// The input string in the SSH prompt.
    ssh_prompt_input: String,
    ssh_prompt_focus: FocusHandle,
    ssh_prompt_needs_focus: bool,
    ssh_prompt_intent: Option<SshPromptIntent>,
    /// Tracks whether IME is currently disabled by an overlay (the SSH target
    /// box, or the agent editor's command field — both ASCII), to avoid redundant
    /// `ImmAssociateContextEx` calls every render frame.
    ime_disabled: bool,
    /// Remembered SSH endpoints (A1). The connector lists these for one-keystroke
    /// reconnect; a successful connect upserts the target here.
    ssh_recents: SshRecents,
    /// Selected row in the connector's recents list (index into the filtered list).
    ssh_prompt_sel: usize,
    /// When `Some`, a favorite recent is being renamed in-place. Kept separate
    /// from the filter string so IME/中文 input can be routed to the nickname.
    ssh_rename: Option<SshRenameDraft>,
    ssh_rename_marked: Option<String>,
    /// `Host` aliases enumerated from `~/.ssh/config` (A4) — the connector's third
    /// section. Refreshed each time the connector opens (the file is tiny and
    /// rarely changes mid-session).
    ssh_config_hosts: Vec<tn_pty::SshHostEntry>,

    // ── 添加/编辑 Agent overlay (the in-app agent editor — no more hand-editing
    // config.toml `[[agents]]`). A config-level (generic) agent: terminal +
    // activity rail, no usage telemetry (that needs a built-in/external adapter).
    /// The agent editor overlay is open.
    agent_form_open: bool,
    /// The working draft (name / command / accent index / cursor ownership).
    agent_form: AgentForm,
    /// Which text field of the editor is being edited.
    agent_form_field: AgentField,
    /// IME preedit buffer for the **name** field (the command field is ASCII, so
    /// IME is disabled while it's focused). Mirrors `ssh_rename_marked`.
    agent_form_marked: Option<String>,
    /// `Some` when **editing** an existing agent (carries the id + the old profile
    /// name to replace in config); `None` when **adding** a new one.
    agent_form_edit: Option<AgentEdit>,
    agent_form_focus: FocusHandle,
    agent_form_needs_focus: bool,
}

/// Which text field of the 添加/编辑 Agent overlay is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentField {
    Name,
    Command,
    /// The advanced telemetry-sidecar command (ASCII, IME off like Command).
    Sidecar,
}

/// The working draft of the 添加/编辑 Agent overlay.
#[derive(Clone, Default)]
struct AgentForm {
    name: String,
    command: String,
    /// Picker index into [`tn_config::ACCENT_SWATCHES`] (default `0`).
    accent_idx: usize,
    /// The agent paints its own cursor (Ink TUI) → the terminal hides its block.
    /// Default on (most agent CLIs are Ink-based).
    manages_cursor: bool,
    /// Advanced: a stdio/JSONL telemetry sidecar command. Empty = generic (no
    /// telemetry). When set, the agent gets the usage ring + realtime chips.
    sidecar: String,
    /// Advanced: the sidecar reaches the network → spawn behind a confirm card
    /// (`remote_daemon` runtime + `allow_network`). Only meaningful with a sidecar.
    networked: bool,
}

impl AgentForm {
    /// The picked accent (`accent_idx` indexes [`tn_config::ACCENT_SWATCHES`];
    /// clamps to the first swatch if somehow out of range).
    fn accent(&self) -> tn_config::Color {
        tn_config::ACCENT_SWATCHES
            .get(self.accent_idx)
            .or_else(|| tn_config::ACCENT_SWATCHES.first())
            .map(|(_, c)| *c)
            .expect("ACCENT_SWATCHES is non-empty")
    }
}

/// Context for **editing** an existing agent (vs adding): the id to preserve and
/// the old profile name, so save can replace the right config blocks.
#[derive(Clone)]
struct AgentEdit {
    old_id: String,
    old_profile_name: String,
}

#[derive(Clone, Copy)]
enum SshPromptIntent {
    Welcome,
    Palette,
    Split(SplitDir),
}

#[derive(Clone)]
struct SshRenameDraft {
    host: String,
    user: String,
    port: u16,
    name: String,
}

/// One connector list row: an auto-recorded recent (`ssh_recents.json`) or a
/// read-only `ssh-config` alias. Owned so the per-row click listeners can capture
/// their data without borrowing `self`.
enum SshConnRow {
    Recent {
        host: String,
        user: String,
        port: u16,
        name: Option<String>,
        target: String,
        favorite: bool,
        auth: AuthBadge,
        last_used: u64,
    },
    /// A `Host` alias from `~/.ssh/config` (A4): not yet connected, just an
    /// endpoint OpenSSH already knows. `target` is `[user@]host[:port]`.
    Config { alias: String, target: String },
}

/// C2 pre-dial validation of a typed `[user@]host[:port]`. Returns `Err(msg)`
/// when the target is obviously unconnectable so the connector can flag it red
/// *before* dialing (empty host, dangling `@`/`:`, out-of-range port). A
/// non-numeric `:suffix` is left alone — it stays part of the host, matching
/// `SshConfig::parse`. Empty input is `Ok` (placeholder state, nothing to flag).
pub(crate) fn validate_ssh_target(typed: &str) -> Result<(), &'static str> {
    let t = typed.trim();
    if t.is_empty() {
        return Ok(());
    }
    let rest = match t.split_once('@') {
        Some(("", _)) => return Err("缺少用户名(@ 前为空)"),
        Some((_, r)) => r,
        None => t,
    };
    if rest.is_empty() {
        return Err("缺少主机");
    }
    if let Some((h, p)) = rest.rsplit_once(':') {
        if p.is_empty() {
            // trailing colon: `host:`
            return Err("端口未填(去掉末尾的 :)");
        }
        // Only a digit-like suffix is meant as a port; otherwise it's hostname.
        if p.chars().all(|c| c.is_ascii_digit()) {
            match p.parse::<u32>() {
                Ok(n) if (1..=65535).contains(&n) => {
                    if h.is_empty() {
                        return Err("缺少主机");
                    }
                }
                _ => return Err("端口无效(应为 1–65535)"),
            }
        }
    }
    Ok(())
}

pub(crate) fn parse_ssh_target_chips(
    typed: &str,
) -> Option<(Option<String>, String, Option<String>)> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    let (user, rest) = match typed.split_once('@') {
        Some((u, r)) if !u.is_empty() => (Some(u.to_string()), r),
        _ => (None, typed),
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => {
            (h.to_string(), Some(p.to_string()))
        }
        _ => (rest.to_string(), None),
    };
    Some((user, host, port))
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let explorer = cx.new(|cx| ExplorerView::new(cx, config.clone()));
        let quick_look = cx.new(|cx| QuickLook::new(cx, config.clone()));
        // Clicking / Space-ing a file in the explorer pops the Quick Look overlay
        // for it (open() also flags the overlay to grab focus on its next render).
        cx.subscribe(&explorer, |ws, _explorer, ev: &OpenFile, cx| {
            ws.ql_rail_pane = None; // navigator returns to explorer scope
            let was_open = ws.quick_look_open;
            ws.quick_look.update(cx, |v, cx| match ev.0.clone() {
                ExplorerFile::Local(path) => v.open(path, cx),
                ExplorerFile::Remote { cfg, id, size } => v.open_remote(cfg, id, size, cx),
            });
            ws.quick_look_open = true;
            // 磷光通道(规则 J):已开 = 切换文件(只闪光点);否则 = 叼光点展开面板。
            let cue = if was_open {
                crate::pet::PetSpatialCue::QuickLookSwitch
            } else {
                crate::pet::PetSpatialCue::QuickLookOpen
            };
            ws.pet.update(cx, |p, cx| p.spatial_cue(cue, cx));
            cx.notify();
        })
        .detach();
        cx.subscribe(&explorer, |ws, _explorer, _ev: &ExplorerChanged, cx| {
            ws.quick_look
                .update(cx, |v, cx| v.refresh_after_external_change(cx));
        })
        .detach();
        // Quick Look keyboard that needs the workspace: `↑↓` change file (drive the
        // tree's selection), `Esc`/`Space` close (give focus back to the terminal).
        cx.subscribe(&quick_look, |ws, _ql, ev: &QuickLookEvent, cx| {
            match ev {
                QuickLookEvent::Nav(delta) => {
                    if let Some(pane_id) = ws.ql_rail_pane {
                        // QuickLook was opened from an agent rail card → navigate
                        // within that pane's changed-file list only.
                        let result = ws
                            .panes
                            .get(&pane_id)
                            .and_then(|v| v.read(cx).rail_nav(ws.ql_rail_idx, *delta));
                        if let Some((new_idx, path)) = result {
                            ws.ql_rail_idx = new_idx;
                            ws.open_quick_look_diff_target(path, cx); // stay on Diff tab
                        }
                    } else {
                        // QuickLook was opened from the explorer → use the old
                        // tree-wide navigation (selects the adjacent file in the tree).
                        let next = ws
                            .explorer
                            .update(cx, |e, cx| e.select_adjacent_file(*delta, cx));
                        if let Some(file) = next {
                            ws.quick_look.update(cx, |v, cx| match file {
                                ExplorerFile::Local(path) => v.open(path, cx),
                                ExplorerFile::Remote { cfg, id, size } => {
                                    v.open_remote(cfg, id, size, cx)
                                }
                            });
                            // 磷光通道(规则 J):嗅光点/换向,内容切换不重推面板。
                            ws.pet.update(cx, |p, cx| {
                                p.spatial_cue(crate::pet::PetSpatialCue::QuickLookSwitch, cx)
                            });
                        }
                    }
                }
                QuickLookEvent::Close => {
                    let closed = ws.quick_look.update(cx, |v, cx| v.request_close(cx));
                    if closed {
                        ws.quick_look_open = false;
                        ws.ql_rail_pane = None; // reset on close
                        ws.ql_refocus_pane = true; // refocus the pane in next render
                        // 磷光通道(规则 J):拉光点收束面板,宠物换岗回主岗台。
                        ws.pet.update(cx, |p, cx| {
                            p.spatial_cue(crate::pet::PetSpatialCue::QuickLookClose, cx)
                        });
                    }
                    cx.notify();
                }
                QuickLookEvent::CloseConfirmed => {
                    ws.quick_look_open = false;
                    ws.ql_rail_pane = None;
                    ws.ql_refocus_pane = true;
                    cx.notify();
                }
                QuickLookEvent::QuitConfirmed => {
                    ws.quick_look_open = false;
                    ws.ql_rail_pane = None;
                    ws.finish_quit(cx);
                }
                QuickLookEvent::FileSaved(_path) => {
                    // Editor saved a file → refresh every agent pane's「本次改动」now
                    // (synchronous + deterministic; `refresh_changes` no-ops on plain
                    // shells and recomputes git only for panes whose cwd covers it).
                    for view in ws.panes.values() {
                        view.update(cx, |v, cx| v.refresh_changes(cx));
                    }
                    // Also mark the explorer stale so git tags refresh.
                    ws.explorer
                        .update(cx, |explorer, _cx| explorer.mark_stale());
                    cx.notify();
                }
                QuickLookEvent::RemoteChangesDirty => {
                    // Remote hunk accept/reject changed the remote tree → refresh
                    // every pane's「本次改动」(remote panes via changes_for_remote)
                    // and the explorer git tags. Same path as a local save.
                    for view in ws.panes.values() {
                        view.update(cx, |v, cx| v.refresh_changes(cx));
                    }
                    ws.explorer
                        .update(cx, |explorer, _cx| explorer.mark_stale());
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
        // 像素宠物(特色③):全局一只,品种在初始化时一次性决定(固定优先,否则随机)。
        let pet = cx.new(|cx| crate::pet::PetView::new(cx, config.clone()));
        // Welcome launchpad events: launch a tile, open the SSH connector, or
        // add/edit/delete a custom agent (the in-app agent editor). Shared with
        // `reload_agents` so the recreated launchpad re-attaches identically.
        Self::subscribe_welcome(&welcome, cx);
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
            remote_dir_picker: None,
            remote_dir_focus: cx.focus_handle(),
            remote_dir_needs_focus: false,
            remote_dir_scroll: UniformListScrollHandle::default(),
            remote_fs: SftpFileService::shared(),
            agent_dir_picker: None,
            agent_dir_focus: cx.focus_handle(),
            agent_dir_needs_focus: false,
            explorer_states: HashMap::new(),
            explorer_pane: None,
            quick_look,
            quick_look_open: false,
            ql_refocus_pane: false,
            ql_refocus_active_pane: false,
            app_menu_open: false,
            welcome,
            pet,
            branch: git_branch(),
            workspace_focus: cx.focus_handle(),
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            palette_wsl: false,
            palette_focus: cx.focus_handle(),
            split_launcher_open: false,
            split_target: None,
            split_dir: SplitDir::Right,
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
            ql_rail_pane: None,
            ql_rail_idx: 0,
            ssh_prompt_open: false,
            ssh_prompt_input: String::new(),
            ssh_prompt_focus: cx.focus_handle(),
            ssh_prompt_needs_focus: false,
            ssh_prompt_intent: None,
            ime_disabled: false,
            ssh_recents: SshRecents::load(),
            ssh_prompt_sel: 0,
            ssh_rename: None,
            ssh_rename_marked: None,
            ssh_config_hosts: tn_pty::list_ssh_config_hosts(),
            agent_form_open: false,
            agent_form: AgentForm::default(),
            agent_form_field: AgentField::Name,
            agent_form_marked: None,
            agent_form_edit: None,
            agent_form_focus: cx.focus_handle(),
            agent_form_needs_focus: false,
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
            ws.quick_look.update(cx, |v, cx| v.open(path, cx));
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
        let reg = crate::agent_host::agent_registry(cx);
        let spec = self
            .launch_profiles
            .get(index)
            .and_then(|p| LaunchSpec::from_profile(p, &reg))
            .unwrap_or_else(LaunchSpec::pwsh);
        let id = self.spawn_pane_with(cx, spec);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.root = Node::Leaf(id);
            tab.focused = id;
            tab.welcome = false;
        }
        // 磷光通道(规则 J):欢迎页 2× 宠物跃入地面裂缝 → 工作区岗台探头。
        self.pet.update(cx, |p, cx| {
            p.spatial_cue(crate::pet::PetSpatialCue::WelcomeToWorkspace, cx)
        });
    }

    fn launch_in_active_tab_with_cwd(
        &mut self,
        index: usize,
        cwd: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        let reg = crate::agent_host::agent_registry(cx);
        let spec = self
            .launch_profiles
            .get(index)
            .and_then(|p| LaunchSpec::from_profile(p, &reg))
            .map(|spec| spec.with_cwd(cwd))
            .unwrap_or_else(LaunchSpec::pwsh);
        let id = self.spawn_pane_with(cx, spec);
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.root = Node::Leaf(id);
            tab.focused = id;
            tab.welcome = false;
        }
        // 磷光通道(规则 J):欢迎页 → 工作区换岗。
        self.pet.update(cx, |p, cx| {
            p.spatial_cue(crate::pet::PetSpatialCue::WelcomeToWorkspace, cx)
        });
    }

    fn welcome_workdir_seed(&self, cx: &Context<Self>) -> Option<std::path::PathBuf> {
        self.explorer
            .read(cx)
            .root_path()
            .filter(|p| is_host_process_path(p.as_path()))
    }

    fn open_agent_dir_picker(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(profile) = self.launch_profiles.get(index) else {
            return;
        };
        let seed = self.welcome_workdir_seed(cx);
        let recents = WorkdirRecents::load().sorted_with_seed(seed.clone());
        let initial = seed
            .or_else(|| recents.first().map(|r| r.path.clone()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(windows_virtual_root);
        let mut picker = LocalDirPicker::new(index, profile.name.clone(), initial, recents);
        self.load_agent_dir_picker_dirs(&mut picker);
        self.agent_dir_picker = Some(picker);
        self.agent_dir_needs_focus = true;
        self.palette_open = false;
        self.app_menu_open = false;
        self.split_launcher_open = false;
        self.layout_manager_open = false;
        self.remote_dir_picker = None;
        cx.notify();
    }

    fn load_agent_dir_picker_dirs(&mut self, picker: &mut LocalDirPicker) {
        match read_local_dirs(&picker.current) {
            Ok(dirs) => picker.apply_dirs(dirs),
            Err(e) => {
                tracing::warn!(path = %picker.current.display(), error = %e, "read local workdir failed");
                picker.apply_dirs(Vec::new());
            }
        }
    }

    fn refresh_agent_dir_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(mut picker) = self.agent_dir_picker.take() {
            self.load_agent_dir_picker_dirs(&mut picker);
            self.agent_dir_picker = Some(picker);
            cx.notify();
        }
    }

    fn cancel_agent_dir_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_dir_picker = None;
        self.refocus_active(window, cx);
        cx.notify();
    }

    fn confirm_agent_dir_picker(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.agent_dir_picker.take() else {
            return;
        };
        let cwd = picker.launch_cwd();
        let mut recents = WorkdirRecents::load();
        recents.record(cwd.clone());
        recents.save();
        self.launch_in_active_tab_with_cwd(picker.agent_index, cwd, cx);
        cx.notify();
    }

    fn browse_agent_dir_picker(&mut self, cx: &mut Context<Self>) {
        let recv = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = recv.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |ws, cx| {
                        if let Some(mut picker) = ws.agent_dir_picker.take() {
                            picker.current = path.clone();
                            picker.selected = path;
                            picker.focus = LocalDirFocus::Directories;
                            picker.dir_sel = 0;
                            ws.load_agent_dir_picker_dirs(&mut picker);
                            ws.agent_dir_picker = Some(picker);
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    /// Welcome Agent workdir picker keys: Tab changes section, arrows navigate,
    /// Enter launches with the highlighted directory.
    fn on_agent_dir_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel_agent_dir_picker(window, cx),
            "tab" => {
                if let Some(picker) = self.agent_dir_picker.as_mut() {
                    if ev.keystroke.modifiers.shift {
                        picker.focus_prev();
                    } else {
                        picker.focus_next();
                    }
                    cx.notify();
                }
            }
            "up" => {
                if let Some(picker) = self.agent_dir_picker.as_mut() {
                    picker.move_selection(-1);
                    cx.notify();
                }
            }
            "down" => {
                if let Some(picker) = self.agent_dir_picker.as_mut() {
                    picker.move_selection(1);
                    cx.notify();
                }
            }
            "left" => {
                if self
                    .agent_dir_picker
                    .as_mut()
                    .and_then(LocalDirPicker::go_focused_parent)
                    .is_some()
                {
                    self.refresh_agent_dir_picker(cx);
                }
            }
            "right" => {
                let action = self
                    .agent_dir_picker
                    .as_mut()
                    .and_then(LocalDirPicker::open_focused_for_navigation);
                match action {
                    Some(LocalDirAction::Open(_)) => self.refresh_agent_dir_picker(cx),
                    Some(LocalDirAction::Browse) => self.browse_agent_dir_picker(cx),
                    None => {}
                }
            }
            "enter" => self.confirm_agent_dir_picker(cx),
            _ => {}
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
    fn on_divider_move(
        &mut self,
        ev: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            let extent = self
                .split_extents
                .borrow()
                .get(&d.path)
                .copied()
                .unwrap_or(0.0);
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
            if let Some(Node::Split { weights, .. }) =
                self.tabs[self.active].root.at_path_mut(&d.path)
            {
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

    fn leaf_exists(&self, id: PaneId) -> bool {
        self.panes.contains_key(&id)
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
        if launch.file_namespace == FileNamespace::Host && launch.cwd.is_none() {
            let inherited = {
                // The very first pane (TN_AUTOQUIT/DEMO) is spawned *before* its tab is
                // pushed, so `self.tabs[self.active]` would be out of bounds — fall back
                // to the explorer root then. `.get` keeps this safe for the first pane.
                self.tabs
                    .get(self.active)
                    .and_then(|tab| self.panes.get(&tab.focused))
                    .and_then(|v| v.read(cx).effective_host_process_cwd())
            };
            let explorer_root = self.explorer.read(cx).root_path();
            launch.cwd = inherited
                .or_else(|| explorer_root.filter(|root| is_host_process_path(root.as_path())));
        }
        let id = self.next_id;
        self.next_id += 1;
        self.install_pane(id, launch, cx);
        id
    }

    /// Create — or **re-create**, for SSH retry — the [`TerminalView`] for `id`,
    /// wire all its subscriptions, and store it. Re-installing an existing `id`
    /// drops the old view (its `Drop` kills the old backend) and reconnects with
    /// the same spec, keeping the pane's position in the tab tree (the tree refers
    /// to `id`, not the view). Used by the SSH error card's 重试.
    fn install_pane(&mut self, id: PaneId, launch: LaunchSpec, cx: &mut Context<Self>) {
        let config = self.config.clone();
        let view = cx.new(|cx| TerminalView::new(cx, config, launch.clone()));
        // Repaint the status bar when this pane's usage changes (only on change,
        // not on every terminal frame — that's why TerminalView emits an event
        // rather than relying on plain `notify`).
        cx.subscribe(&view, |_ws, _view, _ev: &UsageUpdated, cx| cx.notify())
            .detach();
        // Repaint workspace when the pane's CWD changes so the explorer follows it.
        cx.subscribe(&view, |_ws, _view, _ev: &CwdChanged, cx| {
            cx.notify();
        })
        .detach();
        // File watcher fired → refresh explorer tree and git tags.
        cx.subscribe(&view, |ws, _view, _ev: &FilesChanged, cx| {
            ws.explorer.update(cx, |explorer, cx| explorer.rebuild(cx));
        })
        .detach();
        // Agent activity-rail card click → open that file in Quick Look (Diff tab)
        // and record which pane + which file-index was clicked so ↑↓ nav stays
        // scoped to that rail's changed-file list.
        cx.subscribe(&view, move |ws, _view, ev: &OpenInQuickLook, cx| {
            let path = ev.0.clone();
            // Find the file index within this pane's rail for nav context.
            let file_idx = ws
                .panes
                .get(&id)
                .and_then(|v| v.read(cx).rail_find_idx(&path))
                .unwrap_or(0);
            ws.ql_rail_pane = Some(id);
            ws.ql_rail_idx = file_idx;
            ws.open_quick_look_diff_target(path, cx);
            cx.notify();
        })
        .detach();
        // SSH pane authenticated + shell open → record its target as a recent
        // connection (A1), tagged with the method that worked. The target lives in
        // this pane's spec (pane_specs[id].ssh).
        cx.subscribe(&view, move |ws, _view, ev: &SshConnected, cx| {
            if let Some(cfg) = ws.pane_specs.get(&id).and_then(|s| s.ssh.clone()) {
                ws.ssh_recents
                    .record(&cfg.host, &cfg.user, cfg.port, AuthBadge::from_pty(ev.0));
                ws.ssh_recents.save();
                cx.notify();
            }
        })
        .detach();
        // SSH error card 重试 → reconnect in place: rebuild this pane's view with
        // the same spec (same id/position), dropping the failed one.
        cx.subscribe(&view, move |ws, _view, _ev: &SshRetryRequested, cx| {
            if let Some(spec) = ws.pane_specs.get(&id).cloned() {
                ws.install_pane(id, spec, cx);
                // 磷光通道(规则 J):重连时宠物守在通道口,从远端 pane 边缘带回光点。
                ws.pet.update(cx, |p, cx| {
                    p.spatial_cue(crate::pet::PetSpatialCue::RemoteReconnect, cx)
                });
                cx.notify();
            }
        })
        .detach();
        // SSH progress/error card 取消 / 关闭 → close this pane.
        cx.subscribe(&view, move |ws, _view, _ev: &SshCloseRequested, cx| {
            ws.close_pane_id(id, cx);
        })
        .detach();
        // 记住密码(仅本会话, B3): cache into this pane's spec (RAM only, never on
        // disk) so a reconnect/retry reuses it instead of re-prompting.
        cx.subscribe(&view, move |ws, _view, ev: &SshRememberPassword, _cx| {
            if let Some(ssh) = ws.pane_specs.get_mut(&id).and_then(|s| s.ssh.as_mut()) {
                ssh.password = Some(ev.0.clone());
            }
        })
        .detach();
        self.panes.insert(id, view);
        self.pane_specs.insert(id, launch);
    }

    /// Close a specific pane by `id` (the SSH cards' 取消 / 关闭). Mirrors
    /// [`close_pane`](Self::close_pane) but targets an arbitrary id in whatever tab
    /// holds it, and defers focus to `render`'s focus reconciliation (no `Window`
    /// in an event callback). If this is the last pane in the last tab, fall back
    /// to the welcome launchpad so SSH cancel/close never leaves a dead card behind.
    fn close_pane_id(&mut self, id: PaneId, cx: &mut Context<Self>) {
        self.force_close_pane_id(id, cx);
    }

    fn force_close_pane_id(&mut self, id: PaneId, cx: &mut Context<Self>) {
        let Some(ti) = self.tabs.iter().position(|t| {
            let mut leaves = Vec::new();
            collect_leaves(&t.root, &mut leaves);
            leaves.contains(&id)
        }) else {
            return;
        };
        self.remove_pane_from_tab(ti, id);
        cx.notify();
    }

    fn remove_pane_from_tab(&mut self, ti: usize, id: PaneId) -> bool {
        if self.tabs[ti].root.leaf_count() <= 1 {
            // Last pane in its tab: drop the whole tab if another remains.
            if self.tabs.len() > 1 {
                self.panes.remove(&id);
                self.pane_specs.remove(&id);
                self.tabs.remove(ti);
                self.active = self.active.min(self.tabs.len() - 1);
            } else {
                self.panes.remove(&id);
                self.pane_specs.remove(&id);
                self.tabs[ti] = Tab::welcome();
                self.active = ti;
            }
            return false;
        }
        let root = std::mem::replace(&mut self.tabs[ti].root, Node::Leaf(0));
        self.tabs[ti].root = prune(root, id).expect("tree non-empty");
        self.panes.remove(&id);
        self.pane_specs.remove(&id);
        if self.tabs[ti].focused == id {
            self.tabs[ti].focused = first_leaf(&self.tabs[ti].root);
        }
        true
    }

    fn request_tab_close(&mut self, tab_index: usize, _cx: &mut Context<Self>) -> bool {
        if tab_index >= self.tabs.len() || self.tabs[tab_index].welcome {
            return true;
        }
        true
    }

    fn force_close_tab_index(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        if !self.tabs[i].welcome {
            let mut leaves = Vec::new();
            collect_leaves(&self.tabs[i].root, &mut leaves);
            for id in leaves {
                self.panes.remove(&id);
                self.pane_specs.remove(&id);
            }
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::welcome());
            self.active = 0;
        } else {
            self.active = self.active.min(self.tabs.len() - 1);
        }
        cx.notify();
    }

    fn finish_quit(&mut self, cx: &mut Context<Self>) {
        crate::platform::QUITTING.store(true, std::sync::atomic::Ordering::Release);
        if let Some(th) = cx.try_global::<crate::TrayHwnd>() {
            crate::platform::remove_tray_icon(th.0);
        }
        cx.quit();
    }

    /// Send `cd <dir>` to a *single* pane, mapping the explorer root to that
    /// pane's namespace. Scoped to one pane (not broadcast to all) so opening a
    /// folder only redirects the pane the user is focused on — agent panes are
    /// left untouched (their cwd is self-managed) and other panes keep their own
    /// directory (面板解耦:同步收敛到目标 pane).
    fn cd_pane_to_root(&self, id: PaneId, root: &ExplorerRoot, cx: &Context<Self>) {
        let Some(spec) = self.pane_specs.get(&id) else {
            return;
        };
        if spec.agent.is_some() {
            return;
        }
        let Some(view) = self.panes.get(&id) else {
            return;
        };
        let Some(target_path) = root.path_for_namespace(&spec.file_namespace) else {
            return;
        };
        let prog = spec.program.to_ascii_lowercase();
        let is_cmd = prog.contains("cmd");

        let line = if spec.ssh.is_some() || spec.file_namespace == FileNamespace::Ssh {
            format!("cd \"{target_path}\"\r")
        } else if matches!(spec.file_namespace, FileNamespace::Wsl { .. }) {
            format!("cd \"{target_path}\"\r")
        } else if is_cmd {
            if !is_host_process_path(std::path::Path::new(&target_path)) {
                return;
            }
            format!("cd /d \"{target_path}\"\r")
        } else {
            if !is_host_process_path(std::path::Path::new(&target_path)) {
                return;
            }
            format!("cd \"{target_path}\"\r")
        };
        view.read(cx).send_bytes(line.as_bytes());
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

    fn reset_explorer_for_welcome_tab(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let default_root = default_host_root();
        let current_root = self.explorer.read(cx).root();
        if !should_reset_explorer_for_welcome_tab(
            tab,
            self.explorer_pane,
            &current_root,
            &default_root,
        ) {
            return;
        }

        if let Some(prev) = self.explorer_pane {
            if prev != WELCOME_DUMMY && self.leaf_exists(prev) {
                let snap = self.explorer.read(cx).snapshot();
                self.explorer_states.insert(prev, snap);
            }
            let live_ids: std::collections::HashSet<PaneId> = self.panes.keys().copied().collect();
            self.explorer_states.retain(|id, _| live_ids.contains(id));
        }

        self.explorer
            .update(cx, |e, cx| e.set_browser_root(default_root, cx));
        self.explorer_pane = Some(WELCOME_DUMMY);
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
            // 磷光通道(规则 J):命令面板浮层打开 → 岗台探头 + 光点升起。
            self.pet.update(cx, |p, cx| {
                p.spatial_cue(crate::pet::PetSpatialCue::OverlayOpen, cx)
            });
        } else {
            self.refocus_active(window, cx);
        }
        cx.notify();
    }

    /// Show/hide the file explorer sidebar (Ctrl+Shift+B).
    fn toggle_explorer(
        &mut self,
        _: &ToggleExplorer,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.explorer_open = !self.explorer_open;
        cx.notify();
    }

    /// Show/hide the Quick Look overlay (Ctrl+Shift+J). On close, return focus to
    /// the active pane (we have a `window` here, so focus directly).
    fn toggle_quick_look(
        &mut self,
        _: &ToggleQuickLook,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quick_look_open = !self.quick_look_open;
        if !self.quick_look_open {
            let closed = self.quick_look.update(cx, |v, cx| v.request_close(cx));
            if closed {
                self.refocus_after_quick_look(window, cx);
                // 磷光通道(规则 J):宠物拉光点 → 面板收束 → 换岗回主岗台。
                self.pet.update(cx, |p, cx| {
                    p.spatial_cue(crate::pet::PetSpatialCue::QuickLookClose, cx)
                });
            } else {
                self.quick_look_open = true;
            }
        } else {
            // 磷光通道(规则 J):叼光点 → 面板贴边展开。
            self.pet.update(cx, |p, cx| {
                p.spatial_cue(crate::pet::PetSpatialCue::QuickLookOpen, cx)
            });
        }
        cx.notify();
    }

    fn open_quick_look_diff_target(&mut self, target: RailFileTarget, cx: &mut Context<Self>) {
        self.quick_look.update(cx, |v, cx| match target {
            RailFileTarget::Local(path) => v.open_diff(path, cx),
            RailFileTarget::Remote(file) => v.open_remote_diff(file, cx),
        });
        self.quick_look_open = true;
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

    fn request_quit(&mut self, cx: &mut Context<Self>) {
        if self.quick_look_open {
            let can_quit = self.quick_look.update(cx, |v, cx| v.request_quit(cx));
            if !can_quit {
                self.app_menu_open = false;
                cx.notify();
                return;
            }
            self.quick_look_open = false;
            self.ql_rail_pane = None;
            self.ql_refocus_active_pane = true;
        }
        self.finish_quit(cx);
    }

    /// The palette's current rows (aggregated + drill-resolved + query-filtered).
    fn palette_rows(&self) -> Vec<LaunchRow> {
        let mut rows = launch_rows(&self.launch_profiles, self.palette_wsl, &self.palette_query);
        // 宠物设置行(SHEET 06-A;规则「命令面板必须能打开宠物设置」):只在根
        // 列表追加,WSL 下钻/分屏启动器不掺(那两处只做会话启动)。
        if !self.palette_wsl {
            let q = self.palette_query.to_ascii_lowercase();
            if q.is_empty()
                || "宠物设置".contains(q.as_str())
                || "pet settings".contains(q.as_str())
            {
                rows.push(LaunchRow::PetSettings);
            }
        }
        rows
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
        let Some(row) = rows.get(self.palette_sel) else {
            return;
        };
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
            LaunchRow::PetSettings => {
                // 关面板 → 打开宠物设置菜单(确保宠物可见;键盘可达性规则)。
                self.palette_open = false;
                self.refocus_active(window, cx);
                self.pet.update(cx, |p, cx| p.open_settings(cx));
                cx.notify();
            }
            LaunchRow::SshPrompt => {
                self.palette_open = false;
                self.ssh_prompt_open = true;
                self.ssh_prompt_needs_focus = true;
                self.ssh_prompt_intent = Some(SshPromptIntent::Palette);
                self.ssh_prompt_input.clear();
                self.ssh_prompt_sel = 0;
                self.ssh_rename = None;
                self.ssh_rename_marked = None;
                self.ssh_config_hosts = tn_pty::list_ssh_config_hosts();
                cx.notify();
            }
        }
    }

    /// Launch the profile at `idx` in a new tab, then close the palette.
    fn launch_profile_in_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let reg = crate::agent_host::agent_registry(cx);
        let Some(spec) = self
            .launch_profiles
            .get(idx)
            .and_then(|p| LaunchSpec::from_profile(p, &reg))
        else {
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
        self.explorer_pane = None;
        cx.notify();
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
        self.evict_background_caches(cx);
        let fid = self.tabs[self.active].focused;
        self.focus_pane(fid, window, cx);
    }

    fn activate_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.evict_background_caches(cx);
            let fid = self.tabs[i].focused;
            self.focus_pane(fid, window, cx);
        }
    }

    /// Explicitly clear the GPUI render caches of all terminal panes in inactive tabs.
    /// This prevents massive GPUI `Div` trees from leaking linearly as the user
    /// opens more tabs and leaves them in the background.
    fn evict_background_caches(&mut self, cx: &mut Context<Self>) {
        let active_idx = self.active;
        for (i, tab) in self.tabs.iter().enumerate() {
            if i == active_idx {
                continue;
            }
            if tab.welcome {
                continue;
            }
            let mut leaves = Vec::new();
            collect_leaves(&tab.root, &mut leaves);
            for id in leaves {
                if let Some(view) = self.panes.get(&id) {
                    view.update(cx, |v, cx| v.clear_render_cache(cx));
                }
            }
        }
    }

    /// Close tab `i` entirely, dropping all its panes (which kills their child
    /// processes via `LocalPty`'s Drop). Never leaves zero tabs — closing the
    /// last one spawns a fresh default pane. Driven by the tab's `×` button.
    fn close_tab_index(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        if !self.request_tab_close(i, cx) {
            return;
        }
        self.force_close_tab_index(i, cx);
        if !self.tabs.is_empty() {
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
        // 磷光通道(规则 J):宠物沿新分隔线小跑,新 pane 像被划开。
        self.pet
            .update(cx, |p, cx| p.spatial_cue(crate::pet::PetSpatialCue::SplitCreate, cx));
    }

    /// `新会话` split direction (app menu). Maps to a (`Axis`, before?) split.
    fn split_session(
        &mut self,
        dir: SplitDir,
        spec: LaunchSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        let new_id = self.spawn_pane_with(cx, spec);
        if self.tabs[active].welcome {
            // No pane to split on a welcome tab — fill the tab with the session.
            self.tabs[active] = Tab::panes(Node::Leaf(new_id), new_id);
        } else {
            // Prefer the target snapshotted at `新会话` invocation (before the
            // launcher overlay stole focus); fall back to the live `focused` field.
            let target = self
                .split_target
                .take()
                .unwrap_or(self.tabs[active].focused);
            let ok = self.tabs[active]
                .root
                .split(target, new_id, dir.axis(), dir.before());
            if !ok {
                // `target` wasn't in the active tree (stale/dummy id) — splitting it
                // would orphan the new pane. Anchor to the first real leaf instead.
                let fallback = first_leaf(&self.tabs[active].root);
                self.tabs[active]
                    .root
                    .split(fallback, new_id, dir.axis(), dir.before());
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
        self.close_pane_id(target, cx);
        if !self.tabs.is_empty() && !self.tabs[self.active].welcome {
            let fid = self.tabs[self.active].focused;
            self.focus_pane(fid, window, cx);
        }
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
                let is_focused = id == focused;
                if let Some(view) = self.panes.get(&id).cloned() {
                    // 磷光板面:内层不透明 L1 基面(契约 1)+ 内缩 1px 圆角让外层
                    // 1px 发丝边露出(plate 范式);零投影,焦点 = 角标(契约 4/5)。
                    let inner = div()
                        .size_full()
                        .rounded(px(R_PANEL - 1.))
                        .overflow_hidden()
                        .bg(col(self.config.theme.ui.surface_1))
                        .child(view);
                    return plate(inner, is_focused)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, window, cx| {
                                this.focus_pane(id, window, cx);
                            }),
                        )
                        .into_any_element();
                }
                let inner = div()
                    .size_full()
                    .rounded(px(R_PANEL - 1.))
                    .overflow_hidden()
                    .bg(col(self.config.theme.ui.surface_1))
                    .child("Pane missing");
                plate(inner, is_focused)
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
                    // 磷光接缝:平铺板面间 2px 缝隙露出 L0 底盘(接缝即深度,
                    // 契约 4)。每侧 pad 1px,加起来正好 SEAM;padding 在
                    // relative(frac) 内 → 不溢出,divider seam (relative(cum)) 仍准。
                    let g = px(SEAM / 2.);
                    wrap = if row {
                        wrap.when(i > 0, |w| w.pl(g)).when(i < last, |w| w.pr(g))
                    } else {
                        wrap.when(i > 0, |w| w.pt(g)).when(i < last, |w| w.pb(g))
                    };
                    let mut child_path = path.clone();
                    child_path.push(i);
                    container =
                        container.child(wrap.child(self.render_node(kid, focused, cx, child_path)));
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
                let accent = self.config.theme.ui.accent;
                let mut cum = 0.0_f32;
                for gap in 0..kids.len().saturating_sub(1) {
                    cum += weights[gap] / sum;
                    let dpath = path.clone();
                    let start_weights = weights.clone();
                    let mut handle = div().absolute();
                    handle = if row {
                        handle
                            .top(px(0.))
                            .bottom(px(0.))
                            .left(relative(cum))
                            .w(px(8.))
                    } else {
                        handle
                            .left(px(0.))
                            .right(px(0.))
                            .top(relative(cum))
                            .h(px(8.))
                    };
                    container =
                        container.child(handle.hover(|s| s.bg(cola(accent, 0.16))).on_mouse_down(
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
                        ));
                }
                // Live preview: a thin accent line at the seam's target position
                // while this split is being dragged (weights only commit on release).
                if let Some(d) = self.divider_drag.as_ref().filter(|d| d.path == path) {
                    let extent = self
                        .split_extents
                        .borrow()
                        .get(&path)
                        .copied()
                        .unwrap_or(0.0);
                    let seam: f32 = weights[..=d.gap].iter().sum::<f32>() / sum;
                    let delta = if extent > 1.0 {
                        (d.cur_pos - d.start_pos) / extent
                    } else {
                        0.0
                    };
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

    /// Accent color for an agent: theme `[agents.<id>]` override → descriptor
    /// default → muted (shell). Agent-agnostic, resolved through the registry.
    fn agent_color(&self, agent: Option<&AgentId>, cx: &App) -> tn_config::Color {
        let t = &self.config.theme;
        match agent {
            Some(id) => {
                let reg = crate::agent_host::agent_registry(cx);
                t.agents
                    .accent_for(id.as_str())
                    .or_else(|| reg.get(id).and_then(|d| d.accent))
                    .unwrap_or(t.ui.muted)
            }
            None => t.ui.muted,
        }
    }

    /// Bottom status bar (M4) — the mockup's multi-segment readout: branch ·
    /// sessions · per-agent context % · … · viewer file·lang ·
    /// encoding · theme. The per-agent ctx is aggregated across panes (one
    /// segment per agent present); detailed tokens/cost live in the pane's
    /// agent header (R2).
    fn render_status_bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let t = &self.config.theme;
        let ui = &t.ui;
        let mono = SharedString::from(self.config.font().family.clone());

        // Aggregate context % per agent across all panes (first pane per agent id
        // wins). Agent-agnostic: one segment per distinct agent present, in the
        // order encountered — no fixed Claude/Codex slots.
        let mut agent_pcts: Vec<(AgentId, u32)> = Vec::new();
        for v in self.panes.values() {
            let v = v.read(cx);
            if let (Some(id), Some(u)) = (v.agent(), v.usage()) {
                if !agent_pcts.iter().any(|(a, _)| a == &id) {
                    let pct = (u.context_frac() * 100.0).round() as u32;
                    agent_pcts.push((id, pct));
                }
            }
        }

        // SHEET 01 `.sb-seg`:仪表读数段 — 全高、px 10、段间 1px h0、hover L1。
        let seg = |children: Vec<AnyElement>| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .h_full()
                .border_l(px(1.))
                .border_color(rgba(H0))
                .hover(|s| s.bg(col(ui.surface_1)).text_color(rgb(T1)))
                .children(children)
        };
        // `.sb-dot`:5px 磷光圆点(idle = t3)。
        let dot = |live: bool| {
            div()
                .w(px(5.))
                .h(px(5.))
                .rounded_full()
                .flex_none()
                .bg(if live { rgb(PH) } else { rgb(T3) })
        };

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(STATUSBAR_H))
            .px(px(10.))
            .flex_none()
            .border_t(px(1.))
            .border_color(rgba(H0))
            .bg(col(ui.chrome_bg)) // L0:状态栏坐在底盘上
            .font_family(mono) // 仪表读数全等宽(SHEET 01)
            .text_size(px(10.))
            .text_color(rgb(T2));

        // branch
        bar = bar.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .h_full()
                .hover(|s| s.bg(col(ui.surface_1)).text_color(rgb(T1)))
                .child(dot(true))
                .child(SharedString::from(
                    self.branch.clone().unwrap_or_else(|| "—".into()),
                )),
        );
        // sessions:只数**非欢迎** tab(欢迎页是 Launchpad,不是会话)。纯新标签欢迎页
        // → 「NO SESSION」+ idle 灰点;有实会话 → 「N SESSIONS」+ 磷光实点(区分新标签
        // 欢迎页与已有工作区欢迎 tab,SHEET 07 板 A / A2)。
        let session_tabs = self.tabs.iter().filter(|t| !t.welcome).count();
        let session_label = if session_tabs == 0 {
            "NO SESSION".to_string()
        } else {
            format!(
                "{} SESSION{}",
                session_tabs,
                if session_tabs == 1 { "" } else { "S" }
            )
        };
        bar = bar.child(seg(vec![
            dot(session_tabs > 0).into_any_element(),
            div()
                .when(session_tabs > 0, |d| d.text_color(rgb(PH)))
                .child(SharedString::from(session_label))
                .into_any_element(),
        ]));
        // per-agent context readouts(活值 = 磷光,SHEET 02)
        for (id, p) in &agent_pcts {
            bar = bar.child(seg(vec![div()
                .text_color(rgb(PH))
                .child(SharedString::from(format!(
                    "{} CTX {p}%",
                    id.as_str().to_ascii_uppercase()
                )))
                .into_any_element()]));
        }

        bar = bar.child(div().flex_1());

        // right cluster: quick look file·lang, encoding, theme, pet
        if let Some((name, lang)) = self.quick_look.read(cx).status() {
            bar = bar.child(seg(vec![div()
                .text_color(rgb(T1))
                .child(SharedString::from(format!("{name} · {lang}")))
                .into_any_element()]));
        }
        bar = bar.child(seg(vec![div().child("UTF-8 · LF").into_any_element()]));
        bar = bar.child(seg(vec![div()
            .child(SharedString::from(t.name.to_ascii_uppercase()))
            .into_any_element()]));
        // 宠物法定席位:WESTIE · IDLE(点击 = 显隐开关;SHEET 05 板 A)
        if let Some(pet_seg) = self.pet.read(cx).status_segment() {
            bar = bar.child(
                div()
                    .id("pet-seg")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .h_full()
                    .border_l(px(1.))
                    .border_color(rgba(H0))
                    .hover(|s| s.bg(col(ui.surface_1)).text_color(rgb(T1)))
                    .child(dot(pet_seg.live))
                    .child(SharedString::from(pet_seg.label))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.pet.update(cx, |p, cx| p.toggle_visible(cx));
                        }),
                    ),
            );
        }
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

        // SHEET 01 板 B `.mi`:行高 30 · px 10 · gap 10 · r4 · sans 12 · t1;
        // hover = L4 + t0(不透明抬升,无 alpha hover)。danger(退出)= err。
        // 右列二选一:kbd 键帽(全拼,差异 1-10)或 note 注记(mono 10 t3,
        // 差异 1-9:布局「7 槽位」、设置「config.toml」)。
        let mi = |icon_name: &'static str,
                  label: &'static str,
                  key: Option<&'static str>,
                  note: Option<&'static str>,
                  danger: bool,
                  act: Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>| {
            let crest = col(ui.palette_selected); // L4
            let fg = if danger {
                rgb(crate::style::ERR)
            } else {
                rgb(T1)
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .h(px(30.))
                .px(px(10.))
                .rounded(px(R_CARD))
                .text_size(px(12.))
                .text_color(fg)
                .hover(move |s| s.bg(crest).text_color(rgb(T0)))
                .child(icon(
                    icon_name,
                    14.,
                    if danger {
                        tn_config::Color::new(0xE8, 0x70, 0x7E)
                    } else {
                        tn_config::Color::new(0x3E, 0x48, 0x60) // t3 结构字符
                    },
                ))
                .child(div().child(label))
                .child(div().flex_1())
                .when_some(note, |d, n| {
                    d.child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(10.))
                            .text_color(rgb(crate::style::T3))
                            .child(n),
                    )
                })
                .when_some(key, |d, k| {
                    d.child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(10.))
                            .text_color(rgb(T2))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(R_CHIP))
                            .bg(col(ui.surface_2)) // .kbd:L2 + h1 边
                            .border_1()
                            .border_color(rgba(H1))
                            .child(k),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, window, cx| {
                        cx.stop_propagation();
                        this.app_menu_open = false;
                        act(this, window, cx);
                        cx.notify();
                    }),
                )
        };
        let sep = || div().h(px(1.)).mx(px(8.)).my(px(5.)).bg(rgba(H1)); // .msep

        // `.float.menu`:L3 浮板 + h2 边 + 浮层投影(全系统唯一投影,契约 4)。
        let popup = shadowed(
            div()
                .absolute()
                .left(px(10.))
                .top(px(TITLEBAR_H + 4.))
                .w(px(248.))
                .p(px(5.))
                .rounded(px(R_PANEL))
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg)) // L3 不透明浮板
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _e, _w, cx| cx.stop_propagation()),
                )
                // SHEET 01-B 分组:[新会话,新标签,打开文件夹…|布局,文件浏览器|
                // 设置,主题,重载配置|关于,退出](差异 1-8)。
                .child(mi(
                    "spark",
                    "新会话…",
                    Some("Ctrl+Shift+N"),
                    None,
                    false,
                    Box::new(|this, w, cx| this.new_session(&NewSession, w, cx)),
                ))
                .child(mi(
                    "plus",
                    "新标签",
                    Some("Ctrl+Shift+T"),
                    None,
                    false,
                    Box::new(|this, w, cx| this.new_tab(&NewTab, w, cx)),
                ))
                .child(mi(
                    "folder",
                    "打开文件夹…",
                    None,
                    None,
                    false,
                    Box::new(|this, _w, cx| this.menu_open_folder(cx)),
                ))
                .child(sep())
                .child(mi(
                    "max",
                    "布局",
                    None,
                    Some("7 槽位"),
                    false,
                    Box::new(|this, _w, cx| this.open_layout_manager(cx)),
                ))
                .child(mi(
                    "sidebar",
                    "文件浏览器",
                    Some("Ctrl+Shift+B"),
                    None,
                    false,
                    Box::new(|this, w, cx| this.toggle_explorer(&ToggleExplorer, w, cx)),
                ))
                .child(sep())
                // 设置 → open config.toml in our own Quick Look editor (Ctrl+S to save).
                .child(mi(
                    "sliders",
                    "设置",
                    None,
                    Some("config.toml"),
                    false,
                    Box::new(|this, _w, cx| {
                        if let Some(p) = tn_config::config_path() {
                            this.quick_look.update(cx, |v, cx| {
                                v.open_for_edit(p, cx);
                            });
                            this.quick_look_open = true;
                        }
                    }),
                ))
                // 主题 — only one theme for now (the default). A real picker comes
                // when there is more than one theme.
                .child(mi(
                    "moon",
                    "主题 · Tn Dark",
                    None,
                    None,
                    false,
                    Box::new(|_t, _w, _cx| {}),
                ))
                // 重载配置 = 非破坏热重载(读你改过的 config),与 Ctrl+Shift+R 同一
                // 动作(SHEET 01-B;差异 1-9 曾错挂「重置为默认」的危险语义,重置
                // 不再有菜单一键入口)。
                .child(mi(
                    "refresh",
                    "重载配置",
                    Some("Ctrl+Shift+R"),
                    None,
                    false,
                    Box::new(|this, w, cx| this.reload_config(&ReloadConfig, w, cx)),
                ))
                .child(sep())
                .child(mi(
                    "info",
                    "关于 Tn",
                    None,
                    None,
                    false,
                    Box::new(|_t, _w, cx| {
                        if let Ok(p) = std::env::current_dir() {
                            let readme = p.join("README.md");
                            if readme.exists() {
                                cx.open_with_system(&readme);
                            }
                        }
                    }),
                ))
                .child(mi(
                    "power",
                    "退出",
                    Some("Ctrl+Shift+Q"),
                    None,
                    true,
                    Box::new(|this, _w, cx| this.request_quit(cx)),
                )),
            shadow_float(),
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
    /// **and** `cd` the *focused* pane into the chosen folder. Scoped to the one
    /// pane the user is on — other panes keep their own directory, and agent panes
    /// are never `cd`'d (面板解耦:同步收敛到目标 pane).
    fn menu_open_folder(&mut self, cx: &mut Context<Self>) {
        // Target = the pane the user is focused on. Capture it now (sync) so the
        // async picker callback re-roots / cds the right pane.
        let Some(target) = self.tabs.get(self.active).map(|t| t.focused) else {
            return;
        };
        if !open_folder_should_use_native_picker(self.pane_specs.get(&target)) {
            // Prefer the pane's live cwd; if it isn't known yet (remote shell
            // integration hasn't reported it), fall back to the filesystem root so
            // the picker still opens and the user can navigate from `/`.
            let root = self
                .panes
                .get(&target)
                .and_then(|view| {
                    explorer_root_for_pane(&view.read(cx), self.pane_specs.get(&target))
                })
                .or_else(|| fallback_remote_root(self.pane_specs.get(&target)));
            if let Some(root) = root {
                self.open_remote_dir_picker(target, root, cx);
            }
            cx.notify();
            return;
        }
        let recv = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = recv.await {
                if let Some(p) = paths.into_iter().next() {
                    let picked_root = ExplorerRoot::from_accessible_path(p.clone());
                    let _ = this.update(cx, |ws, cx| {
                        ws.explorer_open = true;
                        // The explorer now shows the target pane; explicit「打开文件夹」
                        // = fresh tree (set_browser_root clears expansion/selection).
                        ws.explorer_pane = Some(target);
                        ws.explorer
                            .update(cx, |e, cx| e.set_browser_root(picked_root.clone(), cx));
                        ws.cd_pane_to_root(target, &picked_root, cx);
                        if let Some(path) = picked_root.path_buf() {
                            if let Some(view) = ws.panes.get(&target) {
                                view.update(cx, |v, cx| v.set_rail_root(&path, cx));
                            }
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn open_remote_dir_picker(
        &mut self,
        target: PaneId,
        root: ExplorerRoot,
        cx: &mut Context<Self>,
    ) {
        // SSH → SFTP source at the remote cwd; WSL → local \\wsl$ source at the
        // Linux cwd. Either way the picker navigates Linux-style paths.
        let (source, current) = if let Some(cfg) = root.ssh_config().cloned() {
            let Some(path) = root.remote_path().cloned() else {
                return;
            };
            (PickerSource::Ssh(cfg), path)
        } else if let Some((distro, linux)) = root.wsl_parts() {
            (PickerSource::Wsl { distro }, RemotePath::new(linux))
        } else {
            return;
        };
        self.app_menu_open = false;
        self.remote_dir_picker = Some(RemoteDirPicker::new(target, source, current));
        self.remote_dir_needs_focus = true;
        self.load_remote_dir_picker(cx);
        cx.notify();
    }

    fn load_remote_dir_picker(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.remote_dir_picker.as_mut() else {
            return;
        };
        let generation = picker.begin_load();
        let source = picker.source.clone();
        let path = picker.current.clone();
        let remote_fs = self.remote_fs.clone();
        cx.notify();

        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = exec
                .spawn(async move {
                    match source {
                        // SSH: list over SFTP, drop the `RemoteId` shape.
                        PickerSource::Ssh(cfg) => remote_fs.list_dir(&cfg, &path).map(|entries| {
                            entries
                                .into_iter()
                                .map(|e| PickerEntry {
                                    name: e.name,
                                    path: e.id.path,
                                    is_dir: e.is_dir,
                                })
                                .collect::<Vec<_>>()
                        }),
                        // WSL: list the local \\wsl$ UNC dir via std::fs.
                        PickerSource::Wsl { distro } => list_wsl_dir(&distro, &path),
                    }
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let Some(picker) = ws.remote_dir_picker.as_mut() else {
                    return;
                };
                if picker.generation != generation {
                    return;
                }
                match result {
                    Ok(entries) => picker.apply_entries(entries),
                    Err(e) => picker.apply_error(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_remote_dir_picker(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.remote_dir_picker.take() else {
            return;
        };
        let root = match &picker.source {
            PickerSource::Ssh(cfg) => ExplorerRoot::ssh(cfg.clone(), picker.current.as_str()),
            PickerSource::Wsl { distro } => {
                let linux = picker.current.as_str();
                let unc = wsl_unc_from_linux_cwd(distro, linux);
                ExplorerRoot::wsl(distro.clone(), linux.to_string(), unc)
            }
        };
        self.explorer_open = true;
        self.explorer_pane = Some(picker.target);
        self.explorer
            .update(cx, |e, cx| e.set_browser_root(root.clone(), cx));
        self.cd_pane_to_root(picker.target, &root, cx);
        cx.notify();
    }

    fn cancel_remote_dir_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remote_dir_picker = None;
        self.refocus_active(window, cx);
        cx.notify();
    }

    fn on_remote_dir_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        let key = ev.keystroke.key.as_str();
        match remote_dir_key_action(
            key,
            ev.keystroke.modifiers.control,
            ev.keystroke.modifiers.platform,
        ) {
            RemoteDirKeyAction::Cancel => self.cancel_remote_dir_picker(window, cx),
            RemoteDirKeyAction::MoveUp => {
                if let Some(picker) = self.remote_dir_picker.as_mut() {
                    picker.move_selection(-1);
                    let sel = picker.selected;
                    self.remote_dir_scroll
                        .scroll_to_item(sel, ScrollStrategy::Center);
                    cx.notify();
                }
            }
            RemoteDirKeyAction::MoveDown => {
                if let Some(picker) = self.remote_dir_picker.as_mut() {
                    picker.move_selection(1);
                    let sel = picker.selected;
                    self.remote_dir_scroll
                        .scroll_to_item(sel, ScrollStrategy::Center);
                    cx.notify();
                }
            }
            RemoteDirKeyAction::Parent => {
                if self
                    .remote_dir_picker
                    .as_mut()
                    .is_some_and(|picker| picker.go_parent())
                {
                    self.load_remote_dir_picker(cx);
                }
            }
            RemoteDirKeyAction::EnterDirectory => {
                if self
                    .remote_dir_picker
                    .as_mut()
                    .is_some_and(|picker| picker.enter_selected())
                {
                    self.load_remote_dir_picker(cx);
                }
            }
            RemoteDirKeyAction::Confirm => self.confirm_remote_dir_picker(cx),
            RemoteDirKeyAction::Reload => self.load_remote_dir_picker(cx),
            RemoteDirKeyAction::Ignore => {}
        }
    }

    fn render_remote_dir_picker(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let picker = self.remote_dir_picker.as_ref()?;
        let t = &self.config.theme;
        let ui = &t.ui;
        let dirs = picker.visible_dirs();
        let selected = picker.selected.min(dirs.len().saturating_sub(1));
        let target = picker.source_label();
        let current = picker.current.as_str().to_string();

        const ROW_H: f32 = 36.0; // SHEET 06 `.prow` 高 36
        let crest = ui.palette_selected; // L4:hover / 选中抬升
        let mut list = div().flex().flex_col().gap(px(2.)).p(px(6.));
        // Clickable「上级目录」row — the only mouse path *up* the tree (dir rows
        // only go deeper). Without it you can't get from /root to /usr or /mnt
        // without the keyboard. Shown whenever a parent exists (i.e. not at `/`).
        // Kept fixed above the scroll list so it's always reachable.
        if let Some(parent) = picker.current.parent() {
            let parent_path = parent.clone();
            let crest_bg = col(crest);
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(ROW_H))
                    .gap(px(9.))
                    .px(px(11.))
                    .rounded(px(R_CHIP))
                    .hover(move |s| s.bg(crest_bg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            if let Some(picker) = this.remote_dir_picker.as_mut() {
                                picker.current = parent_path.clone();
                                picker.entries.clear();
                                picker.selected = 0;
                            }
                            this.load_remote_dir_picker(cx);
                        }),
                    )
                    .child(icon("folder", 14., ui.muted))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.5))
                            .text_color(col(ui.muted))
                            .child(".. 上级目录"),
                    ),
            );
        }
        if picker.loading {
            list = list.child(
                div()
                    .px(px(12.))
                    .py(px(14.))
                    .text_size(px(12.))
                    .text_color(col(ui.muted))
                    .child("正在读取远端目录..."),
            );
        } else if let Some(error) = &picker.error {
            list = list.child(
                div()
                    .px(px(12.))
                    .py(px(14.))
                    .text_size(px(12.))
                    .text_color(col(t.ansi.red))
                    .child(SharedString::from(format!("读取失败: {error}"))),
            );
        } else if dirs.is_empty() {
            list = list.child(
                div()
                    .px(px(12.))
                    .py(px(14.))
                    .text_size(px(12.))
                    .text_color(col(ui.muted))
                    .child("此目录没有可进入的子目录"),
            );
        } else {
            // Virtualized + scrollable list (mouse wheel + keyboard scroll-to-selected)
            // so long directories (`/` has dozens of entries) aren't clipped. The
            // container gets a definite height = min(content, 320) for uniform_list's
            // auto-sizing to scroll past it. `'static` closure → capture a weak handle.
            let dirs_rc: std::rc::Rc<Vec<PickerEntry>> = std::rc::Rc::new(dirs.clone());
            let entity = cx.entity().downgrade();
            // 目录 = info 蓝(SHEET 02 树语法)
            let info = tn_config::Color::new((INFO >> 16) as u8, (INFO >> 8) as u8, INFO as u8);
            let fg = ui.foreground;
            let count = dirs_rc.len();
            let list_h = (count as f32 * ROW_H).clamp(ROW_H, 320.0);
            let crest_bg = col(crest);
            list = list.child(
                div().h(px(list_h)).flex().flex_col().min_h(px(0.)).child(
                    uniform_list("remote-dir-list", count, move |range, _w, _cx| {
                        range
                            .map(|i| {
                                let entry = &dirs_rc[i];
                                let is_sel = i == selected;
                                let path = entry.path.clone();
                                let name = entry.name.clone();
                                let entity = entity.clone();
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .h(px(ROW_H))
                                    .gap(px(9.))
                                    .px(px(11.))
                                    .rounded(px(R_CHIP))
                                    .relative()
                                    // 选中 = L4 + 左 2px 磷光脊(浮层家族统一语法)
                                    .when(is_sel, |d| {
                                        d.bg(crest_bg).child(
                                            div()
                                                .absolute()
                                                .left(px(0.))
                                                .top(px(6.))
                                                .bottom(px(6.))
                                                .w(px(2.))
                                                .bg(rgb(PH)),
                                        )
                                    })
                                    .when(!is_sel, |d| d.hover(move |s| s.bg(crest_bg)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_e: &MouseDownEvent, _w, app| {
                                            let _ = entity.update(app, |this, cx| {
                                                if let Some(picker) =
                                                    this.remote_dir_picker.as_mut()
                                                {
                                                    picker.current = path.clone();
                                                    picker.entries.clear();
                                                    picker.selected = 0;
                                                }
                                                this.load_remote_dir_picker(cx);
                                            });
                                            app.stop_propagation();
                                        },
                                    )
                                    .child(icon("folder", 14., info))
                                    .child(
                                        div()
                                            .flex_1()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_size(px(12.))
                                            .text_color(col(fg))
                                            .child(SharedString::from(name)),
                                    )
                            })
                            .collect::<Vec<_>>()
                    })
                    .flex_1()
                    .min_h(px(0.))
                    .track_scroll(self.remote_dir_scroll.clone()),
                ),
            );
        }

        let mono = SharedString::from(self.config.font().family.clone());
        // 浮层家族:L3 面 + h2 边 + r6 + float 投影;head = L4 / foot = kbd 提示。
        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(560.))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg)) // L3 不透明浮板
                .child(
                    // float-head:高 38 · L4 · 底 1px h1 · mono 12
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(38.))
                        .px(px(14.))
                        .flex_none()
                        .bg(col(ui.palette_selected))
                        .border_b(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .text_color(rgb(T1))
                        .child(icon("folder", 14., ui.accent_alt))
                        .child(div().text_color(rgb(T0)).child("选择远端目录"))
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(T2))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(format!("{target}:{current}"))),
                        ),
                )
                .child(list)
                .child(
                    // float-foot:kbd 提示 + 操作按钮(高 46 动作脚,SHEET 06)
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .h(px(46.))
                        .px(px(14.))
                        .flex_none()
                        .border_t(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .text_size(px(10.))
                        .text_color(rgb(T2))
                        .child(crate::style::kbd("↑↓", mono.clone()))
                        .child(div().child("选择"))
                        .child(crate::style::kbd("←", mono.clone()))
                        .child(div().child("上级"))
                        .child(crate::style::kbd("→", mono.clone()))
                        .child(div().child("进入"))
                        .child(crate::style::kbd("Enter", mono.clone()))
                        .child(div().child("确认"))
                        .child(div().flex_1())
                        .child(crate::style::btn("取消").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| {
                                this.cancel_remote_dir_picker(w, cx);
                            }),
                        ))
                        .child(crate::style::btn_primary("确认").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                this.confirm_remote_dir_picker(cx);
                            }),
                        )),
                ),
            shadow_float(),
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(SCRIM)) // 纯色压暗 scrim,无模糊(契约 7)
                .track_focus(&self.remote_dir_focus)
                .on_key_down(
                    cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_remote_dir_key(ev, w, cx)),
                )
                // 浮层 scrim 统一吞滚轮,不驱动底层终端(BUG发现 #5 同类)。
                .on_scroll_wheel(cx.listener(|_t, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
                .child(div().h(px(110.)))
                .child(panel),
        )
    }

    /// Panic button: overwrite the on-disk `config.toml` + `themes/tn-dark.toml`
    /// with the built-in defaults, then reload — recovering from a broken
    /// hand-edited config. **Destructive**: discards user edits. 菜单的「重载配置」
    /// 已按 SHEET 01-B 接回非破坏热重载,本钮暂无 UI 入口(危险操作不一键化);
    /// 留作将来「重置配置」确认流程的后端。
    #[allow(dead_code)]
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

    fn ssh_commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.ssh_rename.take() else {
            return;
        };
        self.ssh_rename_marked = None;
        self.ssh_recents
            .rename(&draft.host, &draft.user, draft.port, &draft.name);
        self.ssh_recents.save();
        cx.notify();
    }

    fn ssh_cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.ssh_rename = None;
        self.ssh_rename_marked = None;
        cx.notify();
    }

    fn on_ssh_prompt_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = ev.keystroke.key.as_str();
        // Printable ASCII (no modifiers, no IME CJK slip-through) for the live
        // filter box.
        let printable = |ev: &KeyDownEvent| -> Option<String> {
            ev.keystroke
                .key_char
                .as_ref()
                .filter(|c| {
                    !ev.keystroke.modifiers.control
                        && !ev.keystroke.modifiers.alt
                        && !ev.keystroke.modifiers.platform
                        && c.chars().all(|ch| ch.is_ascii_graphic() || ch == ' ')
                })
                .cloned()
        };

        if self.ssh_rename.is_some() {
            match key {
                "escape" => self.ssh_cancel_rename(cx),
                "enter" => self.ssh_commit_rename(cx),
                "backspace" => {
                    if self.ssh_rename_marked.take().is_some() {
                        cx.notify();
                    } else if let Some(d) = self.ssh_rename.as_mut() {
                        d.name.pop();
                        cx.notify();
                    }
                }
                _ => {
                    if let Some(c) = printable(ev) {
                        if let Some(d) = self.ssh_rename.as_mut() {
                            d.name.push_str(&c);
                        }
                        cx.notify();
                    }
                }
            }
            cx.stop_propagation();
            return;
        }

        // The input box doubles as a live filter over the combined recents +
        // ssh-config list.
        let rows = self.ssh_conn_rows();
        let n = rows.len();
        match key {
            "escape" => {
                self.ssh_prompt_open = false;
                self.ssh_prompt_intent = None;
                self.ssh_rename = None;
                self.ssh_rename_marked = None;
                self.refocus_active(window, cx);
                cx.notify();
            }
            "down" => {
                if n > 0 {
                    self.ssh_prompt_sel = (self.ssh_prompt_sel + 1).min(n - 1);
                    cx.notify();
                }
            }
            "up" => {
                self.ssh_prompt_sel = self.ssh_prompt_sel.saturating_sub(1);
                cx.notify();
            }
            "enter" => {
                // Selected row → connect it; no rows → connect whatever was typed.
                let target: Option<String> = if n > 0 {
                    let i = self.ssh_prompt_sel.min(n - 1);
                    Some(match &rows[i] {
                        SshConnRow::Recent { target, .. } => target.clone(),
                        // Connect via the alias so ssh-config's HostName/User/Port/
                        // IdentityFile all apply (SshConfig::parse re-queries config).
                        SshConnRow::Config { alias, .. } => alias.clone(),
                    })
                } else {
                    let t = self.ssh_prompt_input.trim();
                    // C2: refuse an obviously-bad typed target (the red hint already
                    // tells the user why); selected rows are always valid.
                    if validate_ssh_target(t).is_err() {
                        cx.stop_propagation();
                        return;
                    }
                    (!t.is_empty()).then(|| t.to_string())
                };
                match target {
                    Some(target) => self.ssh_connect(&target, window, cx),
                    None => {
                        self.ssh_prompt_open = false;
                        self.ssh_prompt_intent = None;
                        self.ssh_rename = None;
                        self.ssh_rename_marked = None;
                        self.refocus_active(window, cx);
                        cx.notify();
                    }
                }
            }
            "backspace" => {
                self.ssh_prompt_input.pop();
                self.ssh_prompt_sel = 0;
                cx.notify();
            }
            _ => {
                if let Some(c) = printable(ev) {
                    self.ssh_prompt_input.push_str(&c);
                    self.ssh_prompt_sel = 0;
                    cx.notify();
                }
            }
        }
        cx.stop_propagation();
    }

    /// The connector's combined rows: favorites/recents (`ssh_recents.json`) first,
    /// then read-only ssh-config aliases — both filtered by the input box and
    /// deduped by endpoint.
    /// Owned so the per-row click listeners can capture freely.
    fn ssh_conn_rows(&self) -> Vec<SshConnRow> {
        let q = self.ssh_prompt_input.trim();
        let ql = q.to_ascii_lowercase();
        let matches = |s: &str| ql.is_empty() || s.to_ascii_lowercase().contains(ql.as_str());

        let mut seen_eps: std::collections::HashSet<(String, String, u16)> =
            std::collections::HashSet::new();
        let mut rows: Vec<SshConnRow> = Vec::new();

        // Auto-recents: favorites stay pinned by `SshRecents`, non-favorites keep
        // the bounded recent-history behavior.
        for r in self.ssh_recents.filtered(q) {
            if seen_eps.contains(&(r.host.to_ascii_lowercase(), r.user.clone(), r.port)) {
                continue;
            }
            seen_eps.insert((r.host.to_ascii_lowercase(), r.user.clone(), r.port));
            rows.push(SshConnRow::Recent {
                host: r.host.clone(),
                user: r.user.clone(),
                port: r.port,
                name: r.name.clone(),
                target: r.target(),
                favorite: r.favorite,
                auth: r.auth,
                last_used: r.last_used,
            });
        }

        // ssh-config Host aliases (A4), minus endpoints already shown above.
        for h in &self.ssh_config_hosts {
            let user = h.user.clone().unwrap_or_default();
            if seen_eps.contains(&(h.host.to_ascii_lowercase(), user.clone(), h.port)) {
                continue;
            }
            let target = crate::ssh_recents::format_target(&user, &h.host, h.port);
            if !(matches(&h.alias) || matches(&h.host) || matches(&user) || matches(&target)) {
                continue;
            }
            seen_eps.insert((h.host.to_ascii_lowercase(), user, h.port));
            rows.push(SshConnRow::Config {
                alias: h.alias.clone(),
                target,
            });
        }
        rows
    }

    /// Connect to an SSH target (`user@host[:port]`) and dispatch to the recorded
    /// intent (welcome / palette / split). Closes the connector. Used by both the
    /// typed-target path and clicking/selecting a recent.
    fn ssh_connect(&mut self, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ssh_prompt_open = false;
        self.ssh_prompt_input.clear();
        self.ssh_prompt_sel = 0;
        self.ssh_rename = None;
        self.ssh_rename_marked = None;
        cx.notify();
        let target = target.trim();
        if target.is_empty() {
            self.ssh_prompt_intent = None;
            return;
        }
        let cfg = tn_pty::SshConfig::parse(target, None);
        let mut program = cfg.host.clone();
        if cfg.user != "root" && !cfg.user.is_empty() {
            program = format!("{}@{}", cfg.user, cfg.host);
        }
        let spec = LaunchSpec {
            program,
            args: vec![],
            env: vec![],
            integrate_pwsh: false,
            shell_integration: None,
            agent: None,
            ssh: Some(cfg),
            cwd: None,
            file_namespace: FileNamespace::Ssh,
        };
        match self.ssh_prompt_intent.take() {
            Some(SshPromptIntent::Welcome) => {
                let id = self.spawn_pane_with(cx, spec);
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.root = Node::Leaf(id);
                    tab.focused = id;
                    tab.welcome = false;
                }
            }
            Some(SshPromptIntent::Palette) => {
                let id = self.spawn_pane_with(cx, spec);
                self.tabs.push(Tab::panes(Node::Leaf(id), id));
                self.active = self.tabs.len() - 1;
                self.focus_pane(id, window, cx);
            }
            Some(SshPromptIntent::Split(dir)) => {
                self.split_session(dir, spec, window, cx);
            }
            None => {}
        }
    }

    fn render_ssh_prompt(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.ssh_prompt_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let mono = SharedString::from(self.config.font().family.clone());
        let placeholder = "user@host:port  (例: root@192.168.1.1:22)";
        let typed = self.ssh_prompt_input.trim().to_string();

        // ── live-parse chips: cheap user@host:port split (no IO) for the input row ──
        let chips = parse_ssh_target_chips(&typed);
        // `.chip`:1px h1 · r3 · mono 10(磷光芯片,无胶囊)
        let chip = |label: &str, val: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(R_CHIP))
                .border_1()
                .border_color(rgba(H1))
                .font_family(mono.clone())
                .text_size(px(10.))
                .child(
                    div()
                        .text_color(rgb(T2))
                        .child(SharedString::from(label.to_string())),
                )
                .child(
                    div()
                        .text_color(col(ui.accent))
                        .child(SharedString::from(val)),
                )
        };
        let chips_row = chips.as_ref().map(|(user, host, port)| {
            let mut r = div().flex().flex_row().items_center().gap(px(5.));
            if let Some(u) = user {
                r = r.child(chip("user", u.clone()));
            }
            r = r.child(chip("host", host.clone()));
            if let Some(p) = port {
                r = r.child(chip("port", p.clone()));
            }
            r
        });
        // C2 pre-dial validation: a bad typed target shows a red chip in place of
        // the parse chips (and Enter is gated in the key handler).
        let ssh_err = validate_ssh_target(&typed).err();
        let red = t.ansi.red;
        let err_chip = ssh_err.map(|msg| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(R_CHIP))
                .border_1()
                .border_color(cola(red, 0.3))
                .bg(rgba(ERR_SOFT))
                .text_size(px(10.))
                .child(icon("alert", 11., red))
                .child(div().text_color(col(red)).child(SharedString::from(msg)))
        });

        let input = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .py(px(13.))
            .text_size(px(14.))
            .child(div().child(icon("globe", 16., ui.muted)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .font_family(mono.clone())
                    .when(!self.ssh_prompt_input.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(col(ui.foreground))
                                .child(SharedString::from(self.ssh_prompt_input.clone())),
                        )
                    })
                    .child(
                        // 磷光块光标(`.cur`):浮层输入行统一块形(与命令面板同
                        // 语法;此前是灰竖线,差异总结 §7 输入行光标不一致)。
                        div().w(px(7.)).h(px(16.)).bg(rgb(PH)).rounded(px(1.)),
                    )
                    .when(self.ssh_prompt_input.is_empty(), |d| {
                        d.child(
                            div()
                                .ml(px(4.))
                                .text_color(col(ui.muted))
                                .child(SharedString::from(placeholder)),
                        )
                    }),
            )
            .child(div().flex_1())
            // red error chip takes priority over the parse chips when invalid.
            .when_some(err_chip, |d, c| d.child(c))
            .when(ssh_err.is_none(), |d| {
                d.when_some(chips_row, |d, c| d.child(c))
            });

        // Connector rows; also drives whether the footer advertises the per-row
        // ★ action (hidden when there are no rows to act on).
        let conn_rows = self.ssh_conn_rows();
        let has_rows = !conn_rows.is_empty();

        // ── panel body: input + combined favorites/recents + ssh-config rows ──
        let panel_body: gpui::Div = {
            let rows = &conn_rows;
            let sel = self.ssh_prompt_sel.min(rows.len().saturating_sub(1));
            let list: gpui::Div = if rows.is_empty() {
                if self.ssh_prompt_input.trim().is_empty() {
                    // C3 first-connect guide: a short three-line walkthrough shown
                    // when the connector is empty (no recent / config hosts).
                    let step = |ic: &'static str, head: &str, sub: &str| {
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(px(10.))
                            .child(div().mt(px(1.)).child(icon(ic, 14., ui.accent)))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.))
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(col(ui.foreground))
                                            .child(SharedString::from(head.to_string())),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(col(ui.muted))
                                            .child(SharedString::from(sub.to_string())),
                                    ),
                            )
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .px(px(16.))
                        .py(px(15.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(col(ui.muted))
                                .child(SharedString::from("首次连接 — 三步上手")),
                        )
                        .child(step(
                            "globe",
                            "1 · 输入地址",
                            "user@host:port,例 root@192.168.1.1:22(端口默认 22 可省)",
                        ))
                        .child(step(
                            "key",
                            "2 · 自动认证",
                            "优先用 ~/.ssh 里的密钥;无密钥则弹密码框(可记住本次会话)",
                        ))
                        .child(step(
                            "star",
                            "3 · 记住连接",
                            "连上后自动进「最近」,点 ★ 收藏长期保留;再点一次取消",
                        ))
                } else {
                    div()
                        .px(px(14.))
                        .py(px(13.))
                        .text_size(px(12.))
                        .text_color(col(ui.muted))
                        .child(SharedString::from("无匹配 · 按 Enter 连接所输入的地址"))
                }
            } else {
                let mut col_div = div()
                    .flex()
                    .flex_col()
                    .p(px(6.))
                    .max_h(px(360.))
                    .overflow_hidden();
                let crest = col(ui.palette_selected); // L4
                for (i, row_data) in rows.iter().enumerate() {
                    let is_sel = i == sel;
                    // `.prow`:r3,选中 = L4 + 左 2px 磷光脊;hover = L4(SHEET 06)
                    let base = || {
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(11.))
                            .px(px(11.))
                            .py(px(9.))
                            .rounded(px(R_CHIP))
                            .relative()
                            .when(is_sel, |d| {
                                d.bg(crest).child(
                                    div()
                                        .absolute()
                                        .left(px(0.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.))
                                        .bg(rgb(PH)),
                                )
                            })
                            .when(!is_sel, |d| d.hover(move |s| s.bg(crest)))
                    };
                    let row = match row_data {
                        SshConnRow::Recent {
                            host,
                            user,
                            port,
                            name,
                            target,
                            favorite,
                            auth,
                            last_used,
                        } => {
                            let (host, user, port, favorite, auth) =
                                (host.clone(), user.clone(), *port, *favorite, *auth);
                            let name = name.clone();
                            let target = target.clone();
                            let rename_active = self.ssh_rename.as_ref().is_some_and(|d| {
                                d.port == port
                                    && d.user == user
                                    && d.host.eq_ignore_ascii_case(&host)
                            });
                            let title_text = self
                                .ssh_rename
                                .as_ref()
                                .filter(|_| rename_active)
                                .map(|d| d.name.clone())
                                .or_else(|| name.clone())
                                .unwrap_or_else(|| host.clone());
                            let marked = rename_active
                                .then(|| self.ssh_rename_marked.clone())
                                .flatten();
                            let when = crate::ssh_recents::rel_time(*last_used);
                            // ⭐ star toggles favorite without triggering the row's connect.
                            let (fav_host, fav_user) = (host.clone(), user.clone());
                            let star = div()
                                .child(icon(
                                    "star",
                                    14.,
                                    if favorite { t.ansi.yellow } else { ui.muted },
                                ))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _e, _w, cx| {
                                        cx.stop_propagation();
                                        this.ssh_recents
                                            .toggle_favorite(&fav_host, &fav_user, port);
                                        if this.ssh_rename.as_ref().is_some_and(|d| {
                                            d.port == port
                                                && d.user == fav_user
                                                && d.host.eq_ignore_ascii_case(&fav_host)
                                        }) {
                                            this.ssh_rename = None;
                                            this.ssh_rename_marked = None;
                                        }
                                        this.ssh_recents.save();
                                        cx.notify();
                                    }),
                                );
                            let badge = match auth {
                                AuthBadge::Key => Some(("key", "密钥", t.ansi.green)),
                                AuthBadge::Password => Some(("lock", "密码", t.ansi.yellow)),
                                AuthBadge::Unknown => None,
                            };
                            let rename_btn = if favorite {
                                let (rename_host, rename_user) = (host.clone(), user.clone());
                                let initial_name =
                                    name.clone().unwrap_or_else(|| rename_host.clone());
                                Some(
                                    div()
                                        .child(icon("pen", 13., ui.muted))
                                        .hover(|s| s.text_color(col(ui.accent)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _e, _w, cx| {
                                                cx.stop_propagation();
                                                this.ssh_rename = Some(SshRenameDraft {
                                                    host: rename_host.clone(),
                                                    user: rename_user.clone(),
                                                    port,
                                                    name: initial_name.clone(),
                                                });
                                                this.ssh_rename_marked = None;
                                                cx.notify();
                                            }),
                                        ),
                                )
                            } else {
                                None
                            };
                            let conn_target = target.clone();
                            base()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _e, w, cx| {
                                        this.ssh_connect(&conn_target, w, cx);
                                    }),
                                )
                                .child(star)
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(1.))
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .text_size(px(13.))
                                                .text_color(if rename_active {
                                                    col(ui.accent)
                                                } else {
                                                    col(ui.foreground)
                                                })
                                                .child(SharedString::from(title_text))
                                                .when_some(marked, |d, m| {
                                                    d.child(
                                                        div()
                                                            .text_color(col(ui.muted))
                                                            .child(SharedString::from(m)),
                                                    )
                                                })
                                                .when(rename_active, |d| {
                                                    d.child(
                                                        div()
                                                            .text_color(col(ui.muted))
                                                            .child(SharedString::from("▏")),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .font_family(mono.clone())
                                                .text_size(px(11.))
                                                .text_color(col(ui.muted))
                                                .child(SharedString::from(target.clone())),
                                        ),
                                )
                                .child(div().flex_1())
                                .when_some(badge, |d, (ic, label, color)| {
                                    d.child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(px(5.))
                                            .px(px(8.))
                                            .py(px(2.))
                                            .rounded(px(999.))
                                            .bg(cola(color, 0.12))
                                            .child(icon(ic, 11., color))
                                            .child(
                                                div()
                                                    .text_size(px(10.))
                                                    .text_color(col(color))
                                                    .child(SharedString::from(label.to_string())),
                                            ),
                                    )
                                })
                                .when_some(rename_btn, |d, b| d.child(b))
                                // relative time — faint(无 token,同 .meta)
                                .child(
                                    div()
                                        .min_w(px(46.))
                                        .text_size(px(10.5))
                                        .text_color(rgb(T3))
                                        .child(SharedString::from(when)),
                                )
                        }
                        SshConnRow::Config { alias, target } => {
                            // A4: connect via the alias so ssh-config rules apply.
                            let conn_alias = alias.clone();
                            base()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _e, w, cx| {
                                        this.ssh_connect(&conn_alias, w, cx);
                                    }),
                                )
                                // hollow dot marker = an endpoint we haven't dialed yet
                                .child(
                                    div().w(px(14.)).flex().justify_center().flex_none().child(
                                        div()
                                            .w(px(8.))
                                            .h(px(8.))
                                            .rounded(px(999.))
                                            .border_1()
                                            .border_color(col(ui.muted)),
                                    ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(1.))
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .text_size(px(13.))
                                                .text_color(col(ui.foreground))
                                                .child(SharedString::from(alias.clone())),
                                        )
                                        .child(
                                            div()
                                                .font_family(mono.clone())
                                                .text_size(px(11.))
                                                .text_color(col(ui.muted))
                                                .child(SharedString::from(target.clone())),
                                        ),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .px(px(8.))
                                        .py(px(2.))
                                        .rounded(px(R_CHIP))
                                        .border_1()
                                        .border_color(rgba(H1))
                                        .child(
                                            div()
                                                .font_family(mono.clone())
                                                .text_size(px(10.))
                                                .text_color(rgb(T2))
                                                .child(SharedString::from("ssh-config")),
                                        ),
                                )
                        }
                    };
                    col_div = col_div.child(row);
                }
                col_div
            };
            div()
                .flex()
                .flex_col()
                .child(input)
                .child(div().h(px(1.)).bg(rgba(H1)))
                .child(list)
        };

        // float-foot 键帽化(浮层家族四件套之一,差异总结 §7:此前是纯文本)。
        // ★ 收藏是点击行为(无键位),保持纯文本 token。
        let khint = |k: &'static str, label: &'static str| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .child(crate::style::kbd(k, mono.clone()))
                .child(div().child(label))
        };
        let footer_hints: Vec<gpui::Div> = if self.ssh_rename.is_some() {
            vec![
                khint("↵", "保存名称"),
                khint("Esc", "取消"),
                div().child("支持中文输入"),
            ]
        } else if has_rows {
            vec![
                khint("↑↓", "选择"),
                khint("↵", "连接"),
                div().child("★ 收藏/取消收藏"),
                khint("Esc", "取消"),
            ]
        } else {
            vec![khint("↵", "连接"), khint("Esc", "取消")]
        };

        let ime_focus = self.ssh_prompt_focus.clone();
        let ime_entity = cx.entity();
        let rename_ime_active = self.ssh_rename.is_some();

        let panel = crate::style::shadowed(
            div()
                .relative()
                .flex()
                .flex_col()
                .w(px(560.))
                .max_w(relative(0.92))
                .max_h(relative(0.86))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg)) // L3 不透明浮板(契约 1)
                .child(panel_body)
                .child(
                    // float-foot:高 30 · 顶 1px h1 · mono 10 t2 · kbd 键帽
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .h(px(30.))
                        .px(px(14.))
                        .flex_none()
                        .border_t(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .text_size(px(10.))
                        .text_color(rgb(T2))
                        .children(footer_hints),
                )
                .when(rename_ime_active, |d| {
                    d.child(
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _state, window, cx| {
                                window.handle_input(
                                    &ime_focus,
                                    ElementInputHandler::new(bounds, ime_entity.clone()),
                                    cx,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                }),
            shadow_float(),
        );

        let pl = if self.explorer_open {
            self.explorer_width + 23.
        } else {
            12.
        };

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .pl(px(pl))
                .pr(px(12.))
                .bg(rgba(SCRIM))
                .track_focus(&self.ssh_prompt_focus)
                .on_key_down(cx.listener(Self::on_ssh_prompt_key))
                // 浮层 scrim 统一吞滚轮,不驱动底层终端(BUG发现 #5 同类)。
                .on_scroll_wheel(cx.listener(|_t, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, w, cx| {
                        this.ssh_prompt_open = false;
                        this.ssh_prompt_intent = None;
                        this.ssh_rename = None;
                        this.ssh_rename_marked = None;
                        this.refocus_active(w, cx);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| {
                                cx.stop_propagation();
                                this.ssh_prompt_focus.focus(w);
                                cx.notify();
                            }),
                        )
                        .child(panel),
                ),
        )
    }

    fn render_agent_dir_picker(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let picker = self.agent_dir_picker.as_ref()?;
        let t = &self.config.theme;
        let ui = &t.ui;
        let mono = SharedString::from(self.config.font().family.clone());
        let focused = picker.focus;
        let selected = picker.launch_cwd().display().to_string();
        let current = picker.current_label();

        let section_label = |label: &'static str, active: bool| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.))
                .h(px(24.))
                .font_family(mono.clone())
                .text_size(px(10.))
                .font_weight(gpui::FontWeight(650.))
                .text_color(if active { rgb(T0) } else { rgb(T2) })
                .child(div().w(px(5.)).h(px(5.)).rounded(px(1.)).bg(if active {
                    rgb(PH)
                } else {
                    rgb(T3)
                }))
                .child(label)
        };

        let recent_rows = if picker.recents.is_empty() {
            div()
                .flex()
                .items_center()
                .h(px(AGENT_DIR_RECENTS_H))
                .overflow_hidden()
                .px(px(11.))
                .text_size(px(12.))
                .text_color(rgb(T2))
                .child("暂无最近工作目录")
        } else {
            let mut list = div().flex().flex_col().gap(px(3.));
            let start = if focused == LocalDirFocus::Recent {
                picker.recent_sel.saturating_sub(4)
            } else {
                0
            };
            for (offset, item) in picker.recents.iter().enumerate().skip(start).take(5) {
                let i = offset;
                let is_sel = focused == LocalDirFocus::Recent && picker.recent_sel == i;
                let path = item.path.clone();
                let label = item.label.clone();
                let sub = item.path.display().to_string();
                list = list.child(
                    div()
                        .relative()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(px(38.))
                        .gap(px(9.))
                        .px(px(11.))
                        .rounded(px(R_CARD))
                        .bg(if is_sel {
                            col(ui.palette_selected)
                        } else {
                            rgba(0x00000000)
                        })
                        .border_1()
                        .border_color(if is_sel { rgba(PH_DIM) } else { rgba(H0) })
                        .hover(|s| s.bg(rgb(crate::style::L2)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                if let Some(picker) = this.agent_dir_picker.as_mut() {
                                    picker.focus = LocalDirFocus::Recent;
                                    picker.recent_sel = i;
                                    picker.current = path.clone();
                                    picker.selected = path.clone();
                                }
                                this.refresh_agent_dir_picker(cx);
                            }),
                        )
                        .child(icon("folder", 14., ui.accent))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(gpui::FontWeight(620.))
                                        .text_color(col(ui.foreground))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(SharedString::from(label)),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(col(ui.muted))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(SharedString::from(sub)),
                                ),
                        ),
                );
            }
            list.h(px(AGENT_DIR_RECENTS_H)).overflow_hidden()
        };

        let mut dir_rows = div()
            .h(px(AGENT_DIR_LIST_H))
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(3.));
        if picker.dirs.is_empty() {
            dir_rows = dir_rows.child(
                div()
                    .flex()
                    .items_center()
                    .h(px(46.))
                    .px(px(11.))
                    .text_size(px(12.))
                    .text_color(rgb(T2))
                    .child("没有可进入的子目录"),
            );
        } else {
            let start = if focused == LocalDirFocus::Directories {
                picker.dir_sel.saturating_sub(6)
            } else {
                0
            };
            for (offset, item) in picker.dirs.iter().enumerate().skip(start).take(7) {
                let i = offset;
                let is_sel = focused == LocalDirFocus::Directories && picker.dir_sel == i;
                let path = item.path.clone();
                let name = item.name.clone();
                let is_git = item.is_git;
                let is_drive = item.is_drive;
                dir_rows = dir_rows.child(
                    div()
                        .relative()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(px(34.))
                        .gap(px(9.))
                        .px(px(11.))
                        .rounded(px(R_CARD))
                        .bg(if is_sel {
                            col(ui.palette_selected)
                        } else {
                            rgba(0x00000000)
                        })
                        .border_1()
                        .border_color(if is_sel { rgba(PH_DIM) } else { rgba(H0) })
                        .hover(|s| s.bg(rgb(crate::style::L2)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                if let Some(picker) = this.agent_dir_picker.as_mut() {
                                    picker.focus = LocalDirFocus::Directories;
                                    picker.dir_sel = i;
                                    picker.current = path.clone();
                                    picker.selected = path.clone();
                                }
                                this.refresh_agent_dir_picker(cx);
                            }),
                        )
                        .child(icon("folder", 14., ui.accent_alt))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight(600.))
                                .text_color(col(ui.foreground))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(name)),
                        )
                        .when(is_git, |d| {
                            d.child(
                                div()
                                    .px(px(7.))
                                    .py(px(1.))
                                    .rounded(px(R_CHIP))
                                    .border_1()
                                    .border_color(cola(t.ansi.yellow, 0.35))
                                    .text_size(px(10.))
                                    .text_color(col(t.ansi.yellow))
                                    .child("git"),
                            )
                        })
                        .when(is_drive, |d| {
                            d.child(
                                div()
                                    .px(px(7.))
                                    .py(px(1.))
                                    .rounded(px(R_CHIP))
                                    .border_1()
                                    .border_color(cola(ui.accent_alt, 0.35))
                                    .text_size(px(10.))
                                    .text_color(col(ui.accent_alt))
                                    .child("盘符"),
                            )
                        }),
                );
            }
        }

        let browse_active = focused == LocalDirFocus::Browse;
        let browse = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(44.))
            .px(px(12.))
            .rounded(px(R_CARD))
            .border_1()
            .border_color(if browse_active {
                rgba(PH_DIM)
            } else {
                rgba(H1)
            })
            .bg(if browse_active {
                col(ui.palette_selected)
            } else {
                rgb(crate::style::L1)
            })
            .hover(|s| s.bg(rgb(crate::style::L2)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    if let Some(picker) = this.agent_dir_picker.as_mut() {
                        picker.focus = LocalDirFocus::Browse;
                    }
                    this.browse_agent_dir_picker(cx);
                }),
            )
            .child(icon("external", 14., ui.accent))
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight(620.))
                    .text_color(col(ui.foreground))
                    .child("浏览本地文件夹"),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(col(ui.muted))
                    .child("→"),
            );

        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(680.))
                .h(px(AGENT_DIR_PANEL_H))
                .max_w(relative(0.92))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(38.))
                        .px(px(14.))
                        .flex_none()
                        .bg(col(ui.palette_selected))
                        .border_b(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .child(icon("folder", 14., ui.accent))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight(650.))
                                .text_color(rgb(T0))
                                .child(SharedString::from(format!(
                                    "{} 工作目录",
                                    picker.agent_name
                                ))),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(T2))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(selected.clone())),
                        ),
                )
                .child(
                    div()
                        .p(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .h(px(428.))
                        .child(
                            div()
                                .px(px(11.))
                                .py(px(9.))
                                .rounded(px(R_CARD))
                                .border_1()
                                .border_color(rgba(H1))
                                .bg(rgb(crate::style::L0))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_family(mono.clone())
                                        .text_color(rgb(T2))
                                        .child("当前工作目录"),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(col(ui.foreground))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(SharedString::from(selected)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(10.))
                                .child(
                                    div()
                                        .w(px(260.))
                                        .flex_none()
                                        .flex()
                                        .flex_col()
                                        .gap(px(5.))
                                        .child(section_label(
                                            "最近工作目录",
                                            focused == LocalDirFocus::Recent,
                                        ))
                                        .child(recent_rows),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(5.))
                                        .child(section_label(
                                            "当前目录",
                                            focused == LocalDirFocus::Directories,
                                        ))
                                        .child(
                                            div()
                                                .text_size(px(10.))
                                                .text_color(col(ui.muted))
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(SharedString::from(current)),
                                        )
                                        .child(dir_rows),
                                ),
                        )
                        .child(section_label("浏览", browse_active))
                        .child(browse),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(42.))
                        .px(px(14.))
                        .flex_none()
                        .border_t(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .text_size(px(10.))
                        .text_color(rgb(T2))
                        .child(crate::style::kbd("Tab", mono.clone()))
                        .child(div().child("焦点"))
                        .child(crate::style::kbd("↑↓", mono.clone()))
                        .child(div().child("选择"))
                        .child(crate::style::kbd("←", mono.clone()))
                        .child(div().child("上级"))
                        .child(crate::style::kbd("→", mono.clone()))
                        .child(div().child("进入"))
                        .child(crate::style::kbd("Enter", mono.clone()))
                        .child(div().child("启动"))
                        .child(div().flex_1())
                        .child(crate::style::btn("取消").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| {
                                this.cancel_agent_dir_picker(w, cx);
                            }),
                        ))
                        .child(crate::style::btn_primary("启动").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                this.confirm_agent_dir_picker(cx);
                            }),
                        )),
                ),
            shadow_float(),
        );

        let pl = if self.explorer_open {
            self.explorer_width + 23.
        } else {
            12.
        };

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .pl(px(pl))
                .pr(px(12.))
                .bg(rgba(SCRIM))
                .track_focus(&self.agent_dir_focus)
                .on_key_down(cx.listener(Self::on_agent_dir_key))
                .on_scroll_wheel(cx.listener(|_t, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, w, cx| {
                        this.cancel_agent_dir_picker(w, cx);
                    }),
                )
                .child(
                    div()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| {
                                cx.stop_propagation();
                                this.agent_dir_focus.focus(w);
                                cx.notify();
                            }),
                        )
                        .child(panel),
                ),
        )
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
        let placeholder = if self.palette_wsl {
            "WSL 发行版 / 搜索…"
        } else {
            "启动会话 / 搜索…"
        };
        let crest = col(ui.palette_selected); // L4
        let lead = if self.palette_wsl {
            div()
                .rounded(px(R_CHIP))
                .hover(move |s| s.bg(crest))
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
            // `.pin` 前缀:磷光 prompt 字形 ❯(SHEET 06)
            div()
                .font_family(mono.clone())
                .text_color(rgb(PH))
                .child(SharedString::from("❯"))
        };
        // `.pin`:高 44 · px 16 · mono 14 · 磷光块光标
        let input = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(44.))
            .px(px(16.))
            .flex_none()
            .font_family(mono.clone())
            .text_size(px(14.))
            .border_b(px(1.))
            .border_color(rgba(H1))
            .child(lead)
            .child(
                // query + block cursor (AT the insertion point) + placeholder-when-empty.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .when(!self.palette_query.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(rgb(T0))
                                .child(SharedString::from(self.palette_query.clone())),
                        )
                    })
                    .child(
                        // 磷光块光标(`.cur`):浮层输入行的「活物」
                        div().w(px(7.)).h(px(16.)).bg(rgb(PH)).rounded(px(1.)),
                    )
                    .when(self.palette_query.is_empty(), |d| {
                        d.child(
                            div()
                                .ml(px(6.))
                                .text_color(rgb(T2))
                                .child(SharedString::from(placeholder)),
                        )
                    }),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(T2))
                    .child(SharedString::from(if self.palette_wsl {
                        "WSL"
                    } else {
                        "PROFILES"
                    })),
            );

        let reg = crate::agent_host::agent_registry(cx);
        let row_divs = rows.iter().enumerate().map(|(i, row)| {
            let is_sel = i == sel;
            let card = row_card(t, &self.launch_profiles, row, &reg); // identity = tiles/.dot
                                                                      // Faint mono meta: a profile's command, or the WSL/SSH card's sub-label.
            let meta = match row {
                LaunchRow::Profile(pi) => self.launch_profiles[*pi]
                    .command
                    .clone()
                    .unwrap_or_default(),
                _ => card.sub.clone(),
            };
            // `.prow`:高 36 · px 14 · sans 12;选中 = L4 + 左 2px 磷光脊
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .h(px(36.))
                .px(px(14.))
                .relative()
                .when(is_sel, |d| {
                    d.bg(crest).child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .top(px(6.))
                            .bottom(px(6.))
                            .w(px(2.))
                            .bg(rgb(PH)),
                    )
                })
                .when(!is_sel, |d| d.hover(move |s| s.bg(crest)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, w, cx| {
                        this.palette_sel = i;
                        this.activate_palette_sel(w, cx);
                    }),
                )
                // `.gi` 身份字形位:mono 600 12 身份字形(SHEET 06 — ❯/⌬/⇄/✳/◆,
                // 不再用无差别色块;差异总结 6-字形系统未实现)。
                .child(
                    div()
                        .w(px(16.))
                        .flex()
                        .justify_center()
                        .flex_none()
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight(600.))
                        .text_color(col(card.accent))
                        .child(SharedString::from(crate::welcome::launch_glyph_ch(
                            card.glyph,
                        ))),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(if is_sel { rgb(T0) } else { rgb(T1) })
                        .child(SharedString::from(card.name)),
                )
                .child(div().flex_1())
                // `.src` 来源 tag:mono 10 · t3
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_size(px(10.))
                        .text_color(rgb(T3))
                        .child(SharedString::from(meta)),
                )
        });

        let n_rows = rows.len();
        // SHEET 06 `.palette`:640px 浮层(L3 + h2 + r6 + float 投影)
        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(640.))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg)) // L3 不透明浮板
                .child(input)
                .child(div().flex().flex_col().py(px(6.)).children(row_divs))
                .child(
                    // float-foot:kbd 提示 + 项数 tag
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .h(px(30.))
                        .px(px(14.))
                        .flex_none()
                        .border_t(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .text_size(px(10.))
                        .text_color(rgb(T2))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.))
                                .child(crate::style::kbd("↑↓", mono.clone()))
                                .child(div().child("选择")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.))
                                .child(crate::style::kbd("↵", mono.clone()))
                                .child(div().child("新标签启动")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.))
                                .child(crate::style::kbd("Esc", mono.clone()))
                                .child(div().child("关闭")),
                        )
                        .child(div().flex_1())
                        .child(div().child(SharedString::from(format!("{n_rows} 项")))),
                ),
            shadow_float(),
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(SCRIM)) // 纯色压暗 scrim,无模糊(契约 7)
                .track_focus(&self.palette_focus)
                .on_key_down(
                    cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_palette_key(ev, w, cx)),
                )
                // 浮层 scrim 统一吞滚轮,不驱动底层终端(BUG发现 #5 同类)。
                .on_scroll_wheel(cx.listener(|_t, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
                .child(div().h(px(110.))) // top spacer (clears the title + tab bar)
                .child(panel),
        )
    }

    /// `新会话` (app menu / Ctrl+Shift+N): open the split launcher (单浮层,
    /// 方向默认「右」— SHEET 06-B 示例态)。
    fn new_session(&mut self, _: &NewSession, _window: &mut Window, cx: &mut Context<Self>) {
        // Snapshot the split target NOW, while the pane the user is on still holds
        // focus — the launcher overlay is about to steal focus, so reading
        // `focused` later (in `split_session`) is fragile.
        self.split_target = Some(self.tabs[self.active].focused);
        self.split_launcher_open = true;
        self.split_dir = SplitDir::Right;
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

    /// Split-launcher keyboard(SHEET 06-B 单浮层):⇥ 循环方向(⇧⇥ 反向),
    /// ↑↓ 选 profile,↵ 按当前方向分屏启动,Esc 退 WSL 下钻 / 关闭。
    fn on_split_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        cx.stop_propagation();
        let n = self.split_rows().len();
        match key {
            "tab" => {
                // ← → ↑ ↓ 环(原型方向格顺序);⇧⇥ 反向。
                const ORDER: [SplitDir; 4] = [
                    SplitDir::Left,
                    SplitDir::Right,
                    SplitDir::Up,
                    SplitDir::Down,
                ];
                let i = ORDER.iter().position(|d| *d == self.split_dir).unwrap_or(1);
                let next = if ev.keystroke.modifiers.shift {
                    (i + ORDER.len() - 1) % ORDER.len()
                } else {
                    (i + 1) % ORDER.len()
                };
                self.split_dir = ORDER[next];
                cx.notify();
            }
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
            "enter" => self.activate_split_sel(self.split_dir, window, cx),
            "escape" => {
                if self.split_wsl {
                    self.split_wsl = false; // back to the root list
                    self.split_sel = 0;
                    cx.notify();
                } else {
                    self.close_split_launcher(window, cx);
                }
            }
            _ => {}
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
        let Some(row) = rows.get(self.split_sel) else {
            return;
        };
        let launch = |this: &mut Self, idx: usize, window: &mut Window, cx: &mut Context<Self>| {
            let reg = crate::agent_host::agent_registry(cx);
            if let Some(spec) = this
                .launch_profiles
                .get(idx)
                .and_then(|p| LaunchSpec::from_profile(p, &reg))
            {
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
            LaunchRow::SshPrompt => {
                self.split_launcher_open = false;
                self.split_wsl = false;
                self.ssh_prompt_open = true;
                self.ssh_prompt_needs_focus = true;
                self.ssh_prompt_intent = Some(SshPromptIntent::Split(dir));
                self.ssh_prompt_input.clear();
                self.ssh_prompt_sel = 0;
                self.ssh_rename = None;
                self.ssh_rename_marked = None;
                self.ssh_config_hosts = tn_pty::list_ssh_config_hosts();
                cx.notify();
            }
            // 分屏启动器只做会话启动;宠物设置行只存在于命令面板(palette_rows)。
            LaunchRow::PetSettings => {}
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

        let crest = col(ui.palette_selected); // L4
        let mono = SharedString::from(self.config.font().family.clone());
        let cur_dir = self.split_dir;

        // ── SHEET 06-B 单浮层(用户定夺改回原型):方向 4 格 + profile 行同屏 ──
        // `.dir`:46×46 · L2 + 1px h1 · r4 · mono 15 t2;on = ph-soft + ph-dim 边 + 磷光字。
        let dir_tile = |d: SplitDir| {
            let (arrow, _) = d.label();
            let on = d == cur_dir;
            div()
                .w(px(46.))
                .h(px(46.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(R_CARD))
                .font_family(mono.clone())
                .text_size(px(15.))
                .border_1()
                .when(on, |x| {
                    x.bg(rgba(crate::style::PH_SOFT))
                        .border_color(rgba(PH_DIM))
                        .text_color(rgb(PH))
                })
                .when(!on, |x| {
                    x.bg(col(ui.surface_2))
                        .border_color(rgba(H1))
                        .text_color(rgb(T2))
                        .hover(|s| s.bg(rgba(crate::style::PH_SOFT)).border_color(rgba(PH_DIM)))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.split_dir = d;
                        cx.notify();
                    }),
                )
                .child(arrow)
        };
        // 方向排 + 读数(mono 10 t2 两行,SHEET 06-B 板)。WSL 下钻时隐藏方向排
        // (方向已定,只换行内容)。
        let dirs =
            (!self.split_wsl).then(|| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(24.))
                    .px(px(16.))
                    .pt(px(16.))
                    .pb(px(6.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(dir_tile(SplitDir::Left))
                            .child(dir_tile(SplitDir::Right))
                            .child(dir_tile(SplitDir::Up))
                            .child(dir_tile(SplitDir::Down)),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(10.))
                            .text_color(rgb(T2))
                            .child(div().child(SharedString::from(format!(
                                "方向:{}",
                                cur_dir.side_label()
                            ))))
                            .child(div().child("预览线先行,松手一次性提交(防 ConPTY 抖动)")),
                    )
            });
        // profile 行(聚合:profiles + WSL 卡 + SSH;WSL 下钻换成发行版)。
        let rows = self.split_rows();
        let sel = self.split_sel.min(rows.len().saturating_sub(1));
        let reg = crate::agent_host::agent_registry(cx);
        let row_divs = rows.iter().enumerate().map(|(i, row)| {
            let is_sel = i == sel;
            let card = row_card(t, &self.launch_profiles, row, &reg);
            // `.prow`:选中 = L4 + 左 2px 磷光脊(浮层家族统一语法)
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .h(px(36.))
                .px(px(14.))
                .relative()
                .when(is_sel, |d| {
                    d.bg(crest).child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .top(px(6.))
                            .bottom(px(6.))
                            .w(px(2.))
                            .bg(rgb(PH)),
                    )
                })
                .when(!is_sel, |d| d.hover(move |s| s.bg(crest)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, w, cx| {
                        this.split_sel = i;
                        this.activate_split_sel(this.split_dir, w, cx);
                    }),
                )
                .child(
                    // `.gi` 身份字形(与命令面板同一映射,差异总结 4-4)
                    div()
                        .w(px(16.))
                        .flex()
                        .justify_center()
                        .flex_none()
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight(600.))
                        .text_color(col(card.accent))
                        .child(SharedString::from(crate::welcome::launch_glyph_ch(
                            card.glyph,
                        ))),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(if is_sel { rgb(T0) } else { rgb(T1) })
                        .child(SharedString::from(card.name)),
                )
        });
        let body = div()
            .flex()
            .flex_col()
            .when_some(dirs, |d, x| d.child(x))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pt(px(4.))
                    .pb(px(10.))
                    .children(row_divs),
            );

        let title = if self.split_wsl {
            "新会话 · 选择 WSL 发行版"
        } else {
            "新会话 · 分屏"
        };

        // float-foot(SHEET 06-B):键帽「⇥ 方向 · ↵ 启动」+ 右端磷光方向 tag
        //「→ 右分屏」;WSL 下钻态换「↑↓ 选择 · ↵ 启动 · Esc 返回」。
        let khint = |k: &'static str, label: &'static str, mono: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .child(crate::style::kbd(k, mono))
                .child(div().child(label))
        };
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .h(px(30.))
            .px(px(14.))
            .flex_none()
            .border_t(px(1.))
            .border_color(rgba(H1))
            .font_family(mono.clone())
            .text_size(px(10.))
            .text_color(rgb(T2));
        let footer = if self.split_wsl {
            footer
                .child(khint("↑↓", "选择", mono.clone()))
                .child(khint("↵", "启动", mono.clone()))
                .child(khint("Esc", "返回", mono.clone()))
        } else {
            footer
                .child(khint("⇥", "方向", mono.clone()))
                .child(khint("↵", "启动", mono.clone()))
                .child(div().flex_1())
                .child(
                    // `.tag ph`:动态方向读数(mono 600 10 磷光)
                    div()
                        .font_weight(gpui::FontWeight(600.))
                        .text_color(rgb(PH))
                        .child(SharedString::from(format!(
                            "{} {}",
                            cur_dir.label().0,
                            cur_dir.side_label()
                        ))),
                )
        };

        // SHEET 06 板 B:浮层家族 — float-head(L4)+ body + float-foot;宽 520。
        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(520.))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg)) // L3 不透明浮板
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(38.))
                        .px(px(14.))
                        .flex_none()
                        .bg(col(ui.palette_selected)) // L4
                        .border_b(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .child(div().text_color(rgb(PH)).child("◫"))
                        .child(div().text_color(rgb(T0)).child(SharedString::from(title)))
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(T2))
                                .child("当前 PANE"),
                        ),
                )
                .child(body)
                .child(footer),
            shadow_float(),
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(SCRIM))
                .track_focus(&self.split_focus)
                .on_key_down(
                    cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_split_key(ev, w, cx)),
                )
                // 浮层 scrim 统一吞滚轮(BUG发现 #5 同类)。
                .on_scroll_wheel(cx.listener(|_t, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
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
                Node::Leaf(id) => specs
                    .get(id)
                    .map(|s| LayoutNode::Pane(LayoutPane::from_spec(s))),
                Node::Split {
                    axis,
                    kids,
                    weights,
                } => {
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
                Node::Split {
                    axis: if *row { Axis::Row } else { Axis::Col },
                    kids,
                    weights,
                }
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
        let Some(layout) = self.layouts.slots.get(slot).cloned().flatten() else {
            return;
        };
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

        // 小按钮(`.btn` 缩水版):L2 + h1 边 + r3;primary = ph-soft + ph。
        let crest = col(ui.palette_selected); // L4
        let pill =
            |label: &'static str,
             accent: bool,
             act: Box<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)>| {
                let (fg, bg, bc) = if accent {
                    (rgb(PH), rgba(crate::style::PH_SOFT), rgba(PH_DIM))
                } else {
                    (rgb(T1), col(ui.surface_2).into(), rgba(H1))
                };
                div()
                    .px(px(9.))
                    .py(px(3.))
                    .rounded(px(R_CHIP))
                    .border_1()
                    .border_color(bc)
                    .text_size(px(11.))
                    .text_color(fg)
                    .bg(bg)
                    .hover(move |s| s.bg(crest))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, w, cx| act(this, w, cx)),
                    )
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
                .rounded(px(R_CHIP))
                .hover(move |s| s.bg(crest))
                .child(
                    div()
                        .w(px(40.))
                        .text_size(px(12.))
                        .text_color(rgb(T0))
                        .child(SharedString::from(format!("槽 {}", i + 1))),
                )
                .child(
                    div()
                        .w(px(56.))
                        .text_size(px(11.))
                        .text_color(col(ui.muted))
                        .child(SharedString::from(status)),
                )
                .child(div().flex_1())
                .when(can_save, |d| {
                    d.child(pill(
                        "保存",
                        true,
                        Box::new(move |this, _w, cx| this.save_layout(i, cx)),
                    ))
                })
                .when(filled.is_some(), |d| {
                    d.child(pill(
                        "加载",
                        false,
                        Box::new(move |this, w, cx| this.load_layout(i, w, cx)),
                    ))
                    .child(pill(
                        "删除",
                        false,
                        Box::new(move |this, _w, cx| this.delete_layout(i, cx)),
                    ))
                })
        });

        let hint = if can_save {
            "保存=把当前标签的分屏存入此槽 · 加载=按该布局替换当前标签 · Esc 关闭"
        } else {
            "当前标签无窗格可保存(欢迎页)· 加载=按布局替换当前标签 · Esc 关闭"
        };
        let mono = SharedString::from(self.config.font().family.clone());
        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(420.))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg)) // L3 不透明浮板
                .child(
                    // float-head:L4 · 高 38 · 底 1px h1
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(38.))
                        .px(px(14.))
                        .flex_none()
                        .bg(col(ui.palette_selected))
                        .border_b(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .child(div().text_color(rgb(PH)).child("▤"))
                        .child(div().text_color(rgb(T0)).child("布局 · 7 槽位")),
                )
                .child(div().p(px(6.)).flex().flex_col().gap(px(1.)).children(rows))
                .child(
                    // float-foot:提示
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(px(30.))
                        .px(px(14.))
                        .flex_none()
                        .border_t(px(1.))
                        .border_color(rgba(H1))
                        .font_family(mono)
                        .text_size(px(10.))
                        .text_color(rgb(T2))
                        .child(hint),
                ),
            shadow_float(),
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .bg(rgba(SCRIM))
                .track_focus(&self.layout_focus)
                // 浮层 scrim 统一吞滚轮(BUG发现 #5 同类)。
                .on_scroll_wheel(cx.listener(|_t, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
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

    // ════════════════════════════════════════════════════════════════════
    // 添加/编辑 Agent overlay — the in-app agent editor (no more hand-editing
    // config.toml `[[agents]]`). A config-level (generic) agent: it appears in
    // the launchpad / header / capability slots, hosts a terminal + activity
    // rail, but has **no usage telemetry** (that needs a built-in/external
    // adapter — we don't fake a usage ring for it).
    // ════════════════════════════════════════════════════════════════════

    /// The agent editor's **name** field is the active IME target (Chinese names).
    fn agent_name_is_ime_target(&self) -> bool {
        self.agent_form_open && self.agent_form_field == AgentField::Name
    }

    /// Whether *some* overlay text field currently accepts IME composition.
    fn ime_target_active(&self) -> bool {
        self.agent_name_is_ime_target() || self.ssh_rename.is_some()
    }

    /// The IME preedit buffer for whichever field is composing (agent name / SSH rename).
    fn active_ime_marked(&self) -> Option<&str> {
        if self.agent_name_is_ime_target() {
            self.agent_form_marked.as_deref()
        } else {
            self.ssh_rename_marked.as_deref()
        }
    }

    /// Wire the welcome launchpad's events to the workspace. Shared by `new()` and
    /// [`reload_agents`](Self::reload_agents) (which recreates the launchpad after a
    /// config change) so both attach the identical set of subscriptions.
    fn subscribe_welcome(welcome: &Entity<WelcomeView>, cx: &mut Context<Self>) {
        cx.subscribe(welcome, |ws, _welcome, ev: &LaunchRequested, cx| {
            if ws
                .launch_profiles
                .get(ev.0)
                .is_some_and(crate::welcome::is_agent_profile)
            {
                ws.open_agent_dir_picker(ev.0, cx);
            } else {
                ws.launch_in_active_tab(ev.0, cx);
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(
            welcome,
            |ws, _welcome, _ev: &crate::welcome::SshPromptRequested, cx| {
                ws.ssh_prompt_open = true;
                ws.ssh_prompt_needs_focus = true;
                ws.ssh_prompt_intent = Some(SshPromptIntent::Welcome);
                ws.ssh_prompt_input.clear();
                ws.ssh_prompt_sel = 0;
                ws.ssh_rename = None;
                ws.ssh_rename_marked = None;
                ws.ssh_config_hosts = tn_pty::list_ssh_config_hosts();
                cx.notify();
            },
        )
        .detach();
        cx.subscribe(
            welcome,
            |ws, _welcome, _ev: &crate::welcome::AddAgentRequested, cx| {
                ws.open_add_agent(cx);
            },
        )
        .detach();
        cx.subscribe(
            welcome,
            |ws, _welcome, ev: &crate::welcome::EditAgentRequested, cx| {
                ws.open_edit_agent(ev.0, cx);
            },
        )
        .detach();
        cx.subscribe(
            welcome,
            |ws, _welcome, ev: &crate::welcome::DeleteAgentRequested, cx| {
                ws.delete_agent(ev.0, cx);
            },
        )
        .detach();
    }

    /// Open the editor to **add** a new agent (empty draft, Ink cursor on).
    fn open_add_agent(&mut self, cx: &mut Context<Self>) {
        self.agent_form = AgentForm {
            name: String::new(),
            command: String::new(),
            accent_idx: 0,
            manages_cursor: true,
            sidecar: String::new(),
            networked: false,
        };
        self.agent_form_field = AgentField::Name;
        self.agent_form_marked = None;
        self.agent_form_edit = None;
        self.agent_form_open = true;
        self.agent_form_needs_focus = true;
        cx.notify();
    }

    /// Open the editor to **edit** the agent launched by `launch_profiles[idx]`,
    /// prefilling the draft from its profile + `[[agents]]` manifest.
    fn open_edit_agent(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(p) = self.launch_profiles.get(idx).cloned() else {
            return;
        };
        let id = p
            .agent
            .clone()
            .or_else(|| p.command.clone())
            .unwrap_or_default();
        let manifest = self
            .config
            .config
            .agents
            .iter()
            .find(|a| a.id == id)
            .cloned();
        let accent = p
            .accent
            .or_else(|| manifest.as_ref().and_then(|m| m.accent));
        let accent_idx = accent
            .and_then(|c| {
                tn_config::ACCENT_SWATCHES
                    .iter()
                    .position(|(_, sc)| *sc == c)
            })
            .unwrap_or(0);
        let manages = manifest
            .as_ref()
            .map(|m| m.manages_own_cursor)
            .unwrap_or(true);
        let label = manifest
            .as_ref()
            .and_then(|m| m.label.clone())
            .unwrap_or_else(|| p.name.clone());
        let sidecar = manifest
            .as_ref()
            .and_then(|m| m.sidecar.clone())
            .unwrap_or_default();
        let networked = manifest.as_ref().map(|m| m.allow_network).unwrap_or(false);
        self.agent_form = AgentForm {
            name: label,
            command: p.command.clone().unwrap_or_default(),
            accent_idx,
            manages_cursor: manages,
            sidecar,
            networked,
        };
        self.agent_form_field = AgentField::Name;
        self.agent_form_marked = None;
        self.agent_form_edit = Some(AgentEdit {
            old_id: id,
            old_profile_name: p.name.clone(),
        });
        self.agent_form_open = true;
        self.agent_form_needs_focus = true;
        cx.notify();
    }

    fn close_agent_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_form_open = false;
        self.agent_form_marked = None;
        self.agent_form_edit = None;
        self.refocus_active(window, cx);
        cx.notify();
    }

    /// Persist a delete: drop the `[[agents]]` manifest by id + the matching
    /// `[[profiles]]` block (comment-preserving, in tn-config).
    fn remove_agent_persisted(&self, id: &str, profile_name: &str) {
        if let Err(e) = tn_config::remove_agent(id) {
            tracing::error!(error = %e, id, "remove agent manifest failed");
        }
        let old_profile = tn_config::Profile {
            name: profile_name.to_string(),
            kind: tn_config::ProfileKind::Agent,
            command: None,
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: None,
            user: None,
            agent: None,
            accent: None,
            glyph: None,
        };
        if let Err(e) = tn_config::remove_profile(&old_profile) {
            tracing::error!(error = %e, "remove agent profile failed");
        }
    }

    /// Validate + persist the draft (write `[[agents]]` + `[[profiles]]`,
    /// replacing the old blocks when editing), then re-register the registry +
    /// refresh the launchpad so the tile appears immediately (no restart).
    fn save_agent_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.agent_form.name.trim().to_string();
        let command = self.agent_form.command.trim().to_string();
        if name.is_empty() || command.is_empty() {
            return; // both required (the 保存 button is dimmed; this is the belt)
        }
        let accent = self.agent_form.accent();
        let manages = self.agent_form.manages_cursor;

        // id: reuse when editing; else slugify (name → command first word), deduped.
        let id = if let Some(edit) = &self.agent_form_edit {
            edit.old_id.clone()
        } else {
            let existing: std::collections::HashSet<String> = self
                .config
                .config
                .agents
                .iter()
                .map(|a| a.id.clone())
                .collect();
            let base = first_nonempty_slug(&[
                name.as_str(),
                command.split_whitespace().next().unwrap_or(""),
            ]);
            unique_agent_id(&base, &existing)
        };
        let alias = command
            .split_whitespace()
            .next()
            .unwrap_or(command.as_str())
            .to_string();
        // Advanced: a telemetry sidecar unlocks the usage ring + realtime chips
        // (otherwise the agent is generic = terminal + git rail, no telemetry —
        // honest, not a stub). A networked sidecar goes behind the confirm card
        // (`remote_daemon` runtime + `allow_network`); a local one spawns directly.
        let sidecar = {
            let s = self.agent_form.sidecar.trim();
            (!s.is_empty()).then(|| s.to_string())
        };
        let networked = sidecar.is_some() && self.agent_form.networked;
        let capabilities = if sidecar.is_some() {
            vec!["usage".to_string()] // ungated chips (status/transcript/permission) show from data
        } else {
            Vec::new()
        };
        // `runtime_support` is where the **agent itself** runs — always PTY for an
        // editor-made agent (it has a command). The sidecar's networkiness rides on
        // `allow_network` alone; putting it in runtime_support would make the agent
        // look non-PTY → the launcher would refuse it → fall back to a plain shell.
        let runtime_support: Vec<String> = Vec::new();
        let manifest = tn_config::AgentManifest {
            id: id.clone(),
            label: Some(name.clone()),
            short: Some(short_name(&name)),
            aliases: vec![alias],
            accent: Some(accent),
            glyph: Some("spark".into()),
            manages_own_cursor: manages,
            capabilities,
            runtime_support,
            allow_network: networked,
            sidecar,
        };
        let profile = tn_config::Profile {
            name: name.clone(),
            kind: tn_config::ProfileKind::Agent,
            command: Some(command),
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: None,
            user: None,
            agent: Some(id.clone()),
            accent: Some(accent),
            glyph: Some("spark".into()),
        };

        // Editing → remove the old blocks first (id may be unchanged; remove +
        // re-append keeps a single entry, comments preserved).
        if let Some(edit) = self.agent_form_edit.clone() {
            self.remove_agent_persisted(&edit.old_id, &edit.old_profile_name);
        }
        if let Err(e) = tn_config::append_agent(&manifest) {
            tracing::error!(error = %e, "save_agent_form: append agent failed");
        }
        if let Err(e) = tn_config::append_profile(&profile) {
            tracing::error!(error = %e, "save_agent_form: append profile failed");
        }

        self.agent_form_open = false;
        self.agent_form_marked = None;
        self.agent_form_edit = None;
        self.reload_agents(cx);
        self.refocus_active(window, cx);
    }

    /// Delete the custom agent launched by `launch_profiles[idx]` (welcome tile ✕).
    fn delete_agent(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(p) = self.launch_profiles.get(idx).cloned() else {
            return;
        };
        let id = p
            .agent
            .clone()
            .or_else(|| p.command.clone())
            .unwrap_or_default();
        self.remove_agent_persisted(&id, &p.name);
        if self.agent_form_open {
            self.agent_form_open = false;
            self.agent_form_edit = None;
            self.agent_form_marked = None;
        }
        self.reload_agents(cx);
    }

    /// Delete the agent currently open in the editor (the 删除 button).
    fn delete_current_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.agent_form_edit.clone() else {
            return;
        };
        self.remove_agent_persisted(&edit.old_id, &edit.old_profile_name);
        self.agent_form_open = false;
        self.agent_form_edit = None;
        self.agent_form_marked = None;
        self.reload_agents(cx);
        self.refocus_active(window, cx);
    }

    /// Reload config from disk, rebuild the agent registry global, and recreate
    /// the welcome launchpad (re-subscribing) so a just-added/edited/deleted
    /// agent shows immediately — no restart.
    fn reload_agents(&mut self, cx: &mut Context<Self>) {
        self.config = Arc::new(tn_config::load());
        // Same build path as startup: a claude/codex-commanded manifest gets the
        // built-in usage parser (real ring, user's color), else a generic agent.
        let registry = crate::agent_host::build_registry(&self.config);
        cx.set_global(crate::agent_host::AgentHost(registry));
        self.launch_profiles = discover_profiles(&self.config);
        let welcome =
            cx.new(|cx| WelcomeView::new(cx, self.config.clone(), self.launch_profiles.clone()));
        Self::subscribe_welcome(&welcome, cx);
        self.welcome = welcome;
        cx.notify();
    }

    fn on_agent_form_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = ev.keystroke.key.as_str();
        // Printable ASCII (no modifiers, no IME CJK slip-through). Chinese in the
        // name field arrives via the IME path (EntityInputHandler), not here.
        let printable = |ev: &KeyDownEvent| -> Option<String> {
            ev.keystroke
                .key_char
                .as_ref()
                .filter(|c| {
                    !ev.keystroke.modifiers.control
                        && !ev.keystroke.modifiers.alt
                        && !ev.keystroke.modifiers.platform
                        && c.chars().all(|ch| ch.is_ascii_graphic() || ch == ' ')
                })
                .cloned()
        };
        match key {
            "escape" => self.close_agent_form(window, cx),
            "tab" => {
                // Cycle Name → Command → Sidecar → Name (Sidecar is the advanced field).
                self.agent_form_field = match self.agent_form_field {
                    AgentField::Name => AgentField::Command,
                    AgentField::Command => AgentField::Sidecar,
                    AgentField::Sidecar => AgentField::Name,
                };
                self.agent_form_marked = None;
                cx.notify();
            }
            "enter" => self.save_agent_form(window, cx),
            "backspace" => {
                // Delete the IME preedit first (name field), else the field text.
                if self.agent_form_field == AgentField::Name
                    && self.agent_form_marked.take().is_some()
                {
                    cx.notify();
                } else {
                    match self.agent_form_field {
                        AgentField::Name => {
                            self.agent_form.name.pop();
                        }
                        AgentField::Command => {
                            self.agent_form.command.pop();
                        }
                        AgentField::Sidecar => {
                            self.agent_form.sidecar.pop();
                        }
                    }
                    cx.notify();
                }
            }
            _ => {
                if let Some(c) = printable(ev) {
                    match self.agent_form_field {
                        AgentField::Name => self.agent_form.name.push_str(&c),
                        AgentField::Command => self.agent_form.command.push_str(&c),
                        AgentField::Sidecar => self.agent_form.sidecar.push_str(&c),
                    }
                    cx.notify();
                }
            }
        }
        cx.stop_propagation();
    }

    /// A labeled text input row of the agent editor; clicking focuses that field.
    /// The active field shows the caret (+ IME preedit, accent-colored, for the
    /// name field).
    fn agent_field_row(
        &self,
        label: &str,
        value: String,
        marked: String,
        placeholder: &str,
        field: AgentField,
        ime: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let ui = &self.config.theme.ui;
        let accent = self.agent_form.accent();
        let active = self.agent_form_field == field;
        let mono = SharedString::from(self.config.font().family.clone());
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(11.))
            .rounded(px(R_CARD))
            .bg(col(ui.surface_1)) // 输入井:L1(比 L3 浮板深一档,不透明)
            .border_1()
            .border_color(if active { rgba(PH_DIM) } else { rgba(H1) })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, w, cx| {
                    cx.stop_propagation();
                    this.agent_form_field = field;
                    this.agent_form_marked = None;
                    this.agent_form_focus.focus(w);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(52.))
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(label.to_string())),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .overflow_hidden()
                    .font_family(mono)
                    .text_size(px(13.5))
                    .when(!value.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(col(ui.foreground))
                                .child(SharedString::from(value.clone())),
                        )
                    })
                    .when(active && ime && !marked.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(col(accent))
                                .child(SharedString::from(marked.clone())),
                        )
                    })
                    .when(active, |d| {
                        d.child(
                            div()
                                .text_color(col(ui.muted))
                                .child(SharedString::from("▏")),
                        )
                    })
                    .when(value.is_empty() && marked.is_empty(), |d| {
                        d.child(
                            div()
                                .ml(px(2.))
                                .text_color(col(ui.muted))
                                .child(SharedString::from(placeholder.to_string())),
                        )
                    }),
            )
    }

    /// The 添加/编辑 Agent overlay, or `None` when closed: a dim scrim + a centered
    /// Calm Glass panel (name / command inputs · color swatches · Ink-cursor
    /// toggle · live preview · 取消/删除/保存).
    fn render_agent_form(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if !self.agent_form_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let editing = self.agent_form_edit.is_some();
        let accent = self.agent_form.accent();
        let name = self.agent_form.name.clone();
        let name_marked = self.agent_form_marked.clone().unwrap_or_default();
        let command = self.agent_form.command.clone();
        let can_save = !name.trim().is_empty() && !command.trim().is_empty();
        let title = if editing {
            "编辑 Agent"
        } else {
            "添加 Agent"
        };

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .pt(px(15.))
            .pb(px(3.))
            .child(
                div()
                    .w(px(28.))
                    .h(px(28.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(cola(accent, 0.16))
                    .child(icon("spark", 17., accent)),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(gpui::FontWeight(680.))
                    .text_color(col(ui.foreground))
                    .child(SharedString::from(title)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from("配置即数据 · 终端托管")),
            );

        let fields = div()
            .flex()
            .flex_col()
            .gap(px(9.))
            .px(px(16.))
            .py(px(8.))
            .child(self.agent_field_row(
                "名称",
                name.clone(),
                name_marked,
                "例:Gemini CLI",
                AgentField::Name,
                true,
                cx,
            ))
            .child(self.agent_field_row(
                "命令",
                command.clone(),
                String::new(),
                "例:gemini",
                AgentField::Command,
                false,
                cx,
            ))
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(col(ui.muted))
                    .pl(px(2.))
                    .child(SharedString::from(
                        "命令首词也用于「在 shell 里敲它自动切 Agent 态」。命令是 claude / codex 时自动显示用量。",
                    )),
            );

        // Color swatches (curated presets; the selected one is ringed).
        let mut swatches = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.))
            .flex_wrap();
        for (i, (_, c)) in tn_config::ACCENT_SWATCHES.iter().enumerate() {
            let sel = self.agent_form.accent_idx == i;
            let c = *c;
            swatches = swatches.child(
                div()
                    .w(px(24.))
                    .h(px(24.))
                    .rounded(px(999.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_2()
                    .border_color(if sel {
                        col(ui.foreground)
                    } else {
                        cola(c, 0.0)
                    })
                    .child(div().w(px(15.)).h(px(15.)).rounded(px(999.)).bg(col(c)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.agent_form.accent_idx = i;
                            cx.notify();
                        }),
                    ),
            );
        }
        let color_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(6.))
            .child(
                div()
                    .w(px(52.))
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from("颜色")),
            )
            .child(swatches);

        // Ink-cursor toggle.
        let on = self.agent_form.manages_cursor;
        let toggle = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .py(px(6.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    cx.stop_propagation();
                    this.agent_form.manages_cursor = !this.agent_form.manages_cursor;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(18.))
                    .h(px(18.))
                    .rounded(px(5.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(if on {
                        cola(accent, 0.9)
                    } else {
                        col(ui.surface_2).into()
                    })
                    .border_1()
                    .border_color(if on { cola(accent, 0.9) } else { rgba(H1) })
                    .when(on, |d| d.child(icon("check", 13., ui.chrome_bg))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(col(ui.foreground))
                            .child(SharedString::from("由 Agent 自绘光标(Ink TUI)")),
                    )
                    .child(div().text_size(px(10.5)).text_color(col(ui.muted)).child(
                        SharedString::from("Claude/Gemini 等 TUI 自管光标;关掉则终端画块光标"),
                    )),
            );

        // ── 高级(可选)· 遥测 sidecar — unlocks the usage ring + realtime chips
        // without a built-in adapter. A networked sidecar gets the confirm gate.
        let has_sidecar = !self.agent_form.sidecar.trim().is_empty();
        let net_on = self.agent_form.networked;
        let advanced = div()
            .flex()
            .flex_col()
            .gap(px(7.))
            .child(div().h(px(1.)).bg(rgba(H1)).mx(px(16.)).mt(px(4.)))
            .child(
                div().px(px(16.)).pt(px(3.)).child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight(600.))
                        .text_color(col(ui.muted))
                        .child(SharedString::from("高级(可选)· 用量遥测 — 不懂就留空")),
                ),
            )
            .child(div().px(px(16.)).child(self.agent_field_row(
                "遥测程序",
                self.agent_form.sidecar.clone(),
                String::new(),
                "留空即可(claude/codex 已自动显示用量)",
                AgentField::Sidecar,
                false,
                cx,
            )))
            .child(
                div()
                    .px(px(16.))
                    .text_size(px(10.5))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(
                        "开发者选项:一个会往屏幕输出 JSON 用量数据的伴随程序,Tn 据此显示用量环/状态。\
                         你大概率不需要它 —— 命令是 claude / codex 时用量会自动出。",
                    )),
            )
            .when(has_sidecar, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .px(px(16.))
                        .py(px(4.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                cx.stop_propagation();
                                this.agent_form.networked = !this.agent_form.networked;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .w(px(18.))
                                .h(px(18.))
                                .rounded(px(5.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if net_on {
                                    cola(accent, 0.9)
                                } else {
                                    col(ui.surface_2).into()
                                })
                                .border_1()
                                .border_color(if net_on {
                                    cola(accent, 0.9)
                                } else {
                                    rgba(H1)
                                })
                                .when(net_on, |d| d.child(icon("check", 13., ui.chrome_bg))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(col(ui.foreground))
                                        .child(SharedString::from("联网 sidecar(连接前需确认)")),
                                )
                                .child(div().text_size(px(10.5)).text_color(col(ui.muted)).child(
                                    SharedString::from("默认拒绝;勾选后启动弹确认卡,允许才连"),
                                )),
                        ),
                )
            });

        // Live preview chip.
        let preview_name = if name.trim().is_empty() {
            "未命名 Agent".to_string()
        } else {
            name.clone()
        };
        let preview = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .py(px(6.))
            .child(
                div()
                    .w(px(52.))
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from("预览")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(R_CARD))
                    .bg(col(ui.surface_2)) // L2 卡片
                    .border_1()
                    .border_color(cola(accent, 0.3))
                    .child(
                        div()
                            .w(px(22.))
                            .h(px(22.))
                            .rounded(px(5.)) // SHEET 02 `.amark` r5
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(cola(accent, 0.16))
                            .child(icon("spark", 14., accent)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(gpui::FontWeight(620.))
                            .text_color(col(ui.foreground))
                            .child(SharedString::from(preview_name)),
                    ),
            );

        // Buttons: [spacer] (删除) 取消 保存/添加.
        let red = t.ansi.red;
        let cancel_btn = crate::style::btn("取消").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, w, cx| {
                cx.stop_propagation();
                this.close_agent_form(w, cx);
            }),
        );
        let save_btn = if can_save {
            crate::style::btn_primary(if editing { "保存" } else { "添加" })
        } else {
            crate::style::btn(if editing { "保存" } else { "添加" })
        }
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, w, cx| {
                cx.stop_propagation();
                this.save_agent_form(w, cx);
            }),
        );
        let mut buttons = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .pt(px(8.))
            .pb(px(14.))
            .child(div().flex_1());
        let _ = red;
        if editing {
            buttons = buttons.child(crate::style::btn_danger("删除").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| {
                    cx.stop_propagation();
                    this.delete_current_agent(w, cx);
                }),
            ));
        }
        buttons = buttons.child(cancel_btn).child(save_btn);

        let ime_focus = self.agent_form_focus.clone();
        let ime_entity = cx.entity();
        let name_field_active = self.agent_form_field == AgentField::Name;

        let panel = shadowed(
            div()
                .relative()
                .flex()
                .flex_col()
                .w(px(460.))
                .max_w(relative(0.92))
                .max_h(relative(0.9))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(H2))
                .bg(col(ui.palette_bg)) // L3 不透明浮板
                .child(header)
                .child(fields)
                .child(color_row)
                .child(toggle)
                .child(advanced)
                .child(preview)
                .child(div().h(px(1.)).bg(rgba(H1)))
                .child(buttons)
                .when(name_field_active, |d| {
                    d.child(
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _state, window, cx| {
                                window.handle_input(
                                    &ime_focus,
                                    ElementInputHandler::new(bounds, ime_entity.clone()),
                                    cx,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                }),
            shadow_float(),
        );

        let pl = if self.explorer_open {
            self.explorer_width + 23.
        } else {
            12.
        };

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .pl(px(pl))
                .pr(px(12.))
                .bg(rgba(SCRIM))
                .track_focus(&self.agent_form_focus)
                .on_key_down(cx.listener(Self::on_agent_form_key))
                // 浮层 scrim 统一吞滚轮(BUG发现 #5 同类)。
                .on_scroll_wheel(cx.listener(|_t, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, w, cx| {
                        this.close_agent_form(w, cx);
                    }),
                )
                .child(
                    div()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| {
                                cx.stop_propagation();
                                this.agent_form_focus.focus(w);
                                cx.notify();
                            }),
                        )
                        .child(panel),
                ),
        )
    }
}

impl EntityInputHandler for Workspace {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self
            .active_ime_marked()
            .unwrap_or("")
            .encode_utf16()
            .collect();
        let start = range.start.min(units.len());
        let end = range.end.min(units.len());
        *adjusted = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !self.ime_target_active() {
            return None;
        }
        let end = self
            .active_ime_marked()
            .map(|s| s.encode_utf16().count())
            .unwrap_or(0);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.active_ime_marked()
            .map(|s| 0..s.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.agent_name_is_ime_target() {
            self.agent_form_marked = None;
        } else {
            self.ssh_rename_marked = None;
        }
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agent_name_is_ime_target() {
            self.agent_form.name.push_str(text);
            self.agent_form_marked = None;
        } else {
            if let Some(draft) = self.ssh_rename.as_mut() {
                draft.name.push_str(text);
            }
            self.ssh_rename_marked = None;
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let marked = (!new_text.is_empty()).then(|| new_text.to_string());
        if self.agent_name_is_ime_target() {
            self.agent_form_marked = marked;
        } else {
            self.ssh_rename_marked = marked;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Where the IME candidate window anchors, relative to the registered
        // canvas (the open overlay's panel): near the name row for the agent
        // editor, near the rename row for the SSH connector.
        let (dx, dy) = if self.agent_form_open {
            (96., 92.)
        } else {
            (72., 112.)
        };
        Some(Bounds {
            origin: gpui::point(
                element_bounds.origin.x + px(dx),
                element_bounds.origin.y + px(dy),
            ),
            size: gpui::size(px(220.), px(28.)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
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
            self.focus_out_sub =
                Some(
                    window.on_focus_out(&self.workspace_focus, cx, move |_ev, window, _cx| {
                        if window.is_window_active() {
                            anchor.focus(window);
                        }
                    }),
                );
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
                    // Guard the OS window close (titlebar ✕ / Alt+F4) with the same
                    // unsaved-edit prompt as the in-app close/quit paths. The titlebar
                    // close button is OS-direct (NC hit via `window_control_area(Close)`),
                    // so without this hook it bypasses the dirty-close guard and silently
                    // drops unsaved Quick Look edits. Returning `false` blocks the close
                    // and shows the save/discard prompt; picking save/discard then emits
                    // `QuitConfirmed` → `cx.quit()`.
                    let weak = cx.entity().downgrade();
                    window.on_window_should_close(cx, move |_window, app| {
                        weak.update(app, |ws, cx| {
                            if ws.quick_look_open {
                                let can_quit = ws.quick_look.update(cx, |v, cx| v.request_quit(cx));
                                if !can_quit {
                                    return false;
                                }
                                ws.quick_look_open = false;
                                ws.ql_rail_pane = None;
                                ws.ql_refocus_active_pane = true;
                            }
                            true
                        })
                        .unwrap_or(true)
                    });
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
        if self.palette_open && (self.palette_needs_focus || !self.palette_focus.is_focused(window))
        {
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
        let other_overlay_open = self.palette_open
            || self.split_launcher_open
            || self.layout_manager_open
            || self.ssh_prompt_open
            || self.remote_dir_picker.is_some()
            || self.agent_dir_picker.is_some()
            || self.agent_form_open;
        if self.quick_look_open && !other_overlay_open {
            let quick_look_focus = self.quick_look.read(cx).focus_handle();
            if !quick_look_focus.is_focused(window) {
                quick_look_focus.focus(window);
            }
        }
        if self.ssh_prompt_open
            && (self.ssh_prompt_needs_focus || !self.ssh_prompt_focus.is_focused(window))
        {
            self.ssh_prompt_needs_focus = false;
            self.ssh_prompt_focus.focus(window);
        }
        if self.remote_dir_picker.is_some()
            && (self.remote_dir_needs_focus || !self.remote_dir_focus.is_focused(window))
        {
            self.remote_dir_needs_focus = false;
            self.remote_dir_focus.focus(window);
        }
        if self.agent_dir_picker.is_some()
            && (self.agent_dir_needs_focus || !self.agent_dir_focus.is_focused(window))
        {
            self.agent_dir_needs_focus = false;
            self.agent_dir_focus.focus(window);
        }
        if self.agent_form_open
            && (self.agent_form_needs_focus || !self.agent_form_focus.is_focused(window))
        {
            self.agent_form_needs_focus = false;
            self.agent_form_focus.focus(window);
        }
        // IME control: disable IME for ASCII overlay fields (the SSH target filter,
        // the agent editor's command field), but keep it on for the SSH favorite
        // rename + the agent editor's name field so Chinese names work.
        //
        // **Navigation-only overlays (remote dir picker / split launcher / layout
        // manager) MUST disable IME too**: they have no text input and drive purely
        // by arrows/Enter/Esc. With an active CJK IME + the `install_ime_keyfix`
        // window subclass (which routes `VK_PROCESSKEY` to the IME), an *enabled* IME
        // claims those navigation keys before they reach gpui's `on_key_down` → the
        // panel's keyboard goes completely dead (踩过的坑:远端目录 picker 按键无反应).
        // Turning IME off makes the keys arrive as plain VKs so the handlers fire.
        let disable_ime = (self.ssh_prompt_open && self.ssh_rename.is_none())
            || (self.agent_form_open && self.agent_form_field != AgentField::Name)
            || self.remote_dir_picker.is_some()
            || self.agent_dir_picker.is_some()
            || self.split_launcher_open
            || self.layout_manager_open
            || self.palette_open; // search reads ASCII key_char; CJK search is parked
        if disable_ime != self.ime_disabled {
            if let Some(hwnd) = crate::platform::hwnd_of(window) {
                crate::platform::set_ime_enabled(hwnd, !disable_ime);
            }
            self.ime_disabled = disable_ime;
        }
        // Quick Look closed via its own keyboard (Esc/Space) — return focus to the
        // file list (or active pane) now (the event callback had no `window`).
        if self.ql_refocus_active_pane {
            self.ql_refocus_active_pane = false;
            self.refocus_active(window, cx);
        }
        if self.ql_refocus_pane {
            self.ql_refocus_pane = false;
            self.refocus_after_quick_look(window, cx);
        }

        // Time the chrome build when TN_PERF is on. Panes are
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
        let overlay_focused = workspace_overlay_freezes_pane_focus(
            self.palette_open,
            self.split_launcher_open,
            self.layout_manager_open,
            self.quick_look_open,
            self.ssh_prompt_open,
            self.agent_form_open,
            self.remote_dir_picker.is_some(),
            self.agent_dir_picker.is_some(),
        );
        if !overlay_focused && !self.tabs[active].welcome {
            let mut leaves = Vec::new();
            collect_leaves(&self.tabs[active].root, &mut leaves);
            if let Some(id) = leaves.into_iter().find(|id| {
                self.panes
                    .get(id)
                    .is_some_and(|v| v.read(cx).focus_handle().is_focused(window))
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
                let explorer_focused =
                    self.explorer_open && self.explorer.read(cx).focus_handle().is_focused(window);
                if !explorer_focused && !self.workspace_focus.is_focused(window) {
                    self.workspace_focus.focus(window);
                }
            }
        }
        let focused = self.tabs[active].focused;

        // Explorer follows the focused pane's effective cwd. Two distinct cases,
        // each with its own tree-state policy (面板解耦):
        //   • Focus moved to a *different* pane → save the outgoing pane's view
        //     snapshot (expansion + selection) and restore the incoming pane's
        //     via `switch_pane`, so each split pane keeps its own tree state.
        //   • Same pane, cwd changed (OSC 7 `cd`) → `follow_root`, which keeps the
        //     expansion for the direct ancestry (子目录保留展开态).
        // Compare roots before re-rooting so we only rebuild when the path
        // actually changed (never every frame). Welcome tabs have no real pane,
        // so they reset the shared explorer to the default Host root.
        // 宠物上下文同步:welcome_only 模式 + 欢迎页 2× 形态(SHEET 05/07)。
        // 2× 形态由 PetView 自己渲染(on_welcome),welcome 不再持品种快照。
        let on_welcome = self.tabs[active].welcome;
        self.pet
            .update(cx, |p, cx| p.set_on_welcome(on_welcome, cx));

        // QuickLook RAIL 读数同步:从活动栏卡片打开时(`ql_rail_pane`)喂入
        //「(当前序号, 该 pane 本次改动文件总数)」,footer 显示 RAIL · n/N;从文件树
        // 打开则为 None(SHEET 03 footer)。
        let ql_rail_pos = self.ql_rail_pane.and_then(|pid| {
            self.panes
                .get(&pid)
                .map(|v| (self.ql_rail_idx, v.read(cx).rail_len()))
        });
        self.quick_look
            .update(cx, |v, cx| v.set_rail_pos(ql_rail_pos, cx));

        if self.tabs[active].welcome {
            self.reset_explorer_for_welcome_tab(cx);
        } else {
            if Some(focused) != self.explorer_pane {
                // Focus switched panes — stash the old pane's state, load the new.
                if let Some(prev) = self.explorer_pane {
                    if prev != WELCOME_DUMMY && self.leaf_exists(prev) {
                        let snap = self.explorer.read(cx).snapshot();
                        self.explorer_states.insert(prev, snap);
                    }
                    // Drop snapshots for panes that have since been closed.
                    let live_ids: std::collections::HashSet<PaneId> =
                        self.panes.keys().copied().collect();
                    self.explorer_states.retain(|id, _| live_ids.contains(id));
                }
                if let Some(new_root) = self.panes.get(&focused).and_then(|v| {
                    explorer_root_for_pane(&v.read(cx), self.pane_specs.get(&focused))
                }) {
                    let snap = self.explorer_states.get(&focused).cloned();
                    self.explorer
                        .update(cx, |e, cx| e.switch_pane(new_root, snap, cx));
                    self.explorer_pane = Some(focused);
                }
            } else if let Some(new_root) = self
                .panes
                .get(&focused)
                .and_then(|v| explorer_root_for_pane(&v.read(cx), self.pane_specs.get(&focused)))
            {
                if self.explorer.read(cx).root() != new_root {
                    self.explorer
                        .update(cx, |e, cx| e.follow_root(new_root, cx));
                }
            }
        }
        let ui = &self.config.theme.ui;

        // Each tab labels itself with its focused pane's OSC title, falling back
        // to "Term N", and carries that pane's agent for an identity dot.
        // Precomputed so the click closures below own `cx` freely.
        // Carry the resolved identity-dot color (Some = an agent pane) so the
        // render closures below don't need `cx`/the registry — agent-agnostic.
        let tab_info: Vec<(String, usize, Option<tn_config::Color>, &'static str)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(_i, tab)| {
                if tab.welcome {
                    return ("欢迎".to_string(), 1, None, "▣"); // launchpad tab
                }
                let pane = self.panes.get(&tab.focused);
                let label = pane
                    .map(|v| truncate_label(&v.read(cx).tab_label(), 24))
                    .unwrap_or_else(|| "shell".into());
                let agent = pane.and_then(|v| v.read(cx).agent());
                let dot = agent.as_ref().map(|id| self.agent_color(Some(id), cx));
                // 身份字形随 agent id(✳/◆/⟡),shell = ❯(差异总结 1-3)。
                let glyph = agent
                    .as_ref()
                    .map(|id| crate::welcome::agent_glyph_ch(id.as_str()))
                    .unwrap_or("❯");
                (label, tab.root.leaf_count(), dot, glyph)
            })
            .collect();

        // SHEET 01 `.tabs`:会话即仪表 — 顶部 2px 身份色棒(磷光=shell,紫/蓝=agent)。
        let surface_1 = ui.surface_1; // L1:hover 抬升
        let surface_2 = ui.surface_2; // L2:激活 tab
        let mono_family = SharedString::from(self.config.font().family.clone());
        let tabs = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .px(px(6.))
            .children(tab_info.into_iter().enumerate().map(
                |(i, (label, panes, agent_dot, tab_glyph))| {
                    let is_active = i == active;
                    // 身份棒颜色:agent 身份色,shell = 磷光(SHEET 01 板 C)。
                    let bar_c = agent_dot.unwrap_or(tn_config::Color::new(0x5B, 0xE7, 0xC4));
                    let hover_bg = col(surface_1);
                    div()
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(8.)) // `.tab` gap 8
                        .h(px(30.)) // 42 − 2×6(`.tab` margin:6px 0)
                        .px(px(14.)) // `.tab` padding 0 14
                        .rounded(px(R_CARD)) // r4
                        .text_size(px(12.)) // sans 12
                        // 激活 = L2 抬升 + 1px h1 + 顶部 2px 身份棒(横向小渐变,契约 2)
                        .when(is_active, |d| {
                            d.bg(col(surface_2))
                                .border_1()
                                .border_color(rgba(H1))
                                .text_color(rgb(T0))
                                .child(
                                    div()
                                        .absolute()
                                        .top(px(-1.))
                                        .left(px(8.))
                                        .right(px(8.))
                                        .h(px(2.))
                                        .rounded_b(px(2.))
                                        .bg(linear_gradient(
                                            90.,
                                            linear_color_stop(cola(bar_c, 0.), 0.),
                                            linear_color_stop(col(bar_c), 0.5),
                                        )),
                                )
                        })
                        .when(!is_active, |d| {
                            d.text_color(rgb(T2))
                                .hover(move |s| s.bg(hover_bg).text_color(rgb(T1)))
                        })
                        // `.gl` 身份字形:agent = ✳/◆/⟡ 身份色;shell = ❯ t3
                        // (mono 600 11;字形表与磁贴/面板同源,差异总结 1-3)
                        .child(
                            div()
                                .font_family(mono_family.clone())
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight(600.))
                                .text_color(if agent_dot.is_some() {
                                    col(bar_c)
                                } else {
                                    rgb(T3)
                                })
                                .child(tab_glyph),
                        )
                        .child(label)
                        .when(panes > 1, |d| {
                            d.child(
                                div()
                                    .font_family(mono_family.clone())
                                    .text_size(px(10.0))
                                    .text_color(rgb(T3))
                                    .child(format!("⌗{panes}")),
                            )
                        })
                        // Close button: kills the tab's process(es). stop_propagation
                        // so it closes the tab instead of just activating it.
                        .child(
                            div()
                                .ml(px(2.))
                                .px(px(2.))
                                .rounded(px(R_CHIP))
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(move |s| s.bg(hover_bg))
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
                },
            ))
            .child(
                // `.tab-new`:26×26 · r4 · hover L1
                div()
                    .w(px(26.))
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(R_CARD))
                    .hover(move |s| s.bg(col(surface_1)))
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
            .gap(px(9.)) // `.brand` gap 9
            .h_full()
            .pl(px(14.)) // `.brand` padding 0 16 0 14
            .pr(px(16.))
            .border_r(px(1.)) // 右接缝:brand 是仪表舱里的独立模块
            .border_color(rgba(H0))
            .when(menu_open, |d| d.bg(col(ui.surface_1)))
            .hover(|s| s.bg(col(ui.surface_1)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.app_menu_open = !this.app_menu_open;
                    cx.notify();
                }),
            )
            .child(
                // `.mark`:18×18 透明 + 1px ph-dim 边 + 8×8 磷光内核 — 最小的「活物」
                div()
                    .w(px(18.))
                    .h(px(18.))
                    .rounded(px(R_CARD))
                    .border_1()
                    .border_color(rgba(PH_DIM))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().w(px(8.)).h(px(8.)).rounded(px(1.)).bg(rgb(PH))),
            )
            .child(
                // `.name`:TN + 磷光下划线光标(mono 600 12)
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .font_family(SharedString::from(self.config.font().family.clone()))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight(600.))
                    .child(div().text_color(rgb(T0)).child("TN"))
                    .child(div().text_color(rgb(PH)).child("_")),
            )
            .child(
                // `.chev`:t3 结构字符;菜单开启时换磷光向上
                crate::assets::icon("chev-d", 12.).text_color(if menu_open {
                    rgb(PH)
                } else {
                    rgb(T3)
                }),
            );

        // Window controls: the OS performs the action from the marked region
        // (HTMINBUTTON / HTMAXBUTTON / HTCLOSE) — no click handler needed.
        // SHEET 01 `.wbtn`:44 宽全高方块,无色面 hover 才表态(L1;关闭 = err-soft)。
        // `.occlude()` (BlockMouse) prevents the root track_focus from intercepting
        // NC mouse-down events and calling prevent_default, which would swallow the
        // OS window command (same pattern as the drag spacer).
        let hover_l1 = col(ui.surface_1);
        let ctl_btn = |name: &'static str, area: WindowControlArea, danger: bool| {
            div()
                .w(px(44.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(move |s| {
                    if danger {
                        s.bg(rgba(ERR_SOFT))
                    } else {
                        s.bg(hover_l1)
                    }
                })
                .occlude()
                .window_control_area(area)
                .child(icon(
                    name,
                    13.,
                    if danger {
                        tn_config::Color::new(0xE8, 0x70, 0x7E)
                    } else {
                        ui.muted
                    },
                ))
        };
        let controls = div()
            .flex()
            .flex_row()
            .h_full()
            .child(ctl_btn("min", WindowControlArea::Min, false))
            .child(ctl_btn("max", WindowControlArea::Max, false))
            .child(ctl_btn("close", WindowControlArea::Close, true));

        // SHEET 01 `.titlebar`:高 42 · L0 · 底 1px h0;brand/tabs/窗控全部贴边。
        let titlebar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(TITLEBAR_H))
            .flex_none()
            .border_b(px(1.))
            .border_color(rgba(H0))
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

        // SHEET 01/02 `.canvas`:L0 底盘露出 2px 接缝,板面平铺其上(接缝即深度,
        // 契约 4 — 平铺零投影)。
        let body =
            div()
                .flex_1()
                .min_h(px(0.)) // let the flex child be bounded by the window, not its content
                .p(px(SEAM))
                .flex()
                .flex_row()
                .gap(px(SEAM))
                // File explorer sidebar (left column), toggled by Ctrl+Shift+B.
                // Width is adjustable by dragging the right edge (same look-and-feel
                // as split-pane dividers). 欢迎页(Launchpad)也常驻 Explorer —— 与工作区
                // 一致的左侧文件树,Launchpad 居中铺在右侧主列(产品决策:欢迎页保留
                // Explorer,已回写 SHEET 07 板 A/A2)。
                .when(self.explorer_open, |d| {
                    let accent = self.config.theme.ui.accent;
                    let ew = self.explorer_width;
                    d.child(
                        div()
                            .w(px(ew))
                            .flex_none()
                            .min_h(px(0.))
                            .flex()
                            .flex_col()
                            .relative()
                            .child(div().flex_1().min_h(px(0.)).child(self.explorer.clone()))
                            // Drag handle on the right edge; straddles the 2px seam
                            // so it doesn't occlude the tree.
                            .child(
                                div()
                                    .absolute()
                                    .top(px(0.))
                                    .bottom(px(0.))
                                    .right(px(-4.)) // 跨过 2px 接缝
                                    .w(px(6.))
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
                    div().flex_1().min_w(px(0.)).min_h(px(0.)).child(
                        if self.tabs[active].welcome {
                            self.welcome.clone().into_any_element()
                        } else {
                            self.render_node(&self.tabs[active].root, focused, cx, Vec::new())
                        },
                    ),
                );

        // Quick Look 速览浮层:绝对定位浮在工作区之上,贴文件树右缘(explorer 开 → 锚到
        // 它右边;关 → 锚到工作区左缘),仅在装了文件时渲染。它**不占分屏**——飘在终端上,
        // Esc/再按 Ctrl+Shift+J 收起。放在 root 的 body/status 之后 = 画在它们之上。
        let quick_look = (self.quick_look_open && self.quick_look.read(cx).has_file()).then(|| {
            // Click-away scrim over the whole workspace body(titlebar/status bar
            // 之外的全部,**含 Explorer** —— SHEET 03-A stage inset 0,scrim 压暗
            // 三块板)。A click on the bare terminal used to `focus_pane` and steal
            // focus to the shell mid-edit (the「焦点漏到底层 shell / 面板穿透」bug);
            // now it closes the overlay cleanly (`ql_refocus` returns focus to the
            // tree / active pane). Clicking the panel itself is swallowed by its own
            // root (see `quick_look.rs` inner `on_mouse_down`). 改动文件间导航走
            // ↑↓(RAIL),不再依赖「隔着浮层点树」。
            div()
                .absolute()
                .top(px(TITLEBAR_H)) // below the titlebar
                .bottom(px(STATUSBAR_H)) // above the status bar
                .left(px(0.))
                .right(px(0.))
                // SHEET 03:纯色压暗 scrim(无模糊,契约 7)—— 把底层终端压暗,QuickLook
                // 才像「工作区之上的临时速览浮层」而非嵌在 pane 内的 child。
                .bg(rgba(SCRIM))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|ws, _e, _w, cx| {
                        let closed = ws.quick_look.update(cx, |v, cx| v.request_close(cx));
                        if closed {
                            ws.quick_look_open = false;
                            ws.ql_refocus_pane = true;
                        }
                        cx.notify();
                    }),
                )
                // scrim 吞滚轮:压暗区上滚动不得驱动底层终端 scrollback
                // (BUG发现 #5 面板穿透;浮层本体的兜底见 quick_look.rs 根节点)。
                .on_scroll_wheel(cx.listener(|_ws, _e: &gpui::ScrollWheelEvent, _w, cx| {
                    cx.stop_propagation();
                }))
                .child(
                    // SHEET 03:居中速览卡 — scrim 内水平/垂直居中。原型硬规格
                    // 1080×520;窗体不够宽/高时按 86%/80% 退让,够则锁在 1080×520
                    // (二轮差异总结 3-3:尺寸不随 pane 漂)。
                    div()
                        .absolute()
                        .top(px(0.))
                        .bottom(px(0.))
                        .left(px(0.))
                        .right(px(0.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(relative(0.86))
                                .max_w(px(1080.))
                                .h(relative(0.80))
                                .max_h(px(520.))
                                .child(self.quick_look.clone()),
                        ),
                )
        });

        let palette = self.render_palette(cx);
        let split_launcher = self.render_split_launcher(cx);
        let layout_manager = self.render_layout_manager(cx);
        let app_menu = self.render_app_menu(cx);
        let ssh_prompt = self.render_ssh_prompt(cx);
        let agent_dir_picker = self.render_agent_dir_picker(cx);
        let agent_form = self.render_agent_form(cx);
        let remote_dir_picker = self.render_remote_dir_picker(cx);

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
            .on_action(cx.listener(|this, _: &Quit, _w, cx| this.request_quit(cx)))
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
            // rounds the actual window corners (Win11). 磷光契约 1:整窗 L0 不透明
            // 底盘 — 大面积零渐变零透明,色带从物理上不可能发生。
            .bg(self.window_glass()) // opaque L0(theme backdrop = solid)
            .text_color(col(ui.foreground))
            .font_family(UI_SANS) // UI sans for chrome; panes set mono themselves
            .child(titlebar)
            .child(body)
            .child(self.render_status_bar(cx))
            .when_some(quick_look, |d, q| d.child(q))
            .when_some(palette, |d, p| d.child(p))
            .when_some(split_launcher, |d, s| d.child(s))
            .when_some(layout_manager, |d, l| d.child(l))
            .when_some(app_menu, |d, m| d.child(m))
            .when_some(ssh_prompt, |d, s| d.child(s))
            .when_some(remote_dir_picker, |d, p| d.child(p))
            .when_some(agent_dir_picker, |d, p| d.child(p))
            .when_some(agent_form, |d, f| d.child(f))
            // 像素宠物:最顶层 overlay(SHEET 05)。置于浮层之上,磷光通道的光点/裂缝
            // 才能盖过 Quick Look / 命令面板的 scrim 被看见(规则 J「宠物打开窗口」)。
            // 宠物根穿透,只有狗本体/菜单/卡片有命中区,不抢浮层焦点。
            .child(self.pet.clone());

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

    #[test]
    fn validate_ssh_target_accepts_good_targets() {
        // C2: well-formed targets (and empty input) pass.
        for ok in [
            "",
            "host",
            "user@host",
            "host:22",
            "root@10.0.0.5:2222",
            "alma",
        ] {
            assert!(validate_ssh_target(ok).is_ok(), "{ok:?} should be ok");
        }
        // A non-numeric suffix stays part of the host (matches SshConfig::parse).
        assert!(validate_ssh_target("host:notaport").is_ok());
    }

    #[test]
    fn validate_ssh_target_flags_bad_targets() {
        // C2: empty host, dangling @/:, and out-of-range ports are caught pre-dial.
        assert!(validate_ssh_target("@host").is_err()); // empty user
        assert!(validate_ssh_target("user@").is_err()); // empty host
        assert!(validate_ssh_target("host:").is_err()); // dangling colon
        assert!(validate_ssh_target("host:0").is_err()); // port 0
        assert!(validate_ssh_target("host:70000").is_err()); // > 65535
        assert!(validate_ssh_target(":22").is_err()); // no host
    }

    #[test]
    fn parse_ssh_target_chips_splits_display_parts() {
        assert_eq!(
            parse_ssh_target_chips("root@192.168.1.1:2222"),
            Some((
                Some("root".to_string()),
                "192.168.1.1".to_string(),
                Some("2222".to_string())
            ))
        );
        assert_eq!(
            parse_ssh_target_chips("host:notaport"),
            Some((None, "host:notaport".to_string(), None))
        );
        assert_eq!(parse_ssh_target_chips(""), None);
    }

    #[test]
    fn discover_profiles_hides_removed_builtin_agent_tiles() {
        let mut loaded = tn_config::Loaded::builtin();
        loaded.config.profiles = vec![
            tn_config::Profile {
                name: "Claude".into(),
                kind: tn_config::ProfileKind::Agent,
                command: Some("claude".into()),
                args: Vec::new(),
                cwd: None,
                distro: None,
                host: None,
                user: None,
                agent: Some("claude".into()),
                accent: None,
                glyph: None,
            },
            tn_config::Profile {
                name: "Codex".into(),
                kind: tn_config::ProfileKind::Agent,
                command: Some("codex".into()),
                args: Vec::new(),
                cwd: None,
                distro: None,
                host: None,
                user: None,
                agent: Some("codex".into()),
                accent: None,
                glyph: None,
            },
            tn_config::Profile {
                name: "Agent".into(),
                kind: tn_config::ProfileKind::Agent,
                command: Some("agent".into()),
                args: Vec::new(),
                cwd: None,
                distro: None,
                host: None,
                user: None,
                agent: Some("agent".into()),
                accent: None,
                glyph: None,
            },
        ];
        loaded.config.agents = vec![tn_config::AgentManifest {
            id: "agent".into(),
            label: Some("Agent".into()),
            short: Some("Agent".into()),
            aliases: vec!["agent".into()],
            accent: None,
            glyph: Some("spark".into()),
            manages_own_cursor: false,
            capabilities: Vec::new(),
            runtime_support: Vec::new(),
            allow_network: false,
            sidecar: None,
        }];

        let profiles = discover_profiles(&loaded);
        assert!(profiles.iter().any(|p| p.name == "Agent"));
        assert!(!profiles.iter().any(|p| p.name == "Claude"));
        assert!(!profiles.iter().any(|p| p.name == "Codex"));
    }

    #[test]
    fn discover_profiles_migrates_old_builtin_agent_tiles_to_generic_agent() {
        let mut loaded = tn_config::Loaded::builtin();
        loaded.config.agents.clear();
        loaded.config.profiles = vec![
            tn_config::Profile {
                name: "Claude".into(),
                kind: tn_config::ProfileKind::Agent,
                command: Some("claude".into()),
                args: Vec::new(),
                cwd: None,
                distro: None,
                host: None,
                user: None,
                agent: Some("claude".into()),
                accent: None,
                glyph: None,
            },
            tn_config::Profile {
                name: "Codex".into(),
                kind: tn_config::ProfileKind::Agent,
                command: Some("codex".into()),
                args: Vec::new(),
                cwd: None,
                distro: None,
                host: None,
                user: None,
                agent: Some("codex".into()),
                accent: None,
                glyph: None,
            },
        ];

        let profiles = discover_profiles(&loaded);
        assert_eq!(
            profiles
                .iter()
                .filter(|p| p.kind == tn_config::ProfileKind::Agent)
                .count(),
            1
        );
        let agent = profiles.iter().find(|p| p.name == "Agent").unwrap();
        assert_eq!(agent.agent.as_deref(), Some("agent"));
        assert_eq!(agent.command.as_deref(), Some("agent"));
        assert!(!profiles.iter().any(|p| p.name == "Claude"));
        assert!(!profiles.iter().any(|p| p.name == "Codex"));
    }

    #[test]
    fn slugify_lowercases_and_collapses_nonalnum() {
        assert_eq!(slugify("Gemini CLI"), "gemini-cli");
        assert_eq!(slugify("  Qwen-Code  "), "qwen-code");
        assert_eq!(slugify("npx @sourcegraph/amp"), "npx-sourcegraph-amp");
        assert_eq!(slugify("通义千问"), ""); // no ascii alnum → empty (caller falls back)
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn first_nonempty_slug_falls_back_through_candidates() {
        // CJK name slugs to empty → fall back to the command's first word (the
        // caller passes `command.split_whitespace().next()`, so it's a bare token).
        assert_eq!(first_nonempty_slug(&["通义千问", "qwen"]), "qwen");
        // Both empty → the generic "agent".
        assert_eq!(first_nonempty_slug(&["通义", "—"]), "agent");
        assert_eq!(first_nonempty_slug(&["Gemini CLI", "gemini"]), "gemini-cli");
    }

    #[test]
    fn unique_agent_id_suffixes_on_collision() {
        let mut existing = std::collections::HashSet::new();
        existing.insert("gemini".to_string());
        existing.insert("gemini-2".to_string());
        assert_eq!(unique_agent_id("gemini", &existing), "gemini-3");
        assert_eq!(unique_agent_id("aider", &existing), "aider"); // free → unchanged
    }

    #[test]
    fn short_name_takes_first_word_capped() {
        assert_eq!(short_name("Gemini CLI"), "Gemini");
        assert_eq!(short_name("Qwen"), "Qwen");
        assert_eq!(short_name("aaaaaaaaaaaaaaaaaaaa").len(), 16); // capped
    }

    #[test]
    fn dedup_agent_profiles_keeps_one_per_agent_latest_wins() {
        let agent = |name: &str, id: &str, accent: u32| tn_config::Profile {
            name: name.into(),
            kind: tn_config::ProfileKind::Agent,
            command: Some(id.into()),
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: None,
            user: None,
            agent: Some(id.into()),
            accent: Some(tn_config::Color::new(
                (accent >> 16) as u8,
                (accent >> 8) as u8,
                accent as u8,
            )),
            glyph: None,
        };
        let shell = tn_config::Profile {
            name: "pwsh".into(),
            kind: tn_config::ProfileKind::Shell,
            command: Some("powershell.exe".into()),
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: None,
            user: None,
            agent: None,
            accent: None,
            glyph: None,
        };
        // A stale claude (old default) + a freshly added one + a codex dup too.
        let out = dedup_agent_profiles(vec![
            agent("claude", "claude", 0xF79AC0), // stale leftover
            shell.clone(),
            agent("codex", "codex", 0x000000),   // stale leftover
            agent("claude", "claude", 0x7AA2F7), // user's new one → kept (latest)
            agent("codex", "codex", 0x73DACA),   // user's new one → kept
        ]);
        // Exactly one claude + one codex + the shell.
        assert_eq!(out.len(), 3);
        let claude: Vec<_> = out
            .iter()
            .filter(|p| p.agent.as_deref() == Some("claude"))
            .collect();
        let codex: Vec<_> = out
            .iter()
            .filter(|p| p.agent.as_deref() == Some("codex"))
            .collect();
        assert_eq!(claude.len(), 1);
        assert_eq!(codex.len(), 1);
        // Latest (last) occurrence wins → the user's accents, not the stale ones.
        assert_eq!(
            claude[0].accent,
            Some(tn_config::Color::new(0x7A, 0xA2, 0xF7))
        );
        assert_eq!(
            codex[0].accent,
            Some(tn_config::Color::new(0x73, 0xDA, 0xCA))
        );
        // Non-agent profiles are never deduped.
        assert!(out.iter().any(|p| p.name == "pwsh"));
    }

    fn ssh_cfg() -> tn_pty::SshConfig {
        tn_pty::SshConfig {
            host: "example.com".into(),
            port: 2222,
            user: "alice".into(),
            key_path: None,
            password: None,
            shell_init: None,
        }
    }

    #[test]
    fn explorer_root_from_parts_maps_ssh_cwd_to_remote_root() {
        let cfg = ssh_cfg();
        let root = explorer_root_from_parts(
            FileNamespace::Ssh,
            Some("/home/alice/project".into()),
            None,
            Some(cfg.clone()),
        )
        .expect("ssh cwd becomes browsable via remote fs");
        assert_eq!(root.path_buf(), None);
        assert_eq!(
            root.remote_path().map(|p| p.as_str()),
            Some("/home/alice/project")
        );
        assert_eq!(
            root.path_for_namespace(&FileNamespace::Ssh),
            Some("/home/alice/project".to_string())
        );
        assert_eq!(root.path_for_namespace(&FileNamespace::Host), None);

        let host = std::path::PathBuf::from(r"D:\coder\Tn");
        let root = explorer_root_from_parts(
            FileNamespace::Host,
            Some(r"D:\coder\Tn".into()),
            Some(host.clone()),
            Some(cfg.clone()),
        )
        .expect("host cwd still maps through host path");
        assert_eq!(root.path_buf(), Some(host));

        assert!(explorer_root_from_parts(
            FileNamespace::Ssh,
            Some(r"D:\not-remote".into()),
            None,
            Some(cfg),
        )
        .is_none());
    }

    #[test]
    fn open_folder_uses_native_picker_only_for_host_and_welcome() {
        // Welcome page (no live pane / no spec): use the native picker so the
        // chosen folder re-roots the explorer and becomes the next launch cwd.
        assert!(open_folder_should_use_native_picker(None));

        // Host shell → native Windows folder picker.
        let host = LaunchSpec::pwsh();
        assert!(open_folder_should_use_native_picker(Some(&host)));

        // WSL → in-app navigable picker (browses \\wsl$ locally), not native.
        let mut wsl = LaunchSpec::pwsh();
        wsl.file_namespace = FileNamespace::Wsl {
            distro: Some("Ubuntu".into()),
        };
        assert!(!open_folder_should_use_native_picker(Some(&wsl)));

        // SSH → in-app SFTP picker, not native.
        let mut ssh = LaunchSpec::pwsh();
        ssh.file_namespace = FileNamespace::Ssh;
        ssh.ssh = Some(ssh_cfg());
        assert!(!open_folder_should_use_native_picker(Some(&ssh)));
    }

    #[test]
    fn quick_look_open_counts_as_focus_freezing_overlay() {
        assert!(workspace_overlay_freezes_pane_focus(
            false, false, false, true, false, false, false, false
        ));
        assert!(workspace_overlay_freezes_pane_focus(
            false, false, false, false, false, false, false, true
        ));

        assert!(!workspace_overlay_freezes_pane_focus(
            false, false, false, false, false, false, false, false
        ));
    }

    #[test]
    fn remote_dir_picker_keymap_uses_arrows_for_navigation_and_enter_for_confirm() {
        assert_eq!(
            remote_dir_key_action("left", false, false),
            RemoteDirKeyAction::Parent
        );
        assert_eq!(
            remote_dir_key_action("right", false, false),
            RemoteDirKeyAction::EnterDirectory
        );
        assert_eq!(
            remote_dir_key_action("enter", false, false),
            RemoteDirKeyAction::Confirm
        );
        assert_eq!(
            remote_dir_key_action("enter", true, false),
            RemoteDirKeyAction::Confirm
        );
        assert_eq!(
            remote_dir_key_action("backspace", false, false),
            RemoteDirKeyAction::Ignore
        );
    }

    #[test]
    fn standalone_editor_pane_feature_is_removed() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let workspace = std::fs::read_to_string(src.join("workspace.rs")).unwrap();
        let quick_look = std::fs::read_to_string(src.join("quick_look.rs")).unwrap();
        let editor_mod = std::fs::read_to_string(src.join("editor").join("mod.rs")).unwrap();

        let editor_pane = format!("Editor{}", "Pane");
        let editor_map = format!("editor_{}", "panes");
        let open_handoff = format!("open_editor_{}", "handoff");
        let open_as_editor = format!("OpenAs{}", "Editor");
        let open_button = format!("打开为{}", "编辑器");
        let pane_mod = format!("pub mod {}", "pane");

        assert!(
            !src.join("editor").join("pane.rs").exists(),
            "standalone editor-pane file should be removed"
        );
        assert!(
            !workspace.contains(&editor_pane),
            "workspace should not import or render the removed pane type"
        );
        assert!(
            !workspace.contains(&editor_map),
            "workspace should not keep an editor pane registry"
        );
        assert!(
            !workspace.contains(&open_handoff),
            "workspace should not promote Quick Look into an editor pane"
        );
        assert!(
            !quick_look.contains(&open_as_editor),
            "Quick Look should not emit the removed editor-pane event"
        );
        assert!(
            !quick_look.contains(&open_button),
            "Quick Look footer should not expose the standalone editor pane action"
        );
        assert!(
            !editor_mod.contains(&pane_mod),
            "editor module should not export the removed pane module"
        );
    }

    #[test]
    fn welcome_tab_resets_explorer_from_previous_pane() {
        let previous_root = ExplorerRoot::ssh(ssh_cfg(), "/home/alice/project");
        let default_root = ExplorerRoot::host(std::path::PathBuf::from(r"C:\Users\Alice"));

        assert!(
            should_reset_explorer_for_welcome_tab(
                &Tab::welcome(),
                Some(42),
                &previous_root,
                &default_root,
            ),
            "welcome tabs must not keep the explorer root/state from the last real pane"
        );
    }

    fn split(axis: Axis, kids: Vec<Node>) -> Node {
        let weights = vec![1.0; kids.len()];
        Node::Split {
            axis,
            kids,
            weights,
        }
    }

    #[test]
    fn resize_adjusts_matching_axis_only() {
        let mut n = split(Axis::Row, vec![Node::Leaf(0), Node::Leaf(1)]);
        assert!(n.resize(1, Axis::Row, 0.5));
        let Node::Split { weights, .. } = &n else {
            panic!()
        };
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
        let Node::Split { weights, kids, .. } = &n else {
            panic!()
        };
        assert_eq!(weights, &vec![1.0, 1.0]); // outer untouched
        let Node::Split { weights: iw, .. } = &kids[1] else {
            panic!()
        };
        assert!((iw[1] - 1.3).abs() < 1e-6); // inner pane grew
    }

    #[test]
    fn split_before_inserts_left_or_after_inserts_right() {
        // `新会话` split direction: before=false (right/down) → new pane AFTER target;
        // before=true (left/up) → new pane BEFORE target.
        let mut n = Node::Leaf(0);
        assert!(n.split(0, 1, Axis::Row, false)); // split right
        let Node::Split { kids, .. } = &n else {
            panic!()
        };
        assert!(
            matches!((&kids[0], &kids[1]), (Node::Leaf(0), Node::Leaf(1))),
            "right → [0,1]"
        );

        let mut n = Node::Leaf(0);
        assert!(n.split(0, 1, Axis::Row, true)); // split left
        let Node::Split { kids, .. } = &n else {
            panic!()
        };
        assert!(
            matches!((&kids[0], &kids[1]), (Node::Leaf(1), Node::Leaf(0))),
            "left → [1,0]"
        );

        // Aligned n-ary insert respects before/after position.
        let mut n = split(Axis::Row, vec![Node::Leaf(0), Node::Leaf(1)]);
        assert!(n.split(1, 2, Axis::Row, true)); // insert 2 before pane 1
        let Node::Split { kids, .. } = &n else {
            panic!()
        };
        let ids: Vec<_> = kids
            .iter()
            .map(|k| {
                matches!(k, Node::Leaf(_))
                    .then(|| if let Node::Leaf(i) = k { *i } else { 0 })
                    .unwrap()
            })
            .collect();
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
        let Node::Split { weights, .. } = &n else {
            panic!()
        };
        assert!(weights[0] >= 0.1 - 1e-6);
    }

    #[test]
    fn at_path_mut_navigates_to_nested_split() {
        // root[Row]: Leaf(0), inner[Col]: Leaf(1), Leaf(2)
        let inner = split(Axis::Col, vec![Node::Leaf(1), Node::Leaf(2)]);
        let mut n = split(Axis::Row, vec![Node::Leaf(0), inner]);
        // [] = root split (Row); [1] = the inner split (Col); [0] = a leaf.
        assert!(matches!(
            n.at_path_mut(&[]),
            Some(Node::Split {
                axis: Axis::Row,
                ..
            })
        ));
        assert!(matches!(
            n.at_path_mut(&[1]),
            Some(Node::Split {
                axis: Axis::Col,
                ..
            })
        ));
        assert!(matches!(n.at_path_mut(&[0]), Some(Node::Leaf(0))));
        // A divider drag sets the inner split's weights via this path.
        if let Some(Node::Split { weights, .. }) = n.at_path_mut(&[1]) {
            weights[0] = 2.0;
            weights[1] = 0.5;
        }
        let Node::Split { kids, .. } = &n else {
            panic!()
        };
        let Node::Split { weights: iw, .. } = &kids[1] else {
            panic!()
        };
        assert_eq!(iw, &vec![2.0, 0.5]);
        // Out-of-range / through-a-leaf paths are None.
        assert!(n.at_path_mut(&[9]).is_none());
        assert!(n.at_path_mut(&[0, 0]).is_none());
    }
}
