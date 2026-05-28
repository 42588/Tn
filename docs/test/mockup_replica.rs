//! Runnable gpui replica of `design/mockup.html` (Tn Dark · Calm Glass).
//!
//! Purpose: a *self-contained* gpui window the owner can run on a real machine
//! and visually compare, side-by-side, against `design/mockup.html` in a
//! browser — to validate our CSS→gpui mapping (`docs/CSS_TO_GPUI.md`).
//!
//! This file is intentionally STANDALONE: it does **not** use any `tn-ui`
//! internal helpers (`crate::style::*` / `assets::*` are `pub(crate)` and not
//! visible to an example). Every value is inlined directly from
//! `docs/CSS_TO_GPUI.md` §16 (the authoritative auto-generated spec) so the
//! replica tests the *documented mapping values*, not tn-ui internals.
//!
//! Wired as a `tn-ui` example (gpui may only be linked in `tn-ui` / `tn-app`):
//!   cargo run -p tn-ui --example mockup_replica
//!
//! Honored gpui 0.2.2 pitfalls (per CSS_TO_GPUI.md):
//!   - No `pointer_events_none()` — non-interactive divs already pass mouse.
//!   - `box_shadow` has no `inset` → top "sheen" highlight is an absolute 1px div.
//!   - Shadows via `el.style().box_shadow = Some(vec![BoxShadow{..}])`.
//!   - `overflow_hidden` only clips rectangles → children with their own bg
//!     carry their own `rounded`.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{
    div, linear_color_stop, linear_gradient, point, px, relative, rgb, rgba, size, svg, App,
    AppContext, Application, Bounds, BoxShadow, Context, Div, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, Rgba, SharedString, Styled, Svg, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

// ─────────────────────────────────────────────────────────────────────────────
// Design tokens — inlined from CSS_TO_GPUI.md §16.1 (single source of truth).
// ─────────────────────────────────────────────────────────────────────────────

// Colors (opaque, 0xRRGGBB).
const FG: u32 = 0xC6D0F5; // --fg
const FG_DIM: u32 = 0xA6AFD4; // --fg-dim
const MUTED: u32 = 0x6E76A0; // --muted
const FAINT: u32 = 0x474E72; // --faint
const ACCENT: u32 = 0x7AA2F7; // --accent
const VIOLET: u32 = 0xBB9AF7; // --violet
const GREEN: u32 = 0x9ECE6A; // --green
const RED: u32 = 0xF7768E; // --red
const YELLOW: u32 = 0xE0AF68; // --yellow
const CYAN: u32 = 0x7DCFFF; // --cyan
const CLAUDE: u32 = 0xF0916D; // --claude
const CODEX: u32 = 0x73DACA; // --codex

// White-overlay material tokens (0xRRGGBBAA, alpha = round(α×255)).
const RIM: u32 = 0xffffff12; // --rim   white @ 7%  (0x12 = 18/255 ≈ .071)
const SHEEN: u32 = 0xffffff1a; // --sheen white @ 10% (0x1a = 26/255 ≈ .102)
const INSET: u32 = 0xffffff0a; // --g2    white @ 4%  (0x0a = 10/255 ≈ .039)
const HOVER: u32 = 0xffffff0f; // --g3    white @ 6%  (0x0f = 15/255 ≈ .059)
const DIVIDER: u32 = 0xffffff0f; // status seg divider, white @ 6%

// Radii. (R_WINDOW/--r-win 不再需要:窗口铺满、圆角交给 DWM。)
const R_PANEL: f32 = 14.0; // --r-pane
const R_CARD: f32 = 11.0; // --r-card

const UI_SANS: &str = "Segoe UI";
const MONO: &str = "Cascadia Code";

// ── small color helpers (inlined; mirror style.rs col/cola semantics) ──

/// Opaque color from an `0xRRGGBB` literal.
fn col(hex: u32) -> Rgba {
    rgb(hex)
}

/// `0xRRGGBB` color at fractional alpha (folds alpha for us — never hand-hex it).
fn cola(hex: u32, a: f32) -> Rgba {
    let r = ((hex >> 16) & 0xff) as f32 / 255.0;
    let g = ((hex >> 8) & 0xff) as f32 / 255.0;
    let b = (hex & 0xff) as f32 / 255.0;
    Rgba { r, g, b, a }
}

/// A soft drop shadow (CSS `0 {y}px {blur}px {spread}px rgba(0,0,0,{alpha})`).
fn soft_shadow(y: f32, blur: f32, spread: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: Hsla::from(Rgba { r: 0., g: 0., b: 0., a: alpha }),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}

/// Attach outset box shadows to a div (gpui 0.2.2 has no fluent `.shadow_*`).
fn shadowed(mut d: Div, shadows: Vec<BoxShadow>) -> Div {
    d.style().box_shadow = Some(shadows);
    d
}

/// Apply a fractional CSS `flex: N` weight. gpui has no fluent fractional flex
/// helper (`flex_1()` hard-codes 1), so set `flex_grow` directly + basis 0 so
/// the weights divide the row/column proportionally (matches the mockup's
/// `flex:0.6 / 1.55 / 2.5 / 0.85 / 1.18` etc.).
fn flex_weight(mut d: Div, weight: f32) -> Div {
    d.style().flex_grow = Some(weight);
    d.style().flex_shrink = Some(1.);
    d.style().flex_basis = Some(relative(0.).into());
    d
}

/// A square line-icon element; tint at the call site (gpui paints an SVG only
/// when a text color is set — an untinted icon is fully transparent).
fn icon(name: &str, sz: f32) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .w(px(sz))
        .h(px(sz))
        .flex_none()
}

/// A 1px absolute top "sheen" highlight (substitute for CSS inset box-shadow).
fn sheen_top() -> Div {
    div()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .h(px(1.))
        .bg(rgba(SHEEN))
}

// ─────────────────────────────────────────────────────────────────────────────
// Inline AssetSource (synthesizes the Calm Glass line icons + usage ring).
// Copied bodies + wrapper from crates/tn-ui/src/assets.rs so the example is
// self-contained (it cannot reach the pub(crate) `Assets`).
// ─────────────────────────────────────────────────────────────────────────────

const ICON_BODIES: &[(&str, &str)] = &[
    ("folder", r#"<path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4l2 2h9A1.5 1.5 0 0 1 21 9.5v8A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5z"/>"#),
    ("file", r#"<path d="M13 3H7a1.5 1.5 0 0 0-1.5 1.5v15A1.5 1.5 0 0 0 7 21h10a1.5 1.5 0 0 0 1.5-1.5V8.5z"/><path d="M13 3v5.5h5.5"/>"#),
    ("chev-r", r#"<path d="M9.5 7l5 5-5 5"/>"#),
    ("chev-d", r#"<path d="M7 9.5l5 5 5-5"/>"#),
    ("spark", r#"<path d="M12 3.4c.42 3.9 1.9 5.38 5.8 5.8-3.9.42-5.38 1.9-5.8 5.8-.42-3.9-1.9-5.38-5.8-5.8 3.9-.42 5.38-1.9 5.8-5.8z"/><path d="M18.5 15.5c.2 1.5.8 2.1 2.3 2.3-1.5.2-2.1.8-2.3 2.3-.2-1.5-.8-2.1-2.3-2.3 1.5-.2 2.1-.8 2.3-2.3z"/>"#),
    ("check", r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#),
    ("diamond", r#"<path d="M12 3.5l8.5 8.5-8.5 8.5L3.5 12z"/>"#),
    ("circle", r#"<circle cx="12" cy="12" r="7"/>"#),
    ("term", r#"<path d="M5 7.5l4.5 4.5L5 16.5"/><path d="M12.5 16.5h6.5"/>"#),
    ("branch", r#"<circle cx="7" cy="6.5" r="2.3"/><circle cx="7" cy="17.5" r="2.3"/><circle cx="17" cy="8.5" r="2.3"/><path d="M7 8.8v6.4"/><path d="M17 10.8c0 4.2-4.2 3.2-7 4.6"/>"#),
    ("min", r#"<path d="M6 12h12"/>"#),
    ("max", r#"<rect x="6.5" y="6.5" width="11" height="11" rx="2"/>"#),
    ("close", r#"<path d="M7 7l10 10M17 7L7 17"/>"#),
    ("plus", r#"<path d="M12 6v12M6 12h12"/>"#),
    ("pen", r#"<path d="M14.5 5.5l4 4M4 20l1-4L16 5a2 2 0 0 1 3 3L8 19z"/>"#),
    ("explorer", r#"<path d="M4 6.5A1.5 1.5 0 0 1 5.5 5H10l1.5 1.5h7A1.5 1.5 0 0 1 20 8v9.5A1.5 1.5 0 0 1 18.5 19h-13A1.5 1.5 0 0 1 4 17.5z"/>"#),
];

fn icon_svg(body: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
    )
}

const RING_R: f32 = 15.0;

fn ring_track_svg() -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36" fill="none"><circle cx="18" cy="18" r="{RING_R}" stroke="#ffffff" stroke-width="3"/></svg>"##
    )
}

fn ring_arc_svg(pct: f32) -> String {
    let circumference = 2.0 * std::f32::consts::PI * RING_R;
    let offset = circumference * (1.0 - (pct / 100.0).clamp(0.0, 1.0));
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36" fill="none"><circle cx="18" cy="18" r="{RING_R}" stroke="#ffffff" stroke-width="3" stroke-linecap="round" stroke-dasharray="{circumference:.2}" stroke-dashoffset="{offset:.2}" transform="rotate(-90 18 18)"/></svg>"##
    )
}

struct ReplicaAssets;

impl gpui::AssetSource for ReplicaAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(name) = path.strip_prefix("icons/").and_then(|p| p.strip_suffix(".svg")) {
            if let Some((_, body)) = ICON_BODIES.iter().find(|(n, _)| *n == name) {
                return Ok(Some(Cow::Owned(icon_svg(body).into_bytes())));
            }
            return Ok(None);
        }
        if let Some(spec) = path.strip_prefix("ring/").and_then(|p| p.strip_suffix(".svg")) {
            let s = if spec == "track" {
                ring_track_svg()
            } else if let Ok(pct) = spec.parse::<f32>() {
                ring_arc_svg(pct)
            } else {
                return Ok(None);
            };
            return Ok(Some(Cow::Owned(s.into_bytes())));
        }
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ICON_BODIES
            .iter()
            .map(|(n, _)| SharedString::from(format!("icons/{n}.svg")))
            .collect())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The view.
// ─────────────────────────────────────────────────────────────────────────────

struct ReplicaView;

impl ReplicaView {
    fn new() -> Self {
        ReplicaView
    }
}

impl Render for ReplicaView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // The window IS the .win card: it fills the OS window edge-to-edge (no
        // desktop margin), and Windows DWM rounds the outer corners. The root is
        // NOT rounded (CLAUDE.md 坑) — that avoids the black corner/edge gap.
        // bg is just a fallback behind the card.
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(col(0x0A0A12))
            .font_family(UI_SANS)
            .text_color(col(FG))
            .child(window_chrome())
    }
}

// ── .win — the rounded glass window shell ──
fn window_chrome() -> impl IntoElement {
    // CSS .win: 1140×700, radius 16, glass gradient, 1px ring (→ rim border),
    // sheen top inset (→ absolute 1px div), big drop shadow.
    let win = div()
        .relative()
        .size_full() // fill the OS window edge-to-edge (DWM rounds the corners)
        .flex()
        .flex_col()
        .overflow_hidden()
        // background: linear-gradient(180deg, rgba(21,22,34,0.62), rgba(15,16,25,0.72))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x1516229e), 0.), // round(0.62×255)=158=0x9e
            linear_color_stop(rgba(0x0f1019b8), 1.), // round(0.72×255)=184=0xb8
        ))
        // box-shadow ① outer ring 0 0 0 1px rgba(255,255,255,0.06) → rim border
        .border_1()
        .border_color(rgba(HOVER))
        // ② inset top highlight rgba(255,255,255,0.11) → absolute 1px div
        .child(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .right(px(0.))
                .h(px(1.))
                .bg(rgba(0xffffff1c)), // round(0.11×255)=28=0x1c
        )
        .child(titlebar())
        .child(workspace())
        .child(status_bar());

    // ③ big soft shadow: 0 64px 140px -34px rgba(0,0,0,0.82)
    shadowed(win, vec![soft_shadow(64., 140., -34., 0.82)])
}

// ── .titlebar — brand · tabs · window controls ──
fn titlebar() -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .h(px(46.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(14.))
        .pl(px(16.))
        .pr(px(10.))
        // brand: mark (gradient rounded square) + name
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(9.))
                .child(
                    // .brand .mark — 21×21, radius 7, accent→violet gradient
                    div()
                        .w(px(21.))
                        .h(px(21.))
                        .rounded(px(7.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(linear_gradient(
                            145.,
                            linear_color_stop(col(ACCENT), 0.),
                            linear_color_stop(col(VIOLET), 1.),
                        ))
                        .child(icon("term", 13.).text_color(col(0x0B0D16))),
                )
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight(680.))
                        .text_color(col(FG))
                        .child("Tn"),
                ),
        )
        // .tabs — flex:1, gap 3, top-aligned (padding-top 10)
        .child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(3.))
                .pt(px(10.))
                .child(tab("Claude", "spark", CLAUDE, true, Some("~/proj/tn")))
                .child(tab("pwsh", "term", ACCENT, false, None))
                .child(tab("Codex", "spark", CODEX, false, None))
                // .newtab — 29×29, radius 9
                .child(
                    div()
                        .w(px(29.))
                        .h(px(29.))
                        .rounded(px(9.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(col(MUTED))
                        .hover(|s| s.bg(rgba(INSET)).text_color(col(FG)))
                        .child(icon("plus", 15.).text_color(col(MUTED))),
                ),
        )
        // .wctl — min / max / close
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(2.))
                .child(wctl_button("min", false))
                .child(wctl_button("max", false))
                .child(wctl_button("close", true)),
        )
}

// .tab — height 34, padding 0 14, radius 11px 11px 0 0, fw 520, fs 12.5
fn tab(
    label: &str,
    icon_name: &'static str,
    accent: u32,
    is_active: bool,
    badge: Option<&str>,
) -> Div {
    let icon_color = if is_active { accent } else { MUTED };
    let mut t = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.))
        .h(px(34.))
        .px(px(14.))
        .rounded_t(px(R_CARD))
        .text_size(px(12.5))
        .font_weight(FontWeight(520.));

    if is_active {
        t = t
            .text_color(col(FG))
            // background: linear-gradient(180deg, rgba(255,255,255,0.055), rgba(255,255,255,0.01))
            .bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0xffffff0e), 0.), // round(0.055×255)=14=0x0e
                linear_color_stop(rgba(0xffffff03), 1.), // round(0.01×255)=3=0x03
            ))
            // ::after top accent strip: left/right 13, top 0, h 2, radius 2
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(13.))
                    .right(px(13.))
                    .h(px(2.))
                    .rounded(px(2.))
                    .bg(col(accent)),
            );
    } else {
        t = t.text_color(col(MUTED));
    }

    t = t
        .child(icon(icon_name, 14.).text_color(col(icon_color)))
        .child(SharedString::from(label.to_string()));

    // .tab .badge — mono, faint, fw 400, fs 11
    if let Some(b) = badge {
        t = t.child(
            div()
                .font_family(MONO)
                .text_size(px(11.))
                .font_weight(FontWeight(400.))
                .text_color(col(FAINT))
                .child(SharedString::from(b.to_string())),
        );
    }
    t
}

// .wctl .b — 35×30, radius 9; close hover → red @ 22%, fg #ffd9e0
fn wctl_button(icon_name: &'static str, is_close: bool) -> Div {
    let mut b = div()
        .w(px(35.))
        .h(px(30.))
        .rounded(px(9.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(col(MUTED))
        .child(icon(icon_name, 13.).text_color(col(MUTED)));
    if is_close {
        b = b.hover(|s| s.bg(cola(RED, 0.22)).text_color(col(0xFFD9E0)));
    } else {
        b = b.hover(|s| s.bg(rgba(INSET)).text_color(col(FG)));
    }
    b
}

// ── .work — three columns (sidebar | center | viewer), gap 11, padding 5/12/11 ──
fn workspace() -> impl IntoElement {
    div()
        .relative()
        .flex_1()
        .flex()
        .flex_row()
        .gap(px(11.))
        .pt(px(5.))
        .px(px(12.))
        .pb(px(11.))
        .min_h(px(0.))
        // .sidebar — flex 0.6
        .child(flex_weight(
            div().flex().flex_col().min_w(px(0.)).min_h(px(0.)).child(sidebar_pane()),
            0.6,
        ))
        // center column — flex 1.55, holds agent pane (2.5) + shell pane (0.85)
        .child(flex_weight(
            div()
                .flex()
                .flex_col()
                .gap(px(11.))
                .min_w(px(0.))
                .min_h(px(0.))
                .child(agent_pane())
                .child(shell_pane()),
            1.55,
        ))
        // .viewer — flex 1.18
        .child(flex_weight(
            div().flex().flex_col().min_w(px(0.)).min_h(px(0.)).child(viewer_pane()),
            1.18,
        ))
}

// ── A frosted .pane shell: g1 gradient, rim border, specular, sheen, shadow ──
fn pane(is_active: bool) -> Div {
    let mut p = div()
        .relative()
        .flex()
        .flex_col()
        .rounded(px(R_PANEL))
        .overflow_hidden()
        .min_w(px(0.))
        .min_h(px(0.))
        // background: var(--g1) = linear-gradient(180deg, rgba(42,46,68,0.42), rgba(26,28,44,0.52))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x2a2e446b), 0.), // round(0.42×255)=107=0x6b
            linear_color_stop(rgba(0x1a1c2c85), 1.), // round(0.52×255)=133=0x85
        ))
        .border_1()
        // rim, or warm claude rim @ 24% when active
        .border_color(if is_active { cola(CLAUDE, 0.24) } else { rgba(RIM) })
        // sheen top highlight (inset box-shadow substitute)
        .child(sheen_top())
        // ::before specular — top 36%, white @ 4% → transparent
        .child(
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .top(px(0.))
                .h(relative(0.36))
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(rgba(0xffffff0a), 0.), // round(0.04×255)=10=0x0a
                    linear_color_stop(rgba(0x00000000), 1.),
                )),
        );

    // active pane: warm 1px ring (→ already in border) + deeper shadow
    let shadows = if is_active {
        vec![soft_shadow(30., 64., -36., 0.9)]
    } else {
        vec![soft_shadow(24., 58., -36., 0.88)]
    };
    p = shadowed(p, shadows);
    p
}

// ── sidebar (explorer) ──
fn sidebar_pane() -> impl IntoElement {
    pane(false)
        // .phead — explorer header
        .child(
            phead().child(icon("explorer", 14.).text_color(col(MUTED))).child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_color(col(FG_DIM))
                    .child("Explorer · ")
                    .child(
                        div()
                            .text_color(col(ACCENT))
                            .font_weight(FontWeight(680.))
                            .child("tn"),
                    ),
            ),
        )
        // .tree — file rows
        .child(
            div()
                .relative()
                .flex_1()
                .flex()
                .flex_col()
                .p(px(6.))
                .text_size(px(12.5))
                .overflow_hidden()
                .child(tnode(0, true, false, "chev-d", "crates", None))
                .child(tnode(1, true, false, "chev-d", "tn-ui", None))
                .child(tnode(2, true, false, "chev-d", "src", None))
                .child(tnode(3, false, true, "file", "element.rs", Some(('M', YELLOW))))
                .child(tnode(3, false, false, "file", "terminal_view.rs", None))
                .child(tnode(3, false, false, "file", "lib.rs", None))
                .child(tnode(1, true, false, "chev-r", "tn-core", None))
                .child(tnode(1, true, false, "chev-r", "tn-pty", None))
                .child(tnode(0, true, false, "chev-d", "docs", None))
                .child(tnode(1, false, false, "file", "UX-DESIGN.md", Some(('U', GREEN))))
                .child(tnode(1, false, false, "file", "BLUEPRINT.md", None))
                .child(tnode(0, false, false, "file", "Cargo.toml", None))
                .child(tnode(0, false, false, "file", "README.md", None)),
        )
}

// .phead — height 36, gap 9, padding 0 13, fs 11.5, fw 560, muted
fn phead() -> Div {
    div()
        .flex_none()
        .h(px(36.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(9.))
        .px(px(13.))
        .text_size(px(11.5))
        .font_weight(FontWeight(560.))
        .text_color(col(MUTED))
}

// .tnode — height 26, gap 7, padding 0 10, radius 8; indents 16px each
fn tnode(
    indent: u8,
    is_dir: bool,
    is_active: bool,
    leading_icon: &'static str,
    name: &str,
    git_tag: Option<(char, u32)>,
) -> Div {
    let ml = px(indent as f32 * 16.);
    // file rows reserve a chevron-sized gap; dirs draw chevron + folder.
    let mut row = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.))
        .h(px(26.))
        .px(px(10.))
        .ml(ml)
        .rounded(px(8.))
        .text_size(px(12.5));

    if is_active {
        row = row
            // active: linear-gradient(180deg, rgba(255,255,255,0.075), rgba(255,255,255,0.025))
            .bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0xffffff13), 0.), // round(0.075×255)=19=0x13
                linear_color_stop(rgba(0xffffff06), 1.), // round(0.025×255)=6=0x06
            ))
            .text_color(col(FG))
            .child(sheen_top());
    } else if is_dir {
        row = row.text_color(col(FG)).font_weight(FontWeight(540.));
    } else {
        row = row.text_color(col(FG_DIM));
    }

    // indent connector line (::before for indented rows)
    if indent > 0 {
        row = row.child(
            div()
                .absolute()
                .left(px(-8.))
                .top(px(-2.))
                .bottom(px(-2.))
                .w(px(1.))
                .bg(rgba(0xffffff0d)), // rgba(255,255,255,0.05) → round(.05×255)=13=0x0d
        );
    }

    if is_dir {
        // chevron (muted) + folder (accent)
        row = row
            .child(icon(leading_icon, 14.).text_color(col(MUTED)))
            .child(icon("folder", 14.).text_color(col(ACCENT)));
    } else {
        // file: chevron-sized spacer + file icon (claude tint if active)
        let glyph = if is_active { CLAUDE } else { MUTED };
        row = row
            .child(div().w(px(14.)).flex_none())
            .child(icon("file", 14.).text_color(col(glyph)));
    }

    row = row.child(SharedString::from(name.to_string()));

    // .tag — margin-left auto, 15×15, radius 5, fs 9, fw 800
    if let Some((ch, c)) = git_tag {
        row = row.child(div().flex_1()).child(
            div()
                .flex_none()
                .w(px(15.))
                .h(px(15.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(9.))
                .font_weight(FontWeight(800.))
                .text_color(col(c))
                .bg(cola(c, 0.15))
                .child(SharedString::from(ch.to_string())),
        );
    }
    row
}

// ── agent pane (active, claude) ──  (center column weight: flex 2.5)
fn agent_pane() -> impl IntoElement {
    flex_weight(pane(true)
        // .agenthead — gap 11, padding 10/14, claude wash
        .child(
            div()
                .relative()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(11.))
                .py(px(10.))
                .px(px(14.))
                // background: linear-gradient(180deg, rgba(240,145,109,0.07), transparent 72%)
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(cola(CLAUDE, 0.07), 0.),
                    linear_color_stop(rgba(0x00000000), 0.72),
                ))
                // .av — 28×28, radius 9, claude tint on claude @ 14% bg
                .child(
                    div()
                        .relative()
                        .w(px(28.))
                        .h(px(28.))
                        .rounded(px(9.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(cola(CLAUDE, 0.14))
                        .child(sheen_top())
                        .child(icon("spark", 16.).text_color(col(CLAUDE))),
                )
                // .who — name + model
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight(680.))
                                .text_color(col(FG))
                                .child("Claude Code"),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight(520.))
                                .text_color(col(MUTED))
                                .child("Sonnet 4.6"),
                        ),
                )
                .child(div().flex_1())
                // .usage — ring + token/cost, pill, white @ 4% bg
                .child(usage_pill())
                // .think — claude, fs 11.5, fw 560, dot + text
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(11.5))
                        .font_weight(FontWeight(560.))
                        .text_color(col(CLAUDE))
                        .child(
                            // .dotpulse — 6×6 dot (static; no animation per §11)
                            div().w(px(6.)).h(px(6.)).rounded_full().bg(col(CLAUDE)),
                        )
                        .child("Thinking…"),
                ),
        )
        // .agentbody — tool rows + say bubble
        .child(
            div()
                .relative()
                .flex_1()
                .flex()
                .flex_col()
                .py(px(12.))
                .px(px(15.))
                .text_size(px(12.5))
                .overflow_hidden()
                .child(tool_row("check", GREEN, "Read", Some("crates/tn-ui/src/terminal_view.rs"), "(218 lines)"))
                .child(tool_row("check", GREEN, "Grep", Some("\"paint_text\""), "→ 2 files"))
                .child(tool_row("diamond", CLAUDE, "Editing", Some("crates/tn-ui/src/element.rs"), "— 用字形图集批量提交 quad"))
                .child(tool_row("circle", FAINT, "为每格上色，合并背景 / 光标 / 选区为 typed-quad…", None, ""))
                // .say — bubble
                .child(
                    div()
                        .relative()
                        .mt(px(12.))
                        .py(px(11.))
                        .px(px(13.))
                        .rounded(px(R_CARD))
                        .text_color(col(FG))
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(rgba(0xffffff0d), 0.), // round(0.05×255)=13=0x0d
                            linear_color_stop(rgba(0xffffff05), 1.), // round(0.018×255)=5=0x05
                        ))
                        .child(sheen_top())
                        .child(
                            div()
                                .child("我会把逐行 paint_text 改成 paint_quads 批处理（见右侧 diff），这样每格颜色与连字都能正确渲染。继续吗？"),
                        ),
                ),
        ),
        2.5,
    )
}

// .usage — pill (gap 11, padding 4/5/4/12, white @ 4% bg)
fn usage_pill() -> Div {
    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(11.))
        .py(px(4.))
        .pl(px(12.))
        .pr(px(5.))
        .rounded_full()
        .bg(rgba(INSET))
        .child(sheen_top())
        // .ring — 32×32, track + arc + centered % label
        .child(
            div()
                .relative()
                .w(px(32.))
                .h(px(32.))
                .child(
                    svg()
                        .path(SharedString::from("ring/track.svg"))
                        .w(px(32.))
                        .h(px(32.))
                        .absolute()
                        .text_color(rgba(SHEEN)),
                )
                .child(
                    svg()
                        .path(SharedString::from("ring/42.svg"))
                        .w(px(32.))
                        .h(px(32.))
                        .absolute()
                        .text_color(col(CLAUDE)),
                )
                .child(
                    div()
                        .absolute()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(9.))
                        .font_weight(FontWeight(760.))
                        .text_color(col(FG))
                        .child("42%"),
                ),
        )
        // .meta — tok / cost, right-aligned
        .child(
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(1.))
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .font_weight(FontWeight(640.))
                        .text_color(col(FG_DIM))
                        .child("84K / 200K"),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(10.5))
                        .font_weight(FontWeight(640.))
                        .text_color(col(GREEN))
                        .child("$0.31"),
                ),
        )
}

// .tool — gap 9, glyph + text; tcode (cyan mono) / tdim (muted mono)
fn tool_row(
    glyph: &'static str,
    glyph_color: u32,
    lead: &str,
    code: Option<&str>,
    trail: &str,
) -> Div {
    let mut text_line = div().flex().flex_row().flex_wrap().items_center().gap(px(5.)).child(
        div()
            .font_family(MONO)
            .text_size(px(11.5))
            .text_color(col(MUTED))
            .child(SharedString::from(lead.to_string())),
    );
    if let Some(c) = code {
        text_line = text_line.child(
            div()
                .font_family(MONO)
                .text_size(px(11.5))
                .text_color(col(CYAN))
                .child(SharedString::from(c.to_string())),
        );
    }
    if !trail.is_empty() {
        text_line = text_line.child(
            div()
                .font_family(MONO)
                .text_size(px(11.5))
                .text_color(col(MUTED))
                .child(SharedString::from(trail.to_string())),
        );
    }

    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(9.))
        .my(px(6.))
        .child(
            div()
                .flex_none()
                .w(px(16.))
                .h(px(16.))
                .mt(px(1.))
                .child(icon(glyph, 16.).text_color(col(glyph_color))),
        )
        .child(text_line)
}

// ── shell pane (pwsh) ──  (center column weight: flex 0.85)
fn shell_pane() -> impl IntoElement {
    flex_weight(pane(false)
        // .phead — term icon + cwd + chip
        .child(
            phead()
                .child(icon("term", 14.).text_color(col(ACCENT)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .text_color(col(FG_DIM))
                        .child("~/proj/")
                        .child(div().text_color(col(ACCENT)).font_weight(FontWeight(680.)).child("tn")),
                )
                .child(div().flex_1())
                // .chip — pwsh 7.4 pill
                .child(
                    div()
                        .relative()
                        .py(px(2.))
                        .px(px(9.))
                        .rounded_full()
                        .text_size(px(10.5))
                        .font_weight(FontWeight(560.))
                        .text_color(col(FG_DIM))
                        .bg(rgba(HOVER))
                        .child(sheen_top())
                        .child("pwsh 7.4"),
                ),
        )
        // .body — terminal output (mono)
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .py(px(11.))
                .px(px(15.))
                .font_family(MONO)
                .text_size(px(12.5))
                .overflow_hidden()
                // .block — command block with green status strip
                .child(
                    div()
                        .relative()
                        .my(px(2.))
                        .mb(px(10.))
                        .rounded(px(R_CARD))
                        .overflow_hidden()
                        .bg(rgba(0xffffff09)) // rgba(255,255,255,0.035) → round=9=0x09
                        .child(sheen_top())
                        // ::before left status strip (green), 3px
                        .child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(0.))
                                .bottom(px(0.))
                                .w(px(3.))
                                .bg(col(GREEN)),
                        )
                        // .bh — command header row
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
                                .text_size(px(12.))
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap(px(5.))
                                        .child("❯")
                                        .child(div().text_color(col(ACCENT)).child("cargo"))
                                        .child("test -p tn-core"),
                                )
                                .child(div().flex_1())
                                // .dur
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .font_weight(FontWeight(640.))
                                        .text_color(col(MUTED))
                                        .child("0.8s"),
                                )
                                // .exit — green check pill
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(3.))
                                        .py(px(1.))
                                        .pl(px(6.))
                                        .pr(px(8.))
                                        .rounded_full()
                                        .bg(cola(GREEN, 0.15))
                                        .child(icon("check", 11.).text_color(col(GREEN))),
                                ),
                        ),
                )
                // test result row
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(5.))
                        .child(div().text_color(col(GREEN)).child("   test result: ok."))
                        .child(div().text_color(col(MUTED)).child("3 passed; 0 failed")),
                )
                // prompt row with seg badges + cursor
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .mt(px(5.))
                        .child(seg_badge("~/proj/tn", ACCENT))
                        .child(seg_badge("main", GREEN))
                        .child("❯")
                        .child(div().w(px(7.)).h(px(14.)).rounded(px(2.)).bg(col(FG))),
                ),
        ),
        0.85,
    )
}

// .seg — colored prompt badge: bg color, dark text, radius 5, fw 660
fn seg_badge(label: &str, bg: u32) -> Div {
    div()
        .py(px(1.))
        .px(px(8.))
        .rounded(px(5.))
        .font_weight(FontWeight(660.))
        .text_color(col(0x0C0E16))
        .bg(col(bg))
        .child(SharedString::from(label.to_string()))
}

// ── viewer pane (diff) ──
fn viewer_pane() -> impl IntoElement {
    pane(false)
        // .vh — viewer header
        .child(
            div()
                .relative()
                .flex_none()
                .h(px(36.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(9.))
                .px(px(13.))
                .text_size(px(11.5))
                .font_weight(FontWeight(560.))
                // background: linear-gradient(180deg, rgba(122,162,247,0.06), transparent 72%)
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(cola(ACCENT, 0.06), 0.),
                    linear_color_stop(rgba(0x00000000), 0.72),
                ))
                .child(icon("file", 14.).text_color(col(ACCENT)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .text_color(col(FG_DIM))
                        .child("crates/tn-ui/src/")
                        .child(div().text_color(col(ACCENT)).child("element.rs")),
                )
                .child(div().flex_1())
                // .by — "编辑中" with pen icon, claude
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(5.))
                        .text_size(px(11.))
                        .font_weight(FontWeight(560.))
                        .text_color(col(CLAUDE))
                        .child(icon("pen", 13.).text_color(col(CLAUDE)))
                        .child("编辑中"),
                )
                // .tabset — Diff / File toggle
                .child(
                    div()
                        .relative()
                        .flex()
                        .flex_row()
                        .gap(px(2.))
                        .p(px(2.))
                        .rounded(px(9.))
                        .bg(rgba(INSET))
                        .child(sheen_top())
                        .child(
                            div()
                                .relative()
                                .py(px(2.))
                                .px(px(10.))
                                .rounded(px(7.))
                                .text_size(px(10.5))
                                .font_weight(FontWeight(640.))
                                .text_color(col(FG))
                                .bg(rgba(HOVER))
                                .child(sheen_top())
                                .child("Diff"),
                        )
                        .child(
                            div()
                                .py(px(2.))
                                .px(px(10.))
                                .rounded(px(7.))
                                .text_size(px(10.5))
                                .font_weight(FontWeight(640.))
                                .text_color(col(MUTED))
                                .child("File"),
                        ),
                ),
        )
        // .code — diff body
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .py(px(8.))
                .font_family(MONO)
                .text_size(px(12.5))
                .overflow_hidden()
                .child(code_line(12, ' ', None, "impl Element for TerminalElement {"))
                .child(code_line(13, ' ', None, "    fn paint(&mut self, win: &mut Window) {"))
                .child(code_line(14, ' ', None, "        let mut quads = Vec::new();"))
                .child(code_line(15, ' ', None, "        for cell in self.row.cells() {"))
                .child(code_line(16, '-', Some(false), "            win.paint_text(cell.ch, cell.point);"))
                .child(code_line(16, '+', Some(true), "            let g = self.atlas.glyph(self.font, cell.ch);"))
                .child(code_line(17, '+', Some(true), "            quads.push(Quad::glyph(g, cell.fg, cell.bg));"))
                .child(code_line(18, ' ', None, "        }"))
                .child(code_line(19, '+', Some(true), "        win.paint_quads(&quads); // 一次提交"))
                .child(code_line(20, ' ', None, "    }"))
                .child(code_line(21, ' ', None, "}")),
        )
}

// .cl — diff code line: line number gutter + marker + text; add/del tinted bg
fn code_line(num: u32, mark: char, added: Option<bool>, text: &str) -> Div {
    let (ln_color, mk_color, bg) = match added {
        Some(true) => (GREEN, GREEN, Some(0x9ece6a17u32)), // add: green @ 9% → round(.09×255)=23=0x17
        Some(false) => (RED, RED, Some(0xf7768e17u32)),    // del: red @ 9%
        None => (FAINT, FG_DIM, None),
    };
    let mut line = div()
        .flex()
        .flex_row()
        .pr(px(12.));
    if let Some(b) = bg {
        line = line.bg(rgba(b));
    }
    line.child(
        // .ln — width 38, right-aligned gutter, fs 11
        div()
            .w(px(38.))
            .flex_none()
            .mr(px(14.))
            .text_size(px(11.))
            .text_color(col(ln_color))
            .flex()
            .justify_end()
            .child(SharedString::from(num.to_string())),
    )
    .child(
        // .mk — 14px marker column, centered
        div()
            .w(px(14.))
            .flex_none()
            .flex()
            .justify_center()
            .text_color(col(mk_color))
            .child(SharedString::from(mark.to_string())),
    )
    .child(
        div()
            .text_color(col(FG_DIM))
            .child(SharedString::from(text.to_string())),
    )
}

// ── .status — bottom bar with segments separated by faint dividers ──
fn status_bar() -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .h(px(30.))
        .flex()
        .flex_row()
        .items_center()
        .px(px(6.))
        .text_size(px(11.))
        .font_weight(FontWeight(510.))
        .text_color(col(MUTED))
        // background: linear-gradient(180deg, transparent, rgba(0,0,0,0.2))
        .bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x00000000), 0.),
            linear_color_stop(rgba(0x00000033), 1.), // round(0.2×255)=51=0x33
        ))
        .child(sheen_top())
        .child(status_seg(false, |d| {
            d.child(icon("branch", 13.).text_color(col(ACCENT))).child("main")
        }))
        .child(status_seg(true, |d| {
            d.child(div().font_family(MONO).font_weight(FontWeight(640.)).text_color(col(FG_DIM)).child("3"))
                .child("sessions")
        }))
        .child(status_seg(true, |d| {
            d.child(icon("spark", 13.).text_color(col(CLAUDE)))
                .child("ctx")
                .child(div().font_family(MONO).font_weight(FontWeight(640.)).text_color(col(FG_DIM)).child("42%"))
        }))
        .child(status_seg(true, |d| {
            d.child(icon("spark", 13.).text_color(col(CODEX)))
                .child("ctx")
                .child(div().font_family(MONO).font_weight(FontWeight(640.)).text_color(col(FG_DIM)).child("18%"))
        }))
        .child(div().flex_1())
        .child(status_seg(false, |d| d.child("element.rs · Rust")))
        .child(status_seg(true, |d| d.child("UTF-8")))
        .child(status_seg(true, |d| d.text_color(col(ACCENT)).child("Tn Dark")))
}

// .status .seg2 — gap 6, padding 0 13, height 18; left divider when not first
fn status_seg(divider: bool, build: impl FnOnce(Div) -> Div) -> Div {
    let mut seg = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .px(px(13.))
        .h(px(18.));
    // .seg2 + .seg2 → box-shadow:-1px 0 0 rgba(255,255,255,0.06) (left divider)
    if divider {
        seg = seg.border_l(px(1.)).border_color(rgba(DIVIDER));
    }
    build(seg)
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point.
// ─────────────────────────────────────────────────────────────────────────────

/// Open the replica window and run the gpui event loop (blocks until quit).
pub fn run() {
    Application::new()
        .with_assets(ReplicaAssets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1220.), px(800.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Tn — mockup replica".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    window_background: WindowBackgroundAppearance::Opaque,
                    ..Default::default()
                },
                |_window, cx| cx.new(|_cx| ReplicaView::new()),
            )
            .expect("failed to open replica window");

            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            cx.activate(true);
        });
}

/// Example-target entry point. When wired via Cargo `[[example]]` with this
/// file as the crate root, Cargo needs a `main`; when included via a shim
/// (`#[path] mod replica; replica::run()`), this `main` is simply unused.
#[allow(dead_code)]
fn main() {
    run();
}
