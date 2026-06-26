use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How deep to walk under a non-repo `root` looking for nested `.git`
/// directories before giving up. A repo more than this many levels below the
/// tree root is unusual.
const MAX_REPO_SCAN_DEPTH: usize = 12;

/// Hard cap on the number of directories the nested-repo scan will enter in one
/// pass. The scan prunes at every repo boundary, so a normal parent-of-repos is
/// covered long before this — the budget only bites on a huge *non*-repo tree
/// (e.g. rooting at `/`), keeping a single scan cheap and bounded instead of
/// crawling the whole filesystem.
const MAX_REPO_SCAN_DIRS: usize = 4096;

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
    /// Matched by a `.gitignore` rule — rendered grey.
    Ignored,
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
    /// Per-path status for changed paths (and their rolled-up ancestor dirs),
    /// plus individually-ignored files. Exact lookup.
    map: HashMap<PathBuf, GitStatus>,
    /// Ignored *directories* (e.g. `target/`, `node_modules/`). git collapses an
    /// ignored directory into one entry instead of listing every file inside, so
    /// these are kept as path prefixes: any path at or under one is [`Ignored`].
    ///
    /// [`Ignored`]: GitStatus::Ignored
    ignored_dirs: Vec<PathBuf>,
}

impl GitStatuses {
    /// An empty snapshot — what callers get when `root` is not in a git repo,
    /// `git` is missing, or the command fails. No path is ever coloured.
    pub fn empty() -> Self {
        Self { map: HashMap::new(), ignored_dirs: Vec::new() }
    }

    /// The status of `path`: an exact change/ignored-file entry, else [`Ignored`]
    /// if it sits under an ignored directory, else `None`.
    ///
    /// [`Ignored`]: GitStatus::Ignored
    pub fn get(&self, path: &Path) -> Option<GitStatus> {
        if let Some(status) = self.map.get(path) {
            return Some(*status);
        }
        if self.ignored_dirs.iter().any(|dir| path.starts_with(dir)) {
            return Some(GitStatus::Ignored);
        }
        None
    }

    /// Build a snapshot directly from `(path, status)` pairs. Test-only: the
    /// real path comes from [`GitStatuses::load`].
    #[cfg(test)]
    pub fn from_entries(entries: impl IntoIterator<Item = (PathBuf, GitStatus)>) -> Self {
        Self { map: entries.into_iter().collect(), ignored_dirs: Vec::new() }
    }

    /// Build a `git status` snapshot for the tree `root`, combining two sources
    /// so colouring works no matter how the tree is rooted:
    ///
    /// * If `root` is inside a git work tree (its `.git` is at or above it), that
    ///   repository's status colours its visible subtree.
    /// * Unless `root` is *itself* a repository top, the tree is also scanned
    ///   downward and every directory holding its own `.git` is coloured from its
    ///   own status. This is the common case the tree pane is rooted at a parent
    ///   folder (e.g. `~/Projects`) holding many independent checkouts — and it
    ///   still works when that parent happens to live inside an unrelated repo.
    ///
    /// Best-effort throughout: a missing `git`, a non-repo, or unreadable output
    /// just contributes nothing, so the tree renders without colour rather than
    /// failing. Only paths under `root` are ever recorded.
    pub fn load(root: &Path) -> Self {
        let mut statuses = Self::empty();
        // The repository containing `root` (could be `root` itself or an ancestor).
        if let Some(toplevel) = repo_toplevel(root) {
            statuses.merge_containing_repo(root, &toplevel);
        }
        // Repositories nested under `root`. Skip this when `root` is itself a
        // repo top: its whole subtree is one repo (already handled above), so
        // there is no point walking it hunting for submodules.
        if !root.join(".git").exists() {
            for repo in find_nested_repos(root) {
                statuses.merge_nested_repo(root, &repo);
            }
        }
        statuses
    }

    /// Merge the status of the repository whose work tree contains `root`. Its
    /// `toplevel` may sit at or above `root`, so paths are re-anchored onto the
    /// original `root` and any change outside it is dropped.
    fn merge_containing_repo(&mut self, root: &Path, toplevel: &Path) {
        let Some(output) = porcelain(root) else {
            return;
        };
        // Porcelain paths are relative to the repo toplevel, which git reports
        // in canonical (symlink-resolved) form — `root` may be given in another
        // form (e.g. `/var` vs `/private/var` on macOS, where the tree's rows
        // also use the un-resolved form). Compute the toplevel→root prefix in
        // canonical space, then re-anchor each path onto the original `root` so
        // the map's keys line up with the tree's `row.path` values.
        let canon_top = fs::canonicalize(toplevel).unwrap_or_else(|_| toplevel.to_path_buf());
        let prefix = fs::canonicalize(root)
            .ok()
            .and_then(|cr| cr.strip_prefix(&canon_top).ok().map(Path::to_path_buf))
            .unwrap_or_default();
        for (rel, status) in parse_porcelain(&output) {
            // Keep only paths inside `root`; drop the prefix and re-anchor.
            let Ok(under_root) = rel.strip_prefix(&prefix) else {
                continue;
            };
            self.record(root.join(under_root), status, root);
        }
    }

    /// Merge the status of a repository nested under `root`. `repo` is the
    /// directory that holds the `.git`, already in `root`-anchored form (built
    /// by the scan from `root`), so it is also the repo's work-tree top: its
    /// porcelain paths are relative to it, and `repo.join(rel)` is exactly the
    /// path the tree uses for that file — no canonicalisation needed.
    fn merge_nested_repo(&mut self, root: &Path, repo: &Path) {
        let Some(output) = porcelain(repo) else {
            return;
        };
        for (rel, status) in parse_porcelain(&output) {
            self.record(repo.join(rel), status, root);
        }
    }

    /// Route one re-anchored entry into the snapshot. Ignored directories become
    /// path prefixes (git collapses them, so their contents aren't listed);
    /// ignored files are stored as exact entries; everything else is a change and
    /// rolls up into its ancestor directories. Ignored paths never roll up — a
    /// directory is only grey if it is itself ignored, not because it contains
    /// an ignored child.
    fn record(&mut self, abs: PathBuf, status: GitStatus, root: &Path) {
        if status == GitStatus::Ignored {
            if abs.is_dir() {
                self.ignored_dirs.push(abs);
            } else {
                self.map.insert(abs, GitStatus::Ignored);
            }
        } else {
            self.insert_path(&abs, status, root);
        }
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

/// Directories under `root` that are their own git repository (they hold a
/// `.git`). Searched **breadth-first** so the shallowest repositories — the
/// usual case, repos sitting directly under a `~/Projects`-style parent — are
/// found first and within budget even when a sibling holds a huge non-repo
/// subtree. The scan descends only through non-repo directories: once a repo is
/// found it is recorded and **not** entered, so a repo's internals (and its
/// `node_modules`/`target`/etc.) are never walked. Paths are returned in
/// `root`-anchored form, matching the tree's row paths.
fn find_nested_repos(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut budget = MAX_REPO_SCAN_DIRS;
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));
    while let Some((dir, depth)) = queue.pop_front() {
        if depth > MAX_REPO_SCAN_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                return out;
            }
            // `file_type()` does not follow symlinks, so symlinked directories
            // are skipped — that avoids both cycles and wandering outside the tree.
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if !ft.is_dir() || entry.file_name() == ".git" {
                continue;
            }
            budget -= 1;
            let path = entry.path();
            // `.git` is a directory for a normal clone and a file for a submodule
            // or linked work tree; either marks `path` as a repository root.
            // Prune there — a repo's own subtree is covered by its status.
            if path.join(".git").exists() {
                out.push(path);
            } else {
                queue.push_back((path, depth + 1));
            }
        }
    }
    out
}

/// `git -C <dir> status --porcelain -z --untracked-files=all --ignored=matching`
/// output, or `None` on failure. `-z` keeps paths intact (NUL-separated, no
/// quoting) so names with spaces or newlines parse correctly. `--ignored=matching`
/// adds `!!` records for ignored paths while keeping a wholly-ignored directory
/// collapsed to one `dir/` entry (plain `--ignored` would expand it into every
/// file under it once `--untracked-files=all` is set), so an ignored directory
/// can be greyed as a unit.
fn porcelain(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain", "-z", "--untracked-files=all", "--ignored=matching"])
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
/// own colour grouping: ignored (`!!`) is grey; untracked (`??`) and any
/// worktree change (`Y` set) are red; an index-only change (`X` set, clean
/// worktree) is the green "staged" state.
fn classify(x: char, y: char) -> GitStatus {
    if x == '!' && y == '!' {
        GitStatus::Ignored
    } else if x == '?' && y == '?' {
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
        // Ignored -> grey.
        assert_eq!(classify('!', '!'), GitStatus::Ignored);
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
    fn ignored_files_and_directories_render_as_ignored() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        init_repo(root);
        // Ignore a whole directory and a single file.
        fs::write(root.join(".gitignore"), "ignored_dir/\nsecret.txt\n").unwrap();
        git(root, &["add", ".gitignore"]);
        git(root, &["commit", "-q", "-m", "ignore"]);

        // An ignored directory holding a file (git collapses it to one entry).
        fs::create_dir(root.join("ignored_dir")).unwrap();
        fs::write(root.join("ignored_dir").join("a.log"), "x").unwrap();
        // A single ignored file, plus a normal untracked file as a control.
        fs::write(root.join("secret.txt"), "s").unwrap();
        fs::write(root.join("visible.txt"), "v").unwrap();

        let st = GitStatuses::load(root);

        // The ignored directory and everything under it are grey (Ignored),
        // even though git only reported the collapsed directory entry.
        assert_eq!(st.get(&root.join("ignored_dir")), Some(GitStatus::Ignored));
        assert_eq!(st.get(&root.join("ignored_dir").join("a.log")), Some(GitStatus::Ignored));
        // The single ignored file is grey too.
        assert_eq!(st.get(&root.join("secret.txt")), Some(GitStatus::Ignored));
        // The untracked control file stays red, not grey.
        assert_eq!(st.get(&root.join("visible.txt")), Some(GitStatus::Untracked));
        // Containing an ignored child does not grey the parent (here, the root).
        assert_eq!(st.get(root), None);
    }

    #[test]
    fn load_outside_a_repo_with_no_nested_repos_is_empty() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "x").unwrap();
        // Not a repo and nothing nested -> empty snapshot, no panic.
        let st = GitStatuses::load(dir.path());
        assert_eq!(st.get(&dir.path().join("a.txt")), None);
    }

    #[test]
    fn load_colours_nested_repos_under_a_non_repo_root() {
        // The tree root is NOT a git repo, but two sub-directories are their own
        // repositories (one of them a level deep). Each repo's status colours
        // its own subtree; the non-repo root and its plain files stay uncoloured.
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert!(repo_toplevel(root).is_none(), "root must not be a repo for this test");

        // repo A: <root>/alpha with an untracked file.
        let alpha = root.join("alpha");
        fs::create_dir(&alpha).unwrap();
        init_repo(&alpha);
        fs::write(alpha.join("loose.txt"), "u").unwrap();

        // repo B: <root>/nested/beta (a level below root) with a staged file.
        fs::create_dir(root.join("nested")).unwrap();
        let beta = root.join("nested").join("beta");
        fs::create_dir(&beta).unwrap();
        init_repo(&beta);
        fs::write(beta.join("new.rs"), "n").unwrap();
        git(&beta, &["add", "new.rs"]);

        // A plain file directly under the non-repo root: never coloured.
        fs::write(root.join("plain.txt"), "p").unwrap();

        let st = GitStatuses::load(root);

        // alpha's untracked file is red; alpha/ rolls up red.
        assert_eq!(st.get(&alpha.join("loose.txt")), Some(GitStatus::Untracked));
        assert_eq!(st.get(&alpha), Some(GitStatus::Untracked));

        // beta's staged file is green; beta/ and the intermediate nested/ roll up.
        assert_eq!(st.get(&beta.join("new.rs")), Some(GitStatus::Staged));
        assert_eq!(st.get(&beta), Some(GitStatus::Staged));
        assert_eq!(st.get(&root.join("nested")), Some(GitStatus::Staged));

        // The non-repo root and its plain file are untouched.
        assert_eq!(st.get(root), None);
        assert_eq!(st.get(&root.join("plain.txt")), None);
    }

    #[test]
    fn nested_scan_does_not_descend_into_a_found_repo() {
        // A repo nested under a non-repo root holds an inner directory that
        // itself looks like a candidate. Because the scan prunes at the repo
        // boundary, the inner path is coloured by the OUTER repo's status (it is
        // tracked there), not treated as a separate repository.
        let dir = tempdir().unwrap();
        let root = dir.path();
        let repo = root.join("proj");
        fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        fs::create_dir(repo.join("inner")).unwrap();
        fs::write(repo.join("inner").join("f.rs"), "x").unwrap(); // untracked in proj

        let st = GitStatuses::load(root);
        assert_eq!(st.get(&repo.join("inner").join("f.rs")), Some(GitStatus::Untracked));
        assert_eq!(st.get(&repo.join("inner")), Some(GitStatus::Untracked));
    }
}
