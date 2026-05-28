//! Theme schema: ANSI 16 + terminal colors + UI chrome + window/agent accents.
//!
//! A theme is a **complete document** (like the bundled `tn-dark.toml`). On load,
//! a missing or malformed theme falls back wholesale to the built-in Tn Dark;
//! per-field theme inheritance is intentionally out of scope here (importers
//! produce complete themes). The `[appearance]`/`[font]` *config* inherits
//! per-field — see [`crate::config`].

use serde::{Deserialize, Serialize};

use crate::color::Color;

/// The authoritative built-in theme: the same file shipped in `config/themes/`.
/// Embedding it keeps a single source of truth and lets first-run write a copy.
pub const TN_DARK_TOML: &str = include_str!("../../../config/themes/tn-dark.toml");

/// Light/dark hint (used for follow-the-system switching later).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

/// Windows 11 window material.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backdrop {
    #[default]
    Mica,
    Acrylic,
    Solid,
}

/// Window corner style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Corner {
    #[default]
    Round,
    Sharp,
}

/// The 16 ANSI palette entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Ansi16 {
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
}

impl Ansi16 {
    /// Palette entries in index order 0..16, as RGB tuples.
    pub fn as_rgb(&self) -> [(u8, u8, u8); 16] {
        [
            self.black.rgb(), self.red.rgb(), self.green.rgb(), self.yellow.rgb(),
            self.blue.rgb(), self.magenta.rgb(), self.cyan.rgb(), self.white.rgb(),
            self.bright_black.rgb(), self.bright_red.rgb(), self.bright_green.rgb(),
            self.bright_yellow.rgb(), self.bright_blue.rgb(), self.bright_magenta.rgb(),
            self.bright_cyan.rgb(), self.bright_white.rgb(),
        ]
    }
}

/// Colors for the terminal drawing area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct TerminalColors {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    pub cursor_text: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
}

/// Semi-transparency / window material (Windows 11). `[ui.window]`.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct WindowChrome {
    #[serde(default)]
    pub backdrop: Backdrop,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub corner: Corner,
}

fn default_opacity() -> f32 {
    0.96
}

impl Default for WindowChrome {
    fn default() -> Self {
        Self {
            backdrop: Backdrop::Mica,
            opacity: default_opacity(),
            corner: Corner::Round,
        }
    }
}

/// UI chrome colors (window / tabs / panels / palette). `[ui]`, with nested
/// `[ui.window]`.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct UiColors {
    pub chrome_bg: Color,
    pub surface_1: Color,
    pub surface_2: Color,
    pub foreground: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub accent_alt: Color,
    pub tab_active_bg: Color,
    pub tab_inactive_fg: Color,
    pub block_border: Color,
    pub block_success: Color,
    pub block_error: Color,
    pub block_running: Color,
    pub palette_bg: Color,
    pub palette_selected: Color,
    #[serde(default)]
    pub window: WindowChrome,
}

/// AI agent accent colors. `[agents]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentColors {
    pub claude: Color,
    pub codex: Color,
}

impl Default for AgentColors {
    fn default() -> Self {
        Self {
            claude: Color::new(0xF0, 0x91, 0x6D),
            codex: Color::new(0x73, 0xDA, 0xCA),
        }
    }
}

/// A complete color theme. See `config/themes/tn-dark.toml` for the canonical
/// example and the authoritative default.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Theme {
    pub name: String,
    #[serde(default)]
    pub appearance: Mode,
    pub ansi: Ansi16,
    pub terminal: TerminalColors,
    pub ui: UiColors,
    #[serde(default)]
    pub agents: AgentColors,
}

impl Theme {
    /// The built-in Tn Dark theme (parsed from the embedded canonical file).
    /// The file is complete, so deserialization never invokes a `Default` — no
    /// recursion through [`Theme::default`].
    pub fn tn_dark() -> Self {
        toml::from_str(TN_DARK_TOML).expect("bundled tn-dark.toml must parse")
    }

    /// Parse a theme from TOML text.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::tn_dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_tn_dark_parses() {
        let t = Theme::tn_dark();
        assert_eq!(t.name, "Tn Dark");
        assert_eq!(t.appearance, Mode::Dark);
        assert_eq!(t.ansi.red, Color::new(0xF7, 0x76, 0x8E));
        assert_eq!(t.terminal.background, Color::new(0x1A, 0x1B, 0x26));
        assert_eq!(t.terminal.foreground, Color::new(0xC0, 0xCA, 0xF5));
        assert_eq!(t.ui.accent, Color::new(0x7A, 0xA2, 0xF7));
        assert_eq!(t.ui.window.backdrop, Backdrop::Acrylic);
        assert!((t.ui.window.opacity - 0.60).abs() < 1e-6);
        assert_eq!(t.agents.claude, Color::new(0xF0, 0x91, 0x6D));
    }

    #[test]
    fn ansi_index_order() {
        let rgb = Theme::tn_dark().ansi.as_rgb();
        assert_eq!(rgb[0], (0x15, 0x16, 0x1E)); // black
        assert_eq!(rgb[1], (0xF7, 0x76, 0x8E)); // red
        assert_eq!(rgb[15], (0xC0, 0xCA, 0xF5)); // bright white
    }

    #[test]
    fn missing_optional_sections_inherit_defaults() {
        // A theme that omits [agents] and [ui.window] still parses; those use
        // their Default. (All required sections present.)
        let mut toml = String::from("name = \"Mini\"\n");
        toml.push_str("[ansi]\n");
        for k in [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
            "bright_black", "bright_red", "bright_green", "bright_yellow", "bright_blue",
            "bright_magenta", "bright_cyan", "bright_white",
        ] {
            toml.push_str(&format!("{k} = \"#000000\"\n"));
        }
        toml.push_str("[terminal]\nbackground=\"#000000\"\nforeground=\"#FFFFFF\"\ncursor=\"#FFFFFF\"\ncursor_text=\"#000000\"\nselection_bg=\"#222222\"\nselection_fg=\"#FFFFFF\"\n");
        toml.push_str("[ui]\nchrome_bg=\"#000000\"\nsurface_1=\"#111111\"\nsurface_2=\"#222222\"\nforeground=\"#FFFFFF\"\nmuted=\"#888888\"\nborder=\"#333333\"\naccent=\"#7AA2F7\"\naccent_alt=\"#BB9AF7\"\ntab_active_bg=\"#000000\"\ntab_inactive_fg=\"#888888\"\nblock_border=\"#333333\"\nblock_success=\"#00FF00\"\nblock_error=\"#FF0000\"\nblock_running=\"#0000FF\"\npalette_bg=\"#111111\"\npalette_selected=\"#222222\"\n");
        let t = Theme::from_toml_str(&toml).expect("mini theme parses");
        assert_eq!(t.name, "Mini");
        assert_eq!(t.agents, AgentColors::default());
        assert_eq!(t.ui.window, WindowChrome::default());
    }
}
