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
use crate::style::{col, cola, icon, HOVER, INSET, R_CARD, UI_SANS};

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

    /// 活动栏里的一张「文件 + 增删」行(mockup `.afile`):图标 + 文件名 + 右侧 +N/−N。
    fn arail_file(&self, name: &str, plus: &str, minus: Option<&str>) -> Div {
        let green = col(self.palette.ansi[2]);
        let red = col(self.palette.ansi[1]);
        let mut pm = div()
            .flex()
            .flex_row()
            .gap(px(5.)) // §16 .pm gap 5
            .flex_none()
            .text_size(px(10.))
            .font_weight(FontWeight(680.))
            .child(div().text_color(green).child(SharedString::from(plus.to_string()))); // .ad green
        if let Some(m) = minus {
            pm = pm.child(div().text_color(red).child(SharedString::from(m.to_string()))); // .dl red
        }
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.)) // §16 .afile gap 7
            .font_family(self.font_family.clone()) // .afile = mono
            .text_size(px(11.5))
            .text_color(gpui::rgb(0xA6AFD4)) // §16 fg-dim(无 token)
            .child(icon("file", 13., self.ui_muted)) // .afile .i 13 muted
            .child(
                // .nm:占满中间、可裁(文件名通常很短)
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(SharedString::from(name.to_string())),
            )
            .child(pm) // .pm 右靠(margin-left:auto)
    }

    /// 活动栏迷你 diff 的一行(mockup `.adiff div`):+ 绿 / − 红。
    fn arail_dline(&self, is_add: bool, text: &str) -> Div {
        let c = if is_add { self.palette.ansi[2] } else { self.palette.ansi[1] };
        div()
            .overflow_hidden()
            .text_color(col(c))
            .child(SharedString::from(text.to_string()))
    }

    /// agent 活动栏(mockup `.arail`):运行状态行 + 「本次改动」diff 卡 + 提示。
    /// 视觉先行 —— 此处为 mockup 示例内容(占位);真实数据(git diff / JSONL,不解析
    /// 终端正文)后续接线。只在 agent 面板渲染(shell 面板正文满宽、无栏)。
    pub(super) fn render_activity_rail(&self) -> Div {
        div()
            .flex_none()
            .w(px(212.)) // §16 .arail flex 0 0 212
            .flex()
            .flex_col()
            .gap(px(11.)) // §16 .arail gap 11
            .pt(px(12.))
            .px(px(12.))
            .pb(px(14.)) // §16 .arail padding 12 12 14
            .min_h(px(0.))
            .overflow_hidden() // mockup overflow:auto;滚动留后续,先裁剪
            .border_l(px(1.))
            .border_color(rgba(0xffffff0d)) // border-left white .05 = round(.05×255)=13
            .font_family(UI_SANS) // 状态/标签/提示 = sans;.afile/.adiff 局部转 mono
            .child(
                // .astat:状态点 + 运行中 · Update + 右对齐时长
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(7.))
                    .text_size(px(11.))
                    .text_color(gpui::rgb(0xA6AFD4)) // fg-dim(无 token)
                    .child(
                        div()
                            .w(px(7.))
                            .h(px(7.))
                            .rounded_full()
                            .flex_none()
                            .bg(col(self.agent_accent())), // .dot = agent 色(mockup --claude)
                    )
                    .child(div().flex_1().child(SharedString::from("运行中 · Update"))) // 文本撑开,.t 右靠
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(col(self.ui_muted)) // .t muted
                            .child(SharedString::from("1m12s")),
                    ),
            )
            .child(
                // .alabel
                div()
                    .text_size(px(10.))
                    .font_weight(FontWeight(680.))
                    .text_color(col(self.ui_muted))
                    .pt(px(2.))
                    .px(px(2.)) // padding 2 2 0
                    .child(SharedString::from("本次改动")),
            )
            .child(
                // .achip.cur:当前文件卡(accent 描边)+ 迷你 diff
                div()
                    .rounded(px(R_CARD))
                    .bg(cola(self.ui_accent, 0.06)) // .cur bg accent@.06
                    .border_1()
                    .border_color(cola(self.ui_accent, 0.22)) // mockup inset 1px(gpui 无 inset 投影→内描边)
                    .py(px(8.))
                    .px(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.arail_file("element.rs", "+3", Some("−1")))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .font_family(self.font_family.clone()) // .adiff = mono
                            .text_size(px(10.))
                            .line_height(px(15.5)) // line-height 1.55 × 10
                            .child(self.arail_dline(false, "- win.paint_text(cell.ch)"))
                            .child(self.arail_dline(true, "+ let g = atlas.glyph(ch)"))
                            .child(self.arail_dline(true, "+ quads.push(Quad::…)")),
                    ),
            )
            .child(
                // .achip:第二张卡
                div()
                    .rounded(px(R_CARD))
                    .bg(rgba(INSET)) // .achip bg white@.04
                    .py(px(8.))
                    .px(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.arail_file("lib.rs", "+1", None)),
            )
            .child(
                // .ahint
                div()
                    .text_size(px(10.))
                    .text_color(gpui::rgb(0x474E72)) // faint(无 token)
                    .px(px(2.))
                    .child(SharedString::from("点卡片 = Space 速览全 diff")),
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
