//! Welcome launchpad (panels/05) — the default content of a **new tab**: a
//! centered glass card inviting you to start a session, with launch tiles (the
//! discovered profiles: Claude / Codex / pwsh / WSL …) + keyboard hints. Empty =
//! an invitation to start a session, not a wall of widgets.
//!
//! It's a Calm Glass pane like the terminal panes / explorer (chrome, not a node
//! in the split tree). Clicking a tile emits [`LaunchRequested`] with the profile
//! index; the workspace spawns that pane into the tab (welcome → panes).
//!
//! The "最近" (recent projects) row from the prototype is 待端口 — it needs a
//! recent-sessions data source (claude/codex session cwd + mtime), tracked
//! separately so we don't fake it.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, rgba, Context, Div, FocusHandle, FontWeight, MouseButton, MouseDownEvent,
    SharedString,
};
use tn_ai::{agent_kind_for_command, AgentKind};
use tn_config::{Loaded, Profile, ProfileKind};

use crate::style::{
    col, cola, glass_pane, icon, pane_fill, INSET, RIM, R_CARD, R_PANEL, UI_SANS,
};

// ── Shared launch-tile helpers (mockup `.tile` / `.dot`) ────────────────────────
// The welcome launchpad, the Quick Terminal launcher, and the command palette all
// render the same per-profile identity (color + icon + sub-label). These free fns
// are the single source so the three can't drift.

/// A launch profile's detected agent (Claude / Codex), from its `agent` field or
/// its command's first token. `None` = a plain shell / WSL / SSH.
pub(crate) fn launch_agent_of(p: &Profile) -> Option<AgentKind> {
    p.agent
        .as_deref()
        .and_then(agent_kind_for_command)
        .or_else(|| p.command.as_deref().and_then(agent_kind_for_command))
}

/// A profile's identity accent (mockup `.tile.claude/.codex/.sh/.wsl` / `.dot`):
/// explicit `accent`, else Claude coral / Codex teal / WSL violet / shell blue.
pub(crate) fn launch_tile_accent(
    t: &tn_config::Theme,
    p: &Profile,
    agent: Option<AgentKind>,
) -> tn_config::Color {
    p.accent.unwrap_or(match (agent, p.kind) {
        (Some(AgentKind::ClaudeCode), _) => t.agents.claude,
        (Some(AgentKind::Codex), _) => t.agents.codex,
        (_, ProfileKind::Wsl) => t.ui.accent_alt, // violet
        _ => t.ui.accent,                         // blue
    })
}

/// Tile sub-label (mockup `.td`: "Claude Code" / "PowerShell" / "Ubuntu").
pub(crate) fn launch_tile_sub(p: &Profile, agent: Option<AgentKind>) -> String {
    match agent {
        Some(AgentKind::ClaudeCode) => "Claude Code".into(),
        Some(AgentKind::Codex) => "Codex".into(),
        None => match p.kind {
            ProfileKind::Wsl => {
                p.distro.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| "WSL".into())
            }
            ProfileKind::Ssh => p.host.clone().unwrap_or_else(|| "SSH".into()),
            _ => {
                let c = p.command.clone().unwrap_or_default().to_ascii_lowercase();
                if c.contains("pwsh") || c.contains("powershell") {
                    "PowerShell".into()
                } else if c.contains("cmd") {
                    "Command Prompt".into()
                } else {
                    p.command.clone().unwrap_or_else(|| "shell".into())
                }
            }
        },
    }
}

// ── Launch-surface aggregation (WSL → one card, SSH → placeholder) ──────────────
// The launch surfaces (welcome / Quick Terminal / split launcher) used to render one
// tile per discovered profile — including *every* WSL distro, which piles up. To cut
// the选择负担 we collapse all distros into ONE "WSL" card (drill in to pick a distro,
// or auto-launch when there's only one) and append a single "SSH" placeholder card
// (the SSH backend is parked — see CLAUDE.md). Shared here so every surface aggregates
// identically.

/// A launch tile's visual identity — everything a surface needs to draw one card,
/// whether it's a profile, the WSL group, or the SSH placeholder.
pub(crate) struct CardId {
    pub name: String,
    pub sub: String,
    pub glyph: &'static str,
    pub accent: tn_config::Color,
}

/// Card identity for a launchable shell/agent profile (Claude / Codex / pwsh / …).
pub(crate) fn profile_card(t: &tn_config::Theme, p: &Profile) -> CardId {
    let agent = launch_agent_of(p);
    CardId {
        name: p.name.clone(),
        sub: launch_tile_sub(p, agent),
        glyph: if agent.is_some() { "spark" } else { "term" },
        accent: launch_tile_accent(t, p, agent),
    }
}

/// The aggregated WSL card (mockup violet `.tile.wsl`): violet accent, "N 个发行版".
pub(crate) fn wsl_card(t: &tn_config::Theme, n: usize) -> CardId {
    CardId { name: "WSL".into(), sub: format!("{n} 个发行版"), glyph: "term", accent: t.ui.accent_alt }
}

/// The SSH placeholder card (parked — a visible entry point, not launchable yet).
pub(crate) fn ssh_card(t: &tn_config::Theme) -> CardId {
    CardId { name: "SSH".into(), sub: "即将支持".into(), glyph: "external", accent: t.ui.muted }
}

/// One launch-surface card after aggregation. Indices are into the surface's
/// `discover_profiles` list, so a click resolves to the exact profile to launch.
pub(crate) enum LaunchEntry {
    /// A directly-launchable shell/agent profile.
    Profile(usize),
    /// All discovered WSL distros, collapsed to one card (drill in, or auto-launch
    /// if there's only one).
    Wsl(Vec<usize>),
    /// SSH placeholder (parked); selecting it is a no-op for now.
    SshSoon,
}

/// Collapse a discovered-profile list into launch cards: each shell/agent profile
/// stays its own card; **all** WSL distros fold into ONE [`LaunchEntry::Wsl`] (only
/// when ≥1 exists); a [`LaunchEntry::SshSoon`] placeholder is always appended.
/// Configured SSH profiles fold away too (not launchable until SSH is un-parked).
pub(crate) fn launch_entries(profiles: &[Profile]) -> Vec<LaunchEntry> {
    let mut agents = Vec::new();
    let mut shells = Vec::new();
    let mut wsl = Vec::new();
    for (i, p) in profiles.iter().enumerate() {
        if !crate::workspace::is_launchable(p) {
            continue;
        }
        match p.kind {
            ProfileKind::Wsl => wsl.push(i),
            ProfileKind::Ssh => {} // folded into the SSH placeholder (parked)
            // Agents (Claude / Codex) lead, then plain shells — the headline use case
            // first, so welcome/launcher read agents-on-top (用户要的排版).
            _ if launch_agent_of(p).is_some() => agents.push(LaunchEntry::Profile(i)),
            _ => shells.push(LaunchEntry::Profile(i)),
        }
    }
    let mut out = agents;
    out.append(&mut shells);
    if !wsl.is_empty() {
        out.push(LaunchEntry::Wsl(wsl));
    }
    out.push(LaunchEntry::SshSoon);
    out
}

/// Indices (into `profiles`) of all discovered WSL distros — the WSL card's members.
pub(crate) fn wsl_distros(profiles: &[Profile]) -> Vec<usize> {
    launch_entries(profiles)
        .into_iter()
        .find_map(|e| match e {
            LaunchEntry::Wsl(v) => Some(v),
            _ => None,
        })
        .unwrap_or_default()
}

/// A flat, selectable launch row for the *search/keyboard* surfaces (command palette,
/// split launcher). Like [`LaunchEntry`] but already drill-resolved + query-filtered.
pub(crate) enum LaunchRow {
    Profile(usize),
    DrillWsl,
    SshSoon,
}

/// The rows to show at the current level, filtered by `query` (case-insensitive
/// substring). At the root: profiles + the WSL card + the SSH placeholder — the WSL
/// card stays visible if the query matches "WSL" *or any distro name* (so typing a
/// distro doesn't hide the way in). When `wsl_drill`: just the distros. `query` is
/// expected to be cleared when crossing the drill boundary (so the level it filters
/// always matches the names shown).
pub(crate) fn launch_rows(profiles: &[Profile], wsl_drill: bool, query: &str) -> Vec<LaunchRow> {
    let q = query.to_ascii_lowercase();
    let m = |s: &str| q.is_empty() || s.to_ascii_lowercase().contains(&q);
    if wsl_drill {
        return wsl_distros(profiles)
            .into_iter()
            .filter(|&i| m(&profiles[i].name))
            .map(LaunchRow::Profile)
            .collect();
    }
    launch_entries(profiles)
        .into_iter()
        .filter_map(|e| match e {
            LaunchEntry::Profile(i) => m(&profiles[i].name).then_some(LaunchRow::Profile(i)),
            LaunchEntry::Wsl(d) => {
                (m("WSL") || d.iter().any(|&i| m(&profiles[i].name))).then_some(LaunchRow::DrillWsl)
            }
            LaunchEntry::SshSoon => m("SSH").then_some(LaunchRow::SshSoon),
        })
        .collect()
}

/// The [`CardId`] for a flat launch row (palette / split-launcher rendering).
pub(crate) fn row_card(t: &tn_config::Theme, profiles: &[Profile], row: &LaunchRow) -> CardId {
    match row {
        LaunchRow::Profile(i) => profile_card(t, &profiles[*i]),
        LaunchRow::DrillWsl => wsl_card(t, wsl_distros(profiles).len()),
        LaunchRow::SshSoon => ssh_card(t),
    }
}

/// Emitted when a launch tile is clicked; carries the index into the profile list
/// the view was constructed with (the workspace's discovered profiles).
pub struct LaunchRequested(pub usize);

pub struct WelcomeView {
    config: Arc<Loaded>,
    profiles: Vec<Profile>,
    /// Drilled into the WSL group's distro sub-grid (vs the root launchpad).
    wsl_open: bool,
    focus_handle: FocusHandle,
}

impl WelcomeView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>, profiles: Vec<Profile>) -> Self {
        Self { config, profiles, wsl_open: false, focus_handle: cx.focus_handle() }
    }

    // `tile_accent` / `tile_sub` / agent detection now live as module-level free fns
    // (`launch_tile_accent` / `launch_tile_sub` / `launch_agent_of`) — shared with the
    // Quick Terminal launcher (and the command palette's identity dot) so all three
    // render the identical mockup `.tile`/.dot identity.

    /// A launch tile (mockup `.tile`) from a [`CardId`] + a mouse-down handler —
    /// shared shape for profile / WSL / SSH / back tiles.
    fn card_tile(
        &self,
        card: CardId,
        on_down: impl Fn(&mut Self, &MouseDownEvent, &mut gpui::Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let ui = &self.config.theme.ui;
        div()
            .w(px(131.)) // (560 − 3×11)/4 ≈ 131:与 mockup grid repeat(4,1fr) 同宽,>4 自动换行
            .flex()
            .flex_col()
            .gap(px(9.)) // §16 .tile gap 9
            .p(px(14.)) // §16 .tile padding 14
            .rounded(px(R_CARD))
            .bg(rgba(INSET)) // .tile bg = g2(.04)
            .border_1()
            .border_color(rgba(RIM)) // .tile border = rim
            .hover(|s| {
                // Enhance hover state with dynamic agent color glow
                s.bg(cola(card.accent, 0.08))
                 .border_color(cola(card.accent, 0.30))
            })
            .on_mouse_down(MouseButton::Left, cx.listener(on_down))
            .child(
                // .ic:30×30 圆角 9,accent@.14 底 + accent 图标
                div()
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(9.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(cola(card.accent, 0.14))
                    .child(icon(card.glyph, 18., card.accent)),
            )
            .child(
                // .tn:13px / 640 / fg
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight(640.))
                    .text_color(col(ui.foreground))
                    .child(SharedString::from(card.name)),
            )
            .child(
                // .td:11px / muted
                div().text_size(px(11.)).text_color(col(ui.muted)).child(SharedString::from(card.sub)),
            )
    }

    /// A profile launch tile → emits [`LaunchRequested`] for its index.
    fn tile(&self, i: usize, p: &Profile, cx: &mut Context<Self>) -> Div {
        self.card_tile(
            profile_card(&self.config.theme, p),
            move |_this, _e, _w, cx| cx.emit(LaunchRequested(i)),
            cx,
        )
    }

    /// The aggregated WSL tile: drill into the distro sub-grid (or launch the lone one).
    fn wsl_tile(&self, distros: Vec<usize>, cx: &mut Context<Self>) -> Div {
        self.card_tile(
            wsl_card(&self.config.theme, distros.len()),
            move |this, _e, _w, cx| {
                if distros.len() == 1 {
                    cx.emit(LaunchRequested(distros[0])); // lone distro → launch直接
                } else {
                    this.wsl_open = true;
                    cx.notify();
                }
            },
            cx,
        )
    }

    /// The SSH placeholder tile (parked) — click is a no-op for now.
    fn ssh_tile(&self, cx: &mut Context<Self>) -> Div {
        self.card_tile(ssh_card(&self.config.theme), |_this, _e, _w, _cx| {}, cx)
    }

    /// Back tile shown in the WSL sub-grid → return to the root launchpad.
    fn back_tile(&self, cx: &mut Context<Self>) -> Div {
        let card = CardId {
            name: "‹ 返回".into(),
            sub: "回到启动器".into(),
            glyph: "chev-l",
            accent: self.config.theme.ui.muted,
        };
        self.card_tile(
            card,
            |this, _e, _w, cx| {
                this.wsl_open = false;
                cx.notify();
            },
            cx,
        )
    }

    /// A keyboard hint chip (mockup `.hk`: key cap + label).
    fn hint(&self, key: &str, label: &str) -> Div {
        let ui = &self.config.theme.ui;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.)) // §16 .hk gap 6
            .child(
                // .k:mono 10px / fg-dim / g2 底 / padding 1 6 / radius 5
                div()
                    .font_family(self.config.font().family.clone())
                    .text_size(px(10.))
                    .text_color(gpui::rgb(0xA6AFD4)) // fg-dim(无 token)
                    .bg(rgba(INSET))
                    .py(px(1.))
                    .px(px(6.))
                    .rounded(px(5.))
                    .child(SharedString::from(key.to_string())),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(label.to_string())),
            )
    }
}

impl gpui::EventEmitter<LaunchRequested> for WelcomeView {}

impl Render for WelcomeView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = &self.config.theme.ui;

        // Grouped for a clean two-row launchpad: agents (Claude/Codex) on top, shells +
        // WSL + SSH below (用户要的排版). Drilling into WSL shows a Back tile + the
        // distros in one wrapping row.
        let row = || div().flex().flex_row().flex_wrap().justify_center().gap(px(11.)); // §16 .tiles gap 11
        let tiles = if self.wsl_open {
            let mut v = vec![self.back_tile(cx)];
            for i in wsl_distros(&self.profiles) {
                let p = self.profiles[i].clone();
                v.push(self.tile(i, &p, cx));
            }
            row().w(px(560.)).children(v)
        } else {
            let mut agents = Vec::new();
            let mut others = Vec::new();
            for e in launch_entries(&self.profiles) {
                match e {
                    LaunchEntry::Profile(i) => {
                        let p = self.profiles[i].clone();
                        let tile = self.tile(i, &p, cx);
                        if launch_agent_of(&p).is_some() {
                            agents.push(tile); // Claude / Codex → top row
                        } else {
                            others.push(tile); // PowerShell → bottom row
                        }
                    }
                    LaunchEntry::Wsl(d) => others.push(self.wsl_tile(d, cx)), // WSL → bottom
                    LaunchEntry::SshSoon => others.push(self.ssh_tile(cx)),   // SSH → bottom
                }
            }
            div()
                .w(px(560.)) // §16 .welcome .tiles width 560
                .flex()
                .flex_col()
                .gap(px(11.))
                .when(!agents.is_empty(), |d| d.child(row().children(agents)))
                .child(row().children(others))
        };

        let hints = div()
            .flex()
            .flex_row()
            .gap(px(18.)) // §16 .whints gap 18
            .child(self.hint("Ctrl+Shift+P", "命令面板"))
            .child(self.hint("Ctrl+Alt+Space", "速唤终端"))
            .child(self.hint("Ctrl+Shift+T", "新标签"));

        // .welcome — centered column (mockup); recent list 待端口。
        let welcome = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(18.)) // §16 .welcome gap 18
            .font_family(UI_SANS)
            .child(
                // .wmark:56×56 圆角16 accent→violet 渐变 + 终端图标
                div()
                    .w(px(56.))
                    .h(px(56.))
                    .rounded(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::linear_gradient(
                        145.,
                        gpui::linear_color_stop(col(ui.accent), 0.),
                        gpui::linear_color_stop(col(ui.accent_alt), 1.),
                    ))
                    .child(icon("term", 30., ui.chrome_bg)),
            )
            .child(
                // 标题组(wt + ws 贴近,对应 mockup .ws margin-top:-14)
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(21.))
                            .font_weight(FontWeight(720.)) // §16 .wt 21/720
                            .text_color(col(ui.foreground))
                            .child("开一个新会话"),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(gpui::rgb(0xA6AFD4)) // §16 .ws fg-dim
                            .child(if self.wsl_open {
                                "选择一个 WSL 发行版,或点「‹ 返回」回到启动器"
                            } else {
                                "托管 AI 编码 CLI,或起一个本地/WSL shell"
                            }),
                    ),
            )
            .child(tiles)
            .child(hints);

        // Inner (rounded 1px tighter for the gradient-border ring) + glass pane.
        let inner = div()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .rounded(px(R_PANEL - 1.)) // 1px tighter for the gradient-border ring (see glass_pane)
            .bg(pane_fill(ui.chrome_bg))
            .child(welcome);
        glass_pane(inner, false, ui.accent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tn_config::{Profile, ProfileKind};

    fn prof(
        name: &str,
        kind: ProfileKind,
        distro: Option<&str>,
        host: Option<&str>,
        cmd: Option<&str>,
    ) -> Profile {
        Profile {
            name: name.into(),
            kind,
            command: cmd.map(Into::into),
            args: Vec::new(),
            cwd: None,
            distro: distro.map(Into::into),
            host: host.map(Into::into),
            user: None,
            agent: None,
            accent: None,
            glyph: None,
        }
    }

    #[test]
    fn launch_entries_collapses_wsl_and_appends_ssh() {
        let profiles = vec![
            prof("pwsh", ProfileKind::Shell, None, None, Some("powershell.exe")),
            prof("Ubuntu", ProfileKind::Wsl, Some("Ubuntu"), None, None),
            prof("Debian", ProfileKind::Wsl, Some("Debian"), None, None),
            prof("box", ProfileKind::Ssh, None, Some("h"), None), // folded into placeholder
            prof("broken", ProfileKind::Wsl, None, None, None),   // no distro → not launchable
        ];
        let e = launch_entries(&profiles);
        assert_eq!(e.len(), 3); // pwsh + WSL group + SSH placeholder
        assert!(matches!(e[0], LaunchEntry::Profile(0)));
        match &e[1] {
            LaunchEntry::Wsl(v) => assert_eq!(v, &vec![1, 2]),
            _ => panic!("expected a collapsed WSL group at [1]"),
        }
        assert!(matches!(e[2], LaunchEntry::SshSoon));
    }

    #[test]
    fn launch_entries_without_wsl_still_appends_ssh() {
        let profiles = vec![prof("pwsh", ProfileKind::Shell, None, None, Some("powershell.exe"))];
        let e = launch_entries(&profiles);
        assert_eq!(e.len(), 2); // pwsh + SSH placeholder, no WSL card
        assert!(matches!(e[0], LaunchEntry::Profile(0)));
        assert!(matches!(e[1], LaunchEntry::SshSoon));
    }

    #[test]
    fn launch_entries_puts_agents_before_shells() {
        let profiles = vec![
            prof("pwsh", ProfileKind::Shell, None, None, Some("powershell.exe")),
            prof("Claude", ProfileKind::Agent, None, None, Some("claude")),
            prof("Codex", ProfileKind::Agent, None, None, Some("codex")),
        ];
        let e = launch_entries(&profiles);
        // agents (Claude=1, Codex=2) lead, then the pwsh shell (0), then SSH placeholder.
        assert!(matches!(e[0], LaunchEntry::Profile(1)));
        assert!(matches!(e[1], LaunchEntry::Profile(2)));
        assert!(matches!(e[2], LaunchEntry::Profile(0)));
        assert!(matches!(e[3], LaunchEntry::SshSoon));
    }
}
