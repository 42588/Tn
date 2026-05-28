//! Agent pane header UI (待优化清单 §6.2): the avatar + name/model + context
//! usage ring shown above Claude/Codex panes. Split out of `mod.rs` to keep the
//! render core lean; `impl super::TerminalView` so it can read the view's
//! private agent/usage/palette state. Only [`render_pane_header`] is called from
//! the parent (`render`); the rest are header-internal.

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, Div, FontWeight, SharedString,
};
use tn_ai::AgentKind;
use tn_core::Rgb;

use super::TerminalView;
use crate::style::{col, cola, HOVER, INSET, UI_SANS};

impl TerminalView {
    /// This pane's identity accent: Claude coral / Codex teal, or the UI accent
    /// for a plain shell.
    fn agent_accent(&self) -> Rgb {
        match self.agent {
            Some(AgentKind::ClaudeCode) => self.claude_accent,
            Some(AgentKind::Codex) => self.codex_accent,
            None => self.ui_accent,
        }
    }

    /// A two-tone context-usage ring (grey track + agent-colored arc) with a
    /// centered percent label — the mockup's signature per-agent readout.
    fn usage_ring(&self, pct: u32, accent: Rgb) -> Div {
        div()
            .relative()
            .w(px(32.))
            .h(px(32.))
            .flex_none()
            .child(
                gpui::svg()
                    .path(crate::assets::ring_track_path())
                    .absolute()
                    .size_full()
                    .text_color(rgba(0xffffff1f)),
            )
            .child(
                gpui::svg()
                    .path(crate::assets::ring_path(pct))
                    .absolute()
                    .size_full()
                    .text_color(col(accent)),
            )
            .child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(9.))
                    .font_weight(FontWeight(760.)) // §16 .lbl weight 760
                    .text_color(col(self.ui_fg)) // §16 .lbl color = fg
                    .child(SharedString::from(format!("{pct}%"))),
            )
    }

    /// Agent pane header: avatar + name/model + usage ring. No "Thinking…"
    /// indicator — we can't observe the agent's think state from the PTY, so we
    /// don't fake one (Calm Glass: honest chrome).
    fn render_agent_header(&self, agent: AgentKind) -> Div {
        let accent = self.agent_accent();
        let name = match agent {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
        };
        let model = self
            .usage
            .as_ref()
            .map(|u| crate::workspace::short_model(&u.model))
            .filter(|m| !m.is_empty());
        let mut who = div().flex().flex_col().child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight(680.)) // §16 .nm weight 680
                .text_color(col(self.ui_fg)) // §16 .nm color = fg
                .child(SharedString::from(name)),
        );
        if let Some(m) = model {
            who = who.child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight(520.)) // §16 .model weight 520
                    .text_color(col(self.ui_muted)) // §16 .model color = muted
                    .child(SharedString::from(m)),
            );
        }
        let avatar = div()
            .w(px(28.))
            .h(px(28.))
            .rounded(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .bg(cola(accent, 0.14))
            .child(crate::assets::icon("spark", 16.).text_color(col(accent)));
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(11.)) // §16 .agenthead gap 11
            .py(px(10.)) // §16 .agenthead padding 10px 14px
            .px(px(14.))
            .flex_none()
            .rounded_t(px(13.)) // top corners follow the pane card
            .font_family(UI_SANS) // chrome = sans, terminal = mono
            // mockup .agenthead bg:rgba(claude,0.07) → transparent 72%(折射,无 glow)
            .bg(linear_gradient(
                180.,
                linear_color_stop(cola(accent, 0.07), 0.),
                linear_color_stop(cola(accent, 0.0), 0.72),
            ))
            .child(avatar)
            .child(who)
            .child(div().flex_1());
        if let Some(u) = &self.usage {
            let pct = (u.context_frac() * 100.0).round() as u32;
            let meta = div()
                .flex()
                .flex_col()
                .items_end()
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight(640.)) // §16 .tok weight 640
                        .text_color(gpui::rgb(0xA6AFD4)) // §16 .tok color = fg-dim(无 token)
                        .child(SharedString::from(format!(
                            "{} / {}",
                            crate::workspace::human_tokens(u.context_used as u64),
                            crate::workspace::human_tokens(u.context_max as u64)
                        ))),
                )
                .when(u.cost_usd > 0.0, |d| {
                    d.child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight(640.)) // §16 .cost weight 640
                            .text_color(col(self.palette.ansi[2])) // green
                            .child(SharedString::from(format!("${:.2}", u.cost_usd))),
                    )
                });
            head = head.child(
                // mockup .usage 药丸:gap 11 · padding 4 5 4 12 · radius 999 · bg g2(.04)
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(11.))
                    .py(px(4.))
                    .pl(px(12.))
                    .pr(px(5.))
                    .rounded_full()
                    .bg(rgba(INSET))
                    .child(meta)
                    .child(self.usage_ring(pct, accent)),
            );
        }
        head
    }

    /// Plain-shell pane header (mockup `.phead`): term icon + cwd + shell-name chip.
    /// 完整复刻 mockup —— 覆盖了早先"普通 shell 极简无头"的取舍(owner 选择严格对齐)。
    fn render_shell_header(&self) -> Div {
        let shell = super::shell_name_of(&self.program);
        let cwd = self.cwd();
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.)) // §16 .phead gap 9
            .h(px(36.)) // §16 .phead height 36
            .px(px(13.)) // §16 .phead padding 0 13
            .flex_none()
            .rounded_t(px(13.))
            .font_family(UI_SANS)
            .text_size(px(11.5)) // §16 .phead 11.5
            .font_weight(FontWeight(560.)) // §16 .phead weight 560
            .text_color(col(self.ui_muted)) // §16 .phead color = muted
            .child(crate::assets::icon("term", 14.).text_color(col(self.ui_accent)));
        let head = match cwd {
            Some(c) => head.child(
                // mockup .phead .cwd = fg-dim(#A6AFD4 无主题 token → 字面量)
                div()
                    .text_color(gpui::rgb(0xA6AFD4))
                    .child(SharedString::from(crate::workspace::short_cwd(&c))),
            ),
            None => head,
        };
        head.child(div().flex_1()) // .sp
            .child(
                // mockup .chip:10.5 · 560 · py2 px9 · radius999 · fg-dim · bg g3(.06)
                div()
                    .text_size(px(10.5))
                    .font_weight(FontWeight(560.))
                    .py(px(2.))
                    .px(px(9.))
                    .rounded_full()
                    .text_color(gpui::rgb(0xA6AFD4))
                    .bg(rgba(HOVER))
                    .child(SharedString::from(shell)),
            )
    }

    /// Per-pane header — agent header for agents, else a shell `.phead`(cwd + chip).
    pub(super) fn render_pane_header(&self) -> Option<Div> {
        Some(match self.agent {
            Some(a) => self.render_agent_header(a),
            None => self.render_shell_header(),
        })
    }
}
