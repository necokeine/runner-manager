use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The git working-tree state of a single path, reduced to what the tree pane
/// needs to colour it. Mirrors `git status`'s own colour policy: staged changes
/// ("Changes to be committed") render green, everything dirty in the worktree
/// (modified-but-unstaged and untracked) renders red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatus {
    /// Index differs from HEAD with a clean worktree — "Changes to be committed".
    Staged,
    /// Tracked file modified (or deleted) in the worktree but not staged.
    Modified,
    /// Not tracked by git at all.
    Untracked,
}

impl GitStatus {
    /// True for the states git colours red (dirty worktree / untracked). Used to
    /// decide precedence when rolling child statuses up into a directory.
    fn is_red(self) -> bool {
        matches!(self, GitStatus::Modified | GitStatus::Untracked)
    }
}

/// A snapshot of `git status` for the tree root, as a map from absolute path to
/// its [`GitStatus`]. Directories that (recursively) contain changes are also
/// present, carrying the rolled-up status of their descendants, so the tree can
/// colour a collapsed directory without re-scanning its subtree every frame.
#[derive(Debug, Default, Clone)]
pub struct GitStatuses {
    map: HashMap<PathBuf, GitStatus>,
}

impl GitStatuses {
    /// An empty snapshot — what callers get when `root` is not in a git repo,
    /// `git` is missing, or the command fails. No path is ever coloured.
    pub fn empty() -> Self {
        Self { map: HashMap::new() }
    }

    /// The status of `path`, if git reported a change at or under it.
    pub fn get(&self, path: &Path) -> Option<GitStatus> {
        self.map.get(path).copied()
    }

    /// Build a snapshot directly from `(path, status)` pairs. Test-only: the
    /// real path comes from [`GitStatuses::load`].
    #[cfg(test)]
    pub fn from_entries(entries: impl IntoIterator<Item = (PathBuf, GitStatus)>) -> Self {
        Self { map: entries.into_iter().collect() }
    }

    /// Run `git status` for the repository containing `root` and build a
    /// snapshot. Best-effort: any failure (not a repo, no `git`, bad output)
    /// yields an empty snapshot rather than an error, so the tree just renders
    /// without colour. Only paths under `root` are recorded — changes elsewhere
    /// in the repo never appear in the tree, so they are skipped.
    pub fn load(root: &Path) -> Self {
        let Some(toplevel) = repo_toplevel(root) else {
            return Self::empty();
        };
        let Some(output) = porcelain(root) else {
            return Self::empty();
        };
        // Porcelain paths are relative to the repo toplevel, which git reports
        // in canonical (symlink-resolved) form — `root` may be given in another
        // form (e.g. `/var` vs `/private/var` on macOS, where the tree's rows
        // also use the un-resolved form). Compute the toplevel→root prefix in
        // canonical space, then re-anchor each path onto the original `root` so
        // the map's keys line up with the tree's `row.path` values.
        let canon_top = std::fs::canonicalize(&toplevel).unwrap_or(toplevel);
        let prefix = std::fs::canonicalize(root)
            .ok()
            .and_then(|cr| cr.strip_prefix(&canon_top).ok().map(Path::to_path_buf))
            .unwrap_or_default();
        let mut statuses = Self::empty();
        for (rel, status) in parse_porcelain(&output) {
            // Keep only paths inside `root`; drop the prefix and re-anchor.
            let Ok(under_root) = rel.strip_prefix(&prefix) else {
                continue;
            };
            let abs = root.join(under_root);
            statuses.insert_path(&abs, status, root);
        }
        statuses
    }

    /// Record `status` for `abs` and roll it up into every ancestor directory
    /// that lies under `root` (excluding `root` itself). A directory keeps the
    /// "worse" of its children's states: any red (dirty/untracked) descendant
    /// makes the directory red; otherwise a staged descendant makes it green.
    fn insert_path(&mut self, abs: &Path, status: GitStatus, root: &Path) {
        self.merge(abs.to_path_buf(), status);
        for anc in abs.ancestors().skip(1) {
            // Stop once we reach the tree root or step outside it: the root row
            // is never coloured, and changes outside the visible tree are moot.
            if anc == root || !anc.starts_with(root) {
                break;
            }
            self.merge(anc.to_path_buf(), status);
        }
    }

    fn merge(&mut self, path: PathBuf, status: GitStatus) {
        self.map
            .entry(path)
            .and_modify(|cur| {
                // Red beats green; between two reds or two greens either wins.
                if status.is_red() && !cur.is_red() {
                    *cur = status;
                }
            })
            .or_insert(status);
    }
}

/// `git -C <dir> rev-parse --show-toplevel`, trimmed — the absolute path of the
/// working tree containing `dir`, or `None` when `dir` is not in a repo.
fn repo_toplevel(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim_end_matches(['\n', '\r']);
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// `git -C <dir> status --porcelain -z --untracked-files=all` output, or `None`
/// on failure. `-z` keeps paths intact (NUL-separated, no quoting) so names with
/// spaces or newlines parse correctly.
fn porcelain(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Parse `git status --porcelain -z` into `(repo-relative path, status)` pairs.
/// Each record is `XY<space>PATH`; rename/copy records (`R`/`C` in either
/// column) are followed by the origin path in the next NUL field, which is
/// consumed and ignored. Paths are relative to the repo toplevel.
fn parse_porcelain(data: &str) -> Vec<(PathBuf, GitStatus)> {
    let mut out = Vec::new();
    let mut fields = data.split('\0');
    while let Some(entry) = fields.next() {
        // A valid record is at least "XY P" (4 bytes). The trailing field after
        // the final NUL is empty and falls through here.
        if entry.len() < 4 {
            continue;
        }
        let bytes = entry.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let path = &entry[3..];
        // Rename/copy carries an extra "origin" field; drop it so it is not read
        // as its own record.
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            let _ = fields.next();
        }
        out.push((PathBuf::from(path), classify(x, y)));
    }
    out
}

/// Map a porcelain `XY` status pair to a single [`GitStatus`], following git's
/// own colour grouping: untracked (`??`) and any worktree change (`Y` set) are
/// red; an index-only change (`X` set, clean worktree) is the green "staged"
/// state.
fn classify(x: char, y: char) -> GitStatus {
    if x == '?' && y == '?' {
        GitStatus::Untracked
    } else if y != ' ' {
        GitStatus::Modified
    } else {
        GitStatus::Staged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn classify_matches_git_status_grouping() {
        // Untracked.
        assert_eq!(classify('?', '?'), GitStatus::Untracked);
        // Staged add / staged modify with a clean worktree -> green.
        assert_eq!(classify('A', ' '), GitStatus::Staged);
        assert_eq!(classify('M', ' '), GitStatus::Staged);
        // Worktree changes (with or without a staged part) -> red.
        assert_eq!(classify(' ', 'M'), GitStatus::Modified);
        assert_eq!(classify('M', 'M'), GitStatus::Modified);
        assert_eq!(classify(' ', 'D'), GitStatus::Modified);
    }

    #[test]
    fn parse_skips_rename_origin_field() {
        // A staged rename "old" -> "new" plus a trailing untracked file. The
        // origin field ("old") must not be parsed as its own record.
        let data = "R  new\0old\0?? extra\0";
        let parsed = parse_porcelain(data);
        assert_eq!(
            parsed,
            vec![
                (PathBuf::from("new"), GitStatus::Staged),
                (PathBuf::from("extra"), GitStatus::Untracked),
            ]
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("run git")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t.t"]);
        git(dir, &["config", "user.name", "t"]);
    }

    #[test]
    fn load_reports_untracked_staged_and_modified_with_rollup() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        init_repo(root);

        // A committed file we will modify in the worktree.
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src").join("committed.rs"), "x").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "init"]);
        fs::write(root.join("src").join("committed.rs"), "changed").unwrap();

        // A staged new file under a fresh directory.
        fs::create_dir(root.join("staged")).unwrap();
        fs::write(root.join("staged").join("new.rs"), "n").unwrap();
        git(root, &["add", "staged/new.rs"]);

        // An untracked file at the top level.
        fs::write(root.join("loose.txt"), "u").unwrap();

        let st = GitStatuses::load(root);

        // Files carry their own status.
        assert_eq!(st.get(&root.join("src").join("committed.rs")), Some(GitStatus::Modified));
        assert_eq!(st.get(&root.join("staged").join("new.rs")), Some(GitStatus::Staged));
        assert_eq!(st.get(&root.join("loose.txt")), Some(GitStatus::Untracked));

        // Directories roll up: src/ is red (modified child), staged/ is green.
        assert_eq!(st.get(&root.join("src")), Some(GitStatus::Modified));
        assert_eq!(st.get(&root.join("staged")), Some(GitStatus::Staged));

        // The tree root itself is never recorded.
        assert_eq!(st.get(root), None);
        // A clean path has no entry.
        assert_eq!(st.get(&root.join("nope.rs")), None);
    }

    #[test]
    fn directory_rollup_prefers_red_over_green() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        // One directory holding both a staged file and an untracked file: the
        // directory should colour red (the worse of the two).
        fs::create_dir(root.join("mix")).unwrap();
        fs::write(root.join("mix").join("a.rs"), "a").unwrap();
        git(root, &["add", "mix/a.rs"]);
        fs::write(root.join("mix").join("b.rs"), "b").unwrap();

        let st = GitStatuses::load(root);
        assert_eq!(st.get(&root.join("mix").join("a.rs")), Some(GitStatus::Staged));
        assert_eq!(st.get(&root.join("mix").join("b.rs")), Some(GitStatus::Untracked));
        assert!(st.get(&root.join("mix")).unwrap().is_red());
    }

    #[test]
    fn load_outside_a_repo_is_empty() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "x").unwrap();
        // No `git init`: not a repo -> empty snapshot, no panic.
        let st = GitStatuses::load(dir.path());
        assert_eq!(st.get(&dir.path().join("a.txt")), None);
    }
}
