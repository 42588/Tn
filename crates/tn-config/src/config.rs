//! User config schema. Mirrors the Windows Terminal model (see
//! docs/参考资料.md §7): nested `[font]`/`[appearance]`, profiles as data, and
//! key bindings split into an action table + a binding table.
//!
//! Every field is `#[serde(default)]` so a partial `config.toml` inherits the
//! built-in defaults field by field — change one color and keep the rest.

use serde::{Deserialize, Serialize};

use crate::color::Color;
use crate::quick_terminal::QuickTerminal;
use crate::theme::Backdrop;

/// The authoritative default `config.toml` (written on first run).
pub const DEFAULT_CONFIG_TOML: &str = include_str!("../../../config/config.toml");

/// Top-level configuration. Sections each default field-by-field.
#[derive(Clone, Debug, PartialEq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub font: Font,
    pub appearance: Appearance,
    #[serde(default)]
    pub quick_terminal: QuickTerminal,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub keybindings: Vec<Keybinding>,
}

impl Config {
    /// Parse from TOML text.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// Serialize `profiles` as one or more `[[profiles]]` TOML blocks — used to
/// **append** a saved connection to an existing `config.toml` (the in-app "save
/// as connection", A2) without rewriting (and thus losing the comments in) the
/// rest of the file. `None` fields are omitted by the TOML serializer, so a
/// minimal SSH profile emits just `name` / `kind` / `host` / `user`.
pub fn profiles_toml_fragment(profiles: &[Profile]) -> Result<String, toml::ser::Error> {
    #[derive(Serialize)]
    struct Fragment<'a> {
        profiles: &'a [Profile],
    }
    toml::to_string(&Fragment { profiles })
}

/// How a pane's usage pill presents its readout. Clicking the pill cycles
/// through the concrete modes **per pane** (runtime, in memory); these values are
/// the *starting* mode for a new pane, chosen by config.
/// - `auto` (default): detect from the agent's auth — a subscription login
///   (Claude Pro/Max, ChatGPT) shows context `%`, a metered API key shows `$`.
///   Members and API users each get the right thing with zero config.
/// - `api`: always the USD cost estimate (`$0.00` for an unpriced/proxy model).
/// - `subscription`: always the context-usage percentage.
/// - `tokens`: always the session's total token throughput.
///
/// Set the global starting default via `[general].billing_mode`, or per agent via
/// `claude_billing` / `codex_billing` (a window can mix a subscription Claude and
/// an API Codex). The per-pane click overrides the starting default at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BillingMode {
    /// Detect from the agent's auth (the smart default — zero config).
    #[default]
    Auto,
    /// Always show the estimated USD cost.
    Api,
    /// Always show the context-usage percentage.
    Subscription,
    /// Always show the session's total token throughput.
    Tokens,
}

/// `[general]`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct General {
    /// Scrollback lines retained per session (not yet wired).
    pub scrollback_lines: usize,
    /// Confirm before closing a session that is still running (not yet wired).
    pub confirm_close: bool,
    /// Starting usage-pill mode for a new pane (see [`BillingMode`]); default
    /// `auto`. Each pane can then cycle its own pill at runtime by clicking it.
    #[serde(default)]
    pub billing_mode: BillingMode,
    /// Per-agent starting-mode overrides of [`billing_mode`]. `None` (default)
    /// inherits the global value. Lets one window default a Claude Pro/Max member
    /// to `%` and a Codex API user to `$` (or `tokens` for a proxy model).
    #[serde(default)]
    pub claude_billing: Option<BillingMode>,
    #[serde(default)]
    pub codex_billing: Option<BillingMode>,
}

impl Default for General {
    fn default() -> Self {
        Self {
            scrollback_lines: 5_000,
            confirm_close: true,
            billing_mode: BillingMode::default(),
            claude_billing: None,
            codex_billing: None,
        }
    }
}

/// `[font]`. `line_height` is a multiple of `size`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Font {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
    /// CJK / emoji fallback families (schema reserved; not yet applied).
    pub fallback: Vec<String>,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: "CaskaydiaCove Nerd Font".to_string(),
            size: 14.0,
            line_height: 1.3,
            fallback: Vec::new(),
        }
    }
}

impl Font {
    /// Pixel line height = `size * line_height`.
    pub fn line_height_px(&self) -> f32 {
        self.size * self.line_height
    }
}

/// `[appearance]`. Picks the active theme by name; `opacity`/`backdrop` override
/// the theme's window chrome when set (not yet applied).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub opacity: Option<f32>,
    pub backdrop: Option<Backdrop>,
    /// Flash the pane briefly when the terminal rings the bell (BEL / `\x07`).
    /// On by default — a quiet visual cue, no sound. (待优化清单 §3.8)
    pub visual_bell: bool,
    /// Also play the system beep on bell. Off by default (audible bells are
    /// widely disliked); opt in for parity with classic terminals.
    pub audio_bell: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "Tn Dark".to_string(),
            opacity: None,
            backdrop: None,
            visual_bell: true,
            audio_bell: false,
        }
    }
}

/// What kind of session a [`Profile`] launches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    #[default]
    Shell,
    Wsl,
    Ssh,
    Agent,
}

/// A session launcher entry (`[[profiles]]`). Consumed by the M4 command
/// palette; parsed and preserved now. Only `name` is required.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub kind: ProfileKind,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub distro: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub accent: Option<Color>,
    #[serde(default)]
    pub glyph: Option<String>,
}

/// A named action (`[[actions]]`): `{ id, command }`. A command may carry args
/// in later revisions; for now it's the action name.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Action {
    pub id: String,
    pub command: String,
}

/// A key binding (`[[keybindings]]`): `{ keys, id }` referencing an action id.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Keybinding {
    pub keys: String,
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_font_and_theme() {
        let c = Config::default();
        assert_eq!(c.font.family, "CaskaydiaCove Nerd Font");
        assert_eq!(c.font.size, 14.0);
        assert!((c.font.line_height_px() - 18.2).abs() < 1e-4);
        assert_eq!(c.appearance.theme, "Tn Dark");
        assert!(c.profiles.is_empty()); // defaults carry no profiles
    }

    #[test]
    fn bundled_default_config_parses() {
        let c = Config::from_toml_str(DEFAULT_CONFIG_TOML).expect("default config.toml parses");
        assert_eq!(c.font.family, "CaskaydiaCove Nerd Font");
        assert_eq!(c.appearance.theme, "Tn Dark");
        // Template ships example profiles + keybindings.
        assert!(c.profiles.iter().any(|p| p.kind == ProfileKind::Agent && p.agent.as_deref() == Some("claude")));
        assert!(c.keybindings.iter().any(|k| k.id == "new_tab"));
        // Quick Terminal section parses and matches its documented defaults.
        assert!(c.quick_terminal.enabled);
        assert_eq!(c.quick_terminal.hotkey, "ctrl+alt+space");
        assert_eq!(c.quick_terminal.position, crate::QuickTermPosition::Top);
    }

    #[test]
    fn quick_terminal_defaults_when_section_absent() {
        // A config with no [quick_terminal] still gets the built-in defaults.
        let c = Config::from_toml_str("[font]\nsize = 16.0\n").expect("partial parses");
        assert_eq!(c.quick_terminal, crate::QuickTerminal::default());
        assert!(c.quick_terminal.enabled);
    }

    #[test]
    fn partial_config_inherits_defaults() {
        let c = Config::from_toml_str("[font]\nsize = 16.0\n").expect("partial parses");
        assert_eq!(c.font.size, 16.0); // overridden
        assert_eq!(c.font.family, "CaskaydiaCove Nerd Font"); // inherited
        assert_eq!(c.appearance.theme, "Tn Dark"); // whole section inherited
    }

    #[test]
    fn bell_defaults_visual_on_audio_off_and_override() {
        // Default: quiet visual flash on, system beep off (待优化清单 §3.8).
        let c = Config::default();
        assert!(c.appearance.visual_bell);
        assert!(!c.appearance.audio_bell);
        // Both are overridable from [appearance]; other fields inherit.
        let c = Config::from_toml_str("[appearance]\nvisual_bell = false\naudio_bell = true\n")
            .expect("appearance bell keys parse");
        assert!(!c.appearance.visual_bell);
        assert!(c.appearance.audio_bell);
        assert_eq!(c.appearance.theme, "Tn Dark"); // inherited
    }

    #[test]
    fn ssh_profile_fragment_roundtrips() {
        // A2: a saved SSH connection serialized to a `[[profiles]]` block must
        // parse back identically — incl. a CJK name, a `host:port`, and an accent.
        let p = Profile {
            name: "我的服务器".into(),
            kind: ProfileKind::Ssh,
            command: None,
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: Some("ex.com:2222".into()),
            user: Some("ubuntu".into()),
            agent: None,
            accent: Some(Color::new(0x7A, 0xA2, 0xF7)),
            glyph: None,
        };
        let frag = profiles_toml_fragment(std::slice::from_ref(&p)).expect("serializes");
        assert!(frag.contains("[[profiles]]"));
        let parsed = Config::from_toml_str(&frag).expect("fragment is valid toml");
        let q = &parsed.profiles[0];
        assert_eq!(q.name, "我的服务器");
        assert_eq!(q.kind, ProfileKind::Ssh);
        assert_eq!(q.host.as_deref(), Some("ex.com:2222"));
        assert_eq!(q.user.as_deref(), Some("ubuntu"));
        assert_eq!(q.accent, Some(Color::new(0x7A, 0xA2, 0xF7)));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let c = Config::from_toml_str("[font]\nfamily = \"JetBrains Mono\"\nweird = 3\n")
            .expect("unknown keys tolerated");
        assert_eq!(c.font.family, "JetBrains Mono");
    }

    #[test]
    fn billing_mode_defaults_and_per_agent_overrides() {
        // Default: global api, no per-agent overrides (inherit).
        let c = Config::default();
        assert_eq!(c.general.billing_mode, BillingMode::Auto); // smart default
        assert_eq!(c.general.claude_billing, None);
        assert_eq!(c.general.codex_billing, None);
        // The owner's case: subscription Claude (%) + token-throughput Codex.
        let c = Config::from_toml_str(
            "[general]\nclaude_billing = \"subscription\"\ncodex_billing = \"tokens\"\n",
        )
        .expect("per-agent billing parses");
        assert_eq!(c.general.billing_mode, BillingMode::Auto); // inherited
        assert_eq!(c.general.claude_billing, Some(BillingMode::Subscription));
        assert_eq!(c.general.codex_billing, Some(BillingMode::Tokens));
    }
}
