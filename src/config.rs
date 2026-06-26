use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Name of the per-project configuration directory created under the tree root.
/// Everything runner-manager persists for a project lives here: the tmux socket
/// (`pjma.sock`) and the saved tree state.
pub const DIR_NAME: &str = ".pjma";

/// File (under the config dir) listing the expanded directories, one
/// root-relative path per line.
const EXPANDED_FILE: &str = "expanded";

/// File (under the config dir) holding the tree-pane width percent — a single
/// integer line. Persists the `<`/`>` and splitter-drag adjustments.
const SPLIT_FILE: &str = "split";

/// The project-local configuration directory (`<root>/.pjma`) and the state
/// runner-manager reads/writes there. Paths are always absolute on the API
/// surface; only the on-disk representation is root-relative so the config
/// survives the project being moved.
pub struct Config {
    root: PathBuf,
    dir: PathBuf,
}

impl Config {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let dir = root.join(DIR_NAME);
        Self { root, dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Create the config directory if it does not yet exist. Called at startup
    /// so the tmux socket and the state file have somewhere to live.
    pub fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    fn expanded_path(&self) -> PathBuf {
        self.dir.join(EXPANDED_FILE)
    }

    /// Load the saved set of expanded directories as absolute paths. A missing
    /// or unreadable state file yields an empty list (nothing pre-expanded).
    pub fn load_expanded(&self) -> Vec<PathBuf> {
        let Ok(text) = fs::read_to_string(self.expanded_path()) else {
            return Vec::new();
        };
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|rel| self.root.join(rel))
            .collect()
    }

    /// Persist the given expanded directories (absolute paths) as root-relative
    /// lines, sorted for a stable file. The root itself, and any path outside
    /// the root, are skipped. Best-effort: the config dir is created first.
    pub fn save_expanded(&self, dirs: &[PathBuf]) -> io::Result<()> {
        self.ensure_dir()?;
        let mut lines: Vec<String> = dirs
            .iter()
            .filter_map(|d| d.strip_prefix(&self.root).ok())
            .map(|rel| rel.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .collect();
        lines.sort();
        fs::write(self.expanded_path(), lines.join("\n"))
    }

    fn split_path(&self) -> PathBuf {
        self.dir.join(SPLIT_FILE)
    }

    /// Load the saved tree-pane width percent, or `None` if no valid value was
    /// persisted (missing/unreadable file, or non-numeric contents). The caller
    /// is responsible for clamping into its allowed range.
    pub fn load_split(&self) -> Option<u16> {
        fs::read_to_string(self.split_path()).ok()?.trim().parse().ok()
    }

    /// Persist the tree-pane width percent. Best-effort: the config dir is
    /// created first.
    pub fn save_split(&self, pct: u16) -> io::Result<()> {
        self.ensure_dir()?;
        fs::write(self.split_path(), pct.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn dir_is_pjma_under_root() {
        let cfg = Config::new("/p");
        assert_eq!(cfg.dir(), Path::new("/p/.pjma"));
    }

    #[test]
    fn save_then_load_roundtrips_as_absolute_paths() {
        let d = tempdir().unwrap();
        let root = d.path();
        let cfg = Config::new(root);
        cfg.save_expanded(&[root.join("src"), root.join("src/proto")]).unwrap();
        let mut loaded = cfg.load_expanded();
        loaded.sort();
        assert_eq!(loaded, vec![root.join("src"), root.join("src/proto")]);
        // The on-disk form is root-relative, one per line.
        let text = fs::read_to_string(root.join(".pjma").join("expanded")).unwrap();
        assert_eq!(text, "src\nsrc/proto");
    }

    #[test]
    fn save_skips_root_and_outside_paths() {
        let d = tempdir().unwrap();
        let root = d.path();
        let cfg = Config::new(root);
        cfg.save_expanded(&[root.to_path_buf(), PathBuf::from("/elsewhere"), root.join("a")])
            .unwrap();
        assert_eq!(cfg.load_expanded(), vec![root.join("a")]);
    }

    #[test]
    fn load_is_empty_when_no_state_file() {
        let d = tempdir().unwrap();
        let cfg = Config::new(d.path());
        assert!(cfg.load_expanded().is_empty());
    }

    #[test]
    fn split_roundtrips() {
        let d = tempdir().unwrap();
        let cfg = Config::new(d.path());
        cfg.save_split(42).unwrap();
        assert_eq!(cfg.load_split(), Some(42));
    }

    #[test]
    fn load_split_is_none_when_missing_or_garbage() {
        let d = tempdir().unwrap();
        let cfg = Config::new(d.path());
        assert_eq!(cfg.load_split(), None);
        cfg.ensure_dir().unwrap();
        fs::write(d.path().join(".pjma").join("split"), "not-a-number").unwrap();
        assert_eq!(cfg.load_split(), None);
    }
}
