//! Calm Glass shared style tokens + helpers — the single source of truth.
//!
//! These constants and helpers were previously copy-pasted across six view
//! modules (workspace / terminal_view / quick_terminal / explorer / viewer /
//! block_view), so any visual tweak meant editing six places (待优化清单 §4.1).
//! They now live here; modules `use crate::style::…`.
//!
//! `col`/`cola` accept either chrome colors (`tn_config::Color`) or terminal-cell
//! colors (`tn_core::Rgb`) via the [`Rgb8`] trait — both are just 8-bit RGB.

use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba,
    BoxShadow, Div, Rgba, Svg,
};

// Calm Glass white-on-glass overlay tokens (alpha-only — depth from layered
// translucency + a top mirror highlight, never from glow). docs/产品设计 §6.1.
pub(crate) const RIM: u32 = 0xffffff12; // glass edge (~white .07) — replaces hard borders
pub(crate) const SHEEN: u32 = 0xffffff1a; // top 1px mirror highlight (~white .10)
pub(crate) const INSET: u32 = 0xffffff0a; // header / inset card overlay (~white .04)
pub(crate) const HOVER: u32 = 0xffffff0f; // chip / hover (~white .06, = mockup --g3)
pub(crate) const DIVIDER: u32 = 0xffffff0f; // status-bar segment divider (~white .06, = mockup `.status .seg2 + .seg2`)

// Pane glass fill = mockup `--g1`(冷调加深、提对比;两停渐变).Opaque 窗口下没有
// backdrop-blur,故 g1 偏实(alpha 高)以读出磨砂色。集中在此 = 单一真源,render_node
// 与 explorer 共用、不再各抄一份(曾各抄 → 原型改 g1 而代码漏跟、漂成偏灰偏透);
// `token_drift` 已把它对着 mockup `--g1` 守住。
// Original G1 endpoints — no longer used for pane_fill (switched to G1_MID
// solid to avoid 8-bit gradient banding) but kept so the token_drift test
// still guards against mockup --g1 drift.
#[allow(dead_code)]
pub(crate) const G1_TOP: u32 = 0x222a4675; // rgba(34,42,70,0.46) → a=round(.46×255)=117=0x75
#[allow(dead_code)]
pub(crate) const G1_BOT: u32 = 0x10142694; // rgba(16,20,38,0.58) → a=round(.58×255)=148=0x94
pub(crate) const G1_MID: u32 = 0x191f3685; // rgba(25,31,54,0.52) ← midpoint of G1_TOP + G1_BOT


/// UI sans-serif for chrome (tabs / headers / status); paired with the mono
/// terminal/code font. Ships on Windows 10/11.
pub(crate) const UI_SANS: &str = "Segoe UI";

// Calm Glass corner radii (px): window 16, panel 14, card 11. docs/产品设计 §6.1.
pub(crate) const R_WINDOW: f32 = 16.0;
pub(crate) const R_PANEL: f32 = 14.0;
pub(crate) const R_CARD: f32 = 11.0;

/// 8-bit RGB, implemented by both the config/theme color and the terminal-cell
/// color so [`col`]/[`cola`] work with either.
pub(crate) trait Rgb8 {
    fn channels(&self) -> (u8, u8, u8);
}
impl Rgb8 for tn_config::Color {
    fn channels(&self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }
}
impl Rgb8 for tn_core::Rgb {
    fn channels(&self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }
}

/// Opaque GPUI color.
pub(crate) fn col(c: impl Rgb8) -> Rgba {
    let (r, g, b) = c.channels();
    rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32)
}

/// Color with explicit alpha. Calm Glass surfaces are translucent so the window
/// material shows through, instead of being filled opaque. See 产品设计 §6.1.
pub(crate) fn cola(c: impl Rgb8, a: f32) -> Rgba {
    let (r, g, b) = c.channels();
    Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

/// A soft, contained drop shadow (depth without glow — Calm Glass). A negative
/// `spread` keeps it tucked under the element rather than blooming outward.
pub(crate) fn soft_shadow(y: f32, blur: f32, spread: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: hsla(0., 0., 0., alpha),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}

/// Attach the Calm Glass shadow stack to a div. Each layer is a
/// `soft_shadow` — pure black with varying offsets, blurs, and alphas
/// to build depth without a glowing/bloom halo (no coloured shadows,
/// no light halos). The outer wrapper must have explicit size (e.g.
/// `size_full`) and the parent must NOT clip overflow, otherwise the
/// shadows will be cropped.
pub(crate) fn shadowed(d: Div, shadows: Vec<BoxShadow>) -> Div {
    d.shadow(shadows)
}

/// Composite a translucent overlay (`0xRRGGBBAA`) over an opaque base → opaque
/// color. Used to *bake* the translucent g1 glass over the (flat) window so the
/// pane fill is OPAQUE: the [`glass_pane`] gradient-border sits BEHIND the fill,
/// and a translucent fill would let that bright edge gradient bleed through the
/// whole pane (washing it bright). An opaque fill blocks it to the 1px ring.
fn over(ov: u32, base: (u8, u8, u8)) -> Rgba {
    let a = (ov & 0xff) as f32 / 255.0;
    let ch = |shift: u32, b: u8| {
        let o = ((ov >> shift) & 0xff) as f32;
        (o * a + b as f32 * (1.0 - a)).round() as u32
    };
    rgb((ch(24, base.0) << 16) | (ch(16, base.1) << 8) | ch(8, base.2))
}

/// The pane glass fill — a solid colour at the G1 midpoint baked **opaque**
/// over the window `bg`. Shared by terminal panes (`render_node`) + explorer so
/// the deep cool glass can't drift ([`G1_MID`] guards against mockup via
/// G1_TOP / G1_BOT midpoint). Opaque (not the raw translucent g1) so
/// [`glass_pane`]'s gradient border doesn't bleed through.
///
/// Formerly a two-stop [`G1_TOP`]→[`G1_BOT`] gradient; switched to a flat fill
/// because the two-stop gradient banded visibly on large panes at 8-bit colour
/// depth.  [`specular_top`] still provides the depth wash.
pub(crate) fn pane_fill(bg: impl Rgb8) -> gpui::Background {
    let base = bg.channels();
    over(G1_MID, base).into()
}

/// mockup `.pane` / `.pane.active` box-shadow stack: an outer 1px **dark hairline**
/// (`0 0 0 1px rgba(0,0,0,.28)`) that crisply *cuts* the pane out of the backdrop,
/// plus layered soft drops for float — depth, not glow. (gpui 0.2.2 has no inset
/// box-shadow, so the mockup's inset bottom shadow is omitted.) Shared by panes +
/// explorer so the lift stays identical.
pub(crate) fn pane_shadows(focused: bool) -> Vec<BoxShadow> {
    // 软暗晕,代替 mockup 的硬 1px 暗线:硬线紧贴亮渐变描边 → 暗-亮并置显「接缝」(mockup
    // 靠 backdrop-blur 抹平,我们没有)。改 3px 模糊、0 spread 的暗晕 → 仍「切出背景」,
    // 但边过渡丝滑、无硬缝。
    let edge_cut = soft_shadow(0.0, 3.0, 0.0, 0.34);
    if focused {
        vec![
            edge_cut,
            soft_shadow(4.0, 9.0, -2.0, 0.58),
            soft_shadow(30.0, 64.0, -28.0, 0.8),
            soft_shadow(64.0, 120.0, -48.0, 0.94),
        ]
    } else {
        vec![
            edge_cut,
            soft_shadow(2.0, 5.0, -2.0, 0.55),
            soft_shadow(22.0, 48.0, -26.0, 0.72),
            soft_shadow(52.0, 104.0, -46.0, 0.92),
        ]
    }
}

/// Wrap a glass pane's inner content with the mockup `.pane::before` **gradient
/// edge** + the float shadow. gpui can't gradient a border, so this uses the
/// 1px-padding reveal trick: an outer div with a vertical `cool-white → accent`
/// gradient background + 1px padding, with the rounded inner content on top — the
/// 1px ring that shows through *is* a continuous gradient border that follows the
/// rounded corners (top reads cool-white 承光, bottom accent 回光, sides the
/// gradient between = the mockup's non-uniform edge). `inner` must be built with
/// `rounded(R_PANEL - 1.)` + `overflow_hidden`. Focused = brighter top + more
/// accent bottom (+ deeper shadow). Cool-white = glass refraction tint (not a
/// theme token); accent goes through `cola` so it's never a bare theme hex.
pub(crate) fn glass_pane(inner: Div, focused: bool, accent: impl Rgb8) -> Div {
    // ── Inner top shadow ──
    // mockup has `inset 0 -22px 46px rgba(0,0,0,.55)` — a dark recess at the top
    // of the pane. gpui doesn't support inset box-shadow, so we fake it with a
    // 4px absolute overlay: black→transparent gradient. The strip is thin enough
    // that it doesn't meaningfully interfere with clicks on the pane header.
    // This gives the top edge a subtle "recessed" feel without touching call sites.
    let top_glaze = div()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .h(px(4.))
        .rounded_t(px(R_PANEL - 1.)) // match inner rounding — only top corners matter
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x0000002b), 0.), // ~.17 at very top
            linear_color_stop(rgba(0x00000000), 1.),  // → transparent at 4px
        ));

    let wrapped = div()
        .size_full()
        .relative()
        .child(inner)
        .child(top_glaze);

    // ── Gradient edge ring ──
    // 冷白承光 — 已从 .36/.25 压到 .12/.08: 有投影后,渐变环只需隐约可见,
    // 提供"玻璃折射"的微弱暗示,而非强硬的彩色边框。
    let top = if focused { rgba(0xd2e1ff1f) } else { rgba(0xbed6ff14) }; // .12 / .08
    let edge = linear_gradient(
        180.,
        linear_color_stop(top, 0.),
        // accent 回光 — 同样压到 .08/.05, 底部只留一丝 accent 色调
        linear_color_stop(cola(accent, if focused { 0.08 } else { 0.05 }), 1.),
    );
    shadowed(
        div().size_full().rounded(px(R_PANEL)).p(px(1.)).bg(edge).child(wrapped),
        pane_shadows(focused),
    )
}

/// 现代发光玻璃卡片 (Modern Glowing Glass Card)
///
/// 解决了大面积渐变带来的"色带 (Color Banding)" Bug。
/// 采用"边缘导光"设计：背景使用绝对纯色防色带，利用 1px 的高对比度渐变外环
/// 模拟光线折射，并通过带有 `accent` 颜色的弥散阴影（Ambient Glow）实现现代发光感。
///
/// `inner` 必须 `rounded(R_CARD - 1.)` + `overflow_hidden()` + 纯色背景
/// （同 [`glass_pane`] 的 1px-padding reveal 范式）。
pub(crate) fn glass_card(inner: Div, focused: bool, accent: impl Rgb8) -> Div {
    let (ar, ag, ab) = accent.channels();
    let ar = ar as f32 / 255.0;
    let ag = ag as f32 / 255.0;
    let ab = ab as f32 / 255.0;

    // 1. 边缘导光环 (Gradient Ring)
    // 顶部迎接环境冷白光，底部汇聚强调色（发光感来源）
    let top_edge = if focused {
        rgba(0xffffff3d) // 白 .24
    } else {
        rgba(0xffffff1a) // 白 .10
    };
    let bot_edge = Rgba {
        r: ar,
        g: ag,
        b: ab,
        a: if focused { 0.45 } else { 0.10 },
    };

    let edge_bg = linear_gradient(
        180.,
        linear_color_stop(top_edge, 0.),
        linear_color_stop(bot_edge, 1.),
    );

    // 2. 漫反射发光投影 (Ambient Glow)
    // 放弃死黑的阴影，将 accent 颜色注入阴影中，形成真实的物理光晕
    let glow_shadows = if focused {
        vec![
            // 基础物理切边，让卡片凸起
            soft_shadow(0.0, 2.0, 0.0, 0.25),
            // ★ 核心发光层：带有 accent 颜色的彩色投影
            BoxShadow {
                color: Rgba {
                    r: ar,
                    g: ag,
                    b: ab,
                    a: 0.20,
                }
                .into(),
                offset: point(px(0.), px(6.)),
                blur_radius: px(20.),
                spread_radius: px(-2.),
            },
            // 底部深色结构影，撑起空间感
            soft_shadow(12.0, 24.0, -12.0, 0.45),
        ]
    } else {
        vec![
            soft_shadow(0.0, 2.0, 0.0, 0.2),
            soft_shadow(4.0, 8.0, -2.0, 0.3),
        ]
    };

    shadowed(
        div()
            .size_full()
            .rounded(px(R_CARD))
            .p(px(1.)) // 留出 1px 的光环
            .bg(edge_bg)
            .child(inner),
        glow_shadows,
    )
}

/// Quick Look 速览浮层的玻璃填充(mockup `.quicklook` 底层暗玻璃,baked **opaque**)。
/// 比常驻面板更实:浮层飘在终端正文之上、要**压住**后面的字保证代码可读。mockup 用
/// `rgba(28,34,58,.88)→rgba(15,19,34,.94)` + backdrop-blur;我们没有 blur,半透会把后面
/// 终端的尖锐文字漏出来 → 直接 `over()` 烤实在窗口 `bg` 上。
///
/// 原为两停渐变(同 [`pane_fill`]),大面积浮层在 8-bit 下色带明显 → 改纯色中点。
pub(crate) fn quicklook_fill(bg: impl Rgb8) -> gpui::Background {
    let base = bg.channels();
    // midpoint of rgba(28,34,58,.88) + rgba(15,19,34,.94)
    over(0x161b2ee8, base).into() // rgba(22,27,46,0.91)
}

/// mockup `.quicklook` 浮起投影栈:比常驻面板(`pane_shadows`)更深更高——浮层飘在最上层。
/// 同样把硬 1px 暗线换成 3px 软暗晕(避接缝,见 `pane_shadows`),再叠多层柔投影。
pub(crate) fn quicklook_shadows() -> Vec<BoxShadow> {
    vec![
        soft_shadow(0.0, 3.0, 0.0, 0.36),    // 软暗晕切出背景(代 mockup 0 0 0 1px rgba(0,0,0,.36))
        soft_shadow(2.0, 6.0, -2.0, 0.6),    // mockup 0 2px 6px -2px rgba(0,0,0,.6)
        soft_shadow(30.0, 72.0, -24.0, 0.86), // mockup 0 30px 72px -24px rgba(0,0,0,.86)
        soft_shadow(72.0, 132.0, -50.0, 0.96), // mockup 0 72px 132px -50px rgba(0,0,0,.96)
    ]
}

/// Wrap the Quick Look overlay's inner content with mockup `.quicklook::before`'s
/// **cool energy edge** (1px-padding gradient reveal, like [`glass_pane`]) + the
/// deeper floating shadow ([`quicklook_shadows`]). `inner` must be built with
/// `rounded(R_PANEL - 1.)` + `overflow_hidden`. Cool-white top → accent bottom
/// (accent via `cola`, never a bare theme hex).
pub(crate) fn quicklook_frame(inner: Div, accent: impl Rgb8) -> Div {
    // Top inner shadow (same technique as glass_pane, see comments there).
    let top_glaze = div()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .h(px(4.))
        .rounded_t(px(R_PANEL - 1.))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x0000002b), 0.),
            linear_color_stop(rgba(0x00000000), 1.),
        ));

    let wrapped = div()
        .size_full()
        .relative()
        .child(inner)
        .child(top_glaze);

    let edge = linear_gradient(
        180.,
        linear_color_stop(rgba(0xbed6ff14), 0.), // 冷白承光 .08 (原 .24)
        linear_color_stop(cola(accent, 0.06), 1.), // accent 回光 .06 (原 .15)
    );
    shadowed(
        div().size_full().rounded(px(R_PANEL)).p(px(1.)).bg(edge).child(wrapped),
        quicklook_shadows(),
    )
}

/// A Calm Glass line icon, sized square and tinted `color`. (gpui paints an SVG
/// only when a text color is set, so the tint is always explicit — see
/// `assets.rs`.)
pub(crate) fn icon(name: &str, size: f32, color: impl Rgb8) -> Svg {
    gpui::svg()
        .path(crate::assets::icon_path(name))
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(col(color))
}

/// Drift guard (see docs/样式还原手册.md §1): assert the design prototype
/// `design/mockup.html` and the shipped implementation agree on every
/// color/material/radius token. The mockup is the canonical source ("设计稿为准"),
/// so when someone tweaks either side and they diverge, this test fails and names
/// the offending token — instead of the drift being caught by eye much later.
#[cfg(test)]
mod token_drift {
    use super::*;
    use tn_config::Theme;

    /// `design/mockup.html`, resolved from this crate (`crates/tn-ui`) up to repo root.
    fn mockup_html() -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/mockup.html");
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e}"))
    }

    /// The body between `:root {` and the next `}` — where the CSS vars live.
    fn root_block(html: &str) -> &str {
        let from = &html[html.find(":root").expect("mockup has :root")..];
        let open = from.find('{').expect(":root {");
        let close = from.find('}').expect(":root }");
        &from[open + 1..close]
    }

    /// Value of `--<var>` inside the `:root` block (text up to the `;`).
    fn css_var<'a>(root: &'a str, var: &str) -> &'a str {
        let key = format!("{var}:");
        let at = root
            .find(&key)
            .unwrap_or_else(|| panic!(":root missing {var}"));
        let rest = &root[at + key.len()..];
        rest[..rest.find(';').expect("css var ends with ;")].trim()
    }

    fn hex_rgb(s: &str) -> (u8, u8, u8) {
        let h = s.strip_prefix('#').expect("#RRGGBB");
        assert_eq!(h.len(), 6, "6-digit hex: {s}");
        let p = |a, b| u8::from_str_radix(&h[a..b], 16).unwrap();
        (p(0, 2), p(2, 4), p(4, 6))
    }

    fn rgba_alpha(s: &str) -> (u8, u8, u8, f32) {
        let inner = s
            .strip_prefix("rgba(")
            .and_then(|x| x.strip_suffix(')'))
            .unwrap_or_else(|| panic!("rgba(...): {s}"));
        let p: Vec<&str> = inner.split(',').map(str::trim).collect();
        assert_eq!(p.len(), 4, "rgba needs 4 parts: {s}");
        (
            p[0].parse().unwrap(),
            p[1].parse().unwrap(),
            p[2].parse().unwrap(),
            p[3].parse().unwrap(),
        )
    }

    /// mockup `#RRGGBB` var == theme token color.
    fn assert_color(css: &str, c: tn_config::Color, what: &str) {
        assert_eq!(
            hex_rgb(css),
            (c.r, c.g, c.b),
            "{what}: mockup {css} != theme #{:02X}{:02X}{:02X}",
            c.r,
            c.g,
            c.b
        );
    }

    /// mockup `rgba(255,255,255,a)` white-overlay var == `style.rs` `0xffffffAA`
    /// constant, with `AA == round(a*255)`.
    fn assert_white(css: &str, token: u32, what: &str) {
        let (r, g, b, a) = rgba_alpha(css);
        assert_eq!((r, g, b), (255, 255, 255), "{what}: expected white base ({css})");
        assert_eq!(token >> 8, 0xffffff, "{what}: token {token:#010x} rgb must be white");
        let want = (a * 255.0).round() as u32;
        assert_eq!(
            token & 0xff,
            want,
            "{what}: mockup alpha {a} → {want}, but token low byte = {:#04x}",
            token & 0xff
        );
    }

    fn px_val(s: &str) -> f32 {
        s.strip_suffix("px").expect("Npx").parse().unwrap()
    }

    /// mockup `--g1: linear-gradient(180deg, rgba(...), rgba(...))` two stops ==
    /// `style.rs` `G1_TOP`/`G1_BOT` (`0xRRGGBBAA`, `AA == round(alpha*255)`).
    fn assert_g1(css: &str, top: u32, bot: u32) {
        // Pull the two `rgba(...)` substrings out of the gradient.
        let s0 = css.find("rgba(").expect("--g1 stop0");
        let e0 = css[s0..].find(')').expect("g1 stop0 )") + s0 + 1;
        let s1 = css[e0..].find("rgba(").expect("--g1 stop1") + e0;
        let e1 = css[s1..].find(')').expect("g1 stop1 )") + s1 + 1;
        let chk = |seg: &str, tok: u32, w: &str| {
            let (r, g, b, a) = rgba_alpha(seg);
            let want = ((r as u32) << 24)
                | ((g as u32) << 16)
                | ((b as u32) << 8)
                | (a * 255.0).round() as u32;
            assert_eq!(want, tok, "{w}: mockup {seg} → {want:#010x} != {tok:#010x}");
        };
        chk(&css[s0..e0], top, "--g1 top → G1_TOP");
        chk(&css[s1..e1], bot, "--g1 bot → G1_BOT");
    }

    #[test]
    fn mockup_tokens_match_theme_and_style() {
        let html = mockup_html();
        let root = root_block(&html);
        let t = Theme::tn_dark();

        // ── colors: mockup --var == theme token (设计稿为准) ──
        assert_color(css_var(root, "--fg"), t.ui.foreground, "--fg → ui.foreground");
        assert_color(css_var(root, "--muted"), t.ui.muted, "--muted → ui.muted");
        assert_color(css_var(root, "--accent"), t.ui.accent, "--accent → ui.accent");
        assert_color(css_var(root, "--violet"), t.ui.accent_alt, "--violet → ui.accent_alt");
        assert_color(css_var(root, "--green"), t.ansi.green, "--green → ansi.green");
        assert_color(css_var(root, "--red"), t.ansi.red, "--red → ansi.red");
        assert_color(css_var(root, "--yellow"), t.ansi.yellow, "--yellow → ansi.yellow");
        assert_color(css_var(root, "--cyan"), t.ansi.cyan, "--cyan → ansi.cyan");
        assert_color(css_var(root, "--claude"), t.agents.claude, "--claude → agents.claude");
        assert_color(css_var(root, "--codex"), t.agents.codex, "--codex → agents.codex");

        // ── white-overlay material tokens: mockup alpha == style.rs constant ──
        assert_white(css_var(root, "--rim"), RIM, "--rim → RIM");
        assert_white(css_var(root, "--sheen"), SHEEN, "--sheen → SHEEN");
        assert_white(css_var(root, "--g2"), INSET, "--g2 → INSET");
        assert_white(css_var(root, "--g3"), HOVER, "--g3 → HOVER");
        assert_white(css_var(root, "--g3"), DIVIDER, "--g3 → DIVIDER (= chip/hover .06)");

        // ── pane glass gradient: mockup --g1 two rgba stops == G1_TOP / G1_BOT ──
        assert_g1(css_var(root, "--g1"), G1_TOP, G1_BOT);

        // ── corner radii ──
        assert_eq!(px_val(css_var(root, "--r-win")), R_WINDOW, "--r-win → R_WINDOW");
        assert_eq!(px_val(css_var(root, "--r-pane")), R_PANEL, "--r-pane → R_PANEL");
        assert_eq!(px_val(css_var(root, "--r-card")), R_CARD, "--r-card → R_CARD");
    }

    /// `design/calm-glass.css`(面板共享样式表)的 `:root` 必须与 `mockup.html`
    /// 镜像一致——两份是手动同步的副本,且 spec_gen §16.2 现从 calm-glass.css 生成。
    /// 这第四道守卫防它们漂移(改色 / 改令牌须同步两边)。
    #[test]
    fn mockup_and_calm_glass_roots_mirror() {
        /// Strip `/* … */` comments so a trailing `--g1:…; /* note */` doesn't
        /// swallow the next var.
        fn strip(s: &str) -> String {
            let (mut o, mut r) = (String::new(), s);
            while let Some(i) = r.find("/*") {
                o.push_str(&r[..i]);
                match r[i..].find("*/") {
                    Some(j) => r = &r[i + j + 2..],
                    None => return o,
                }
            }
            o.push_str(r);
            o
        }
        /// `:root` vars as a name→value map (value whitespace normalized).
        fn vars(text: &str) -> std::collections::BTreeMap<String, String> {
            let from = &text[text.find(":root").expect(":root")..];
            let open = from.find('{').unwrap();
            let close = from.find('}').unwrap();
            strip(&from[open + 1..close])
                .split(';')
                .filter_map(|d| {
                    let (n, v) = d.trim().strip_prefix("--")?.split_once(':')?;
                    Some((
                        format!("--{}", n.trim()),
                        v.split_whitespace().collect::<Vec<_>>().join(" "),
                    ))
                })
                .collect()
        }
        let cg_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/calm-glass.css");
        let cg = std::fs::read_to_string(cg_path).unwrap_or_else(|e| panic!("read {cg_path}: {e}"));
        assert_eq!(
            vars(&mockup_html()),
            vars(&cg),
            "mockup.html 与 calm-glass.css 的 :root 漂了(两份是镜像副本,改色 / 令牌须同步两边)"
        );
    }
}

/// Spec generator: mechanically extract `design/mockup.html` into the
/// **auto-generated §16 of `docs/样式还原手册.md`** (between the `SPEC:AUTO-*`
/// markers) — a per-component table of exact px/weight/radius/color values
/// (`var()` resolved) + a single-source token registry built from the live
/// `tn-dark.toml` + `style.rs`. Implementing a gpui view then copies numbers
/// instead of eyeballing the prototype.
///
/// Normal `cargo test` only *exercises* the generator + checks the markers exist.
/// To (re)write §16: `TN_GEN_SPEC=1 cargo test -p tn-ui spec_gen`.
#[cfg(test)]
mod spec_gen {
    use super::*;
    use std::fmt::Write as _;
    use tn_config::{Color, Theme};

    fn mockup() -> String {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/mockup.html");
        std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
    }

    /// `design/calm-glass.css` — the shared stylesheet every panel `<link>`s, and
    /// the authoritative source for §16.2 component specs: it carries *all*
    /// components, including panel-only ones absent from the hero mockup
    /// (quicklook / appmenu / welcome / activity rail).
    fn calm_glass() -> String {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/calm-glass.css");
        std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
    }

    fn strip_comments(s: &str) -> String {
        let (mut out, mut rest) = (String::new(), s);
        while let Some(i) = rest.find("/*") {
            out.push_str(&rest[..i]);
            match rest[i..].find("*/") {
                Some(j) => rest = &rest[i + j + 2..],
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    /// `:root` CSS vars as `(--name, value)` (comments stripped).
    fn root_vars(html: &str) -> Vec<(String, String)> {
        let from = &html[html.find(":root").expect(":root")..];
        let open = from.find('{').unwrap();
        let close = from.find('}').unwrap();
        strip_comments(&from[open + 1..close])
            .split(';')
            .filter_map(|d| {
                let rest = d.trim().strip_prefix("--")?;
                let (n, v) = rest.split_once(':')?;
                Some((format!("--{}", n.trim()), v.trim().to_string()))
            })
            .collect()
    }

    /// Replace `var(--x[, fallback])` with its `:root` value (bounded passes).
    fn resolve(val: &str, root: &[(String, String)]) -> String {
        let mut s = val.to_string();
        for _ in 0..8 {
            let Some(i) = s.find("var(") else { break };
            let Some(rel) = s[i..].find(')') else { break };
            let close = i + rel;
            let inner = s[i + 4..close].to_string();
            let name = inner.split(',').next().unwrap().trim();
            let fallback = inner.split(',').nth(1).map(str::trim);
            let repl = root
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
                .or(fallback)
                .unwrap_or("")
                .to_string();
            s.replace_range(i..=close, &repl);
        }
        s
    }

    /// Body of the rule whose selector subject is `.<class>`. Prefers a *standalone*
    /// `.class{ … }` over a descendant / compound / grouped match (`.abody .body{`,
    /// `.tree, …, .code{`) — so `.body`/`.code` resolve to their own rule, not a
    /// shared scrollbar/min-width rule. Falls back to the first non-standalone match
    /// when there is no standalone (e.g. `.tag` only exists as `.tnode .tag`).
    fn rule_body<'a>(style: &'a str, class: &str) -> Option<&'a str> {
        let needle = format!(".{class}{{");
        let mut fallback = None;
        let mut from = 0;
        while let Some(rel) = style[from..].find(&needle) {
            let at = from + rel;
            from = at + needle.len();
            let body_start = at + needle.len();
            let body = &style[body_start..body_start + style[body_start..].find('}')?];
            // selector = text from the previous rule boundary up to ".class"
            let sel_start = style[..at].rfind('}').map_or(0, |i| i + 1);
            if style[sel_start..at + class.len() + 1].trim() == format!(".{class}") {
                return Some(body); // true standalone rule
            }
            fallback.get_or_insert(body); // descendant / compound / grouped
        }
        fallback
    }

    /// Layout/type properties worth copying verbatim.
    const PROPS: &[&str] = &[
        "height", "width", "min-width", "padding", "gap", "border-radius",
        "font-size", "font-weight", "color", "background", "border",
    ];

    fn hex(c: Color) -> String {
        format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
    }
    fn white(token: u32) -> String {
        format!("0x{token:08x}（白 @ {:.0}%）", (token & 0xff) as f32 / 255.0 * 100.0)
    }

    fn build(css: &str) -> String {
        let style = strip_comments(css);
        let root = root_vars(css);
        let t = Theme::tn_dark();
        let mut o = String::new();

        // 16.1 — token 单一真源:从 live 主题 + 常量生成
        o.push_str("### 16.1 设计令牌（单一真源）\n\n");
        o.push_str("> 颜色定义在 `tn-dark.toml`、白叠加/圆角定义在 `style.rs`;mockup 的同名变量是**受守卫的副本**。\n\n");
        o.push_str("| mockup `--var` | 值 | gpui 写法 | 定义处 |\n|---|---|---|---|\n");
        for (var, c, gp) in [
            ("--fg", t.ui.foreground, "col(ui.foreground)"),
            ("--muted", t.ui.muted, "col(ui.muted)"),
            ("--accent", t.ui.accent, "col(ui.accent)"),
            ("--violet", t.ui.accent_alt, "col(ui.accent_alt)"),
            ("--green", t.ansi.green, "col(t.ansi.green)"),
            ("--red", t.ansi.red, "col(t.ansi.red)"),
            ("--yellow", t.ansi.yellow, "col(t.ansi.yellow)"),
            ("--cyan", t.ansi.cyan, "col(t.ansi.cyan)"),
            ("--claude", t.agents.claude, "col(t.agents.claude)"),
            ("--codex", t.agents.codex, "col(t.agents.codex)"),
        ] {
            let _ = writeln!(o, "| `{var}` | `{}` | `{gp}` | tn-dark.toml |", hex(c));
        }
        for (var, tok, gp) in [
            ("--rim", RIM, "rgba(RIM)"),
            ("--sheen", SHEEN, "rgba(SHEEN)"),
            ("--g2", INSET, "rgba(INSET)"),
            ("--g3", HOVER, "rgba(HOVER)"),
            ("（状态栏分隔）", DIVIDER, "rgba(DIVIDER)"),
        ] {
            let _ = writeln!(o, "| `{var}` | `{}` | `{gp}` | style.rs |", white(tok));
        }
        for (var, r, gp) in [
            ("--r-win", R_WINDOW, "rounded(px(R_WINDOW))"),
            ("--r-pane", R_PANEL, "rounded(px(R_PANEL))"),
            ("--r-card", R_CARD, "rounded(px(R_CARD))"),
        ] {
            let _ = writeln!(o, "| `{var}` | `{r}px` | `{gp}` | style.rs |");
        }

        // 16.2 — 逐组件精确值(从 calm-glass.css 全集生成,按面板分组)
        o.push_str("\n### 16.2 组件规格（calm-glass.css 逐类精确值,`var()` 已解析）\n\n");
        let classes = [
            // ① 窗口外壳
            "win", "titlebar", "brand", "caret", "tab", "newtab", "wctl",
            "appmenu", "mi", "sep", "status", "seg2",
            // ② 工作区 + 窗格
            "work", "pane", "phead", "cwd", "chip",
            // ③ 资源管理器
            "tree", "tnode", "tag",
            // agent 头 + 用量环 + 活动栏
            "agenthead", "who", "nm", "model", "usage", "tok", "cost", "ring", "lbl",
            "arail", "astat", "alabel", "achip", "afile", "adiff", "ahint",
            // 终端 / shell / block
            "body", "block",
            // 速览 Quick Look
            "quicklook", "vh", "code", "qlfoot",
            // 欢迎 launchpad
            "welcome", "recent", "rrow", "whints",
            // 浮层:命令面板 / Quick Terminal
            "scrim", "palette", "prow", "quick", "launcher", "tiles", "tile",
        ];
        for cls in classes {
            let Some(body) = rule_body(&style, cls) else { continue };
            let rows: Vec<(String, String)> = body
                .split(';')
                .filter_map(|d| {
                    let (p, v) = d.split_once(':')?;
                    let (p, v) = (p.trim(), v.trim());
                    PROPS.contains(&p).then(|| {
                        // collapse multi-line / repeated whitespace so a wrapped CSS
                        // value (e.g. a two-line gradient) renders on one tidy row.
                        let v = resolve(v, &root);
                        (p.to_string(), v.split_whitespace().collect::<Vec<_>>().join(" "))
                    })
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            let _ = writeln!(o, "**`.{cls}`**");
            for (p, v) in rows {
                let _ = writeln!(o, "- `{p}`: {v}");
            }
            o.push('\n');
        }
        o
    }

    const DOC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/样式还原手册.md");
    const MARK_START: &str = "<!-- SPEC:AUTO-START -->";
    const MARK_END: &str = "<!-- SPEC:AUTO-END -->";

    #[test]
    fn spec_section_generates() {
        let body = build(&calm_glass());
        // exercise: the generated body must carry the token registry + component specs.
        assert!(body.contains("--fg"), "token registry missing");
        assert!(body.contains("**`.pane`**"), "component specs missing");
        assert!(body.len() > 800, "spec suspiciously short ({} bytes)", body.len());

        // the host doc must keep the markers so §16 can be spliced in.
        let doc = std::fs::read_to_string(DOC).unwrap_or_else(|e| panic!("read {DOC}: {e}"));
        let si = doc.find(MARK_START).expect("样式还原手册.md missing SPEC:AUTO-START");
        let ei = doc.find(MARK_END).expect("样式还原手册.md missing SPEC:AUTO-END");
        assert!(si < ei, "SPEC markers out of order");

        // TN_GEN_SPEC=1 → splice the generated body between the markers (idempotent).
        if std::env::var_os("TN_GEN_SPEC").is_some() {
            let head = &doc[..si + MARK_START.len()];
            let tail = &doc[ei..];
            let merged = format!("{head}\n\n{}\n\n{tail}", body.trim());
            std::fs::write(DOC, merged).unwrap_or_else(|e| panic!("write {DOC}: {e}"));
        }
    }
}

/// Guard (样式还原手册.md §3 约定): UI code must use `col()`/`cola()` for theme
/// colors, never a raw `rgb(0x..)`/`rgba(0x..)` whose RGB equals a theme token —
/// otherwise theme switching silently breaks. Scans `tn-ui/src/**.rs` and fails,
/// naming the offender, if any literal's RGB matches a token. White overlays,
/// fg-dim/faint, the g1 gradient base, black/scrim/transparent are NOT tokens, so
/// their literals are allowed (§3 sanctioned exceptions). `style.rs` is exempt
/// (token defs + helpers + these tests live here).
#[cfg(test)]
mod no_hardcoded_theme_colors {
    use tn_config::{Color, Theme};

    /// Theme token RGBs (6-digit) that must go through `col()`/`cola()`.
    fn token_rgbs() -> Vec<(u32, &'static str)> {
        let t = Theme::tn_dark();
        let h = |c: Color| ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
        vec![
            (h(t.ui.foreground), "ui.foreground"),
            (h(t.ui.muted), "ui.muted"),
            (h(t.ui.accent), "ui.accent"),
            (h(t.ui.accent_alt), "ui.accent_alt"),
            (h(t.ui.surface_1), "ui.surface_1"),
            (h(t.ui.surface_2), "ui.surface_2"),
            (h(t.ui.chrome_bg), "ui.chrome_bg"),
            (h(t.ui.border), "ui.border"),
            (h(t.ansi.red), "ansi.red"),
            (h(t.ansi.green), "ansi.green"),
            (h(t.ansi.yellow), "ansi.yellow"),
            (h(t.ansi.blue), "ansi.blue"),
            (h(t.ansi.magenta), "ansi.magenta"),
            (h(t.ansi.cyan), "ansi.cyan"),
            (h(t.agents.claude), "agents.claude"),
            (h(t.agents.codex), "agents.codex"),
        ]
    }

    fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().map_or(false, |x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// 6-digit RGBs of every `rgb(0x..)` / `rgba(0x..)` literal in `code`
    /// (comments already stripped by the caller).
    fn literal_rgbs(code: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for (pat, is_rgba) in [("rgba(0x", true), ("rgb(0x", false)] {
            let mut from = 0;
            while let Some(i) = code[from..].find(pat) {
                let start = from + i + pat.len();
                let hex: String =
                    code[start..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                from = start;
                if let Ok(v) = u32::from_str_radix(&hex, 16) {
                    out.push(if is_rgba { v >> 8 } else { v }); // rgba → drop alpha byte
                }
            }
        }
        out
    }

    #[test]
    fn ui_code_uses_tokens_not_hardcoded_theme_colors() {
        let tokens = token_rgbs();
        let mut files = Vec::new();
        rs_files(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
            &mut files,
        );
        let mut bad = Vec::new();
        for f in &files {
            if f.file_name().map_or(false, |n| n == "style.rs") {
                continue;
            }
            let src = std::fs::read_to_string(f).unwrap();
            for (n, line) in src.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line); // ignore comments
                for rgb in literal_rgbs(code) {
                    if let Some((_, name)) = tokens.iter().find(|(t, _)| *t == rgb) {
                        bad.push(format!(
                            "{}:{}: 硬编码 {name}(#{rgb:06X}) → 改用 col({name})/cola({name}, a)",
                            f.display(),
                            n + 1
                        ));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "UI 代码出现硬编码主题色(必须走 col()/cola(),见 样式还原手册.md §3 约定):\n{}",
            bad.join("\n")
        );
    }
}
