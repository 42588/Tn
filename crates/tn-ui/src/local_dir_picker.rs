use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_RECENTS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalDirFocus {
    Recent,
    Directories,
    Browse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecentWorkdir {
    pub(crate) label: String,
    pub(crate) path: PathBuf,
    #[serde(default)]
    pub(crate) source: RecentSource,
    #[serde(default)]
    pub(crate) last_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RecentSource {
    Recent,
    Explorer,
}

impl Default for RecentSource {
    fn default() -> Self {
        Self::Recent
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalDirEntry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) is_git: bool,
    pub(crate) is_drive: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct LocalDirPicker {
    pub(crate) agent_index: usize,
    pub(crate) agent_name: String,
    pub(crate) current: PathBuf,
    pub(crate) selected: PathBuf,
    pub(crate) focus: LocalDirFocus,
    pub(crate) recent_sel: usize,
    pub(crate) dir_sel: usize,
    pub(crate) recents: Vec<RecentWorkdir>,
    pub(crate) dirs: Vec<LocalDirEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocalDirAction {
    Open(PathBuf),
    Browse,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub(crate) struct WorkdirRecents {
    #[serde(default)]
    pub(crate) entries: Vec<RecentWorkdir>,
}

impl LocalDirPicker {
    pub(crate) fn new(
        agent_index: usize,
        agent_name: impl Into<String>,
        initial: PathBuf,
        recents: Vec<RecentWorkdir>,
    ) -> Self {
        Self {
            agent_index,
            agent_name: agent_name.into(),
            current: initial.clone(),
            selected: initial,
            focus: LocalDirFocus::Recent,
            recent_sel: 0,
            dir_sel: 0,
            recents,
            dirs: Vec::new(),
        }
    }

    pub(crate) fn focus_next(&mut self) {
        self.focus = match self.focus {
            LocalDirFocus::Recent => LocalDirFocus::Directories,
            LocalDirFocus::Directories => LocalDirFocus::Browse,
            LocalDirFocus::Browse => LocalDirFocus::Recent,
        };
        self.sync_selected_to_focus();
    }

    pub(crate) fn focus_prev(&mut self) {
        self.focus = match self.focus {
            LocalDirFocus::Recent => LocalDirFocus::Browse,
            LocalDirFocus::Directories => LocalDirFocus::Recent,
            LocalDirFocus::Browse => LocalDirFocus::Directories,
        };
        self.sync_selected_to_focus();
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
        match self.focus {
            LocalDirFocus::Recent => {
                self.recent_sel = move_index(self.recent_sel, self.recents.len(), delta);
                if let Some(r) = self.recents.get(self.recent_sel) {
                    self.selected = r.path.clone();
                }
            }
            LocalDirFocus::Directories => {
                self.dir_sel = move_index(self.dir_sel, self.dirs.len(), delta);
                if let Some(d) = self.dirs.get(self.dir_sel) {
                    self.selected = d.path.clone();
                }
            }
            LocalDirFocus::Browse => {}
        }
    }

    pub(crate) fn open_selected(&mut self) -> Option<LocalDirAction> {
        match self.focus {
            LocalDirFocus::Recent => {
                let path = self.recents.get(self.recent_sel)?.path.clone();
                self.current = path.clone();
                self.selected = path.clone();
                Some(LocalDirAction::Open(path))
            }
            LocalDirFocus::Directories => {
                let path = self.dirs.get(self.dir_sel)?.path.clone();
                self.current = path.clone();
                self.selected = path.clone();
                Some(LocalDirAction::Open(path))
            }
            LocalDirFocus::Browse => Some(LocalDirAction::Browse),
        }
    }

    pub(crate) fn go_parent(&mut self) -> Option<PathBuf> {
        let parent = self.current.parent()?.to_path_buf();
        self.current = parent.clone();
        self.selected = parent.clone();
        Some(parent)
    }

    pub(crate) fn launch_cwd(&self) -> PathBuf {
        self.selected.clone()
    }

    pub(crate) fn apply_dirs(&mut self, dirs: Vec<LocalDirEntry>) {
        self.dirs = dirs;
        self.dir_sel = self.dir_sel.min(self.dirs.len().saturating_sub(1));
        self.sync_selected_to_focus();
    }

    fn sync_selected_to_focus(&mut self) {
        match self.focus {
            LocalDirFocus::Recent => {
                if let Some(r) = self.recents.get(self.recent_sel) {
                    self.selected = r.path.clone();
                }
            }
            LocalDirFocus::Directories => {
                if let Some(d) = self.dirs.get(self.dir_sel) {
                    self.selected = d.path.clone();
                }
            }
            LocalDirFocus::Browse => {}
        }
    }
}

impl WorkdirRecents {
    pub(crate) fn path() -> Option<PathBuf> {
        tn_config::config_dir().map(|d| d.join("workdir_recents.json"))
    }

    pub(crate) fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<WorkdirRecents>(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::error!(path = %path.display(), error = %e, "save workdir recents failed");
                }
            }
            Err(e) => tracing::error!(error = %e, "serialize workdir recents failed"),
        }
    }

    pub(crate) fn record(&mut self, path: PathBuf) {
        let now = now_secs();
        if let Some(e) = self.entries.iter_mut().find(|e| same_path(&e.path, &path)) {
            e.last_used = now;
            e.source = RecentSource::Recent;
            if e.label.is_empty() {
                e.label = label_for_path(&path);
            }
        } else {
            self.entries.push(RecentWorkdir {
                label: label_for_path(&path),
                path,
                source: RecentSource::Recent,
                last_used: now,
            });
        }
        self.trim();
    }

    pub(crate) fn sorted_with_seed(&self, seed: Option<PathBuf>) -> Vec<RecentWorkdir> {
        let mut out = Vec::new();
        if let Some(path) = seed {
            out.push(RecentWorkdir {
                label: "Explorer root".into(),
                path,
                source: RecentSource::Explorer,
                last_used: u64::MAX,
            });
        }
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        for e in entries {
            if out
                .iter()
                .any(|existing| same_path(&existing.path, &e.path))
            {
                continue;
            }
            out.push(e);
        }
        out
    }

    fn trim(&mut self) {
        self.entries.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        self.entries.truncate(MAX_RECENTS);
    }
}

pub(crate) fn read_local_dirs(path: &std::path::Path) -> std::io::Result<Vec<LocalDirEntry>> {
    let mut out = local_drive_entries();
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) if !out.is_empty() => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(ty) = entry.file_type() else { continue };
        if !ty.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_git = path.join(".git").exists();
        out.push(LocalDirEntry {
            name,
            path,
            is_git,
            is_drive: false,
        });
    }
    out.sort_by(|a, b| {
        match (a.is_drive, b.is_drive) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    Ok(out)
}

#[cfg(windows)]
fn local_drive_entries() -> Vec<LocalDirEntry> {
    ('A'..='Z')
        .filter_map(|letter| {
            let root = PathBuf::from(format!("{letter}:\\"));
            root.exists().then(|| LocalDirEntry {
                name: format!("{letter}:"),
                path: root,
                is_git: false,
                is_drive: true,
            })
        })
        .collect()
}

#[cfg(not(windows))]
fn local_drive_entries() -> Vec<LocalDirEntry> {
    Vec::new()
}

fn label_for_path(path: &std::path::Path) -> String {
    if let Some(name) = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
    {
        name.to_string()
    } else {
        path.to_string_lossy().to_string()
    }
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current.saturating_add(delta as usize).min(len - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recent(label: &str, path: &str) -> RecentWorkdir {
        RecentWorkdir {
            label: label.into(),
            path: PathBuf::from(path),
            source: RecentSource::Recent,
            last_used: 1,
        }
    }

    #[test]
    fn tab_cycles_focus_between_recents_directories_and_browse() {
        let mut picker = LocalDirPicker::new(
            2,
            "Codex",
            PathBuf::from(r"D:\coder"),
            vec![recent("Tn", r"D:\coder\Tn")],
        );

        assert_eq!(picker.focus, LocalDirFocus::Recent);
        picker.focus_next();
        assert_eq!(picker.focus, LocalDirFocus::Directories);
        picker.focus_next();
        assert_eq!(picker.focus, LocalDirFocus::Browse);
        picker.focus_next();
        assert_eq!(picker.focus, LocalDirFocus::Recent);
        picker.focus_prev();
        assert_eq!(picker.focus, LocalDirFocus::Browse);
    }

    #[test]
    fn arrows_move_only_inside_focused_region_and_update_selection() {
        let mut picker = LocalDirPicker::new(
            0,
            "Codex",
            PathBuf::from(r"D:\coder"),
            vec![recent("Tn", r"D:\coder\Tn"), recent("Lab", r"D:\lab")],
        );
        picker.apply_dirs(vec![
            LocalDirEntry {
                name: "Tn".into(),
                path: PathBuf::from(r"D:\coder\Tn"),
                is_git: true,
                is_drive: false,
            },
            LocalDirEntry {
                name: "Playground".into(),
                path: PathBuf::from(r"D:\coder\playground"),
                is_git: false,
                is_drive: false,
            },
        ]);

        picker.move_selection(1);
        assert_eq!(picker.recent_sel, 1);
        assert_eq!(picker.dir_sel, 0);
        assert_eq!(picker.selected, PathBuf::from(r"D:\lab"));

        picker.focus_next();
        picker.move_selection(1);
        assert_eq!(picker.recent_sel, 1);
        assert_eq!(picker.dir_sel, 1);
        assert_eq!(picker.selected, PathBuf::from(r"D:\coder\playground"));
    }

    #[test]
    fn tab_focus_switch_tracks_the_visible_highlight_for_launching() {
        let mut picker = LocalDirPicker::new(
            0,
            "Codex",
            PathBuf::from(r"D:\coder"),
            vec![recent("Lab", r"D:\lab")],
        );
        picker.apply_dirs(vec![LocalDirEntry {
            name: "Tn".into(),
            path: PathBuf::from(r"D:\coder\Tn"),
            is_git: true,
            is_drive: false,
        }]);

        assert_eq!(picker.launch_cwd(), PathBuf::from(r"D:\lab"));
        picker.focus_next();
        assert_eq!(picker.focus, LocalDirFocus::Directories);
        assert_eq!(picker.launch_cwd(), PathBuf::from(r"D:\coder\Tn"));
        picker.focus_prev();
        assert_eq!(picker.focus, LocalDirFocus::Recent);
        assert_eq!(picker.launch_cwd(), PathBuf::from(r"D:\lab"));
    }

    #[test]
    fn open_selected_enters_directory_and_parent_returns_up_when_possible() {
        let mut picker = LocalDirPicker::new(
            0,
            "Codex",
            PathBuf::from(r"D:\coder\Tn"),
            vec![recent("Tn", r"D:\coder\Tn")],
        );
        picker.focus = LocalDirFocus::Directories;
        picker.apply_dirs(vec![LocalDirEntry {
            name: "src".into(),
            path: PathBuf::from(r"D:\coder\Tn\src"),
            is_git: false,
            is_drive: false,
        }]);

        assert_eq!(
            picker.open_selected(),
            Some(LocalDirAction::Open(PathBuf::from(r"D:\coder\Tn\src")))
        );
        assert_eq!(picker.current, PathBuf::from(r"D:\coder\Tn\src"));
        assert_eq!(picker.go_parent(), Some(PathBuf::from(r"D:\coder\Tn")));
    }

    #[test]
    fn launch_cwd_tracks_the_highlighted_directory_without_entering_it() {
        let mut picker = LocalDirPicker::new(
            0,
            "Codex",
            PathBuf::from(r"D:\coder"),
            vec![recent("Tn", r"D:\coder\Tn")],
        );
        picker.focus = LocalDirFocus::Directories;
        picker.apply_dirs(vec![
            LocalDirEntry {
                name: "src".into(),
                path: PathBuf::from(r"D:\coder\Tn\src"),
                is_git: false,
                is_drive: false,
            },
            LocalDirEntry {
                name: "docs".into(),
                path: PathBuf::from(r"D:\coder\Tn\docs"),
                is_git: false,
                is_drive: false,
            },
        ]);

        assert_eq!(picker.launch_cwd(), PathBuf::from(r"D:\coder\Tn\src"));
        picker.move_selection(1);
        assert_eq!(picker.launch_cwd(), PathBuf::from(r"D:\coder\Tn\docs"));
        assert_eq!(picker.current, PathBuf::from(r"D:\coder"));
    }

    #[test]
    fn recents_are_upserted_sorted_and_seeded_with_explorer_root() {
        let mut recents = WorkdirRecents::default();
        recents.record(PathBuf::from(r"D:\coder\Tn"));
        recents.record(PathBuf::from(r"D:\lab"));
        recents.record(PathBuf::from(r"d:\coder\tn"));

        assert_eq!(recents.entries.len(), 2);
        let sorted = recents.sorted_with_seed(Some(PathBuf::from(r"D:\welcome")));
        assert_eq!(sorted[0].source, RecentSource::Explorer);
        assert_eq!(sorted[0].path, PathBuf::from(r"D:\welcome"));
        assert_eq!(sorted.len(), 3);
    }

    #[test]
    fn read_local_dirs_returns_only_directories_sorted_and_marks_git() {
        let base =
            std::env::temp_dir().join(format!("tn-local-dir-picker-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("b")).unwrap();
        std::fs::create_dir_all(base.join("a").join(".git")).unwrap();
        std::fs::write(base.join("z.txt"), "not a dir").unwrap();

        let dirs = read_local_dirs(&base).unwrap();
        let local = dirs.iter().filter(|d| !d.is_drive).collect::<Vec<_>>();
        assert_eq!(
            local.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(local[0].is_git);
        assert!(!local[1].is_git);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn read_local_dirs_includes_existing_windows_drive_roots_first() {
        let base = std::env::temp_dir();
        let dirs = read_local_dirs(&base).unwrap();
        let expected = ('A'..='Z')
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .find(|path| path.exists())
            .expect("Windows must expose at least one drive root");

        assert_eq!(dirs.first().map(|d| d.path.clone()), Some(expected));
        assert!(dirs.first().is_some_and(|d| d.is_drive));
    }

    #[cfg(windows)]
    #[test]
    fn read_local_dirs_keeps_drive_roots_when_current_path_is_missing() {
        let missing = PathBuf::from(r"Z:\tn-definitely-missing-current-directory");
        let dirs = read_local_dirs(&missing).unwrap();

        assert!(dirs.iter().any(|d| d.is_drive));
    }
}
