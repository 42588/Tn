//! Runnable gpui replica of the **overlay / state panels** that the full-window
//! `mockup_replica.rs` doesn't isolate: command palette, Quick Terminal launcher,
//! welcome/empty state, the Warp block card (3 states) and the status bar.
//!
//! Together with `mockup_replica.rs` (the composed window: titlebar / explorer /
//! agent / shell / viewer / status) this covers every interface in the catalog
//! (`docs/UI-CATALOG.md`). Same discipline: every value inlined from the design
//! prototypes (`design/panels/*.html` + `design/calm-glass.css`, which mirror
//! `docs/CSS_TO_GPUI.md` §16) so the replica tests the *documented mapping*.
//!
//! Run:  cargo run -p tn-ui --example panels_replica
//!
//! Standalone (examples can't reach tn-ui's `pub(crate)` helpers), so the small
//! col/cola/shadow/icon scaffolding is inlined, mirroring `mockup_replica.rs`.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{
    div, linear_color_stop, linear_gradient, point, px, relative, rgb, rgba, size, svg, App,
    AppContext, Application, Bounds, BoxShadow, Context, Div, FontWeight, Hsla, IntoElement,
    ParentElement, Render, Rgba, SharedString, Styled, Svg, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

// ── tokens (CSS_TO_GPUI.md §16 / design/calm-glass.css :root) ──
const FG: u32 = 0xC6D0F5;
const FG_DIM: u32 = 0xA6AFD4;
const MUTED: u32 = 0x6E76A0;
const FAINT: u32 = 0x474E72;
const ACCENT: u32 = 0x7AA2F7;
const VIOLET: u32 = 0xBB9AF7;
const GREEN: u32 = 0x9ECE6A;
const RED: u32 = 0xF7768E;
const CLAUDE: u32 = 0xF0916D;
const CODEX: u32 = 0x73DACA;

const INSET: u32 = 0xffffff0a; // --g2 white .04
const HOVER: u32 = 0xffffff0f; // --g3 white .06
const RIM: u32 = 0xffffff12; // --rim white .07

const R_PANEL: f32 = 14.0;
const R_CARD: f32 = 11.0;
const UI_SANS: &str = "Segoe UI";
const MONO: &str = "Cascadia Code";

fn col(hex: u32) -> Rgba {
    rgb(hex)
}
fn cola(hex: u32, a: f32) -> Rgba {
    let r = ((hex >> 16) & 0xff) as f32 / 255.0;
    let g = ((hex >> 8) & 0xff) as f32 / 255.0;
    let b = (hex & 0xff) as f32 / 255.0;
    Rgba { r, g, b, a }
}
fn soft_shadow(y: f32, blur: f32, spread: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: Hsla::from(Rgba { r: 0., g: 0., b: 0., a: alpha }),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}
fn shadowed(mut d: Div, shadows: Vec<BoxShadow>) -> Div {
    d.style().box_shadow = Some(shadows);
    d
}
fn icon(name: &str, sz: f32, color: u32) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .w(px(sz))
        .h(px(sz))
        .flex_none()
        .text_color(col(color))
}

// ── inline AssetSource (subset of crates/tn-ui/src/assets.rs icons) ──
const ICON_BODIES: &[(&str, &str)] = &[
    ("term", r#"<path d="M5 7.5l4.5 4.5L5 16.5"/><path d="M12.5 16.5h6.5"/>"#),
    ("spark", r#"<path d="M12 3.4c.42 3.9 1.9 5.38 5.8 5.8-3.9.42-5.38 1.9-5.8 5.8-.42-3.9-1.9-5.38-5.8-5.8 3.9-.42 5.38-1.9 5.8-5.8z"/><path d="M18.5 15.5c.2 1.5.8 2.1 2.3 2.3-1.5.2-2.1.8-2.3 2.3-.2-1.5-.8-2.1-2.3-2.3 1.5-.2 2.1-.8 2.3-2.3z"/>"#),
    ("check", r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#),
    ("diamond", r#"<path d="M12 3.5l8.5 8.5-8.5 8.5L3.5 12z"/>"#),
    ("close", r#"<path d="M7 7l10 10M17 7L7 17"/>"#),
    ("branch", r#"<circle cx="7" cy="6.5" r="2.3"/><circle cx="7" cy="17.5" r="2.3"/><circle cx="17" cy="8.5" r="2.3"/><path d="M7 8.8v6.4"/><path d="M17 10.8c0 4.2-4.2 3.2-7 4.6"/>"#),
];
fn icon_svg(body: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
    )
}
struct PanelAssets;
impl gpui::AssetSource for PanelAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(name) = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg")) {
            if let Some((_, body)) = ICON_BODIES.iter().find(|(n, _)| *n == name) {
                return Ok(Some(Cow::Owned(icon_svg(body).into_bytes())));
            }
        }
        Ok(None)
    }
    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ICON_BODIES.iter().map(|(n, _)| SharedString::from(format!("icons/{n}.svg"))).collect())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
struct PanelsView;

impl Render for PanelsView {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(18.))
            .p(px(28.))
            .bg(col(0x0A0A12))
            .font_family(UI_SANS)
            .text_color(col(FG))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight(720.))
                    .child("Tn 浮层 / 状态屏 — gpui 还原(对照 design/panels/04·05)"),
            )
            // two columns of demo cards
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(18.))
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(18.))
                            .flex_1()
                            .child(demo("命令面板 · workspace::render_palette", command_palette()))
                            .child(demo("Quick 启动器 · quick_terminal", quick_launcher())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(18.))
                            .flex_1()
                            .child(demo("欢迎 / 空状态 · 后置", welcome()))
                            .child(demo("Block 卡三态 · block_view", block_cards()))
                            .child(demo("状态栏 · render_status_bar", status_bar())),
                    ),
            )
    }
}

/// A labeled demo cell on the desktop canvas.
fn demo(label: &str, body: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight(640.))
                .text_color(col(MUTED))
                .child(SharedString::from(label.to_string())),
        )
        .child(body)
}

/// A frosted pane shell (g1 + rim + specular + shadow) — mirrors style::specular_top.
fn pane() -> Div {
    let p = div()
        .relative()
        .flex()
        .flex_col()
        .rounded(px(R_PANEL))
        .overflow_hidden()
        .border_1()
        .border_color(rgba(RIM))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x2a2e446b), 0.),
            linear_color_stop(rgba(0x1a1c2c85), 1.),
        ))
        .child(
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .top(px(0.))
                .h(relative(0.36))
                .rounded(px(R_PANEL))
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(rgba(0xffffff0a), 0.),
                    linear_color_stop(rgba(0x00000000), 1.),
                )),
        );
    shadowed(p, vec![soft_shadow(24., 58., -36., 0.88)])
}

// ── command palette ──
fn command_palette() -> Div {
    let row = |dot: u32, label: &str, meta: &str, sel: bool| {
        let mut r = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .py(px(9.))
            .px(px(12.))
            .rounded(px(9.))
            .text_size(px(13.))
            .text_color(col(if sel { FG } else { FG_DIM }));
        if sel {
            r = r.bg(rgba(HOVER));
        }
        r.child(div().w(px(7.)).h(px(7.)).rounded_full().bg(col(dot)))
            .child(SharedString::from(label.to_string()))
            .child(div().flex_1())
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .text_color(col(FAINT))
                    .child(SharedString::from(meta.to_string())),
            )
    };
    div()
        .w_full()
        .rounded(px(R_PANEL))
        .overflow_hidden()
        .border_1()
        .border_color(rgba(RIM))
        .bg(linear_gradient(
            180.,
            linear_color_stop(cola(0x1F2335, 0.92), 0.),
            linear_color_stop(cola(0x161826, 0.92), 1.),
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .py(px(13.))
                .px(px(16.))
                .border_b_1()
                .border_color(rgba(0xffffff10))
                .text_size(px(14.))
                .child(icon("term", 16., MUTED))
                .child(div().text_color(col(FG)).child("cla"))
                .child(div().text_color(col(MUTED)).child("|")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .p(px(6.))
                .child(row(CLAUDE, "Start Claude Code here", "~/proj/tn", true))
                .child(row(CODEX, "Start Codex here", "~/proj/tn", false))
                .child(row(ACCENT, "New pwsh", "profile", false))
                .child(row(VIOLET, "WSL · Ubuntu", "wsl -d Ubuntu", false)),
        )
}

// ── quick terminal launcher tiles ──
fn launch_tile(ic_bg: u32, ic_fg: u32, name: &str, desc: &str, sel: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(9.))
        .p(px(14.))
        .flex_1()
        .rounded(px(R_CARD))
        .border_1()
        .border_color(if sel { cola(CLAUDE, 0.4) } else { rgba(RIM) })
        .bg(rgba(if sel { HOVER } else { INSET }))
        .child(
            div()
                .w(px(30.))
                .h(px(30.))
                .rounded(px(9.))
                .flex()
                .items_center()
                .justify_center()
                .bg(cola(ic_bg, 0.14))
                .child(icon("spark", 18., ic_fg)),
        )
        .child(div().text_size(px(13.)).font_weight(FontWeight(640.)).text_color(col(FG)).child(SharedString::from(name.to_string())))
        .child(div().text_size(px(11.)).text_color(col(MUTED)).child(SharedString::from(desc.to_string())))
}
fn quick_launcher() -> Div {
    pane().child(
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(20.))
            .child(div().text_size(px(13.)).font_weight(FontWeight(640.)).text_color(col(FG_DIM)).child("起一个会话 — Quick Terminal"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(11.))
                    .child(launch_tile(CLAUDE, CLAUDE, "Claude", "~/proj/tn", true))
                    .child(launch_tile(CODEX, CODEX, "Codex", "最近目录", false))
                    .child(launch_tile(ACCENT, ACCENT, "pwsh", "PowerShell", false))
                    .child(launch_tile(VIOLET, VIOLET, "WSL", "Ubuntu", false)),
            ),
    )
}

// ── welcome / empty state ──
fn welcome() -> Div {
    pane().child(
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .py(px(34.))
            .px(px(24.))
            .child(
                div()
                    .w(px(52.))
                    .h(px(52.))
                    .rounded(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(linear_gradient(
                        145.,
                        linear_color_stop(col(ACCENT), 0.),
                        linear_color_stop(col(VIOLET), 1.),
                    ))
                    .child(icon("term", 28., 0x0B0D16)),
            )
            .child(div().text_size(px(20.)).font_weight(FontWeight(720.)).child("开一个新会话"))
            .child(div().text_size(px(13.)).text_color(col(FG_DIM)).child("托管 AI 编码 CLI,或起一个本地/WSL shell"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(11.))
                    .child(launch_tile(CLAUDE, CLAUDE, "Claude", "Claude Code", false))
                    .child(launch_tile(CODEX, CODEX, "Codex", "OpenAI Codex", false))
                    .child(launch_tile(ACCENT, ACCENT, "pwsh", "PowerShell", false)),
            ),
    )
}

// ── block cards (3 states) ──
fn block_card(stripe: u32, cmd_fn: &str, cmd_tail: &str, dur: &str, chip: Div) -> Div {
    div()
        .relative()
        .rounded(px(R_CARD))
        .overflow_hidden()
        .bg(rgba(0xffffff09))
        .child(div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(px(3.)).bg(col(stripe)))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(9.))
                .pt(px(8.))
                .pb(px(8.))
                .pl(px(14.))
                .pr(px(12.))
                .font_family(MONO)
                .text_size(px(12.))
                .text_color(col(FG))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(5.))
                        .child("❯")
                        .child(div().text_color(col(ACCENT)).child(SharedString::from(cmd_fn.to_string())))
                        .child(SharedString::from(cmd_tail.to_string())),
                )
                .child(div().flex_1())
                .child(div().text_size(px(10.5)).font_weight(FontWeight(640.)).text_color(col(MUTED)).child(SharedString::from(dur.to_string())))
                .child(chip),
        )
}
fn exit_chip(color: u32, glyph: &str, text: &str) -> Div {
    let mut c = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.))
        .text_size(px(10.))
        .font_weight(FontWeight(680.))
        .text_color(col(color))
        .child(icon(glyph, 11., color));
    if !text.is_empty() {
        c = c.child(SharedString::from(text.to_string()));
    }
    c
}
fn block_cards() -> Div {
    pane().child(
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(14.))
            .child(block_card(GREEN, "cargo", "test -p tn-core", "0.8s", exit_chip(GREEN, "check", "")))
            .child(block_card(ACCENT, "cargo", "build --workspace", "12.4s", exit_chip(ACCENT, "diamond", "运行中")))
            .child(block_card(RED, "cargo", "clippy", "3.1s", exit_chip(RED, "close", "exit 101"))),
    )
}

// ── status bar ──
fn seg(divider: bool, build: impl FnOnce(Div) -> Div) -> Div {
    let mut s = div().flex().flex_row().items_center().gap(px(6.)).px(px(13.)).h(px(18.));
    if divider {
        s = s.border_l(px(1.)).border_color(rgba(0xffffff0f));
    }
    build(s)
}
fn num(text: &str) -> Div {
    div().font_family(MONO).font_weight(FontWeight(640.)).text_color(col(FG_DIM)).child(SharedString::from(text.to_string()))
}
fn status_bar() -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(30.))
        .px(px(6.))
        .rounded(px(R_CARD))
        .overflow_hidden()
        .text_size(px(11.))
        .font_weight(FontWeight(510.))
        .text_color(col(MUTED))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x141620ff), 0.),
            linear_color_stop(rgba(0x0c0d15ff), 1.),
        ))
        .child(seg(false, |d| d.child(icon("branch", 13., ACCENT)).child("main")))
        .child(seg(true, |d| d.child(num("3")).child("sessions")))
        .child(seg(true, |d| d.child(icon("spark", 13., CLAUDE)).child("ctx").child(num("42%"))))
        .child(seg(true, |d| d.child(icon("spark", 13., CODEX)).child("ctx").child(num("18%"))))
        .child(div().flex_1())
        .child(seg(false, |d| d.child("element.rs · Rust")))
        .child(seg(true, |d| d.child("UTF-8")))
        .child(seg(true, |d| d.text_color(col(ACCENT)).child("Tn Dark")))
}

// ─────────────────────────────────────────────────────────────────────────────
pub fn run() {
    Application::new().with_assets(PanelAssets).run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1180.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Tn — panels replica".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_background: WindowBackgroundAppearance::Opaque,
                ..Default::default()
            },
            |_w, cx| cx.new(|_cx| PanelsView),
        )
        .expect("failed to open panels window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

#[allow(dead_code)]
fn main() {
    run();
}
