use tn_pty::remote_fs::RemotePath;

use crate::workspace::PaneId;

/// Where the in-app directory picker reads from. SSH browses over SFTP; WSL
/// browses the local `\\wsl$\<distro>` UNC tree via `std::fs` — both present the
/// same Linux-style `RemotePath` navigation so the UI/keys are identical.
#[derive(Clone)]
pub(crate) enum PickerSource {
    Ssh(tn_pty::SshConfig),
    Wsl { distro: String },
}

/// One navigable entry (directories drive navigation; files are listed by the
/// backend but filtered out of [`RemoteDirPicker::visible_dirs`]). Decoupled from
/// SSH's `RemoteId` so WSL local entries fit the same model.
#[derive(Clone)]
pub(crate) struct PickerEntry {
    pub(crate) name: String,
    pub(crate) path: RemotePath,
    pub(crate) is_dir: bool,
}

#[derive(Clone)]
pub(crate) struct RemoteDirPicker {
    pub(crate) target: PaneId,
    pub(crate) source: PickerSource,
    pub(crate) current: RemotePath,
    pub(crate) entries: Vec<PickerEntry>,
    pub(crate) selected: usize,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) generation: u64,
}

impl RemoteDirPicker {
    pub(crate) fn new(target: PaneId, source: PickerSource, current: RemotePath) -> Self {
        Self {
            target,
            source,
            current,
            entries: Vec::new(),
            selected: 0,
            loading: false,
            error: None,
            generation: 0,
        }
    }

    /// A short label for the panel header (`user@host:port` or `wsl:<distro>`).
    pub(crate) fn source_label(&self) -> String {
        match &self.source {
            PickerSource::Ssh(cfg) => {
                crate::ssh_recents::format_target(&cfg.user, &cfg.host, cfg.port)
            }
            PickerSource::Wsl { distro } => format!("wsl:{distro}"),
        }
    }

    pub(crate) fn visible_dirs(&self) -> Vec<PickerEntry> {
        let mut dirs: Vec<_> = self.entries.iter().filter(|entry| entry.is_dir).cloned().collect();
        dirs.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        dirs
    }

    pub(crate) fn selected_dir(&self) -> Option<RemotePath> {
        self.visible_dirs()
            .get(self.selected)
            .map(|entry| entry.path.clone())
    }

    pub(crate) fn begin_load(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.loading = true;
        self.error = None;
        self.generation
    }

    pub(crate) fn apply_entries(&mut self, entries: Vec<PickerEntry>) {
        self.entries = entries;
        self.selected = self.selected.min(self.visible_dirs().len().saturating_sub(1));
        self.loading = false;
        self.error = None;
    }

    pub(crate) fn apply_error(&mut self, error: String) {
        self.entries.clear();
        self.selected = 0;
        self.loading = false;
        self.error = Some(error);
    }

    pub(crate) fn move_selection(&mut self, delta: i32) {
        let len = self.visible_dirs().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let cur = self.selected.min(len - 1) as i32;
        self.selected = (cur + delta).clamp(0, len as i32 - 1) as usize;
    }

    pub(crate) fn enter_selected(&mut self) -> bool {
        let Some(path) = self.selected_dir() else {
            return false;
        };
        self.current = path;
        self.entries.clear();
        self.selected = 0;
        true
    }

    pub(crate) fn go_parent(&mut self) -> bool {
        let Some(parent) = self.current.parent() else {
            return false;
        };
        self.current = parent;
        self.entries.clear();
        self.selected = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> tn_pty::SshConfig {
        tn_pty::SshConfig {
            host: "box".into(),
            port: 22,
            user: "alice".into(),
            key_path: None,
            password: None,
        }
    }

    fn entry(path: &str, name: &str, is_dir: bool) -> PickerEntry {
        PickerEntry {
            name: name.into(),
            path: RemotePath::new(path),
            is_dir,
        }
    }

    #[test]
    fn picker_keeps_only_directories_for_selection() {
        let mut picker =
            RemoteDirPicker::new(7, PickerSource::Ssh(cfg()), RemotePath::new("/home/alice"));
        picker.entries = vec![
            entry("/home/alice/main.rs", "main.rs", false),
            entry("/home/alice/src", "src", true),
        ];

        assert_eq!(picker.visible_dirs()[0].name, "src");
        assert_eq!(picker.selected_dir().unwrap().as_str(), "/home/alice/src");
    }

    #[test]
    fn picker_navigation_changes_current_without_selecting_files() {
        let mut picker =
            RemoteDirPicker::new(7, PickerSource::Ssh(cfg()), RemotePath::new("/home/alice"));
        picker.apply_entries(vec![
            entry("/home/alice/zeta", "zeta", true),
            entry("/home/alice/app.log", "app.log", false),
            entry("/home/alice/src", "src", true),
        ]);

        assert_eq!(
            picker
                .visible_dirs()
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["src", "zeta"]
        );
        assert_eq!(picker.selected_dir().unwrap().as_str(), "/home/alice/src");
        picker.move_selection(1);
        assert_eq!(picker.selected_dir().unwrap().as_str(), "/home/alice/zeta");
        assert!(picker.enter_selected());
        assert_eq!(picker.current.as_str(), "/home/alice/zeta");
        assert!(picker.entries.is_empty());
        assert!(picker.go_parent());
        assert_eq!(picker.current.as_str(), "/home/alice");
    }

    #[test]
    fn picker_load_state_tracks_error_and_clamps_selection() {
        let mut picker =
            RemoteDirPicker::new(7, PickerSource::Ssh(cfg()), RemotePath::new("/home/alice"));
        assert_eq!(picker.begin_load(), 1);
        assert!(picker.loading);

        picker.selected = 9;
        picker.apply_entries(vec![entry("/home/alice/src", "src", true)]);
        assert!(!picker.loading);
        assert_eq!(picker.selected, 0);
        assert!(picker.error.is_none());

        picker.apply_error("permission denied".into());
        assert_eq!(picker.visible_dirs().len(), 0);
        assert_eq!(picker.error.as_deref(), Some("permission denied"));
        assert!(!picker.loading);
    }

    #[test]
    fn wsl_source_label_shows_distro() {
        let picker = RemoteDirPicker::new(
            3,
            PickerSource::Wsl {
                distro: "Ubuntu".into(),
            },
            RemotePath::new("/home/me"),
        );
        assert_eq!(picker.source_label(), "wsl:Ubuntu");
    }
}
