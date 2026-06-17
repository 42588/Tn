//! Tn configuration + theming. Headless: TOML schema, file locations, theme
//! loading, first-run defaults. The GPUI layer (`tn-ui`) reads a [`Loaded`] and
//! maps the terminal color subset into `tn_core::Palette`.
//!
//! Layering: built-in defaults ← `%APPDATA%\Tn\config.toml`. On first run the
//! bundled default config and the Tn Dark theme are written to disk so users
//! have a commented starting point. Anything unreadable falls back to built-ins
//! (logged via `tracing`) — [`load`] never fails.

mod color;
mod config;
mod paths;
mod quick_terminal;
mod theme;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub use color::{Color, ColorError, ACCENT_SWATCHES};
pub use config::{
    Action, AgentManifest, Appearance, BillingMode, Config, Editor, EditorAnimations,
    EffectiveMotion, Font, General, Keybinding, Profile, ProfileKind, DEFAULT_CONFIG_TOML,
    DEFAULT_SCROLLBACK_LINES, LEGACY_DEFAULT_SCROLLBACK_LINES,
};
pub use paths::{config_dir, config_path, themes_dir};
pub use quick_terminal::{
    ease_out_cubic, ease_out_back, ease_in_back, lerp_rect, parse_hotkey, HotkeySpec, QuickTermPosition, QuickTerminal, Rect,
};
pub use theme::{
    AgentColors, Ansi16, Backdrop, Corner, Mode, TerminalColors, Theme, UiColors, WindowChrome,
    TN_DARK_TOML,
};

/// The fully-resolved configuration the UI consumes: the parsed [`Config`], the
/// active [`Theme`] (resolved by `appearance.theme`), every available theme, and
/// the path the config was read from (if any).
#[derive(Clone, Debug)]
pub struct Loaded {
    pub config: Config,
    pub theme: Theme,
    pub themes: HashMap<String, Theme>,
    pub config_path: Option<PathBuf>,
}

impl Loaded {
    /// Built-in defaults only (no disk access).
    pub fn builtin() -> Self {
        let mut themes = HashMap::new();
        let tn_dark = Theme::tn_dark();
        themes.insert(tn_dark.name.clone(), tn_dark.clone());
        Self {
            config: Config::default(),
            theme: tn_dark,
            themes,
            config_path: None,
        }
    }

    /// Shorthand for `&self.config.font`.
    pub fn font(&self) -> &Font {
        &self.config.font
    }
}

impl Default for Loaded {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Load configuration from the standard location (`%APPDATA%\Tn`), writing the
/// default config + bundled theme on first run. Falls back to built-ins if no
/// config directory can be resolved.
pub fn load() -> Loaded {
    match paths::config_dir() {
        Some(dir) => load_from(&dir, true),
        None => Loaded::builtin(),
    }
}

/// Load configuration from an explicit directory. When `write_defaults` is set,
/// a missing `config.toml` / `themes/tn-dark.toml` are created from the embedded
/// canonical copies. Testable without touching the real config location.
pub fn load_from(dir: &Path, write_defaults: bool) -> Loaded {
    let config_file = dir.join("config.toml");
    let themes_path = dir.join("themes");

    if write_defaults {
        write_if_absent(&config_file, DEFAULT_CONFIG_TOML, dir);
        // tn-dark.toml 是 app 管辖的内置主题拷贝:随版本**同步覆盖**。曾用
        // write_if_absent,导致改版前的旧调色永久滞留用户目录(磷光主题从未
        // 到达真机的根因,见 design/原型与真机截图差异总结.md §1)。自定义
        // 主题请另存新文件名,那些文件永不被触碰。
        sync_managed(
            &themes_path.join("tn-dark.toml"),
            TN_DARK_TOML,
            &themes_path,
        );
    }

    let mut config = read_config(&config_file);
    lift_legacy_scrollback_default(&mut config);
    let themes = load_themes(&themes_path);
    let theme = themes
        .get(&config.appearance.theme)
        .cloned()
        .unwrap_or_else(Theme::tn_dark);

    Loaded {
        config,
        theme,
        themes,
        config_path: Some(config_file),
    }
}

/// Append a `[[profiles]]` entry to the user's `config.toml`, preserving
/// everything already in it (comments and all). The in-app "save as connection"
/// (A2) uses this to persist a named SSH connection so it survives restarts and
/// is hand-editable. Errors if no config directory can be resolved.
pub fn append_profile(profile: &Profile) -> std::io::Result<()> {
    let path = config_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
    append_profile_to(&path, profile)
}

/// [`append_profile`] targeting an explicit file — testable without touching the
/// real config location. Creates the file (and parent dir) if absent. The new
/// block is appended after a blank line so it never merges into a preceding
/// table; appending a fresh `[[profiles]]` header at EOF is valid regardless of
/// what comes before it.
pub fn append_profile_to(path: &Path, profile: &Profile) -> std::io::Result<()> {
    let fragment = config::profiles_toml_fragment(std::slice::from_ref(profile))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    append_fragment_to(path, &fragment)
}

/// Append a `[[agents]]` manifest to the user's `config.toml`, preserving
/// existing comments/format. The in-app "添加 Agent" form uses this so a
/// user-created agent (identity + capabilities for the Agent Host) survives
/// restarts and stays hand-editable. Errors if no config directory resolves.
pub fn append_agent(manifest: &AgentManifest) -> std::io::Result<()> {
    let path = config_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
    append_agent_to(&path, manifest)
}

/// [`append_agent`] targeting an explicit file — testable without touching the
/// real config location.
pub fn append_agent_to(path: &Path, manifest: &AgentManifest) -> std::io::Result<()> {
    let fragment = config::agents_toml_fragment(std::slice::from_ref(manifest))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    append_fragment_to(path, &fragment)
}

/// Append a serialized `[[…]]` block to `path` after a blank line (so it never
/// merges into a preceding table; a fresh array-of-tables header at EOF is valid
/// regardless of what precedes it). Creates the file + parent dir if absent.
/// Shared by [`append_profile_to`] / [`append_agent_to`].
fn append_fragment_to(path: &Path, fragment: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut text = fs::read_to_string(path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push('\n');
    text.push_str(fragment);
    fs::write(path, text)
}

/// Remove a matching `[[profiles]]` entry from the user's `config.toml`,
/// preserving unrelated text and comments. Returns `Ok(true)` when a block was
/// removed. The match is exact on the serialized identity fields the SSH
/// connector owns (`name`, `kind`, `host`, `user`).
pub fn remove_profile(profile: &Profile) -> std::io::Result<bool> {
    let path = config_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
    remove_profile_from(&path, profile)
}

/// [`remove_profile`] targeting an explicit file — testable without touching the
/// real config location. This scans top-level `[[profiles]]` blocks, parses each
/// candidate block as TOML, and removes only the first exact match.
pub fn remove_profile_from(path: &Path, profile: &Profile) -> std::io::Result<bool> {
    let text = fs::read_to_string(path)?;
    let Some(range) = find_profile_block(&text, profile)? else {
        return Ok(false);
    };
    let mut out = text;
    out.replace_range(range, "");
    fs::write(path, out)?;
    Ok(true)
}

/// Remove the first `[[agents]]` block whose `id` matches, preserving unrelated
/// text/comments. Returns `Ok(true)` when a block was removed. The in-app agent
/// editor uses this to delete a custom agent (or replace one when editing).
pub fn remove_agent(id: &str) -> std::io::Result<bool> {
    let path = config_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
    remove_agent_from(&path, id)
}

/// [`remove_agent`] targeting an explicit file — testable without touching the
/// real config location.
pub fn remove_agent_from(path: &Path, id: &str) -> std::io::Result<bool> {
    let text = fs::read_to_string(path)?;
    let Some(range) = find_agent_block(&text, id)? else {
        return Ok(false);
    };
    let mut out = text;
    out.replace_range(range, "");
    fs::write(path, out)?;
    Ok(true)
}

fn find_profile_block(
    text: &str,
    needle: &Profile,
) -> std::io::Result<Option<std::ops::Range<usize>>> {
    for range in block_ranges(text, "[[profiles]]") {
        let block = &text[range.clone()];
        let parsed = Config::from_toml_str(block)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if parsed
            .profiles
            .iter()
            .any(|p| profile_identity_eq(p, needle))
        {
            return Ok(Some(trim_block_range(text, range)));
        }
    }
    Ok(None)
}

/// First `[[agents]]` block declaring `id` (parsed per-block so we match on the
/// real `id` key, not a substring). Mirror of [`find_profile_block`].
fn find_agent_block(text: &str, id: &str) -> std::io::Result<Option<std::ops::Range<usize>>> {
    for range in block_ranges(text, "[[agents]]") {
        let block = &text[range.clone()];
        let parsed = Config::from_toml_str(block)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if parsed.agents.iter().any(|a| a.id == id) {
            return Ok(Some(trim_block_range(text, range)));
        }
    }
    Ok(None)
}

fn profile_identity_eq(a: &Profile, b: &Profile) -> bool {
    a.name == b.name && a.kind == b.kind && a.host == b.host && a.user == b.user
}

/// Byte ranges of each top-level array-of-tables block with the given `header`
/// (`[[profiles]]` / `[[agents]]`): from a header line to the next top-level `[`
/// (any table / array header). Shared by the comment-preserving block removers.
fn block_ranges(text: &str, header: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut current: Option<usize> = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with(header) {
            if let Some(start) = current.replace(pos) {
                ranges.push(start..pos);
            }
        } else if current.is_some() && trimmed.starts_with('[') {
            if let Some(start) = current.take() {
                ranges.push(start..pos);
            }
        }
        pos += line.len();
    }
    if let Some(start) = current {
        ranges.push(start..text.len());
    }
    ranges
}

fn trim_block_range(text: &str, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    // A2 append writes a blank separator before each saved profile. Remove that
    // single separator too so delete does not leave a growing trail of blank
    // lines, but keep user comments immediately above the block.
    let mut start = range.start;
    if start >= 2 && &text.as_bytes()[start - 2..start] == b"\n\n" {
        start -= 1;
    }
    start..range.end
}

/// Write `contents` to `file` if it doesn't already exist, creating `parent`.
fn write_if_absent(file: &Path, contents: &str, parent: &Path) {
    if file.exists() {
        return;
    }
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let _ = fs::write(file, contents);
}

/// Keep an app-managed file byte-identical to the bundled copy:missing **或**
/// 内容不同都重写。内置主题用它,使主题改版能到达存量用户目录(对应
/// write_if_absent 的「首启冻结」坑);用户自定义内容不属于受管文件。
fn sync_managed(file: &Path, contents: &str, parent: &Path) {
    if fs::read_to_string(file).is_ok_and(|cur| cur == contents) {
        return;
    }
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let _ = fs::write(file, contents);
}

/// Read + parse `config.toml`, falling back to defaults on any error.
fn read_config(file: &Path) -> Config {
    match fs::read_to_string(file) {
        Ok(text) => match Config::from_toml_str(&text) {
            Ok(c) => c,
            Err(_) => Config::default(),
        },
        Err(_) => Config::default(),
    }
}

fn lift_legacy_scrollback_default(config: &mut Config) {
    if config.general.scrollback_lines == LEGACY_DEFAULT_SCROLLBACK_LINES {
        config.general.scrollback_lines = DEFAULT_SCROLLBACK_LINES;
    }
}

/// Load every `*.toml` theme in `dir`, keyed by `Theme::name`. The built-in
/// Tn Dark is always present (a same-named on-disk theme overrides it).
fn load_themes(dir: &Path) -> HashMap<String, Theme> {
    let mut themes = HashMap::new();
    let tn_dark = Theme::tn_dark();
    themes.insert(tn_dark.name.clone(), tn_dark);

    let Ok(entries) = fs::read_dir(dir) else {
        return themes;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match fs::read_to_string(&path).map(|s| Theme::from_toml_str(&s)) {
            Ok(Ok(theme)) => {
                themes.insert(theme.name.clone(), theme);
            }
            Ok(Err(_)) | Err(_) => {} // invalid / unreadable theme — skipped
        }
    }
    themes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_temp() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("tn-config-test-{}-{nanos}-{n}", std::process::id()))
    }

    #[test]
    fn first_run_writes_defaults_and_reads_back() {
        let dir = unique_temp();
        let loaded = load_from(&dir, true);

        assert!(dir.join("config.toml").exists());
        assert!(dir.join("themes").join("tn-dark.toml").exists());
        assert_eq!(loaded.theme.name, "Tn Dark");
        assert_eq!(loaded.config.font.family, "JetBrainsMono Nerd Font");
        assert!(loaded.themes.contains_key("Tn Dark"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_run_does_not_overwrite_user_edits() {
        let dir = unique_temp();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), "[font]\nsize = 20.0\n").unwrap();

        let loaded = load_from(&dir, true);
        assert_eq!(loaded.config.font.size, 20.0); // user value kept
        assert_eq!(loaded.config.font.family, "JetBrainsMono Nerd Font"); // inherited default

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_default_scrollback_is_lifted_on_load() {
        let dir = unique_temp();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.toml"),
            "[general]\nscrollback_lines = 5000\n",
        )
        .unwrap();

        let loaded = load_from(&dir, true);
        assert_eq!(
            loaded.config.general.scrollback_lines, 50_000,
            "old first-run configs used 5k and must inherit the bugfix in memory"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_builtin_theme_is_refreshed_on_load() {
        // 真机坑位:首启写入的 tn-dark.toml 旧调色永不更新,磷光改版到不了
        // 存量用户目录(差异总结 §1 根因)。受管主题文件必须随版本同步。
        let dir = unique_temp();
        let themes = dir.join("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("tn-dark.toml"),
            "name = \"Tn Dark\"\n[ui]\naccent = \"#7AA2F7\"\n",
        )
        .unwrap();

        let loaded = load_from(&dir, true);
        let on_disk = fs::read_to_string(themes.join("tn-dark.toml")).unwrap();
        assert_eq!(on_disk, TN_DARK_TOML); // 旧拷贝被内置版覆盖
        assert_eq!(loaded.theme.ui.accent, Color::new(0x5B, 0xE7, 0xC4)); // 磷光生效

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_profile_preserves_existing_and_adds_entry() {
        // A2: appending a saved SSH connection must keep the user's edits intact
        // (comments / other keys) and parse back as a profile.
        let dir = unique_temp();
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        fs::write(&cfg, "[font]\nsize = 20.0\n").unwrap();

        let p = Profile {
            name: "WSL Test".into(),
            kind: ProfileKind::Ssh,
            command: None,
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: Some("172.24.16.162:2222".into()),
            user: Some("root".into()),
            agent: None,
            accent: None,
            glyph: None,
        };
        append_profile_to(&cfg, &p).unwrap();

        let parsed = Config::from_toml_str(&fs::read_to_string(&cfg).unwrap())
            .expect("still valid toml after append");
        assert_eq!(parsed.font.size, 20.0); // user edit preserved
        let saved = parsed
            .profiles
            .iter()
            .find(|x| x.name == "WSL Test")
            .expect("profile appended");
        assert_eq!(saved.kind, ProfileKind::Ssh);
        assert_eq!(saved.host.as_deref(), Some("172.24.16.162:2222"));
        assert_eq!(saved.user.as_deref(), Some("root"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_profile_creates_file_when_absent() {
        let dir = unique_temp();
        let cfg = dir.join("config.toml");
        let p = Profile {
            name: "srv".into(),
            kind: ProfileKind::Ssh,
            command: None,
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: Some("h".into()),
            user: None,
            agent: None,
            accent: None,
            glyph: None,
        };
        append_profile_to(&cfg, &p).unwrap();
        let parsed = Config::from_toml_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(parsed.profiles.iter().any(|x| x.name == "srv"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_profile_preserves_comments_and_other_profiles() {
        let dir = unique_temp();
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        fs::write(
            &cfg,
            r#"# keep this header
[font]
size = 20.0

[[profiles]]
name = "keep"
kind = "ssh"
host = "keep.example"
user = "root"

# remove only the next profile
[[profiles]]
name = "drop"
kind = "ssh"
host = "drop.example:2222"
user = "admin"

[appearance]
theme = "Tn Dark"
"#,
        )
        .unwrap();
        let drop = Profile {
            name: "drop".into(),
            kind: ProfileKind::Ssh,
            command: None,
            args: Vec::new(),
            cwd: None,
            distro: None,
            host: Some("drop.example:2222".into()),
            user: Some("admin".into()),
            agent: None,
            accent: None,
            glyph: None,
        };

        assert!(remove_profile_from(&cfg, &drop).unwrap());
        let text = fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("# keep this header"));
        assert!(text.contains("# remove only the next profile"));
        assert!(text.contains("name = \"keep\""));
        assert!(!text.contains("name = \"drop\""));
        assert!(text.contains("[appearance]"));
        let parsed = Config::from_toml_str(&text).expect("still valid toml after remove");
        assert_eq!(parsed.font.size, 20.0);
        assert!(parsed.profiles.iter().any(|p| p.name == "keep"));
        assert!(!parsed.profiles.iter().any(|p| p.name == "drop"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_agent_preserves_existing_and_parses_back() {
        // The in-app "添加 Agent" form appends an `[[agents]]` manifest; it must
        // keep user edits/comments and round-trip (incl. a CJK label + accent).
        let dir = unique_temp();
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        fs::write(&cfg, "# my header\n[font]\nsize = 18.0\n").unwrap();

        let m = AgentManifest {
            id: "qwen".into(),
            label: Some("通义千问".into()),
            short: Some("Qwen".into()),
            aliases: vec!["qwen".into()],
            accent: Some(Color::new(0x73, 0xDA, 0xCA)),
            glyph: Some("spark".into()),
            manages_own_cursor: true,
            capabilities: Vec::new(),
            runtime_support: Vec::new(),
            allow_network: false,
            sidecar: None,
        };
        append_agent_to(&cfg, &m).unwrap();

        let text = fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("# my header")); // comment preserved
        let parsed = Config::from_toml_str(&text).expect("still valid toml after append");
        assert_eq!(parsed.font.size, 18.0); // user edit preserved
        let saved = parsed
            .agents
            .iter()
            .find(|a| a.id == "qwen")
            .expect("agent appended");
        assert_eq!(saved.label.as_deref(), Some("通义千问"));
        assert_eq!(saved.accent, Some(Color::new(0x73, 0xDA, 0xCA)));
        assert!(saved.manages_own_cursor);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_agent_preserves_comments_and_other_agents() {
        let dir = unique_temp();
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        fs::write(
            &cfg,
            r#"# keep me
[[agents]]
id = "keep"
label = "Keep"

# drop only the next agent
[[agents]]
id = "drop"
label = "Drop"

[[profiles]]
name = "P"
kind = "agent"
agent = "keep"
command = "keep"
"#,
        )
        .unwrap();

        assert!(remove_agent_from(&cfg, "drop").unwrap());
        let text = fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("# keep me"));
        assert!(text.contains("# drop only the next agent")); // comment kept
        assert!(text.contains("id = \"keep\""));
        assert!(!text.contains("id = \"drop\""));
        assert!(text.contains("[[profiles]]")); // unrelated block untouched
        let parsed = Config::from_toml_str(&text).expect("valid toml after remove");
        assert!(parsed.agents.iter().any(|a| a.id == "keep"));
        assert!(!parsed.agents.iter().any(|a| a.id == "drop"));

        // Removing a non-existent id is a no-op (Ok(false)).
        assert!(!remove_agent_from(&cfg, "nope").unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn accent_swatches_are_nonempty_and_distinct() {
        // The agent editor's color picker reads these; ensure they exist and the
        // labels are unique (so two swatches never read identically).
        assert!(!ACCENT_SWATCHES.is_empty());
        let mut labels: Vec<&str> = ACCENT_SWATCHES.iter().map(|(l, _)| *l).collect();
        labels.sort_unstable();
        let n = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), n, "swatch labels must be distinct");
    }

    #[test]
    fn unknown_theme_falls_back_to_tn_dark() {
        let dir = unique_temp();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), "[appearance]\ntheme = \"Nope\"\n").unwrap();

        let loaded = load_from(&dir, false);
        assert_eq!(loaded.theme.name, "Tn Dark");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_has_tn_dark() {
        let l = Loaded::builtin();
        assert_eq!(l.theme.name, "Tn Dark");
        assert!(l.config_path.is_none());
    }
}
