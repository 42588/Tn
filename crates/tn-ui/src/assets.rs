//! Embedded SVG asset source: the Calm Glass line-icon set + a dynamically
//! generated context-usage ring.
//!
//! gpui's `svg()` element renders an SVG to an **alpha mask** tinted by the
//! element's `text_color` (colors in the SVG are ignored — only coverage
//! matters), so every icon here is a single opaque-stroke line drawing and we
//! tint it at the call site. The icon geometry mirrors `design/mockup.html`'s
//! `<symbol>` sprite (Lucide-style, `stroke-width` ~1.8, round caps/joins) —
//! the design forbids stand-in Unicode glyphs (✓ ◐ ✻) for icons.
//!
//! Paths the renderer understands (see [`Assets::load`]):
//!   - `icons/<name>.svg`         — a static line icon from [`ICON_BODIES`]
//!   - `ring/track.svg`           — the full-circle usage-ring track
//!   - `ring/<pct>.svg`           — the usage-ring arc filled to `<pct>` (0–100)

use std::borrow::Cow;

use anyhow::Result;
use gpui::{px, svg, AssetSource, SharedString, Styled, Svg};

/// Inner SVG markup (no `<svg>` wrapper) for each named icon, viewBox `0 0 24 24`.
const ICON_BODIES: &[(&str, &str)] = &[
    (
        "folder",
        r#"<path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4l2 2h9A1.5 1.5 0 0 1 21 9.5v8A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5z"/>"#,
    ),
    (
        "file",
        r#"<path d="M13 3H7a1.5 1.5 0 0 0-1.5 1.5v15A1.5 1.5 0 0 0 7 21h10a1.5 1.5 0 0 0 1.5-1.5V8.5z"/><path d="M13 3v5.5h5.5"/>"#,
    ),
    ("chev-r", r#"<path d="M9.5 7l5 5-5 5"/>"#),
    ("chev-l", r#"<path d="M14.5 7l-5 5 5 5"/>"#),
    ("chev-d", r#"<path d="M7 9.5l5 5 5-5"/>"#),
    (
        "spark",
        r#"<path d="M12 3.4c.42 3.9 1.9 5.38 5.8 5.8-3.9.42-5.38 1.9-5.8 5.8-.42-3.9-1.9-5.38-5.8-5.8 3.9-.42 5.38-1.9 5.8-5.8z"/><path d="M18.5 15.5c.2 1.5.8 2.1 2.3 2.3-1.5.2-2.1.8-2.3 2.3-.2-1.5-.8-2.1-2.3-2.3 1.5-.2 2.1-.8 2.3-2.3z"/>"#,
    ),
    ("check", r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#),
    ("diamond", r#"<path d="M12 3.5l8.5 8.5-8.5 8.5L3.5 12z"/>"#),
    ("circle", r#"<circle cx="12" cy="12" r="7"/>"#),
    (
        "term",
        r#"<path d="M5 7.5l4.5 4.5L5 16.5"/><path d="M12.5 16.5h6.5"/>"#,
    ),
    (
        "branch",
        r#"<circle cx="7" cy="6.5" r="2.3"/><circle cx="7" cy="17.5" r="2.3"/><circle cx="17" cy="8.5" r="2.3"/><path d="M7 8.8v6.4"/><path d="M17 10.8c0 4.2-4.2 3.2-7 4.6"/>"#,
    ),
    ("min", r#"<path d="M6 12h12"/>"#),
    (
        "max",
        r#"<rect x="6.5" y="6.5" width="11" height="11" rx="2"/>"#,
    ),
    ("close", r#"<path d="M7 7l10 10M17 7L7 17"/>"#),
    ("plus", r#"<path d="M12 6v12M6 12h12"/>"#),
    (
        "pen",
        r#"<path d="M14.5 5.5l4 4M4 20l1-4L16 5a2 2 0 0 1 3 3L8 19z"/>"#,
    ),
    (
        "explorer",
        r#"<path d="M4 6.5A1.5 1.5 0 0 1 5.5 5H10l1.5 1.5h7A1.5 1.5 0 0 1 20 8v9.5A1.5 1.5 0 0 1 18.5 19h-13A1.5 1.5 0 0 1 4 17.5z"/>"#,
    ),
    // app-menu icons (mockup 01-window-chrome.html <symbol> sprite)
    (
        "external",
        r#"<path d="M13 5h6v6"/><path d="M19 5l-8 8"/><path d="M17 13.5V18a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 5 18V8a1.5 1.5 0 0 1 1.5-1.5H11"/>"#,
    ),
    (
        "sidebar",
        r#"<rect x="4" y="5.5" width="16" height="13" rx="2"/><path d="M9.5 5.5v13"/>"#,
    ),
    (
        "sliders",
        r#"<path d="M4 8h10"/><path d="M4 16h7"/><circle cx="17.5" cy="8" r="2.3"/><circle cx="14.5" cy="16" r="2.3"/>"#,
    ),
    (
        "moon",
        r#"<path d="M17 13.5A6.5 6.5 0 1 1 10.5 7 5 5 0 0 0 17 13.5z"/>"#,
    ),
    (
        "refresh",
        r#"<path d="M19 12a7 7 0 1 1-2.1-5"/><path d="M19.5 4.5V9H15"/>"#,
    ),
    (
        "info",
        r#"<circle cx="12" cy="12" r="8"/><path d="M12 11.5v4.5"/><path d="M12 8.2h.01"/>"#,
    ),
    (
        "power",
        r#"<path d="M12 4.5v6.5"/><path d="M7.8 7.8a6.5 6.5 0 1 0 8.4 0"/>"#,
    ),
    // SSH connector (06-ssh.html): remote globe, key/password auth badges, favorite star
    (
        "globe",
        r#"<circle cx="12" cy="12" r="8.2"/><path d="M3.8 12h16.4M12 3.8c2.4 2.3 2.4 14.1 0 16.4M12 3.8c-2.4 2.3-2.4 14.1 0 16.4"/>"#,
    ),
    (
        "key",
        r#"<circle cx="9" cy="14" r="3.2"/><path d="M11.3 11.7 19 4M16.4 6.6l2 2M14.3 8.7l1.8 1.8"/>"#,
    ),
    (
        "lock",
        r#"<rect x="5.5" y="11" width="13" height="8.5" rx="2"/><path d="M8.2 11V8a3.8 3.8 0 0 1 7.6 0v3"/>"#,
    ),
    (
        "star",
        r#"<path d="M12 4l2.5 5.1 5.6.8-4 3.9.95 5.6L12 16.8 7 19.4l.95-5.6-4-3.9 5.6-.8z"/>"#,
    ),
    // SSH connection cards (06-ssh.html): warning triangle, password reveal eye, return key
    (
        "alert",
        r#"<path d="M12 3.8 20.5 19.5a1 1 0 0 1-.9 1.5H4.4a1 1 0 0 1-.9-1.5z"/><path d="M12 9.5v4.5"/><path d="M12 17.3h.01"/>"#,
    ),
    (
        "eye",
        r#"<path d="M2.6 12S6 5.6 12 5.6 21.4 12 21.4 12 18 18.4 12 18.4 2.6 12 2.6 12z"/><circle cx="12" cy="12" r="2.7"/>"#,
    ),
    (
        "enter",
        r#"<path d="M20 6v5a3 3 0 0 1-3 3H5"/><path d="M8 11l-3 3 3 3"/>"#,
    ),
    // host-key trust (B2 TOFU)
    (
        "shield",
        r#"<path d="M12 3.2 19 6v5c0 4.6-3 8.2-7 10-4-1.8-7-5.4-7-10V6z"/><path d="M9 12l2 2 4-4"/>"#,
    ),
    // SSH 身份字形 ⇄(SHEET 06/07:warn 琥珀双向箭头)
    (
        "exchange",
        r#"<path d="M4 8.2h13.5"/><path d="M14.2 4.8 17.6 8.2l-3.4 3.4"/><path d="M20 15.8H6.5"/><path d="M9.8 12.4 6.4 15.8l3.4 3.4"/>"#,
    ),
];

/// Wrap icon body markup in a 24×24 line-icon `<svg>` (opaque white stroke so
/// the alpha mask covers the strokes; gpui re-tints with the element color).
fn icon_svg(body: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
    )
}

const RING_R: f32 = 15.0;

/// The full-circle ring track (a 36×36 stroked circle).
fn ring_track_svg() -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36" fill="none"><circle cx="18" cy="18" r="{RING_R}" stroke="#ffffff" stroke-width="3"/></svg>"##
    )
}

/// The ring arc filled to `pct` (0–100), starting at the top and going
/// clockwise (`rotate(-90)`), with a rounded cap — the context-usage indicator.
fn ring_arc_svg(pct: f32) -> String {
    let circumference = 2.0 * std::f32::consts::PI * RING_R; // ≈ 94.25
    let offset = circumference * (1.0 - (pct / 100.0).clamp(0.0, 1.0));
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36" fill="none"><circle cx="18" cy="18" r="{RING_R}" stroke="#ffffff" stroke-width="3" stroke-linecap="round" stroke-dasharray="{circumference:.2}" stroke-dashoffset="{offset:.2}" transform="rotate(-90 18 18)"/></svg>"##
    )
}

/// The embedded asset source. Register once via `Application::with_assets`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // Static line icons: icons/<name>.svg
        if let Some(name) = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"))
        {
            if let Some((_, body)) = ICON_BODIES.iter().find(|(n, _)| *n == name) {
                return Ok(Some(Cow::Owned(icon_svg(body).into_bytes())));
            }
            return Ok(None);
        }
        // Usage ring: ring/track.svg or ring/<pct>.svg
        if let Some(spec) = path
            .strip_prefix("ring/")
            .and_then(|p| p.strip_suffix(".svg"))
        {
            let svg = if spec == "track" {
                ring_track_svg()
            } else if let Ok(pct) = spec.parse::<f32>() {
                ring_arc_svg(pct)
            } else {
                return Ok(None);
            };
            return Ok(Some(Cow::Owned(svg.into_bytes())));
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

/// Asset path for a named line icon (`spark` → `icons/spark.svg`).
pub fn icon_path(name: &str) -> SharedString {
    SharedString::from(format!("icons/{name}.svg"))
}

/// A square line-icon element. Tint it at the call site with `.text_color(…)`
/// (gpui paints an SVG only when a text color is set).
pub fn icon(name: &str, size: f32) -> Svg {
    svg()
        .path(icon_path(name))
        .w(px(size))
        .h(px(size))
        .flex_none()
}

/// Asset path for the usage-ring arc at `pct` (0–100).
pub fn ring_path(pct: u32) -> SharedString {
    SharedString::from(format!("ring/{}.svg", pct.min(100)))
}

/// Asset path for the full-circle usage-ring track.
pub fn ring_track_path() -> SharedString {
    SharedString::from("ring/track.svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_known_icon_as_wrapped_svg() {
        let bytes = Assets.load("icons/spark.svg").unwrap().unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.starts_with("<svg"));
        assert!(s.contains("viewBox=\"0 0 24 24\""));
    }

    #[test]
    fn unknown_icon_is_none() {
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        assert!(Assets.load("totally/other.png").unwrap().is_none());
    }

    #[test]
    fn ring_arc_offset_tracks_percent() {
        let full = 2.0 * std::f32::consts::PI * RING_R;
        // 0% → dashoffset == full circumference (nothing drawn).
        assert!(ring_arc_svg(0.0).contains(&format!("stroke-dashoffset=\"{full:.2}\"")));
        // 100% → dashoffset 0 (full ring).
        assert!(ring_arc_svg(100.0).contains("stroke-dashoffset=\"0.00\""));
        // track + arc both load
        assert!(Assets.load("ring/track.svg").unwrap().is_some());
        assert!(Assets.load("ring/42.svg").unwrap().is_some());
    }
}
