use std::env;
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

/// File (under the config dir) toggling the git-status colouring feature. The
/// feature is **off by default** — a full `git status` of a large tree (a
/// parent of many repos) is expensive, so colouring is opt-in. A file whose
/// contents are truthy turns it on.
const GIT_FILE: &str = "git";

/// Environment override for the git-status colouring toggle. When set it wins
/// over the persisted `.pjma/git` file, matching the `RM_SOCKET` /
/// `RM_CLAUDE_PROJECTS` override convention.
const GIT_ENV: &str = "RM_GIT_STATUS";

/// Parse a human-written on/off value. Truthy: `1`, `true`, `on`, `yes`, `y`
/// (case-insensitive, surrounding whitespace ignored); everything else — the
/// empty string included — is false.
fn parse_bool(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes" | "y"
    )
}

/// The project-local configuration directory (`<root>/.pjma`) and the state
/// runner-manager reads/writes there. Paths are always absolute on the API
/// surface; only the on-disk representation is root-relative so the config
/// survives the project being moved.
pub struct Config {
    root: PathBuf,
    dir: PathBuf,
}

impl Config {
    /// Build the config handle for a tree root. Performs no I/O; call
    /// [`Config::ensure_dir`] before anything needs to be persisted.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let dir = root.join(DIR_NAME);
        Self { root, dir }
    }

    /// The config directory itself (`<root>/.pjma`).
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
    /// Lines that are absolute or escape the root (`..`) — only possible in a
    /// hand-edited or corrupt file — are dropped rather than joined.
    pub fn load_expanded(&self) -> Vec<PathBuf> {
        let Ok(text) = fs::read_to_string(self.expanded_path()) else {
            return Vec::new();
        };
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter(|l| {
                Path::new(l)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
            })
            .map(|rel| self.root.join(rel))
            .collect()
    }

    /// Persist the given expanded directories (absolute paths) as root-relative
    /// lines, sorted for a stable file. The root itself, any path outside the
    /// root, and any path whose name contains a newline (which would corrupt
    /// the line-based format) are skipped. Best-effort: the config dir is
    /// created first.
    pub fn save_expanded(&self, dirs: &[PathBuf]) -> io::Result<()> {
        self.ensure_dir()?;
        let mut lines: Vec<String> = dirs
            .iter()
            .filter_map(|d| d.strip_prefix(&self.root).ok())
            .map(|rel| rel.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty() && !s.contains(['\n', '\r']))
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
        fs::read_to_string(self.split_path())
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Persist the tree-pane width percent. Best-effort: the config dir is
    /// created first.
    pub fn save_split(&self, pct: u16) -> io::Result<()> {
        self.ensure_dir()?;
        fs::write(self.split_path(), pct.to_string())
    }

    fn git_path(&self) -> PathBuf {
        self.dir.join(GIT_FILE)
    }

    /// Whether the git-status colouring feature is enabled. **Disabled by
    /// default.** The `RM_GIT_STATUS` env var, if present, overrides the
    /// persisted `.pjma/git` file; both accept `1`/`true`/`on`/`yes`
    /// (case-insensitive). A missing/unreadable file means off.
    pub fn git_status_enabled(&self) -> bool {
        if let Some(v) = env::var_os(GIT_ENV) {
            return parse_bool(&v.to_string_lossy());
        }
        fs::read_to_string(self.git_path())
            .map(|s| parse_bool(&s))
            .unwrap_or(false)
    }

    /// Persist the git-status colouring toggle to `.pjma/git`. Best-effort: the
    /// config dir is created first. Note the `RM_GIT_STATUS` env var, when set,
    /// still overrides whatever is written here on the next read.
    pub fn save_git_status(&self, enabled: bool) -> io::Result<()> {
        self.ensure_dir()?;
        fs::write(self.git_path(), if enabled { "on" } else { "off" })
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
        cfg.save_expanded(&[root.join("src"), root.join("src/proto")])
            .unwrap();
        let mut loaded = cfg.load_expanded();
        loaded.sort();
        assert_eq!(loaded, vec![root.join("src"), root.join("src/proto")]);
        // The on-disk form is root-relative, one per line.
        let text = fs::read_to_string(root.join(".pjma").join("expanded")).unwrap();
        assert_eq!(text, "src\nsrc/proto");
    }

    #[test]
    fn load_drops_absolute_and_parent_escaping_lines() {
        // Only reachable via a hand-edited or corrupt state file: an absolute
        // line would *replace* the root in `root.join`, and `..` would escape
        // it. Both are dropped instead of joined.
        let d = tempdir().unwrap();
        let root = d.path();
        let cfg = Config::new(root);
        cfg.ensure_dir().unwrap();
        fs::write(
            root.join(".pjma").join("expanded"),
            "/etc\n../outside\nsrc\n",
        )
        .unwrap();
        assert_eq!(cfg.load_expanded(), vec![root.join("src")]);
    }

    #[test]
    fn save_skips_paths_that_would_corrupt_the_line_format() {
        let d = tempdir().unwrap();
        let root = d.path();
        let cfg = Config::new(root);
        cfg.save_expanded(&[root.join("a\nb"), root.join("ok")])
            .unwrap();
        assert_eq!(cfg.load_expanded(), vec![root.join("ok")]);
    }

    #[test]
    fn save_skips_root_and_outside_paths() {
        let d = tempdir().unwrap();
        let root = d.path();
        let cfg = Config::new(root);
        cfg.save_expanded(&[
            root.to_path_buf(),
            PathBuf::from("/elsewhere"),
            root.join("a"),
        ])
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

    #[test]
    fn git_status_is_disabled_by_default() {
        let d = tempdir().unwrap();
        let cfg = Config::new(d.path());
        // No file at all -> off.
        assert!(!cfg.git_status_enabled());
    }

    #[test]
    fn git_status_save_then_load_roundtrips() {
        let d = tempdir().unwrap();
        let cfg = Config::new(d.path());
        cfg.save_git_status(true).unwrap();
        assert!(cfg.git_status_enabled());
        cfg.save_git_status(false).unwrap();
        assert!(!cfg.git_status_enabled());
    }

    #[test]
    fn git_status_accepts_truthy_and_rejects_junk() {
        let d = tempdir().unwrap();
        let cfg = Config::new(d.path());
        cfg.ensure_dir().unwrap();
        let path = d.path().join(".pjma").join("git");
        for truthy in ["1", "true", "ON", " yes\n", "Y"] {
            fs::write(&path, truthy).unwrap();
            assert!(cfg.git_status_enabled(), "{truthy:?} should be on");
        }
        for falsy in ["0", "false", "off", "", "nonsense"] {
            fs::write(&path, falsy).unwrap();
            assert!(!cfg.git_status_enabled(), "{falsy:?} should be off");
        }
    }
}
