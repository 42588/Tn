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

pub use color::{Color, ColorError};
pub use config::{
    Action, Appearance, BillingMode, Config, Font, General, Keybinding, Profile, ProfileKind,
    DEFAULT_CONFIG_TOML,
};
pub use paths::{config_dir, config_path, themes_dir};
pub use quick_terminal::{
    ease_out_cubic, lerp_rect, parse_hotkey, HotkeySpec, QuickTermPosition, QuickTerminal, Rect,
};
pub use theme::{
    Ansi16, AgentColors, Backdrop, Corner, Mode, TerminalColors, Theme, UiColors, WindowChrome,
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
        write_if_absent(&themes_path.join("tn-dark.toml"), TN_DARK_TOML, &themes_path);
    }

    let config = read_config(&config_file);
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
        assert_eq!(loaded.config.font.family, "CaskaydiaCove Nerd Font");
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
        assert_eq!(loaded.config.font.family, "CaskaydiaCove Nerd Font"); // inherited default

        let _ = fs::remove_dir_all(&dir);
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
