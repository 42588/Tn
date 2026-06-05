//! Agent pane header UI (待优化清单 §6.2): the avatar + name/model + context
//! usage ring shown above Claude/Codex panes. Split out of `mod.rs` to keep the
//! render core lean; `impl super::TerminalView` so it can read the view's
//! private agent/usage/palette state. Only [`render_pane_header`] is called from
//! the parent (`render`); the rest are header-internal.

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, AnyElement, App, Context, Div,
    FontWeight, MouseButton, Overflow, SharedString, WeakEntity,
};
use tn_ai::AgentKind;
use tn_config::BillingMode;
use tn_core::Rgb;

use super::TerminalView;
use crate::style::{col, cola, glass_card, icon, HOVER, INSET, R_CARD, UI_SANS};

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

    /// Effective billing display mode for THIS pane's agent: a per-agent override
    /// (`[general].claude_billing` / `codex_billing`) if set, else the global
    /// `[general].billing_mode`. Lets one window mix a subscription Claude (`%`)
    /// and an API Codex (`$`). Resolved from `self.agent` so it tracks a
    /// shell-typed agent (sync_shell_agent), not just launch intent.
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
    fn render_agent_header(&self, agent: AgentKind, weak: WeakEntity<Self>) -> Div {
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
            // This pane's chosen display mode (WYSIWYG): API always shows the
            // dollar estimate ($0.00 for an unpriced/proxy model like `moonbridge`),
            // subscription always the context %, tokens the throughput. No
            // cross-fallback — clicking the pill cycles it, per pane (usage_mode).
            let billing = self.usage_mode;
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
                .when(billing == BillingMode::Api, |d| {
                    d.child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight(640.)) // §16 .cost weight 640
                            .text_color(col(self.palette.ansi[2])) // green
                            .child(SharedString::from(format!("${:.2}", u.cost_usd))),
                    )
                })
                .when(billing == BillingMode::Subscription, |d| {
                    d.child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight(640.))
                            .text_color(col(self.ui_fg))
                            .child(SharedString::from(format!("{}%", pct))),
                    )
                })
                // Tokens: total session throughput (input+output+cache), the
                // raw-consumption view for proxy models with no priced cost.
                .when(billing == BillingMode::Tokens, |d| {
                    d.child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight(640.))
                            .text_color(gpui::rgb(0xA6AFD4)) // fg-dim (无 token)
                            .child(SharedString::from(format!(
                                "{} tok",
                                crate::workspace::human_tokens(u.total_tokens())
                            ))),
                    )
                });
            head = head.child(
                // mockup .usage 药丸:gap 11 · padding 4 5 4 12 · radius 999 · bg g2(.04)
                // Clickable: cycles THIS pane's display mode ($ → % → tokens) in
                // memory (usage_mode). cursor + hover signal it's a control.
                div()
                    .id("usage-pill")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(11.))
                    .py(px(4.))
                    .pl(px(12.))
                    .pr(px(5.))
                    .rounded_full()
                    .bg(rgba(INSET))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(HOVER)))
                    .on_mouse_down(MouseButton::Left, move |_e, _w, app: &mut App| {
                        // Per-pane: cycle just this pane's pill ($ → % → tokens) at
                        // CLICK time. The pane isn't leased then; calling update during
                        // workspace render (the old `pane.update`) re-leased an already
                        // -leased pane → unwrap panic / window crash (see workspace).
                        let _ = weak.update(app, |v, c| {
                            v.usage_mode = crate::usage_display::cycle(v.usage_mode);
                            c.notify();
                        });
                    })
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
        let mut head = head.child(div().flex_1()); // .sp
        // B4/C3: SSH connection state as one polished info pill:
        // `已连接 root@host:port 登录方案 密钥`.
        if let Some(state) = self.ssh_conn {
            use super::SshConnState::*;
            let (color, status) = match state {
                Connecting => (self.ui_accent, "连接中"),
                Connected => (self.palette.ansi[2], "已连接"),
                Reconnecting => (self.palette.ansi[3], "重连中"),
                Disconnected => (self.palette.ansi[1], "已断开"),
            };
            let method = match self.ssh_conn_method {
                Some(tn_pty::AuthKind::PublicKey) => "密钥",
                Some(tn_pty::AuthKind::Password) => "密码",
                Some(tn_pty::AuthKind::KeyboardInteractive) => "交互",
                None => "检测中",
            };
            let target = if self.ssh_target.chars().count() > 34 {
                let mut s: String = self.ssh_target.chars().take(33).collect();
                s.push('…');
                s
            } else {
                self.ssh_target.clone()
            };
            head = head.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .py(px(4.))
                    .pl(px(9.))
                    .pr(px(10.))
                    .rounded_full()
                    .bg(cola(color, 0.10))
                    .border_1()
                    .border_color(cola(color, 0.28))
                    .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(col(color)))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight(700.))
                            .text_color(col(color))
                            .child(SharedString::from(status)),
                    )
                    .child(
                        div()
                            .font_family(UI_SANS)
                            .text_size(px(11.))
                            .font_weight(FontWeight(650.))
                            .text_color(gpui::rgb(0xD7DDF6))
                            .child(SharedString::from(target)),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(FontWeight(620.))
                            .text_color(col(self.ui_muted))
                            .child(SharedString::from("登录方案")),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight(720.))
                            .text_color(col(color))
                            .child(SharedString::from(method)),
                    ),
            );
            return head;
        }
        head.child(
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
            .child(
                div()
                    .text_color(green)
                    .child(SharedString::from(plus.to_string())),
            ); // .ad green
        if let Some(m) = minus {
            pm = pm.child(
                div()
                    .text_color(red)
                    .child(SharedString::from(m.to_string())),
            ); // .dl red
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

    /// agent 活动栏(mockup `.arail`):诚实状态行 + 「本次改动」真实 git diff 卡 + 提示。
    /// 数据 = `git diff HEAD`(pane cwd,后台有界 git 计算),**不解析终端正文**。
    /// **不伪造「运行中」实时态** → 状态行只显诚实的 git 摘要。
    /// 点卡片发 [`OpenInQuickLook`] 让 workspace 弹 Quick Look 看全 diff。
    ///
    /// ## Render-pure guarantee
    /// The render body performs **zero computation** — it only reads the pre-built
    /// `RailState` enum. All git I/O lives in `refresh_changes` on the background
    /// executor. `Loading` returns a skeleton immediately; the real cards appear
    /// when `Ready` arrives via channel delivery.
    pub(super) fn render_activity_rail(&self, cx: &mut Context<Self>) -> Div {
        let green = col(self.palette.ansi[2]);
        let red = col(self.palette.ansi[1]);

        // ── Build the chrome shell (status row + left border) once ──
        let rail_shell = |status: Div, body: AnyElement| -> Div {
            div()
                .flex_none()
                .w(px(212.))
                .flex()
                .flex_col()
                .gap(px(11.))
                .pt(px(12.))
                // .px(12) 已移除 → 改为各子元素自行加 px/mx，为 glass_card 光晕
                // 阴影腾出 12px 的「发光隔离带」，避免被 overflow_hidden 截断
                .pb(px(14.))
                .min_h(px(0.))
                .overflow_hidden()
                .border_l(px(1.))
                .border_color(rgba(0xffffff0d))
                .font_family(UI_SANS)
                .child(status)
                .child(body)
        };

        // ── Status row (shared by all states) ──
        let build_status = |summary: &str, add: Option<u32>, del: Option<u32>| -> Div {
            let mut s = div()
                .px(px(12.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.))
                .text_size(px(11.))
                .text_color(gpui::rgb(0xA6AFD4))
                .child(
                    div()
                        .w(px(7.))
                        .h(px(7.))
                        .rounded_full()
                        .flex_none()
                        .bg(col(self.agent_accent())),
                )
                .child(
                    div()
                        .flex_1()
                        .child(SharedString::from(summary.to_string())),
                );
            if let (Some(a), Some(d)) = (add, del) {
                if a > 0 || d > 0 {
                    s = s.child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(5.))
                            .flex_none()
                            .text_size(px(10.5))
                            .font_weight(FontWeight(680.))
                            .child(
                                div()
                                    .text_color(green)
                                    .child(SharedString::from(format!("+{a}"))),
                            )
                            .child(
                                div()
                                    .text_color(red)
                                    .child(SharedString::from(format!("−{d}"))),
                            ),
                    );
                }
            }
            s
        };

        match &self.rail_state {
            // ── Loading: skeleton placeholders ──
            super::RailState::Loading => {
                let status = build_status("正在分析改动…", None, None);
                let skeleton =
                    div()
                        .px(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .children((0..3).map(|_| {
                            div()
                                .w_full()
                                .h(px(32.))
                                .rounded(px(R_CARD))
                                .bg(rgba(INSET))
                        }));
                rail_shell(status, skeleton.into_any_element())
            }

            // ── Ready: real cards ──
            super::RailState::Ready { files, root } => {
                let total_add: u32 = files.iter().map(|f| f.add).sum();
                let total_del: u32 = files.iter().map(|f| f.del).sum();
                let summary = if files.is_empty() {
                    "工作区干净".to_string()
                } else {
                    format!("{} 个文件改动", files.len())
                };
                let has_files = !files.is_empty();
                let status = build_status(
                    &summary,
                    has_files.then_some(total_add),
                    has_files.then_some(total_del),
                );

                if !has_files {
                    return rail_shell(
                        status,
                        div()
                            .text_size(px(10.5))
                            .text_color(col(self.ui_muted))
                            .pt(px(2.))
                            .px(px(12.))
                            .child(SharedString::from("agent 改动会实时显示在这里"))
                            .into_any_element(),
                    );
                }

                let mut scrollable = div()
                    .id("arail-scrollable")
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(11.))
                    .pb(px(14.))
                    .overflow_hidden();
                scrollable.interactivity().base_style.overflow.y = Some(Overflow::Scroll);

                scrollable = scrollable.child(
                    div()
                        .px(px(12.))
                        .text_size(px(10.))
                        .font_weight(FontWeight(680.))
                        .text_color(col(self.ui_muted))
                        .pt(px(2.))
                        .child(SharedString::from("本次改动")),
                );

                let mut list_inner = div()
                    .w_full()
                    .rounded(px(R_CARD - 1.))
                    .overflow_hidden()
                    .bg(gpui::rgb(0x121626)); // ★ 死色垫底

                let mut rows_container = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .p(px(4.)) // 增加内边距，让内部 hover 形成舒适的胶囊感
                    .gap(px(2.)) // 行与行之间留极小间隙
                    .bg(rgba(INSET));

                for f in files.iter() {
                    let plus = format!("+{}", f.add);
                    let minus = (f.del > 0).then(|| format!("−{}", f.del));

                    let abs = root.join(&f.path);
                    let row = div()
                        .w_full()
                        .rounded(px(6.)) // 内部胶囊圆角
                        .py(px(6.))
                        .px(px(8.))
                        .hover(|s| s.bg(rgba(HOVER))) // 极简胶囊 hover
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_this, _e, _w, cx| {
                                cx.emit(super::OpenInQuickLook(abs.clone()));
                            }),
                        )
                        .child(self.arail_file(f.name(), &plus, minus.as_deref()));

                    rows_container = rows_container.child(row);
                }

                list_inner = list_inner.child(rows_container);
                let single_card = glass_card(list_inner, true, self.agent_accent());

                // ★ 外层 div 物理隔离：px(12) 左右安全区 + py(6) 上下舒展空间。
                // flex_none() 禁止 scrollable 的 flex 容器挤压卡片高度，
                // 否则上下光晕被压扁、发散空间不足。
                scrollable =
                    scrollable.child(div().flex_none().px(px(12.)).py(px(6.)).child(single_card));

                scrollable = scrollable.child(
                    div()
                        .text_size(px(10.))
                        .text_color(gpui::rgb(0x474E72))
                        .px(px(12.))
                        .child(SharedString::from("点卡片 = 速览全 diff")),
                );

                rail_shell(status, scrollable.into_any_element())
            }

            // ── Idle: shouldn't render (called only when agent is present) ──
            super::RailState::Idle => div(),
        }
    }

    /// Per-pane header — agent header for agents, else a shell `.phead`(cwd + chip).
    /// `weak` = a handle to THIS pane, captured by the usage-pill click closure so
    /// it can cycle the display mode at event time. The caller (workspace) passes
    /// `pane.downgrade()` and renders via `read` — never `update` during render
    /// (that re-leases the pane mid-render → panic).
    pub(super) fn render_pane_header(&self, weak: WeakEntity<Self>) -> Option<Div> {
        Some(match self.agent {
            Some(a) => self.render_agent_header(a, weak),
            None => self.render_shell_header(),
        })
    }
}
