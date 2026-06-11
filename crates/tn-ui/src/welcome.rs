//! Welcome launchpad(SHEET 07 板 A)— the default content of a **new tab**:
//! 居中 Launchpad:hero-mark + TN_ + 磁贴(发现的 profiles / WSL / SSH)+ kbd
//! 提示 + 宠物 2× 形态。空状态克制:无营销 hero、无大渐变(磷光契约 1)。
//!
//! It's a Phosphor plate like the terminal panes (chrome, not a node in the
//! split tree). Clicking a tile emits [`LaunchRequested`] with the profile
//! index; the workspace spawns that pane into the tab (welcome → panes).
//!
//! The "最近" (recent projects) row from the prototype is 待端口 — it needs a
//! recent-sessions data source (agent session cwd + mtime), tracked
//! separately so we don't fake it.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, rgb, rgba, Context, Div, FocusHandle, FontWeight, MouseButton,
    MouseDownEvent, SharedString,
};
use tn_agent::{AgentId, AgentRegistry};
use tn_config::{Loaded, Profile, ProfileKind};

use crate::style::{
    col, cola, icon, plate, H0, H1, PH, PH_DIM, R_CARD, R_CHIP, R_PANEL, T0, T2, T3, UI_SANS,
};

// ── Shared launch-tile helpers (mockup `.tile` / `.dot`) ────────────────────────
// The welcome launchpad, the Quick Terminal launcher, and the command palette all
// render the same per-profile identity (color + icon + sub-label). These free fns
// are the single source so the three can't drift.

/// A launch profile's detected agent, from its `agent` field or
/// its command's first token. `None` = a plain shell / WSL / SSH.
/// Whether a profile launches an agent — for the launch-surface grouping
/// (agents-on-top). Registry-free: a declared `agent` field or `kind = "agent"`.
/// The card itself resolves the descriptor for display; this only sorts.
pub(crate) fn is_agent_profile(p: &Profile) -> bool {
    p.agent.is_some() || matches!(p.kind, ProfileKind::Agent)
}

pub(crate) fn launch_agent_of(p: &Profile, reg: &AgentRegistry) -> Option<AgentId> {
    // Same launch-intent resolution as `LaunchSpec::from_profile`: explicit
    // `agent = "..."` matched against registered aliases then taken literally,
    // else inferred from the command — agent-agnostic, no per-agent arm.
    p.agent
        .as_deref()
        .map(|a| reg.match_command(a).unwrap_or_else(|| AgentId::new(a)))
        .or_else(|| p.command.as_deref().and_then(|c| reg.match_command(c)))
}

/// A profile's identity accent (mockup `.tile.agent/.sh/.wsl` / `.dot`):
/// explicit `accent`, else the agent's themed/descriptor accent / WSL violet /
/// shell blue. Agent accent = theme `[agents.<id>]` override, then the agent
/// descriptor's default, then the UI accent.
pub(crate) fn launch_tile_accent(
    t: &tn_config::Theme,
    p: &Profile,
    agent: Option<&AgentId>,
    reg: &AgentRegistry,
) -> tn_config::Color {
    if let Some(a) = p.accent {
        return a;
    }
    if let Some(id) = agent {
        return t
            .agents
            .accent_for(id.as_str())
            .or_else(|| reg.get(id).and_then(|d| d.accent))
            .unwrap_or(t.ui.accent);
    }
    match p.kind {
        ProfileKind::Wsl => t.ui.accent_alt, // violet
        _ => t.ui.accent,                    // blue
    }
}

/// Tile sub-label (mockup `.td`: "Agent" / "PowerShell" / "Ubuntu"). For an
/// agent it's the descriptor label (built-in or generic); else WSL distro / SSH
/// host / a shell-kind label.
pub(crate) fn launch_tile_sub(p: &Profile, agent: Option<&AgentId>, reg: &AgentRegistry) -> String {
    if let Some(id) = agent {
        return reg.descriptor_or_generic(id, &p.name).label;
    }
    match p.kind {
        ProfileKind::Wsl => p
            .distro
            .clone()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "WSL".into()),
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
    }
}

// ── Launch-surface aggregation (WSL → one card, SSH → one connector card) ───────
// The launch surfaces (welcome / Quick Terminal / split launcher) used to render one
// tile per discovered profile — including *every* WSL distro, which piles up. To cut
// the选择负担 we collapse all distros into ONE "WSL" card (drill in to pick a distro,
// or auto-launch when there's only one) and append a single "SSH" connector card.
// Shared here so every surface aggregates identically.

/// A launch tile's visual identity — everything a surface needs to draw one card,
/// whether it's a profile, the WSL group, or the SSH connector.
pub(crate) struct CardId {
    pub name: String,
    pub sub: String,
    pub glyph: &'static str,
    pub accent: tn_config::Color,
}

/// Card identity for a launchable shell/agent profile (Agent / pwsh / …).
pub(crate) fn profile_card(t: &tn_config::Theme, p: &Profile, reg: &AgentRegistry) -> CardId {
    let agent = launch_agent_of(p, reg);
    CardId {
        name: p.name.clone(),
        sub: launch_tile_sub(p, agent.as_ref(), reg),
        glyph: if agent.is_some() { "spark" } else { "term" },
        accent: launch_tile_accent(t, p, agent.as_ref(), reg),
    }
}

/// The aggregated WSL card (mockup violet `.tile.wsl`): violet accent, "N 个发行版".
pub(crate) fn wsl_card(t: &tn_config::Theme, n: usize) -> CardId {
    CardId {
        name: "WSL".into(),
        sub: format!("{n} 个发行版"),
        glyph: "term",
        accent: t.ui.accent_alt,
    }
}

/// The SSH prompt card (user@host input modal).
pub(crate) fn ssh_card(t: &tn_config::Theme) -> CardId {
    CardId {
        name: "SSH".into(),
        sub: "快速连接".into(),
        glyph: "external",
        accent: t.ui.accent,
    }
}

/// One launch-surface card after aggregation. Indices are into the surface's
/// `discover_profiles` list, so a click resolves to the exact profile to launch.
pub(crate) enum LaunchEntry {
    /// A directly-launchable shell/agent profile.
    Profile(usize),
    /// All discovered WSL distros, collapsed to one card (drill in, or auto-launch
    /// if there's only one).
    Wsl(Vec<usize>),
    /// Interactive SSH prompt launcher.
    SshPrompt,
}

/// Collapse a discovered-profile list into launch cards: each shell/agent profile
/// stays its own card; **all** WSL distros fold into ONE [`LaunchEntry::Wsl`] (only
/// when ≥1 exists); a single SSH prompt card is always appended. Configured SSH
/// profiles fold into that prompt instead of appearing as separate launch tiles.
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
            ProfileKind::Ssh => {} // folded into the SSH prompt launcher
            // Agents lead, then plain shells — the headline use case first, so
            // welcome/launcher read agents-on-top (用户要的排版). Grouping is by the
            // profile's declared `agent` field (registry-free; the card itself
            // resolves the descriptor for display).
            _ if is_agent_profile(p) => agents.push(LaunchEntry::Profile(i)),
            _ => shells.push(LaunchEntry::Profile(i)),
        }
    }
    let mut out = agents;
    out.append(&mut shells);
    if !wsl.is_empty() {
        out.push(LaunchEntry::Wsl(wsl));
    }
    out.push(LaunchEntry::SshPrompt);
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
    SshPrompt,
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
            LaunchEntry::SshPrompt => m("SSH").then_some(LaunchRow::SshPrompt),
        })
        .collect()
}

/// The [`CardId`] for a flat launch row (palette / split-launcher rendering).
pub(crate) fn row_card(
    t: &tn_config::Theme,
    profiles: &[Profile],
    row: &LaunchRow,
    reg: &AgentRegistry,
) -> CardId {
    match row {
        LaunchRow::Profile(i) => profile_card(t, &profiles[*i], reg),
        LaunchRow::DrillWsl => wsl_card(t, wsl_distros(profiles).len()),
        LaunchRow::SshPrompt => ssh_card(t),
    }
}

/// Emitted when a launch tile is clicked; carries the index into the profile list
/// the view was constructed with (the workspace's discovered profiles).
pub struct LaunchRequested(pub usize);

/// Emitted when the SSH tile is clicked to request the interactive SSH connector prompt.
pub struct SshPromptRequested;

/// Emitted by the「+ 添加 Agent」tile → workspace opens the agent editor (add mode).
pub struct AddAgentRequested;

/// Emitted by a custom agent tile's ✎ → workspace opens the editor (edit mode).
/// Carries the profile index into the view's profile list.
pub struct EditAgentRequested(pub usize);

/// Emitted by a custom agent tile's ✕ → workspace deletes that agent.
/// Carries the profile index into the view's profile list.
pub struct DeleteAgentRequested(pub usize);

pub struct WelcomeView {
    config: Arc<Loaded>,
    profiles: Vec<Profile>,
    /// Drilled into the WSL group's distro sub-grid (vs the root launchpad).
    wsl_open: bool,
    /// 当前宠物品种(欢迎页 2× 形态;`None` = 宠物隐藏/关闭)。由 workspace 喂入。
    pet_breed: Option<crate::pet::Breed>,
    focus_handle: FocusHandle,
}

impl WelcomeView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>, profiles: Vec<Profile>) -> Self {
        Self {
            config,
            profiles,
            wsl_open: false,
            pet_breed: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// workspace 每帧同步:宠物品种(SHEET 07 欢迎页 2× 形态)。
    pub(crate) fn set_pet_breed(&mut self, breed: Option<crate::pet::Breed>) {
        self.pet_breed = breed;
    }

    // `tile_accent` / `tile_sub` / agent detection now live as module-level free fns
    // (`launch_tile_accent` / `launch_tile_sub` / `launch_agent_of`) — shared with the
    // Quick Terminal launcher (and the command palette's identity dot) so all three
    // render the identical mockup `.tile`/.dot identity.

    /// 启动磁贴(SHEET 07 `.ltile`):150 宽 · L2 + 1px h0 + r4 · 身份字形 mono 600 15
    /// · 名 sans 600 12 t0 · 副标 mono 9 t2 大写;hover = L4 + h1。
    /// shared shape for profile / WSL / SSH / back tiles.
    fn card_tile(
        &self,
        card: CardId,
        on_down: impl Fn(&mut Self, &MouseDownEvent, &mut gpui::Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let ui = &self.config.theme.ui;
        let crest = col(ui.palette_selected); // L4
        let mono = SharedString::from(self.config.font().family.clone());
        // 身份字形:磁贴语法的「会话所有者」记号(svg 图标 → mono 字形,SHEET 07)。
        let glyph_ch = match card.glyph {
            "spark" => "✻",
            "term" => "❯",
            "external" => "⇄",
            "plus" => "+",
            "chev-l" => "‹",
            _ => "▣",
        };
        div()
            .w(px(150.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .pt(px(14.))
            .px(px(14.))
            .pb(px(12.))
            .rounded(px(R_CARD))
            .bg(col(ui.surface_2)) // L2
            .border_1()
            .border_color(rgba(H0))
            .hover(move |s| s.bg(crest).border_color(rgba(H1)))
            .on_mouse_down(MouseButton::Left, cx.listener(on_down))
            .child(
                // `.g`:身份字形 mono 600 15
                div()
                    .font_family(mono.clone())
                    .text_size(px(15.))
                    .font_weight(FontWeight(600.))
                    .text_color(col(card.accent))
                    .child(SharedString::from(glyph_ch)),
            )
            .child(
                // `.n`:sans 600 12 t0
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight(600.))
                    .text_color(rgb(T0))
                    .child(SharedString::from(card.name)),
            )
            .child(
                // `.s`:mono 9 t2 全大写
                div()
                    .font_family(mono)
                    .text_size(px(9.))
                    .text_color(rgb(T2))
                    .child(SharedString::from(card.sub.to_uppercase())),
            )
    }

    /// A profile launch tile → emits [`LaunchRequested`] for its index. A
    /// user-created agent (declared in config `[[agents]]`) also gets inline
    /// ✎/✕ affordances so it's editable/removable without touching config.toml.
    fn tile(&self, i: usize, p: &Profile, cx: &mut Context<Self>) -> Div {
        let reg = crate::agent_host::agent_registry(cx);
        let card = self.card_tile(
            profile_card(&self.config.theme, p, &reg),
            move |_this, _e, _w, cx| cx.emit(LaunchRequested(i)),
            cx,
        );
        if self.is_managed_agent(p) {
            card.relative().child(self.agent_tile_actions(i, cx))
        } else {
            card
        }
    }

    /// Whether this profile is a user-created agent (its `agent` id is declared in
    /// config `[[agents]]`) → gets the inline edit/delete affordances. The shipped
    /// generic "agent" qualifies too — it's just a config entry like any other.
    fn is_managed_agent(&self, p: &Profile) -> bool {
        p.kind == ProfileKind::Agent
            && p.agent
                .as_deref()
                .is_some_and(|id| self.config.config.agents.iter().any(|a| a.id == id))
    }

    /// The「+ 添加 Agent」tile (always present in the agents row) → opens the
    /// in-app agent editor so a new CLI can be added without editing config.toml.
    /// SHEET 07 `.ltile.add`:透底 + h0 边 + 居中内容(与实体磁贴区分的「空位」)。
    fn add_agent_tile(&self, cx: &mut Context<Self>) -> Div {
        let ui = &self.config.theme.ui;
        let crest = col(ui.palette_selected); // L4
        let mono = SharedString::from(self.config.font().family.clone());
        div()
            .w(px(150.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .pt(px(14.))
            .px(px(14.))
            .pb(px(12.))
            .rounded(px(R_CARD))
            .border_1()
            .border_color(rgba(H0))
            .hover(move |s| s.bg(crest).border_color(rgba(H1)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _e, _w, cx| cx.emit(AddAgentRequested)),
            )
            .child(
                div()
                    .font_family(mono)
                    .text_size(px(15.))
                    .font_weight(FontWeight(600.))
                    .text_color(rgb(T3))
                    .child("+"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight(500.))
                    .text_color(rgb(T2))
                    .child("添加 Agent"),
            )
    }

    /// The inline ✎ (edit) / ✕ (delete) cluster at a custom agent tile's top-right.
    /// Each button `stop_propagation`s so it doesn't also launch the tile.
    fn agent_tile_actions(&self, i: usize, cx: &mut Context<Self>) -> Div {
        let ui = &self.config.theme.ui;
        let red = self.config.theme.ansi.red;
        let raised = col(ui.surface_2); // L2 小件
        div()
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .flex()
            .flex_row()
            .gap(px(4.))
            .child(
                div()
                    .w(px(20.))
                    .h(px(20.))
                    .rounded(px(R_CHIP))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(raised)
                    .border_1()
                    .border_color(rgba(H1))
                    .hover(|s| s.border_color(rgba(PH_DIM)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_t, _e, _w, cx| {
                            cx.stop_propagation();
                            cx.emit(EditAgentRequested(i));
                        }),
                    )
                    .child(icon("pen", 11., ui.muted)),
            )
            .child(
                div()
                    .w(px(20.))
                    .h(px(20.))
                    .rounded(px(R_CHIP))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(raised)
                    .border_1()
                    .border_color(rgba(H1))
                    .hover(|s| s.border_color(cola(red, 0.5)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_t, _e, _w, cx| {
                            cx.stop_propagation();
                            cx.emit(DeleteAgentRequested(i));
                        }),
                    )
                    .child(icon("close", 11., ui.muted)),
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

    /// The SSH interactive connector tile.
    fn ssh_tile(&self, cx: &mut Context<Self>) -> Div {
        self.card_tile(
            ssh_card(&self.config.theme),
            |_this, _e, _w, cx| cx.emit(SshPromptRequested),
            cx,
        )
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

    /// A keyboard hint chip(`.kbd` + 标签,mono 10 t2 — SHEET 07)。
    fn hint(&self, key: &str, label: &str) -> Div {
        let mono = SharedString::from(self.config.font().family.clone());
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .child(crate::style::kbd(key.to_string(), mono.clone()))
            .child(
                div()
                    .font_family(mono)
                    .text_size(px(10.))
                    .text_color(rgb(T2))
                    .child(SharedString::from(label.to_string())),
            )
    }
}

impl gpui::EventEmitter<LaunchRequested> for WelcomeView {}
impl gpui::EventEmitter<SshPromptRequested> for WelcomeView {}
impl gpui::EventEmitter<AddAgentRequested> for WelcomeView {}
impl gpui::EventEmitter<EditAgentRequested> for WelcomeView {}
impl gpui::EventEmitter<DeleteAgentRequested> for WelcomeView {}

impl Render for WelcomeView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = &self.config.theme.ui;
        let mono = SharedString::from(self.config.font().family.clone());

        // Grouped for a clean two-row launchpad: configured agents on top, shells +
        // WSL + SSH below (用户要的排版). Drilling into WSL shows a Back tile + the
        // distros in one wrapping row.
        let row = || {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .justify_center()
                .gap(px(10.)) // SHEET 07 `.launch` gap 10
        };
        let tiles = if self.wsl_open {
            let mut v = vec![self.back_tile(cx)];
            for i in wsl_distros(&self.profiles) {
                let p = self.profiles[i].clone();
                v.push(self.tile(i, &p, cx));
            }
            row().w(px(640.)).children(v)
        } else {
            let mut agents = Vec::new();
            let mut others = Vec::new();
            for e in launch_entries(&self.profiles) {
                match e {
                    LaunchEntry::Profile(i) => {
                        let p = self.profiles[i].clone();
                        let tile = self.tile(i, &p, cx);
                        if is_agent_profile(&p) {
                            agents.push(tile); // agents → top row
                        } else {
                            others.push(tile); // PowerShell → bottom row
                        }
                    }
                    LaunchEntry::Wsl(d) => others.push(self.wsl_tile(d, cx)), // WSL → bottom
                    LaunchEntry::SshPrompt => others.push(self.ssh_tile(cx)), // SSH → bottom
                }
            }
            // The「+ 添加 Agent」tile closes the agents row — always offered, so the
            // launchpad is the entry point for adding a custom CLI (no config.toml).
            agents.push(self.add_agent_tile(cx));
            div()
                .w(px(640.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(row().children(agents)) // always non-empty (≥ the 添加 tile)
                .child(row().children(others))
        };

        let hints = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_center()
            .gap(px(18.))
            .mt(px(6.))
            .child(self.hint("Ctrl+Shift+P", "命令面板"))
            .child(self.hint("Ctrl+Shift+N", "分屏会话"))
            .child(self.hint("Ctrl+Alt+Space", "幽灵终端"));

        // SHEET 07 板 A:hero-mark(44×44 ph-dim 边 + 22×22 磷光内核)+ TN_ +
        // 副标(mono 11 t2)— 居中不放大,克制。
        let welcome = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(18.))
            .px(px(40.))
            .py(px(32.))
            .font_family(UI_SANS)
            .child(
                div()
                    .w(px(44.))
                    .h(px(44.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgba(PH_DIM))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().w(px(22.)).h(px(22.)).rounded(px(2.)).bg(rgb(PH))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .font_family(mono.clone())
                            .text_size(px(26.))
                            .font_weight(FontWeight(600.))
                            .child(div().text_color(rgb(T0)).child("TN"))
                            .child(div().text_color(rgb(PH)).child("_")),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(11.))
                            .text_color(rgb(T2))
                            .child(if self.wsl_open {
                                "选择一个 WSL 发行版 — ‹ 返回 回到启动器"
                            } else {
                                "TERMINAL INSTRUMENT — 人与智能体共用的终端"
                            }),
                    ),
            )
            .child(tiles)
            .child(hints);

        // 宠物 2× 形态(右下栖位 + 岗台 + 标签,SHEET 07 `.wperch`)。
        let perch = self.pet_breed.map(|breed| {
            div()
                .absolute()
                .right(px(46.))
                .bottom(px(52.))
                .flex()
                .flex_col()
                .items_center()
                .child(crate::pet::sprite_block(breed, 2.0))
                .child(
                    // 岗台:1px h1 + 左 28px 磷光点睛
                    div()
                        .w(px(180.))
                        .h(px(1.))
                        .bg(rgba(H1))
                        .relative()
                        .child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(0.))
                                .w(px(28.))
                                .h(px(1.))
                                .bg(rgba(PH_DIM)),
                        ),
                )
                .child(
                    div()
                        .mt(px(6.))
                        .font_family(mono.clone())
                        .text_size(px(9.))
                        .text_color(rgb(T3))
                        .child(SharedString::from(format!(
                            "{} · IDLE(欢迎页 2× 形态)",
                            breed.tag()
                        ))),
                )
        });

        // 磷光板面:不透明 L1 基面 + 1px 发丝边(plate 范式,零投影)。
        let inner = div()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .rounded(px(R_PANEL - 1.))
            .bg(col(ui.surface_1))
            .child(welcome)
            .when_some(perch, |d, p| d.child(p));
        plate(inner, false)
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
            prof(
                "pwsh",
                ProfileKind::Shell,
                None,
                None,
                Some("powershell.exe"),
            ),
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
        assert!(matches!(e[2], LaunchEntry::SshPrompt));
    }

    #[test]
    fn launch_entries_without_wsl_still_appends_ssh() {
        let profiles = vec![prof(
            "pwsh",
            ProfileKind::Shell,
            None,
            None,
            Some("powershell.exe"),
        )];
        let e = launch_entries(&profiles);
        assert_eq!(e.len(), 2); // pwsh + SSH placeholder, no WSL card
        assert!(matches!(e[0], LaunchEntry::Profile(0)));
        assert!(matches!(e[1], LaunchEntry::SshPrompt));
    }

    #[test]
    fn launch_entries_puts_agents_before_shells() {
        let profiles = vec![
            prof(
                "pwsh",
                ProfileKind::Shell,
                None,
                None,
                Some("powershell.exe"),
            ),
            prof("Agent", ProfileKind::Agent, None, None, Some("agent")),
        ];
        let e = launch_entries(&profiles);
        // The generic Agent tile leads, then the pwsh shell, then SSH placeholder.
        assert!(matches!(e[0], LaunchEntry::Profile(1)));
        assert!(matches!(e[1], LaunchEntry::Profile(0)));
        assert!(matches!(e[2], LaunchEntry::SshPrompt));
    }
}
