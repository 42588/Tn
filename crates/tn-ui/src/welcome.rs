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
    div, prelude::*, px, rgba, Context, Div, FocusHandle, FontWeight, MouseButton, SharedString,
};
use tn_ai::{agent_kind_for_command, AgentKind};
use tn_config::{Loaded, Profile, ProfileKind};

use crate::style::{
    col, cola, glass_pane, icon, pane_fill, specular_top, INSET, RIM, R_CARD, R_PANEL, UI_SANS,
};

/// Emitted when a launch tile is clicked; carries the index into the profile list
/// the view was constructed with (the workspace's discovered profiles).
pub struct LaunchRequested(pub usize);

pub struct WelcomeView {
    config: Arc<Loaded>,
    profiles: Vec<Profile>,
    focus_handle: FocusHandle,
}

impl WelcomeView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>, profiles: Vec<Profile>) -> Self {
        Self { config, profiles, focus_handle: cx.focus_handle() }
    }

    /// A profile's identity accent (mockup `.tile.claude/.codex/.sh/.wsl`):
    /// explicit `accent`, else Claude coral / Codex teal / WSL violet / shell blue.
    fn tile_accent(&self, p: &Profile, agent: Option<AgentKind>) -> tn_config::Color {
        let t = &self.config.theme;
        p.accent.unwrap_or(match (agent, p.kind) {
            (Some(AgentKind::ClaudeCode), _) => t.agents.claude,
            (Some(AgentKind::Codex), _) => t.agents.codex,
            (_, ProfileKind::Wsl) => t.ui.accent_alt, // violet
            _ => t.ui.accent,                         // blue
        })
    }

    /// Tile sub-label (mockup `.td`: "Claude Code" / "PowerShell" / "Ubuntu").
    fn tile_sub(p: &Profile, agent: Option<AgentKind>) -> String {
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

    /// One launch tile (mockup `.tile`): icon chip + name + sub-label.
    fn tile(&self, i: usize, p: &Profile, cx: &mut Context<Self>) -> Div {
        let ui = &self.config.theme.ui;
        let agent = p
            .agent
            .as_deref()
            .and_then(agent_kind_for_command)
            .or_else(|| p.command.as_deref().and_then(agent_kind_for_command));
        let accent = self.tile_accent(p, agent);
        let glyph = if agent.is_some() { "spark" } else { "term" };
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
            .hover(|s| s.bg(rgba(crate::style::HOVER))) // 轻微提亮(原型 .tile.sel 用 g3)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, _e, _w, cx| cx.emit(LaunchRequested(i))),
            )
            .child(
                // .ic:30×30 圆角 9,accent@.14 底 + accent 图标
                div()
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(9.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(cola(accent, 0.14))
                    .child(icon(glyph, 18., accent)),
            )
            .child(
                // .tn:13px / 640 / fg
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight(640.))
                    .text_color(col(ui.foreground))
                    .child(SharedString::from(p.name.clone())),
            )
            .child(
                // .td:11px / muted
                div()
                    .text_size(px(11.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(Self::tile_sub(p, agent))),
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

        let tiles = div()
            .w(px(560.)) // §16 .welcome .tiles width 560
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_center()
            .gap(px(11.)) // §16 .tiles gap 11
            .children((0..self.profiles.len()).map(|i| {
                let p = self.profiles[i].clone();
                self.tile(i, &p, cx)
            }));

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
                            .child("托管 AI 编码 CLI,或起一个本地/WSL shell"),
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
            .child(specular_top())
            .child(welcome);
        glass_pane(inner, false, ui.accent)
    }
}
