//! Config file locations. On Windows the config root is `%APPDATA%\Tn`
//! (Roaming AppData), with `config.toml` and a `themes/` subdirectory under it.

use std::path::PathBuf;

use directories::BaseDirs;

/// The Tn config root: `%APPDATA%\Tn` on Windows (Roaming), the platform config
/// dir joined with `Tn` elsewhere. `None` if no home directory is resolvable.
pub fn config_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|b| b.config_dir().join("Tn"))
}

/// `<config_dir>/config.toml`.
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// `<config_dir>/themes` — where `*.toml` theme files live.
pub fn themes_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("themes"))
}
