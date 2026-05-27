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
pub(crate) const HOVER: u32 = 0xffffff14; // chip / hover (~white .08)

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
