//! User config schema. Mirrors the Windows Terminal model (see
//! docs/外部参考资料索引.md §7): nested `[font]`/`[appearance]`, profiles as data, and
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
    pub editor: Editor,
    #[serde(default)]
    pub quick_terminal: QuickTerminal,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// User-declared agent types (`[[agents]]`) — identity + capabilities for the
    /// Agent Host, so a new agent appears in Tn from config alone (no code change).
    #[serde(default)]
    pub agents: Vec<AgentManifest>,
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

/// Serialize `agents` as one or more `[[agents]]` TOML blocks — used to **append**
/// a user-created agent (the in-app "添加 Agent" form) to an existing `config.toml`
/// without rewriting (and losing the comments in) the rest of the file. `None`
/// fields are omitted by the TOML serializer, so a minimal agent emits just its
/// `id` / `aliases` / `manages_own_cursor` / `capabilities`.
pub fn agents_toml_fragment(agents: &[AgentManifest]) -> Result<String, toml::ser::Error> {
    #[derive(Serialize)]
    struct Fragment<'a> {
        agents: &'a [AgentManifest],
    }
    toml::to_string(&Fragment { agents })
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
    /// Scrollback lines retained per session (the agent/shell history you can wheel
    /// back through). Wired at `terminal_view`'s `Terminal::with_scrollback`. Grows
    /// lazily (memory scales with actual output, not the cap) and a backgrounded
    /// tab's grid is parked to disk (`swap_out_async`), so a generous default is
    /// affordable. This value *is* the bound (the engine retains exactly this many
    /// history lines); raise it in config for marathon runs.
    pub scrollback_lines: usize,
    /// Confirm before closing a session that is still running (not yet wired).
    pub confirm_close: bool,
    /// Starting usage-pill mode for a new pane (see [`BillingMode`]); default
    /// `auto`. Each pane can then cycle its own pill at runtime by clicking it.
    #[serde(default)]
    pub billing_mode: BillingMode,
    /// Refresh the usage-ring pricing/context table from a public price list on
    /// startup (LiteLLM JSON; cached, with a built-in fallback so offline still
    /// works). Default `true`. Set `false` to stay fully offline on the bundled
    /// table. See [`pricing_url`](Self::pricing_url).
    #[serde(default = "default_true")]
    pub pricing_auto_refresh: bool,
    /// Where to fetch the pricing table (LiteLLM's public `model_prices_and_
    /// context_window.json`). Overridable so a moved URL is fixable without a
    /// rebuild; an unreachable URL just leaves the built-in fallback in place.
    #[serde(default = "default_pricing_url")]
    pub pricing_url: String,
    /// Per-agent starting-mode overrides of [`billing_mode`]. `None` (default)
    /// inherits the global value. Lets one window default a Claude Pro/Max member
    /// to `%` and a Codex API user to `$` (or `tokens` for a proxy model).
    #[serde(default)]
    pub claude_billing: Option<BillingMode>,
    #[serde(default)]
    pub codex_billing: Option<BillingMode>,
    /// Agent-agnostic per-id billing overrides, keyed by `AgentId` string
    /// (`[general.billing] gemini = "subscription"`). Wins over the legacy
    /// `claude_billing`/`codex_billing` fields. Prefer [`billing_for`](Self::billing_for).
    #[serde(default)]
    pub billing: std::collections::HashMap<String, BillingMode>,
}

fn default_true() -> bool {
    true
}

/// LiteLLM's public, community-maintained price + context-window list (covers
/// Anthropic, OpenAI/Codex, and more). Costs are per token; tn-agent converts.
fn default_pricing_url() -> String {
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
        .to_string()
}

impl Default for General {
    fn default() -> Self {
        Self {
            // 5k truncated long agent conversations (BUG: 历史「太长就看不到」). Lazy
            // allocation + disk-park of idle tabs make 50k affordable.
            scrollback_lines: 50_000,
            confirm_close: true,
            pricing_auto_refresh: default_true(),
            pricing_url: default_pricing_url(),
            billing_mode: BillingMode::default(),
            claude_billing: None,
            codex_billing: None,
            billing: std::collections::HashMap::new(),
        }
    }
}

impl General {
    /// Starting billing-mode override for an agent id: the agent-agnostic
    /// `[general.billing]` map wins, then the legacy `claude_billing` /
    /// `codex_billing` fields, else `None` (inherit the global `billing_mode`).
    pub fn billing_for(&self, id: &str) -> Option<BillingMode> {
        self.billing.get(id).copied().or_else(|| match id {
            "claude" => self.claude_billing,
            "codex" => self.codex_billing,
            _ => None,
        })
    }
}

/// Editor animation level (`[editor] animations`). Drives the renderer's motion
/// policy; the actual effects (TnE-18) are separate. `subtle` is the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorAnimations {
    /// No motion: the caret is an instant inverse block, no glide / settle.
    Off,
    /// Gentle, perf-gated typing feedback (caret glide, char settle).
    #[default]
    Subtle,
    /// All `subtle` effects without the conservative perf caps.
    Full,
}

/// The motion policy a renderer should actually apply this frame, after folding in
/// runtime conditions (OS reduced-motion, high render load). Anything other than
/// the user's plain setting collapses to [`EffectiveMotion::Instant`] so motion
/// never fights performance or accessibility — see [`Editor::effective_motion`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveMotion {
    /// Snap everything immediately (no animation). The TnE-12 baseline behavior.
    Instant,
    /// Apply the gentle, perf-capped effects.
    Subtle,
    /// Apply effects without the conservative caps.
    Full,
}

/// `[editor]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Editor {
    /// Animation level for the editor / Quick Look renderer. Default `subtle`.
    pub animations: EditorAnimations,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            animations: EditorAnimations::default(),
        }
    }
}

impl Editor {
    /// Resolve the motion policy a renderer should apply, given runtime conditions.
    /// `off`, OS reduced-motion, or a high render load all force
    /// [`EffectiveMotion::Instant`] so the caret stays exact and input never lags
    /// (the renderer must behave exactly like TnE-12 in that case). Otherwise the
    /// user's `subtle` / `full` choice passes through.
    pub fn effective_motion(&self, reduced_motion: bool, high_load: bool) -> EffectiveMotion {
        if reduced_motion || high_load {
            return EffectiveMotion::Instant;
        }
        match self.animations {
            EditorAnimations::Off => EffectiveMotion::Instant,
            EditorAnimations::Subtle => EffectiveMotion::Subtle,
            EditorAnimations::Full => EffectiveMotion::Full,
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
    /// On by default — a quiet visual cue, no sound.
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

/// A user-declared agent type (`[[agents]]`): identity + presentation + capability
/// flags. Lets a new agent enter the Agent Host from config alone — launcher tile,
/// header accent, and capability slots — without a code change (the "config-level"
/// access tier). Telemetry (usage) still needs a built-in or external adapter; a
/// config-only agent hosts as a terminal (+ activity rail). `capabilities` lists the
/// enabled slots beyond the always-on `terminal` + `cwd_sync` + `git_diff` baseline
/// (e.g. `["usage", "transcript"]`). Non-PTY runtimes are opt-in through
/// `runtime_support`; networked runtimes still default to denied unless
/// `allow_network = true` is present, and the host layer must ask the user before
/// connecting.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AgentManifest {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub short: Option<String>,
    /// Command substrings that identify this agent (defaults to `[id]`).
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub accent: Option<Color>,
    #[serde(default)]
    pub glyph: Option<String>,
    /// The agent paints its own cursor (Ink TUI) → the terminal hides its block.
    #[serde(default)]
    pub manages_own_cursor: bool,
    /// Extra capability slots enabled beyond the terminal baseline.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Runtime locations/protocols this agent supports. Empty = PTY family
    /// (`local_pty`, `wsl_pty`, `ssh_pty`) for backward compatibility.
    #[serde(default)]
    pub runtime_support: Vec<String>,
    /// Whether this manifest may request networked runtimes (`http`,
    /// `websocket`, `remote_daemon`). False by default: network access is denied.
    /// True still means "requires user confirmation", not silent allow.
    #[serde(default)]
    pub allow_network: bool,
    /// Optional stdio/JSONL **telemetry sidecar** command. When set, a launched
    /// pane for this agent spawns it and reads realtime [`AgentEvent`]s (usage /
    /// transcript / status / permission) — the realtime (observation) tier
    /// **without** a built-in adapter. Whitespace-split into argv. A sidecar whose
    /// `runtime_support` is networked (`http`/`websocket`/`remote_daemon`) only
    /// spawns after user confirmation (and only if `allow_network = true`).
    #[serde(default)]
    pub sidecar: Option<String>,
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
        // Template ships one generic Agent profile (no Claude/Codex tiles) + keybindings.
        assert!(c
            .profiles
            .iter()
            .any(|p| p.kind == ProfileKind::Agent && p.agent.as_deref() == Some("agent")));
        assert!(c.agents.iter().any(|a| a.id == "agent"));
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
    fn editor_animations_default_subtle_and_parse() {
        // Absent section → subtle.
        let c = Config::from_toml_str("[font]\nsize = 16.0\n").expect("partial parses");
        assert_eq!(c.editor.animations, EditorAnimations::Subtle);
        // Explicit values parse (lowercase).
        for (toml, want) in [
            ("off", EditorAnimations::Off),
            ("subtle", EditorAnimations::Subtle),
            ("full", EditorAnimations::Full),
        ] {
            let c = Config::from_toml_str(&format!("[editor]\nanimations = \"{toml}\"\n"))
                .expect("editor parses");
            assert_eq!(c.editor.animations, want);
        }
    }

    #[test]
    fn editor_effective_motion_degrades_for_off_reduced_and_load() {
        let subtle = Editor {
            animations: EditorAnimations::Subtle,
        };
        // Plain conditions pass the user's choice through.
        assert_eq!(
            subtle.effective_motion(false, false),
            EffectiveMotion::Subtle
        );
        // Reduced-motion or high load → instant, regardless of setting.
        assert_eq!(
            subtle.effective_motion(true, false),
            EffectiveMotion::Instant
        );
        assert_eq!(
            subtle.effective_motion(false, true),
            EffectiveMotion::Instant
        );

        let full = Editor {
            animations: EditorAnimations::Full,
        };
        assert_eq!(full.effective_motion(false, false), EffectiveMotion::Full);
        assert_eq!(full.effective_motion(false, true), EffectiveMotion::Instant);

        let off = Editor {
            animations: EditorAnimations::Off,
        };
        // Off is always instant — the TnE-12 baseline.
        assert_eq!(off.effective_motion(false, false), EffectiveMotion::Instant);
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
        // Default: quiet visual flash on, system beep off.
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
        // The agent-agnostic accessor reads the legacy fields by id.
        assert_eq!(
            c.general.billing_for("claude"),
            Some(BillingMode::Subscription)
        );
        assert_eq!(c.general.billing_for("codex"), Some(BillingMode::Tokens));
        assert_eq!(c.general.billing_for("gemini"), None);
    }

    #[test]
    fn agents_manifest_parses_and_defaults_empty() {
        // No `[[agents]]` → empty (built-ins come from code, not config).
        assert!(Config::default().agents.is_empty());
        let c = Config::from_toml_str(
            "[[agents]]\nid = \"gemini\"\nlabel = \"Gemini CLI\"\naliases = [\"gemini\"]\ncapabilities = [\"usage\"]\nmanages_own_cursor = true\n",
        )
        .expect("agent manifest parses");
        assert_eq!(c.agents.len(), 1);
        assert_eq!(c.agents[0].id, "gemini");
        assert_eq!(c.agents[0].label.as_deref(), Some("Gemini CLI"));
        assert!(c.agents[0].manages_own_cursor);
        assert_eq!(c.agents[0].capabilities, vec!["usage".to_string()]);
        assert!(c.agents[0].runtime_support.is_empty());
        assert!(!c.agents[0].allow_network);
        assert_eq!(c.agents[0].sidecar, None); // no sidecar by default
    }

    #[test]
    fn agents_manifest_sidecar_parses_and_roundtrips() {
        // A telemetry sidecar command survives parse + fragment round-trip (so the
        // realtime tier is reachable from config alone).
        let c = Config::from_toml_str(
            "[[agents]]\nid = \"gemini\"\nsidecar = \"gemini-telemetry --json\"\n",
        )
        .expect("sidecar manifest parses");
        assert_eq!(
            c.agents[0].sidecar.as_deref(),
            Some("gemini-telemetry --json")
        );
        let frag = agents_toml_fragment(&c.agents).expect("serializes");
        let back = Config::from_toml_str(&frag).expect("fragment is valid toml");
        assert_eq!(
            back.agents[0].sidecar.as_deref(),
            Some("gemini-telemetry --json")
        );
    }

    #[test]
    fn agents_manifest_parses_runtime_and_network_policy() {
        let c = Config::from_toml_str(
            "[[agents]]\nid = \"bridge\"\nruntime_support = [\"structured\", \"http\", \"websocket\"]\nallow_network = true\ncapabilities = [\"usage\", \"permission_prompts\"]\n",
        )
        .expect("agent runtime manifest parses");
        let agent = &c.agents[0];
        assert_eq!(
            agent.runtime_support,
            vec![
                "structured".to_string(),
                "http".to_string(),
                "websocket".to_string(),
            ]
        );
        assert!(agent.allow_network);
        assert_eq!(
            agent.capabilities,
            vec!["usage".to_string(), "permission_prompts".to_string()]
        );
    }

    #[test]
    fn billing_open_to_arbitrary_ids_and_overrides_legacy() {
        // `[general.billing]` keys any agent id; it also wins over the legacy
        // claude_billing field for the same agent.
        let c = Config::from_toml_str(
            "[general]\nclaude_billing = \"tokens\"\n[general.billing]\nclaude = \"subscription\"\ngemini = \"api\"\n",
        )
        .expect("per-id billing parses");
        assert_eq!(
            c.general.billing_for("claude"),
            Some(BillingMode::Subscription)
        ); // map wins
        assert_eq!(c.general.billing_for("gemini"), Some(BillingMode::Api));
        assert_eq!(c.general.billing_for("aider"), None);
    }
}
