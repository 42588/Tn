//! Calm Glass shared style tokens + helpers — the single source of truth.
//!
//! These constants and helpers were previously copy-pasted across six view
//! modules (workspace / terminal_view / quick_terminal / explorer / viewer /
//! block_view), so any visual tweak meant editing six places (待优化清单 §4.1).
//! They now live here; modules `use crate::style::…`.
//!
//! `col`/`cola` accept either chrome colors (`tn_config::Color`) or terminal-cell
//! colors (`tn_core::Rgb`) via the [`Rgb8`] trait — both are just 8-bit RGB.

use gpui::{hsla, point, prelude::*, px, rgb, BoxShadow, Div, Rgba, Svg};

// Calm Glass white-on-glass overlay tokens (alpha-only — depth from layered
// translucency + a top mirror highlight, never from glow). docs/UX-DESIGN §6.1.
pub(crate) const RIM: u32 = 0xffffff12; // glass edge (~white .07) — replaces hard borders
pub(crate) const SHEEN: u32 = 0xffffff1a; // top 1px mirror highlight (~white .10)
pub(crate) const INSET: u32 = 0xffffff0a; // header / inset card overlay (~white .04)
pub(crate) const HOVER: u32 = 0xffffff0f; // chip / hover (~white .06, = mockup --g3)
pub(crate) const DIVIDER: u32 = 0xffffff0f; // status-bar segment divider (~white .06, = mockup `.status .seg2 + .seg2`)

/// UI sans-serif for chrome (tabs / headers / status); paired with the mono
/// terminal/code font. Ships on Windows 10/11.
pub(crate) const UI_SANS: &str = "Segoe UI";

// Calm Glass corner radii (px): window 16, panel 14, card 11. docs/UX-DESIGN §6.1.
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
/// material shows through, instead of being filled opaque. See UX-DESIGN §6.1.
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

/// Attach box shadows to a div (gpui 0.2.2 has no fluent `.shadow_*` helper).
pub(crate) fn shadowed(mut d: Div, shadows: Vec<BoxShadow>) -> Div {
    d.style().box_shadow = Some(shadows);
    d
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

/// Drift guard (see docs/CSS_TO_GPUI.md §1): assert the design prototype
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

        // ── corner radii ──
        assert_eq!(px_val(css_var(root, "--r-win")), R_WINDOW, "--r-win → R_WINDOW");
        assert_eq!(px_val(css_var(root, "--r-pane")), R_PANEL, "--r-pane → R_PANEL");
        assert_eq!(px_val(css_var(root, "--r-card")), R_CARD, "--r-card → R_CARD");
    }
}

/// Spec-sheet generator (docs/CSS_TO_GPUI.md §17 流程的"照抄不估"那步): mechanically
/// extract `design/mockup.html` into `design/SPEC.md` — a per-component table of
/// exact px/weight/radius/color values (`var()` resolved) + a single-source token
/// registry built from the live `tn-dark.toml` + `style.rs`. Implementing a gpui
/// view then copies numbers instead of eyeballing the prototype.
///
/// Normal `cargo test` only *exercises* the generator (asserts non-empty). To
/// (re)write the file: `TN_GEN_SPEC=1 cargo test -p tn-ui spec_gen`.
#[cfg(test)]
mod spec_gen {
    use super::*;
    use std::fmt::Write as _;
    use tn_config::{Color, Theme};

    fn mockup() -> String {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/mockup.html");
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

    /// Body of the base rule `.<class>{ … }` (first match; `None` if absent).
    fn rule_body<'a>(style: &'a str, class: &str) -> Option<&'a str> {
        let at = style.find(&format!(".{class}{{"))?;
        let after = &style[at + class.len() + 2..];
        Some(&after[..after.find('}')?])
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

    fn build(html: &str) -> String {
        let style = {
            let i = html.find("<style").expect("<style");
            let open = i + html[i..].find('>').unwrap() + 1;
            let end = html[open..].find("</style>").map_or(html.len(), |j| open + j);
            strip_comments(&html[open..end])
        };
        let root = root_vars(html);
        let t = Theme::tn_dark();
        let mut o = String::new();

        o.push_str("# Tn 界面规格单（SPEC）\n\n");
        o.push_str("> **本文件由 `TN_GEN_SPEC=1 cargo test -p tn-ui spec_gen` 生成**,取自 `design/mockup.html`\n");
        o.push_str("> + `tn-dark.toml` + `style.rs`。**勿手改**——改原件后重跑。实现 gpui 界面时**照抄数值、别看图估**;\n");
        o.push_str("> 网页↔代码的颜色一致性另由 `style::token_drift` 测试守卫。翻译查 [CSS_TO_GPUI.md](CSS_TO_GPUI.md)。\n\n");

        // §1 — token 单一真源(③):从 live 主题 + 常量生成
        o.push_str("## 1. 设计令牌（单一真源）\n\n");
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

        // §2 — 逐组件精确值(④)
        o.push_str("\n## 2. 组件规格（mockup 逐类精确值,`var()` 已解析）\n\n");
        let classes = [
            "work", "pane", "phead", "cwd", "chip", "sidebar", "tnode", "tag",
            "agenthead", "who", "nm", "model", "usage", "tok", "cost", "ring",
            "lbl", "agentbody", "tool", "say", "body", "status", "seg2", "tab",
            "newtab", "wctl",
        ];
        for cls in classes {
            let Some(body) = rule_body(&style, cls) else { continue };
            let rows: Vec<(String, String)> = body
                .split(';')
                .filter_map(|d| {
                    let (p, v) = d.split_once(':')?;
                    let (p, v) = (p.trim(), v.trim());
                    PROPS.contains(&p).then(|| (p.to_string(), resolve(v, &root)))
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

    #[test]
    fn spec_md_generates() {
        let md = build(&mockup());
        // exercise: the generator must produce the token registry + component specs.
        assert!(md.contains("--fg"), "token registry missing");
        assert!(md.contains("**`.pane`**"), "component specs missing");
        assert!(md.len() > 800, "spec suspiciously short ({} bytes)", md.len());

        if std::env::var_os("TN_GEN_SPEC").is_some() {
            let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/SPEC.md");
            std::fs::write(p, md).unwrap_or_else(|e| panic!("write {p}: {e}"));
        }
    }
}
