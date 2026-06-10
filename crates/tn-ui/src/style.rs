//! Calm Glass shared style tokens + helpers — the single source of truth.
//!
//! These constants and helpers were previously copy-pasted across six view
//! modules (workspace / terminal_view / quick_terminal / explorer / viewer /
//! block_view), so any visual tweak meant editing six places (see docs/修复与优化/基础性能与审查勘误.md).
//! They now live here; modules `use crate::style::…`.
//!
//! `col`/`cola` accept either chrome colors (`tn_config::Color`) or terminal-cell
//! colors (`tn_core::Rgb`) via the [`Rgb8`] trait — both are just 8-bit RGB.

use gpui::{
    div, hsla, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba, BoxShadow,
    Div, Rgba, Svg,
};

// Calm Glass white-on-glass overlay tokens (alpha-only — depth from layered
// translucency + a top mirror highlight, never from glow). See docs/产品体验/玻璃视觉原则.md.
pub(crate) const RIM: u32 = 0xffffff12; // glass edge (~white .07) — replaces hard borders
pub(crate) const SHEEN: u32 = 0xffffff1a; // top 1px mirror highlight (~white .10)
pub(crate) const INSET: u32 = 0xffffff0a; // header / inset card overlay (~white .04)
pub(crate) const HOVER: u32 = 0xffffff0f; // chip / hover (~white .06, = mockup --g3)
pub(crate) const DIVIDER: u32 = 0xffffff0f; // status-bar segment divider (~white .06, = mockup `.status .seg2 + .seg2`)

// Pane glass fill midpoint baked over the current window chrome. The HTML
// prototype can use backdrop blur + noise to smooth full-panel gradients; GPUI
// cannot, so large surfaces use one stable base color and keep depth at edges.
pub(crate) const G1_MID: u32 = 0x191f3685; // midpoint of prototype g1
pub(crate) const QL_MID: u32 = 0x161b2ee8; // midpoint of prototype quicklook fill

/// UI sans-serif for chrome (tabs / headers / status); paired with the mono
/// terminal/code font. Ships on Windows 10/11.
pub(crate) const UI_SANS: &str = "Segoe UI";

// Calm Glass corner radii (px): window 16, panel 14, card 11. See docs/产品体验/玻璃视觉原则.md.
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
/// material shows through, instead of being filled opaque. See docs/产品体验/玻璃视觉原则.md.
pub(crate) fn cola(c: impl Rgb8, a: f32) -> Rgba {
    let (r, g, b) = c.channels();
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
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

/// The pane glass fill — prototype g1 midpoint baked opaque over the window `bg`.
/// Shared by terminal panes, explorer, and the welcome surface. Keep it flat:
/// full-panel gradients create visible bands on large dark panes without CSS
/// blur/noise.
pub(crate) fn pane_fill(bg: impl Rgb8) -> gpui::Background {
    let base = bg.channels();
    over(G1_MID, base).into()
}

/// Pane interior material overlay.
///
/// Keep this visually empty. The HTML prototype uses backdrop blur and noise to
/// hide transparent-gradient banding; GPUI surfaces do not, so full-panel
/// "glass wash" layers produce uneven blocks/lines in large empty areas.
pub(crate) fn specular_wash(_focused: bool, _accent: impl Rgb8) -> Div {
    div()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .w(px(0.))
        .h(px(0.))
}

/// mockup `.pane` / `.pane.active` box-shadow stack: an outer 1px **dark hairline**
/// (`0 0 0 1px rgba(0,0,0,.28)`) that crisply *cuts* the pane out of the backdrop,
/// plus layered soft drops for float — depth, not glow. (gpui 0.2.2 has no inset
/// box-shadow, so the mockup's inset bottom shadow is omitted.) Shared by panes +
/// explorer so the lift stays identical.
#[allow(dead_code)]
pub(crate) fn pane_shadows(focused: bool) -> Vec<BoxShadow> {
    // 软暗晕,代替 mockup 的硬 1px 暗线:硬线紧贴亮渐变描边 → 暗-亮并置显「接缝」(mockup
    // 靠 backdrop-blur 抹平,我们没有)。改 3px 模糊、0 spread 的暗晕 → 仍「切出背景」,
    // 但边过渡丝滑、无硬缝。
    let edge_cut = soft_shadow(0.0, 3.0, 0.0, 0.15); // reduced from 0.34
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
/// edge** (1px-padding reveal trick, see below). No box-shadows — tiled split panes
/// are tightly packed; any outward shadow would bleed into the neighbour below and
/// show as dark banding through the semi-transparent glass fill.
///
/// `inner` must be built with `rounded(R_PANEL - 1.)` + `overflow_hidden`.
/// Focused = brighter top + more accent bottom. Cool-white = glass refraction
/// tint (not a theme token); accent goes through `cola` so it's never a bare
/// theme hex.
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
            linear_color_stop(rgba(0x00000000), 1.), // → transparent at 4px
        ));

    let wrapped = div().size_full().relative().child(inner).child(top_glaze);

    // ── Gradient edge ring ──
    // 顶端冷白高亮 (32% / 17%) + 底端强调色回光 (28% / 15%) — 强化边缘折射与品质感
    let top = if focused {
        rgba(0xffffff4d)
    } else {
        rgba(0xffffff33)
    };
    let edge = linear_gradient(
        180.,
        linear_color_stop(top, 0.),
        linear_color_stop(cola(accent, if focused { 0.18 } else { 0.13 }), 1.),
    );
    shadowed(
        div()
            .size_full()
            .rounded(px(R_PANEL))
            .p(px(1.))
            .bg(edge)
            .child(wrapped),
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
#[allow(dead_code)]
pub(crate) fn glass_card(inner: Div, focused: bool, accent: impl Rgb8) -> Div {
    let (ar, ag, ab) = accent.channels();
    let ar = ar as f32 / 255.0;
    let ag = ag as f32 / 255.0;
    let ab = ab as f32 / 255.0;

    // 1. 边缘导光环 (Gradient Ring)
    // 顶部迎接环境冷白光，底部汇聚强调色（发光感来源）
    let top_edge = if focused {
        cola(accent, 0.30) // 白 .32 -> 彩色高亮 .30 (消除生硬白边)
    } else {
        rgba(0xffffff0d) // 白 .14 -> .05 (极柔和)
    };
    let bot_edge = Rgba {
        r: ar,
        g: ag,
        b: ab,
        a: if focused { 0.15 } else { 0.06 }, // .30 -> .15 (focus), .15 -> .06 (unfocused)
    };

    let edge_bg = linear_gradient(
        180.,
        linear_color_stop(top_edge, 0.),
        linear_color_stop(bot_edge, 1.),
    );

    // 2. 发光投影 (Glow) -> 彻底去除黑色阴影 (避免暗色主题下显得脏乱)
    // 现代暗黑设计中，实体黑影容易让 UI 显得沉闷。我们只保留边缘渐变环，
    // 并在 focus 时增加一层极其干净、微弱的同色背光，彻底告别黑色 drop-shadow。
    let glow_shadows = if focused {
        vec![
            // 纯粹的微弱彩色背光，无任何黑色成分
            BoxShadow {
                color: Rgba {
                    r: ar,
                    g: ag,
                    b: ab,
                    a: 0.18, // 柔和的透明度
                }
                .into(),
                offset: point(px(0.), px(0.)),
                blur_radius: px(12.), // 足够散开，形成高级的弥散背光
                spread_radius: px(0.),
            },
        ]
    } else {
        vec![] // 未激活时完全无阴影，极致干净
    };

    shadowed(
        div()
            // w_full() 而非 size_full()：在 flex_col 中避免高度坍塌
            .w_full()
            .rounded(px(R_CARD))
            .p(px(1.)) // 留出 1px 的光环
            .bg(edge_bg)
            .child(
                // ★ 只保留 w_full()；h_full() 在 intrinsic-height 父容器中
                // 引发高度计算死锁 → 图层渲染异常
                inner.w_full(),
            ),
        glow_shadows,
    )
}

/// Quick Look 速览浮层的玻璃填充:比常驻面板更实,同样使用均匀烤实底色以避免
/// 大面积渐变在代码预览空白处形成色带。
pub(crate) fn quicklook_fill(bg: impl Rgb8) -> gpui::Background {
    let base = bg.channels();
    over(QL_MID, base).into()
}

/// mockup `.quicklook` 浮起投影栈:比常驻面板(`pane_shadows`)更深更高——浮层飘在最上层。
/// 同样把硬 1px 暗线换成 3px 软暗晕(避接缝,见 `pane_shadows`),再叠多层柔投影。
pub(crate) fn quicklook_shadows() -> Vec<BoxShadow> {
    vec![
        soft_shadow(0.0, 3.0, 0.0, 0.16), // 软暗晕切出背景(代 mockup 0 0 0 1px rgba(0,0,0,.36), reduced to .16)
        soft_shadow(2.0, 6.0, -2.0, 0.6), // mockup 0 2px 6px -2px rgba(0,0,0,.6)
        soft_shadow(30.0, 72.0, -24.0, 0.86), // mockup 0 30px 72px -24px rgba(0,0,0,.86)
        soft_shadow(72.0, 132.0, -50.0, 0.96), // mockup 0 72px 132px -50px rgba(0,0,0,.96)
    ]
}

/// Compact dropdown shadow for the brand app menu.
pub(crate) fn app_menu_shadows() -> Vec<BoxShadow> {
    vec![soft_shadow(30.0, 80.0, -24.0, 0.9)]
}

/// Centered overlay panel shadow used by command/search/config popups.
pub(crate) fn overlay_panel_shadows() -> Vec<BoxShadow> {
    vec![soft_shadow(40.0, 120.0, -30.0, 0.9)]
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

    let wrapped = div().size_full().relative().child(inner).child(top_glaze);

    let edge = linear_gradient(
        180.,
        linear_color_stop(rgba(0xffffff3d), 0.),
        linear_color_stop(cola(accent, 0.15), 1.), // bottom accent .15 (原 .06)
    );
    shadowed(
        div()
            .size_full()
            .rounded(px(R_PANEL))
            .p(px(1.))
            .bg(edge)
            .child(wrapped),
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
