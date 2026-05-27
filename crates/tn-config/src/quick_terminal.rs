//! Quick Terminal (M5): the Quake/Guake-style drop-down floating terminal —
//! summoned anywhere by a global hotkey, slides in from a screen edge, hosts an
//! AI agent / shell, and slides away on blur.
//!
//! This module is **headless**: it owns the `[quick_terminal]` config schema plus
//! the pure geometry (edge placement + slide interpolation) and hotkey-string
//! parsing that the platform layer (`tn-ui` / Win32) drives. No GPUI / Win32 here,
//! so every piece below is unit-testable without a window.

use serde::{Deserialize, Serialize};

/// Where the Quick Terminal docks / slides in from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QuickTermPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
    Center,
}

/// `[quick_terminal]` — the drop-down floating terminal. Every field is
/// `#[serde(default)]` so a partial section inherits these defaults.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct QuickTerminal {
    /// Whether the global hotkey is registered at all.
    pub enabled: bool,
    /// Which edge it docks to (or `center`).
    pub position: QuickTermPosition,
    /// Height as a percentage of the monitor work-area (top / bottom / center).
    pub height_percent: f32,
    /// Width as a percentage of the monitor work-area (left / right / center).
    pub width_percent: f32,
    /// Slide animation duration in milliseconds (`0` = snap, no animation).
    pub animation_ms: u64,
    /// Hide automatically when the window loses focus.
    pub autohide: bool,
    /// Global hotkey that toggles it, e.g. `"ctrl+alt+space"`. See [`parse_hotkey`].
    pub hotkey: String,
    /// Name of a `[[profiles]]` entry to launch inside it. `None` = default pwsh.
    pub profile: Option<String>,
}

impl Default for QuickTerminal {
    fn default() -> Self {
        Self {
            enabled: true,
            position: QuickTermPosition::Top,
            height_percent: 45.0,
            width_percent: 60.0,
            animation_ms: 150,
            autohide: true,
            hotkey: "ctrl+alt+space".to_string(),
            profile: None,
        }
    }
}

/// A screen rectangle in device-independent pixels. Unit-agnostic on purpose so
/// the geometry stays headless; `tn-ui` converts to/from a GPUI `Bounds`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }
}

impl QuickTerminal {
    /// The on-screen ("revealed") placement inside the monitor `work` area
    /// (work area = monitor minus taskbar). Percentages are clamped to a sane
    /// range so a misconfigured value never produces a zero-size or oversized
    /// window.
    pub fn shown_rect(&self, work: Rect) -> Rect {
        let hpct = (self.height_percent / 100.0).clamp(0.1, 1.0);
        let wpct = (self.width_percent / 100.0).clamp(0.1, 1.0);
        match self.position {
            QuickTermPosition::Top => {
                Rect::new(work.x, work.y, work.width, work.height * hpct)
            }
            QuickTermPosition::Bottom => {
                let h = work.height * hpct;
                Rect::new(work.x, work.y + work.height - h, work.width, h)
            }
            QuickTermPosition::Left => {
                Rect::new(work.x, work.y, work.width * wpct, work.height)
            }
            QuickTermPosition::Right => {
                let w = work.width * wpct;
                Rect::new(work.x + work.width - w, work.y, w, work.height)
            }
            QuickTermPosition::Center => {
                let w = work.width * wpct;
                let h = work.height * hpct;
                Rect::new(
                    work.x + (work.width - w) / 2.0,
                    work.y + (work.height - h) / 2.0,
                    w,
                    h,
                )
            }
        }
    }

    /// The off-screen ("hidden") placement: same size as [`shown_rect`], shifted
    /// fully past the docking edge so the slide animation travels exactly its own
    /// extent. `Center` slides down from above (no edge of its own).
    pub fn hidden_rect(&self, work: Rect) -> Rect {
        let shown = self.shown_rect(work);
        match self.position {
            QuickTermPosition::Top | QuickTermPosition::Center => {
                Rect::new(shown.x, work.y - shown.height, shown.width, shown.height)
            }
            QuickTermPosition::Bottom => {
                Rect::new(shown.x, work.y + work.height, shown.width, shown.height)
            }
            QuickTermPosition::Left => {
                Rect::new(work.x - shown.width, shown.y, shown.width, shown.height)
            }
            QuickTermPosition::Right => {
                Rect::new(work.x + work.width, shown.y, shown.width, shown.height)
            }
        }
    }

    /// Interpolated rect for an animation progress `t` in `[0, 1]`, where `0` is
    /// fully hidden and `1` is fully shown. Eased with `ease_out_cubic` for a
    /// natural slide; the size is constant (only the position moves), so the
    /// platform can use a move-only `SetWindowPos` and avoid swapchain churn.
    pub fn frame_rect(&self, work: Rect, t: f32) -> Rect {
        let hidden = self.hidden_rect(work);
        let shown = self.shown_rect(work);
        lerp_rect(hidden, shown, ease_out_cubic(t.clamp(0.0, 1.0)))
    }
}

/// Component-wise linear interpolation between two rects.
pub fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Rect::new(l(a.x, b.x), l(a.y, b.y), l(a.width, b.width), l(a.height, b.height))
}

/// Cubic ease-out: fast start, gentle settle. `f(0)=0`, `f(1)=1`.
pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u
}

/// A parsed global hotkey: a set of modifiers plus a single key token. The
/// platform layer maps `key` to a virtual-key code and the booleans to the
/// OS modifier bitmask.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// The non-modifier key token, lowercased (e.g. `"space"`, `"a"`, `"f5"`, `"`"`).
    pub key: String,
}

/// Parse a hotkey string like `"ctrl+alt+space"` into a [`HotkeySpec`].
///
/// Tokens are `+`-separated and case-insensitive. Recognized modifiers:
/// `ctrl`/`control`, `alt`/`option`, `shift`, `win`/`super`/`cmd`/`meta`. Exactly
/// one non-modifier token is required, and at least one of ctrl/alt/win must be
/// present (a global hotkey on `shift`+key alone is unreliable). Returns `None`
/// for anything malformed so the caller can fall back / warn.
pub fn parse_hotkey(s: &str) -> Option<HotkeySpec> {
    let mut spec = HotkeySpec::default();
    let mut key: Option<String> = None;
    for raw in s.split('+') {
        let tok = raw.trim().to_ascii_lowercase();
        if tok.is_empty() {
            continue;
        }
        match tok.as_str() {
            "ctrl" | "control" => spec.ctrl = true,
            "alt" | "option" => spec.alt = true,
            "shift" => spec.shift = true,
            "win" | "super" | "cmd" | "meta" => spec.win = true,
            other => {
                if key.is_some() {
                    return None; // more than one non-modifier key
                }
                key = Some(other.to_string());
            }
        }
    }
    let key = key?;
    if !(spec.ctrl || spec.alt || spec.win) {
        return None; // require a global-friendly modifier
    }
    spec.key = key;
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect { x: 0.0, y: 0.0, width: 1000.0, height: 800.0 };

    #[test]
    fn defaults_are_sane() {
        let q = QuickTerminal::default();
        assert!(q.enabled);
        assert_eq!(q.position, QuickTermPosition::Top);
        assert_eq!(q.hotkey, "ctrl+alt+space");
        assert!(q.profile.is_none());
        assert_eq!(parse_hotkey(&q.hotkey).unwrap().key, "space");
    }

    #[test]
    fn top_spans_width_and_docks_top() {
        let q = QuickTerminal { position: QuickTermPosition::Top, height_percent: 50.0, ..Default::default() };
        let s = q.shown_rect(WORK);
        assert_eq!(s, Rect::new(0.0, 0.0, 1000.0, 400.0));
        // Hidden is fully above the work area.
        let h = q.hidden_rect(WORK);
        assert_eq!(h, Rect::new(0.0, -400.0, 1000.0, 400.0));
    }

    #[test]
    fn bottom_docks_bottom() {
        let q = QuickTerminal { position: QuickTermPosition::Bottom, height_percent: 25.0, ..Default::default() };
        let s = q.shown_rect(WORK);
        assert_eq!(s, Rect::new(0.0, 600.0, 1000.0, 200.0));
        let h = q.hidden_rect(WORK);
        assert_eq!(h, Rect::new(0.0, 800.0, 1000.0, 200.0)); // just below work area
    }

    #[test]
    fn left_and_right_span_height() {
        let l = QuickTerminal { position: QuickTermPosition::Left, width_percent: 30.0, ..Default::default() };
        assert_eq!(l.shown_rect(WORK), Rect::new(0.0, 0.0, 300.0, 800.0));
        assert_eq!(l.hidden_rect(WORK), Rect::new(-300.0, 0.0, 300.0, 800.0));

        let r = QuickTerminal { position: QuickTermPosition::Right, width_percent: 40.0, ..Default::default() };
        assert_eq!(r.shown_rect(WORK), Rect::new(600.0, 0.0, 400.0, 800.0));
        assert_eq!(r.hidden_rect(WORK), Rect::new(1000.0, 0.0, 400.0, 800.0));
    }

    #[test]
    fn center_is_centered() {
        let q = QuickTerminal {
            position: QuickTermPosition::Center,
            width_percent: 50.0,
            height_percent: 50.0,
            ..Default::default()
        };
        let s = q.shown_rect(WORK);
        assert_eq!(s, Rect::new(250.0, 200.0, 500.0, 400.0));
    }

    #[test]
    fn frame_endpoints_match_hidden_and_shown() {
        let q = QuickTerminal { position: QuickTermPosition::Top, height_percent: 50.0, ..Default::default() };
        assert_eq!(q.frame_rect(WORK, 0.0), q.hidden_rect(WORK));
        assert_eq!(q.frame_rect(WORK, 1.0), q.shown_rect(WORK));
        // Midway, size is unchanged and y is between hidden and shown.
        let mid = q.frame_rect(WORK, 0.5);
        assert_eq!(mid.height, 400.0);
        assert!(mid.y > -400.0 && mid.y < 0.0);
    }

    #[test]
    fn percentages_are_clamped() {
        let q = QuickTerminal { position: QuickTermPosition::Top, height_percent: 999.0, ..Default::default() };
        assert_eq!(q.shown_rect(WORK).height, 800.0); // clamped to 100%
        let q0 = QuickTerminal { position: QuickTermPosition::Top, height_percent: 0.0, ..Default::default() };
        assert_eq!(q0.shown_rect(WORK).height, 80.0); // clamped to 10%
    }

    #[test]
    fn ease_out_cubic_endpoints() {
        assert!((ease_out_cubic(0.0)).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
        assert!(ease_out_cubic(0.5) > 0.5); // ease-out is ahead of linear early
    }

    #[test]
    fn parse_hotkey_basic() {
        let h = parse_hotkey("ctrl+alt+space").unwrap();
        assert!(h.ctrl && h.alt && !h.shift && !h.win);
        assert_eq!(h.key, "space");
    }

    #[test]
    fn parse_hotkey_case_and_aliases() {
        let h = parse_hotkey("Control + Super + F5").unwrap();
        assert!(h.ctrl && h.win);
        assert_eq!(h.key, "f5");
    }

    #[test]
    fn parse_hotkey_rejects_no_modifier_or_no_key() {
        assert!(parse_hotkey("space").is_none()); // no modifier
        assert!(parse_hotkey("shift+a").is_none()); // shift alone is unreliable
        assert!(parse_hotkey("ctrl+alt").is_none()); // no non-modifier key
        assert!(parse_hotkey("ctrl+a+b").is_none()); // two keys
    }
}
