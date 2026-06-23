//! Agent pane header UI: the avatar + name/model + context
//! usage ring shown above agent panes. Split out of `mod.rs` to keep the
//! render core lean; `impl super::TerminalView` so it can read the view's
//! private agent/usage/palette state. Only [`render_pane_header`] is called from
//! the parent (`render`); the rest are header-internal.
//!
//! 磷光(SHEET 02):agent 头高 38 / shell 头高 34,都坐 L2 抬升层 + 底 1px h0;
//! 身份色只出现在头像方标、用量环与 chip,不污染正文。

use gpui::{
    div, prelude::*, px, rgba, AnyElement, App, Context, Div, FontWeight, MouseButton, Overflow,
    SharedString, WeakEntity,
};
use tn_agent::{AgentStatus, AiUsage};
use tn_core::Rgb;

use super::TerminalView;
use crate::style::{
    col, cola, AGENT_HEAD_H, H0, H1, PH, PLATE_HEAD_H, R_CARD, R_CHIP, T0, T1, T2, T3, UI_SANS,
};

/// Render a 5h-window reset as a relative "重置 ~Xh Ym 后" string. Past / unknown →
/// "重置 ~即将". Pure (takes `now` implicitly via `SystemTime::now`).
fn fmt_reset(reset: Option<std::time::SystemTime>) -> String {
    match reset.and_then(|r| r.duration_since(std::time::SystemTime::now()).ok()) {
        Some(d) => {
            let mins = d.as_secs() / 60;
            format!("重置 ~{}h{:02}m 后", mins / 60, mins % 60)
        }
        None => "重置 ~即将".to_string(),
    }
}

fn short_chip(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    let total = trimmed.chars().count();
    if total <= max_chars {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn usage_token_chip_label(u: &AiUsage, _mode: tn_config::BillingMode) -> String {
    format!("{} TOK", crate::workspace::human_tokens(u.total_tokens()))
}

impl TerminalView {
    /// This pane's identity accent: the agent's resolved accent, or the UI accent
    /// for a plain shell. Resolved on agent change into `agent_accent` (no per-agent
    /// match in the render path).
    fn agent_accent(&self) -> Rgb {
        self.agent_accent
    }

    /// 用量环(SHEET 02 SPEC:Ø18 · 孔 3 · 身份色弧 + 余量轨)— GPUI path 弧线
    /// (svg 资产即路径,渲染契约的 conic 豁免项)。无内嵌文字 — 读数在旁边的 chip。
    fn usage_ring(&self, pct: u32, accent: Rgb) -> Div {
        div()
            .relative()
            .w(px(18.))
            .h(px(18.))
            .flex_none()
            .child(
                gpui::svg()
                    .path(crate::assets::ring_track_path())
                    .absolute()
                    .size_full()
                    // 余量轨 = L4 顶面(SHEET 02 SPEC「身份色弧 + L4 余量」;
                    // 曾是白 8% 合成出 #272D3A,差异总结 2-6)。
                    .text_color(gpui::rgb(crate::style::L4)),
            )
            .child(
                gpui::svg()
                    .path(crate::assets::ring_path(pct))
                    .absolute()
                    .size_full()
                    .text_color(col(accent)),
            )
    }

    /// 状态 chip(phosphor `.chip`):1px 同色 ·30% 边 + soft 底 + r3 + mono 10。
    fn agent_chip(&self, label: String, color: Rgb) -> Div {
        div()
            .flex_none()
            .px(px(8.))
            .py(px(2.))
            .rounded(px(R_CHIP))
            .bg(cola(color, 0.12))
            .border_1()
            .border_color(cola(color, 0.3))
            .font_family(self.font_family.clone())
            .text_size(px(crate::style::FS_MICRO))
            .text_color(col(color))
            .child(SharedString::from(label))
    }

    /// Agent pane header(SHEET 02 板 B):高 38 · L2 · 底 1px h0 — 方标 + 名/模型 +
    /// 用量读数 + Ø18 环 + chip。No "Thinking…" indicator — we can't observe the
    /// agent's think state from the PTY, so we don't fake one(诚实 chrome)。
    fn render_agent_header(&self, weak: WeakEntity<Self>) -> Div {
        let accent = self.agent_accent();
        // The agent's display name comes from its descriptor (resolved into
        // `agent_label` on agent change); fall back to a neutral label defensively.
        let name = self
            .agent_label
            .clone()
            .unwrap_or_else(|| SharedString::from("Agent"));
        let model = self
            .agent_model
            .as_ref()
            .map(|m| crate::workspace::short_model(m))
            .or_else(|| {
                self.usage
                    .as_ref()
                    .map(|u| crate::workspace::short_model(&u.model))
            })
            .filter(|m| !m.is_empty());
        // `.amark`:22×22 · r5 · 1px 身份色 ·35% 边 + soft 底 + ✳ 字形。
        let avatar = div()
            .w(px(22.))
            .h(px(22.))
            .rounded(px(5.))
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .bg(cola(accent, 0.13))
            .border_1()
            .border_color(cola(accent, 0.35))
            .font_family(self.font_family.clone())
            .text_size(px(crate::style::FS_CAPTION))
            .font_weight(FontWeight(600.))
            .text_color(col(accent))
            // 身份字形 ✳/◆/⟡ 与 tab/磁贴同表(差异总结 2-5)。
            .child(SharedString::from(
                self.agent
                    .as_ref()
                    .map(|id| crate::welcome::agent_glyph_ch(id.as_str()))
                    .unwrap_or("⟡"),
            ));
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(AGENT_HEAD_H))
            .px(px(12.))
            .flex_none()
            .bg(gpui::rgb(crate::style::L2)) // L2 抬升(不透明,契约 1)
            .border_b(px(1.))
            .border_color(rgba(H0))
            .font_family(UI_SANS) // chrome = sans, terminal = mono
            .child(avatar)
            .child(
                div()
                    .text_size(px(crate::style::FS_CAPTION))
                    .font_weight(FontWeight(600.))
                    .text_color(gpui::rgb(T0))
                    .child(name),
            );
        if let Some(m) = model {
            head = head.child(
                div()
                    .font_family(self.font_family.clone())
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(gpui::rgb(T2))
                    .child(SharedString::from(m)),
            );
        }
        head = head.child(div().flex_1());

        if let Some(err) = &self.agent_error {
            head = head.child(self.agent_chip(short_chip(err, 30), self.palette.ansi[1]));
        } else if let Some(prompt) = &self.agent_permission_prompt {
            head = head.child(
                self.agent_chip(format!("权限: {}", short_chip(prompt, 24)), self.ui_yellow),
            );
        } else if let Some(status) = &self.agent_status {
            let (label, color) = match status {
                AgentStatus::Starting => ("启动中".to_string(), self.ui_accent),
                AgentStatus::Idle => ("空闲".to_string(), self.ui_muted),
                AgentStatus::Running => ("RUN".to_string(), self.ui_accent), // 运行语义 = 磷光
                AgentStatus::Exited => ("已退出".to_string(), self.ui_muted),
                AgentStatus::Error => ("错误".to_string(), self.palette.ansi[1]),
            };
            head = head.child(self.agent_chip(label, color));
        } else if let Some(text) = &self.agent_transcript_tail {
            head = head.child(self.agent_chip(short_chip(text, 26), self.ui_muted));
        }
        // Usage ring/pill is a capability slot: only agents that declare `usage`
        // (i.e. have a telemetry adapter) show it. A config-level agent hosts
        // without it instead of showing an empty ring.
        if let Some(u) = self.usage.as_ref().filter(|_| self.agent_caps.usage) {
            let pct = (u.context_frac() * 100.0).round() as u32;
            let reading = usage_token_chip_label(u, self.usage_mode);
            // SHEET 02:`84K / 200K` 读数(mono t2)+ Ø18 环 + 身份色 chip。Click the
            // readout/ring → 额度 popover (windowed spend). The right chip is kept
            // token-only; context remains on the left readout/ring.
            let weak_quota = weak.clone();
            head = head.child(
                div()
                    .id("usage-pill")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_e, _w, app: &mut App| {
                        let _ = weak_quota.update(app, |v, c| v.toggle_quota(c));
                    })
                    .child(
                        div()
                            .font_family(self.font_family.clone())
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(gpui::rgb(T2))
                            .child(SharedString::from(format!(
                                "{} / {}",
                                crate::workspace::human_tokens(u.context_used as u64),
                                crate::workspace::human_tokens(u.context_max as u64)
                            ))),
                    )
                    .child(self.usage_ring(pct, accent))
                    .child(
                        div()
                            .id("usage-mode")
                            .child(self.agent_chip(reading, accent)),
                    ),
            );
        }
        head
    }

    /// 额度 popover(Stage 2):account-wide windowed spend (5h/日/周) anchored under
    /// the usage readout. Honestly labelled 本地实算; the official cap/reset (Stage 3,
    /// claude.ai) is a reserved row. Opened by clicking the usage readout/ring.
    pub(super) fn render_quota_panel(&self) -> Div {
        use tn_ai::usage_windows::Window;
        let green = col(self.palette.ansi[2]);
        let fmt_cost = |c: f64| -> String {
            if c < 1.0 {
                format!("${c:.3}")
            } else {
                format!("${c:.2}")
            }
        };
        let row = |label: &str, w: &Window| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .h(px(22.))
                .child(
                    div()
                        .w(px(48.))
                        .flex_none()
                        .text_color(gpui::rgb(T2))
                        .child(SharedString::from(label.to_string())),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .w(px(54.))
                        .flex_none()
                        .text_color(green)
                        .child(SharedString::from(fmt_cost(w.cost_usd))),
                )
                .child(
                    div()
                        .w(px(64.))
                        .flex_none()
                        .text_color(gpui::rgb(T1))
                        .child(SharedString::from(format!(
                            "{} tok",
                            crate::workspace::human_tokens(w.tokens)
                        ))),
                )
                .child(
                    div()
                        .w(px(34.))
                        .flex_none()
                        .text_color(gpui::rgb(T3))
                        .child(SharedString::from(format!("{}条", w.turns))),
                )
        };

        let body: AnyElement = match &self.quota {
            None => div()
                .px(px(12.))
                .py(px(12.))
                .text_color(gpui::rgb(T3))
                .child(SharedString::from("计算中…"))
                .into_any_element(),
            Some(q) => div()
                .flex()
                .flex_col()
                .px(px(12.))
                .py(px(8.))
                .gap(px(2.))
                .child(row("5 小时", &q.five_hour))
                .child(
                    div()
                        .pl(px(48.))
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(gpui::rgb(T3))
                        .child(SharedString::from(fmt_reset(q.reset_5h))),
                )
                .child(row("今天", &q.day))
                .child(row("本周", &q.week))
                .child(div().h(px(1.)).bg(rgba(H1)).my(px(6.)))
                .child(
                    div()
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(gpui::rgb(T3))
                        .child(SharedString::from("官方配额 · 暂未接入")),
                )
                .into_any_element(),
        };

        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .px(px(12.))
            .flex_none()
            .border_b(px(1.))
            .border_color(rgba(H0))
            .child(
                div()
                    .w(px(5.))
                    .h(px(5.))
                    .rounded_full()
                    .flex_none()
                    .bg(gpui::rgb(PH)),
            )
            .child(
                div()
                    .flex_1()
                    .text_color(gpui::rgb(T2))
                    .child(SharedString::from("用量额度")),
            )
            .child(
                div()
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(gpui::rgb(T3))
                    .child(SharedString::from("本地实算")),
            );

        crate::style::shadowed(
            div()
                .absolute()
                .top(px(40.))
                .right(px(10.))
                .w(px(272.))
                .flex()
                .flex_col()
                .rounded(px(crate::style::R_PANEL - 1.))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(crate::style::H2))
                .bg(gpui::rgb(crate::style::L3))
                .font_family(self.font_family.clone())
                .text_size(px(crate::style::FS_MICRO))
                // Clicks inside the popover stay inside (don't toggle it shut).
                .on_mouse_down(MouseButton::Left, |_e, _w, cx| cx.stop_propagation())
                .child(head)
                .child(body),
            crate::style::shadow_float(),
        )
    }

    /// Plain-shell pane header(SHEET 02 `.plate-head`):高 34 · L2 · 底 1px h0 ·
    /// mono 11 — 磷光 ❯ + cwd + shell chip。
    fn render_shell_header(&self) -> Div {
        let shell = super::shell_name_of(&self.program);
        let cwd = self.cwd();
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(PLATE_HEAD_H))
            .px(px(12.))
            .flex_none()
            .bg(gpui::rgb(crate::style::L2)) // L2 抬升
            .border_b(px(1.))
            .border_color(rgba(H0))
            .font_family(self.font_family.clone())
            .text_size(px(crate::style::FS_MICRO))
            .text_color(gpui::rgb(T1))
            .child(
                div()
                    .font_weight(FontWeight(600.))
                    .text_color(gpui::rgb(PH))
                    .child(SharedString::from("❯")),
            );
        let head = match cwd {
            Some(c) => head.child(
                div()
                    .text_color(gpui::rgb(T1))
                    .child(SharedString::from(crate::workspace::short_cwd(&c))),
            ),
            None => head,
        };
        let mut head = head.child(div().flex_1());
        // B4/C3: SSH connection state as one chip:`已连接 root@host:port · 密钥`。
        if let Some(state) = self.ssh_conn {
            use super::SshConnState::*;
            let (color, status) = match state {
                Connecting => (self.ui_accent, "连接中"),
                Connected => (self.palette.ansi[2], "已连接"),
                Reconnecting => (self.palette.ansi[3], "重连中"),
                Disconnected => (self.palette.ansi[1], "已断开"),
            };
            let method = match self.ssh_conn_method {
                Some(tn_pty::AuthKind::PublicKey) => "KEY",
                Some(tn_pty::AuthKind::Password) => "PASSWORD",
                Some(tn_pty::AuthKind::KeyboardInteractive) => "INTERACTIVE",
                None => "…",
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
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(R_CHIP))
                    .border_1()
                    .border_color(cola(color, 0.3))
                    .bg(cola(color, 0.10))
                    .child(div().w(px(5.)).h(px(5.)).rounded_full().bg(col(color)))
                    .child(
                        div()
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(col(color))
                            .child(SharedString::from(status)),
                    )
                    .child(
                        div()
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(gpui::rgb(T1))
                            .child(SharedString::from(target)),
                    )
                    .child(
                        div()
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(gpui::rgb(T2))
                            .child(SharedString::from(method)),
                    ),
            );
            return head;
        }
        head.child(
            // `.chip`:1px h1 · r3 · mono 10 · t1
            div()
                .px(px(8.))
                .py(px(2.))
                .rounded(px(R_CHIP))
                .border_1()
                .border_color(rgba(H1))
                .text_size(px(crate::style::FS_MICRO))
                .text_color(gpui::rgb(T1))
                .child(SharedString::from(shell)),
        )
    }

    /// 活动栏里的一张「文件 + 增删」行:文件名 mono + 右侧 +N/−N(ok/err)。
    fn arail_file(&self, name: &str, plus: &str, minus: Option<&str>) -> Div {
        let green = col(self.palette.ansi[2]);
        let red = col(self.palette.ansi[1]);
        let mut pm = div()
            .flex()
            .flex_row()
            .gap(px(5.))
            .flex_none()
            .text_size(px(crate::style::FS_MICRO))
            .font_weight(FontWeight(600.))
            .child(
                div()
                    .text_color(green)
                    .child(SharedString::from(plus.to_string())),
            );
        if let Some(m) = minus {
            pm = pm.child(
                div()
                    .text_color(red)
                    .child(SharedString::from(m.to_string())),
            );
        }
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .font_family(self.font_family.clone())
            .text_size(px(crate::style::FS_MICRO))
            .text_color(gpui::rgb(T0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(name.to_string())),
            )
            .child(pm)
    }

    /// agent 活动栏(SHEET 02 `.rail`):宽 248 · L1 · 左 1px h0;rail-head(高 30 ·
    /// 磷光点 + 本次改动 + git diff)+ `.dcard` 改动卡(L2 + h0 + r4)。
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
        let mono = self.font_family.clone();

        // ── rail 外壳:宽 248 · 左 1px h0 接缝 · rail-head ──
        let rail_shell = |status: Div, body: AnyElement| -> Div {
            div()
                .flex_none()
                .w(px(248.))
                .flex()
                .flex_col()
                .min_h(px(0.))
                .overflow_hidden()
                .border_l(px(1.))
                .border_color(rgba(H0))
                .child(status)
                .child(body)
        };

        // ── rail-head:高 30 · 底 1px h0 · mono 10 t2 · 磷光点 ──
        let build_status = |summary: &str, add: Option<u32>, del: Option<u32>| -> Div {
            let mut s = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(12.))
                .flex_none()
                .border_b(px(1.))
                .border_color(rgba(H0))
                .font_family(mono.clone())
                .text_size(px(crate::style::FS_MICRO))
                .text_color(gpui::rgb(T2))
                .child(
                    div()
                        .w(px(5.))
                        .h(px(5.))
                        .rounded_full()
                        .flex_none()
                        .bg(gpui::rgb(PH)),
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
                            .font_weight(FontWeight(600.))
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
                } else {
                    s = s.child(div().text_color(gpui::rgb(T3)).child("git diff"));
                }
            } else {
                s = s.child(div().text_color(gpui::rgb(T3)).child("git diff"));
            }
            s
        };

        match &self.rail_state {
            // ── Loading: skeleton placeholders ──
            super::RailState::Loading => {
                let status = build_status("正在分析改动…", None, None);
                let skeleton = div()
                    .px(px(10.))
                    .pt(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children((0..3).map(|_| {
                        div()
                            .w_full()
                            .h(px(32.))
                            .rounded(px(R_CARD))
                            .bg(gpui::rgb(crate::style::L2))
                            .border_1()
                            .border_color(rgba(H0))
                    }));
                rail_shell(status, skeleton.into_any_element())
            }

            // ── Ready: real cards ──
            super::RailState::Ready { files, source } => {
                let total_add: u32 = files.iter().map(|f| f.add).sum();
                let total_del: u32 = files.iter().map(|f| f.del).sum();
                let summary = if files.is_empty() {
                    "工作区干净".to_string()
                } else {
                    format!("本次改动 · {}", files.len())
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
                            .font_family(mono.clone())
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(gpui::rgb(T3))
                            .pt(px(10.))
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
                    .gap(px(6.))
                    .px(px(10.))
                    .pt(px(8.))
                    .pb(px(10.))
                    .overflow_hidden();
                scrollable.interactivity().base_style.overflow.y = Some(Overflow::Scroll);

                for f in files.iter() {
                    let plus = format!("+{}", f.add);
                    let minus = (f.del > 0).then(|| format!("−{}", f.del));

                    let target = source.target_for(&f.path);
                    // `.dcard`:L2 + 1px h0 + r4;hover = L4 + h1(SHEET 02)
                    let row = div()
                        .w_full()
                        .rounded(px(R_CARD))
                        .py(px(7.))
                        .px(px(10.))
                        .bg(gpui::rgb(crate::style::L2))
                        .border_1()
                        .border_color(rgba(H0))
                        .hover(|s| s.bg(gpui::rgb(crate::style::L4)).border_color(rgba(H1)))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_this, _e, _w, cx| {
                                cx.emit(super::OpenInQuickLook(target.clone()));
                            }),
                        )
                        .child(self.arail_file(f.name(), &plus, minus.as_deref()));

                    scrollable = scrollable.child(row);
                }

                scrollable = scrollable.child(
                    div()
                        .font_family(mono.clone())
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(gpui::rgb(T3))
                        .child(SharedString::from("点击卡片 → QuickLook · Diff")),
                );

                rail_shell(status, scrollable.into_any_element())
            }

            // ── Idle: shouldn't render (called only when agent is present) ──
            super::RailState::Idle => div(),
        }
    }

    /// Per-pane header — agent header for agents, else a shell `.plate-head`(cwd + chip).
    /// `weak` = a handle to THIS pane, captured by the usage-pill click closure so
    /// it can cycle the display mode at event time. The caller (workspace) passes
    /// `pane.downgrade()` and renders via `read` — never `update` during render
    /// (that re-leases the pane mid-render → panic).
    pub(super) fn render_pane_header(&self, weak: WeakEntity<Self>) -> Option<Div> {
        match self.agent {
            Some(_) => Some(self.render_agent_header(weak)),
            // 幽灵窗 shell 会话:窗体自带 GHOST_ 头(SHEET 04 板 C),不再叠
            // 普通板头(差异总结 4-3)。agent 头保留 — 用量环不可丢。
            None if self.ghost_chrome => None,
            None => Some(self.render_shell_header()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_reset, short_chip, usage_token_chip_label};
    use std::time::{Duration, SystemTime};
    use tn_agent::AiUsage;
    use tn_config::BillingMode;

    #[test]
    fn fmt_reset_unknown_and_past_say_imminent() {
        assert_eq!(fmt_reset(None), "重置 ~即将");
        let past = SystemTime::now() - Duration::from_secs(3600);
        assert_eq!(fmt_reset(Some(past)), "重置 ~即将");
    }

    #[test]
    fn fmt_reset_future_formats_hours_and_minutes() {
        // +2h with a 30s cushion so time elapsed during the call can't drop a minute.
        let future = SystemTime::now() + Duration::from_secs(2 * 3600 + 30);
        assert_eq!(fmt_reset(Some(future)), "重置 ~2h00m 后");
    }

    #[test]
    fn short_chip_trims_and_passes_through_when_short() {
        assert_eq!(short_chip("  hi  ", 8), "hi");
        assert_eq!(short_chip("exact", 5), "exact");
    }

    #[test]
    fn short_chip_truncates_with_ellipsis_counting_chars() {
        // ASCII: keep max-1 chars + '…' = max chars total.
        assert_eq!(short_chip("hello world", 5), "hell…");
        // CJK counted per char, not per display column.
        assert_eq!(short_chip("你好世界一二三", 5), "你好世界…");
    }

    #[test]
    fn usage_token_chip_label_ignores_cost_and_context_modes() {
        let usage = AiUsage {
            model: "m".to_string(),
            input: 444_000,
            output: 731,
            cache_create: 0,
            cache_read: 0,
            context_used: 45_000,
            context_max: 100_000,
            cost_usd: 12.34,
            turns: 3,
        };

        assert_eq!(usage_token_chip_label(&usage, BillingMode::Api), "444K TOK");
        assert_eq!(
            usage_token_chip_label(&usage, BillingMode::Subscription),
            "444K TOK"
        );
        assert_eq!(
            usage_token_chip_label(&usage, BillingMode::Tokens),
            "444K TOK"
        );
        assert_eq!(usage_token_chip_label(&usage, BillingMode::Auto), "444K TOK");
    }
}
