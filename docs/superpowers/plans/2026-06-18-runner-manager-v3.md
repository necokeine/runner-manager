# runner-manager v3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Multiple sessions per directory (shell/claude chooser), sessions shown as auto-syncing tree rows (badge removed), and an inline read-only file viewer in the right pane.

**Architecture:** A `SessionStore` tracks per-run sessions and reconciles against `tmux list-sessions`. A new `rows.rs` flattens the filesystem node tree + the session map into typed `Row`s (Dir → Sessions → Files). The right pane shows either the embedded PTY or a `viewer::FileView`. Focus becomes Tree/Right; a `Popup` enum drives the help and chooser overlays.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, portable-pty 0.9, tui-term 0.2, vt100 (via `tui_term::vt100`), tempfile (dev).

## Global Constraints

- Inner tmux socket is `runner`. The embedded PTY still runs `tmux -L runner new-session -A -s scratch` (unchanged from v2). The `scratch` session is never shown as a row.
- Slugs are unique on the socket: `<dir-slug>-<kind>` with `-2`,`-3`… on collision; `<dir-slug>` uses the existing `session::slugify`.
- Session row labels: kind plus a 1-based index when a directory has more than one of that kind: `shell`, `shell 2`, `claude`.
- Shell session command: none (default `$SHELL`). Claude session command: `claude`.
- Sessions are never killed by a key; they are pruned when `tmux list-sessions` no longer lists their slug. There is NO `x` key.
- Files open a read-only viewer in the right pane (never `$EDITOR`/tmux). Only one viewer at a time; selecting a session or another file replaces it.
- Focus is `Focus { Tree, Right }`. `Ctrl-q` toggles; click focuses a pane. Selecting any tree row keeps Tree focus.
- Tests are inline `#[cfg(test)]`. TDD for pure units. PTY/render/event-loop/popups are manual-verified.
- The `●` badge is removed entirely.

## File Structure

- `tmux.rs` — `new_session` gains an optional command arg (Task 1).
- `session.rs` — add `SessionKind`, `SessionEntry`, `SessionRow`, `SessionStore` (Task 2); remove `SessionRegistry` in the rewire (Task 5).
- `viewer.rs` (new) — `FileView` (Task 3).
- `rows.rs` (new) — `RowKind`, `Row`, `build_rows` (Task 4).
- `app.rs`, `ui.rs`, `run.rs` — rewired together (Task 5).
- `tree.rs` — keeps the `Node`/`Tree`/lazy-load model; its old `Row`/`visible_rows` are removed in the rewire (Task 5).
- `README.md` — updated (Task 6).

---

### Task 1: tmux `new_session` optional command

**Files:**
- Modify: `src/tmux.rs`
- Modify: `src/app.rs` (one call site, to keep the crate compiling)

**Interfaces:**
- Produces: `Tmux::new_session(&self, slug: &str, dir: &Path, command: Option<&str>) -> io::Result<()>` — appends `command` as a trailing argument when `Some`.

- [ ] **Step 1: Update the existing test and add a command test** — in `src/tmux.rs`, replace the `new_session_builds_detached_with_dir` test body and add a second test:

```rust
    #[test]
    fn new_session_builds_detached_with_dir() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.new_session("src", Path::new("/tmp/proj/src"), None).unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "new-session", "-d", "-s", "src", "-c", "/tmp/proj/src"]
        );
    }

    #[test]
    fn new_session_with_command_appends_it() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.new_session("src", Path::new("/tmp/proj/src"), Some("claude"))
            .unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "new-session", "-d", "-s", "src", "-c", "/tmp/proj/src", "claude"]
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib tmux 2>&1 | tail -20`
Expected: FAIL to compile (`new_session` takes 2 args, test passes 3).

- [ ] **Step 3: Change `new_session`** in `src/tmux.rs`:

```rust
    pub fn new_session(&self, slug: &str, dir: &Path, command: Option<&str>) -> io::Result<()> {
        let dir = dir.to_str().unwrap_or(".");
        let mut args: Vec<&str> = vec!["new-session", "-d", "-s", slug, "-c", dir];
        if let Some(cmd) = command {
            args.push(cmd);
        }
        self.run(&args)?;
        Ok(())
    }
```

- [ ] **Step 4: Fix the one existing caller** in `src/app.rs` — find `self.tmux.new_session(&slug, dir)?;` (in `ensure_session`) and change it to `self.tmux.new_session(&slug, dir, None)?;`.

- [ ] **Step 5: Run tests**

Run: `cargo test 2>&1 | tail -5`
Expected: all pass (the two tmux tests included).

- [ ] **Step 6: Commit**

```bash
git add src/tmux.rs src/app.rs
git commit -m "feat: new_session accepts an optional command"
```

---

### Task 2: `SessionStore` (added alongside the old registry)

**Files:**
- Modify: `src/session.rs` (add the new types; leave `SessionRegistry` in place — the rewire removes it)

**Interfaces:**
- Produces:
  - `enum SessionKind { Shell, Claude }` (derives `Debug, Clone, Copy, PartialEq, Eq, Hash`) with `label_base(&self) -> &'static str` (`"shell"`/`"claude"`) and `command(&self) -> Option<&'static str>` (`None`/`Some("claude")`).
  - `struct SessionRow { pub slug: String, pub kind: SessionKind, pub label: String }` (derives `Debug, Clone, PartialEq, Eq`).
  - `struct SessionStore` with `new()`, `create(&mut self, dir: &Path, root: &Path, kind: SessionKind) -> String` (returns the new slug), `sync(&mut self, live: &std::collections::HashSet<String>)`, `by_dir(&self) -> HashMap<PathBuf, Vec<SessionRow>>`.

- [ ] **Step 1: Write the failing tests** (append to `src/session.rs`'s test module, inside `mod tests`)

```rust
    #[test]
    fn store_creates_unique_slugs_per_kind() {
        let mut s = SessionStore::new();
        let root = Path::new("/p");
        let a = s.create(Path::new("/p/src"), root, SessionKind::Shell);
        let b = s.create(Path::new("/p/src"), root, SessionKind::Shell);
        let c = s.create(Path::new("/p/src"), root, SessionKind::Claude);
        assert_eq!(a, "src-shell");
        assert_eq!(b, "src-shell-2");
        assert_eq!(c, "src-claude");
    }

    #[test]
    fn store_labels_index_duplicates_by_dir_and_kind() {
        let mut s = SessionStore::new();
        let root = Path::new("/p");
        s.create(Path::new("/p/src"), root, SessionKind::Shell);
        s.create(Path::new("/p/src"), root, SessionKind::Shell);
        s.create(Path::new("/p/src"), root, SessionKind::Claude);
        let by = s.by_dir();
        let rows = &by[&PathBuf::from("/p/src")];
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["shell", "shell 2", "claude"]);
    }

    #[test]
    fn store_sync_prunes_dead_sessions() {
        let mut s = SessionStore::new();
        let root = Path::new("/p");
        let a = s.create(Path::new("/p/src"), root, SessionKind::Shell);
        let _b = s.create(Path::new("/p/src"), root, SessionKind::Claude);
        let live: std::collections::HashSet<String> = [a.clone()].into_iter().collect();
        s.sync(&live);
        let by = s.by_dir();
        let rows = &by[&PathBuf::from("/p/src")];
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, a);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib session 2>&1 | tail -20`
Expected: FAIL — `SessionStore`/`SessionKind` not found.

- [ ] **Step 3: Add the implementation** to `src/session.rs` (above the test module; keep `slugify` and `SessionRegistry`). Ensure the file's top has `use std::collections::HashSet;` alongside the existing `HashMap` import.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKind {
    Shell,
    Claude,
}

impl SessionKind {
    pub fn label_base(&self) -> &'static str {
        match self {
            SessionKind::Shell => "shell",
            SessionKind::Claude => "claude",
        }
    }
    pub fn command(&self) -> Option<&'static str> {
        match self {
            SessionKind::Shell => None,
            SessionKind::Claude => Some("claude"),
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    dir: PathBuf,
    kind: SessionKind,
    slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub slug: String,
    pub kind: SessionKind,
    pub label: String,
}

#[derive(Default)]
pub struct SessionStore {
    entries: Vec<Entry>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, dir: &Path, root: &Path, kind: SessionKind) -> String {
        let dir_slug = slugify(&dir.strip_prefix(root).unwrap_or(dir).to_string_lossy());
        let base = format!("{dir_slug}-{}", kind.label_base());
        let mut slug = base.clone();
        let mut n = 2;
        while self.entries.iter().any(|e| e.slug == slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        self.entries.push(Entry {
            dir: dir.to_path_buf(),
            kind,
            slug: slug.clone(),
        });
        slug
    }

    pub fn sync(&mut self, live: &HashSet<String>) {
        self.entries.retain(|e| live.contains(&e.slug));
    }

    pub fn by_dir(&self) -> HashMap<PathBuf, Vec<SessionRow>> {
        let mut map: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        let mut counts: HashMap<(PathBuf, SessionKind), usize> = HashMap::new();
        for e in &self.entries {
            let c = counts.entry((e.dir.clone(), e.kind)).or_insert(0);
            *c += 1;
            let label = if *c == 1 {
                e.kind.label_base().to_string()
            } else {
                format!("{} {}", e.kind.label_base(), c)
            };
            map.entry(e.dir.clone()).or_default().push(SessionRow {
                slug: e.slug.clone(),
                kind: e.kind,
                label,
            });
        }
        map
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib session 2>&1 | tail -10`
Expected: PASS (existing slugify/registry tests + 3 new).

- [ ] **Step 5: Commit**

```bash
git add src/session.rs
git commit -m "feat: add SessionStore (multi-session per directory)"
```

---

### Task 3: `viewer.rs` read-only file viewer model

**Files:**
- Create: `src/viewer.rs`
- Modify: `src/lib.rs` (add `pub mod viewer;`)

**Interfaces:**
- Produces: `struct FileView { pub path: PathBuf, pub lines: Vec<String>, pub scroll: usize }` with `load(path: &Path) -> FileView`, `scroll_down(&mut self, n: usize)`, `scroll_up(&mut self, n: usize)`.

- [ ] **Step 1: Write the failing tests** (create `src/viewer.rs` with just the test module first)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn load_reads_utf8_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
        let v = FileView::load(f.path());
        assert_eq!(v.lines, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn load_binary_shows_placeholder() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0xff, 0xfe, 0x00, 0x01]).unwrap();
        let v = FileView::load(f.path());
        assert_eq!(v.lines.len(), 1);
        assert!(v.lines[0].starts_with("<binary file:"));
    }

    #[test]
    fn load_caps_line_count() {
        let mut f = NamedTempFile::new().unwrap();
        for _ in 0..6000 {
            writeln!(f, "x").unwrap();
        }
        let v = FileView::load(f.path());
        assert_eq!(v.lines.len(), 5000);
    }

    #[test]
    fn scroll_clamps() {
        let v0 = FileView {
            path: std::path::PathBuf::from("/x"),
            lines: vec!["a".into(), "b".into(), "c".into()],
            scroll: 0,
        };
        let mut v = v0;
        v.scroll_down(10);
        assert_eq!(v.scroll, 2); // clamped to lines.len()-1
        v.scroll_up(10);
        assert_eq!(v.scroll, 0);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib viewer 2>&1 | tail -20`
Expected: FAIL — `FileView` not found / module not declared.

- [ ] **Step 3: Write the implementation** (prepend above the test module in `src/viewer.rs`) and add `pub mod viewer;` to `src/lib.rs`.

```rust
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 5000;

pub struct FileView {
    pub path: PathBuf,
    pub lines: Vec<String>,
    pub scroll: usize,
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

impl FileView {
    pub fn load(path: &Path) -> FileView {
        let lines = match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text.lines().take(MAX_LINES).map(|l| l.to_string()).collect(),
                Err(_) => vec![format!("<binary file: {}>", name(path))],
            },
            Err(_) => vec![format!("<unable to read: {}>", name(path))],
        };
        FileView {
            path: path.to_path_buf(),
            lines,
            scroll: 0,
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max = self.lines.len().saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib viewer 2>&1 | tail -10`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/viewer.rs src/lib.rs
git commit -m "feat: add read-only file viewer model"
```

---

### Task 4: `rows.rs` typed rows + `build_rows`

**Files:**
- Create: `src/rows.rs`
- Modify: `src/lib.rs` (add `pub mod rows;`)

**Interfaces:**
- Consumes: `crate::tree::Node` (fields `path`, `name`, `is_dir`, `expanded`, `children` are public), `crate::session::{SessionKind, SessionRow}`.
- Produces:
  - `enum RowKind { Dir { expanded: bool }, Session { slug: String, kind: SessionKind }, File }` (derives `Debug, Clone, PartialEq, Eq`).
  - `struct Row { pub path: PathBuf, pub label: String, pub depth: usize, pub kind: RowKind }` (derives `Debug, Clone, PartialEq, Eq`).
  - `fn build_rows(root: &Node, sessions: &HashMap<PathBuf, Vec<SessionRow>>) -> Vec<Row>`.

- [ ] **Step 1: Write the failing tests** (create `src/rows.rs` with just the test module first)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionKind, SessionRow};
    use crate::tree::Tree;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn rows_show_dir_then_sessions_then_files_when_expanded() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("readme.md"), "x").unwrap();
        let tree = Tree::new(dir.path().to_path_buf()); // root expanded, children loaded
        let mut sessions: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        sessions.insert(
            dir.path().to_path_buf(),
            vec![SessionRow { slug: "root-shell".into(), kind: SessionKind::Shell, label: "shell".into() }],
        );
        let rows = build_rows(&tree.root, &sessions);
        // row 0 = root dir; row 1 = its shell session (depth 1); row 2 = readme.md (depth 1)
        assert!(matches!(rows[0].kind, RowKind::Dir { .. }));
        assert!(matches!(rows[1].kind, RowKind::Session { .. }));
        assert_eq!(rows[1].label, "shell");
        assert_eq!(rows[1].depth, 1);
        assert!(matches!(rows[2].kind, RowKind::File));
        assert_eq!(rows[2].label, "readme.md");
    }

    #[test]
    fn sessions_show_even_when_dir_collapsed() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("a.txt"), "x").unwrap();
        let tree = Tree::new(dir.path().to_path_buf());
        // 'sub' is collapsed (not expanded), but give it a session
        let mut sessions: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        sessions.insert(
            dir.path().join("sub"),
            vec![SessionRow { slug: "sub-shell".into(), kind: SessionKind::Shell, label: "shell".into() }],
        );
        let rows = build_rows(&tree.root, &sessions);
        // 'sub' dir row is present, immediately followed by its session row,
        // and a.txt is NOT present (sub is collapsed)
        let sub_idx = rows.iter().position(|r| r.label == "sub").unwrap();
        assert!(matches!(rows[sub_idx + 1].kind, RowKind::Session { .. }));
        assert!(!rows.iter().any(|r| r.label == "a.txt"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib rows 2>&1 | tail -20`
Expected: FAIL — `build_rows`/`Row`/`RowKind` not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module) and add `pub mod rows;` to `src/lib.rs`.

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use crate::session::{SessionKind, SessionRow};
use crate::tree::Node;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Dir { expanded: bool },
    Session { slug: String, kind: SessionKind },
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub label: String,
    pub depth: usize,
    pub kind: RowKind,
}

pub fn build_rows(root: &Node, sessions: &HashMap<PathBuf, Vec<SessionRow>>) -> Vec<Row> {
    let mut out = Vec::new();
    collect(root, 0, sessions, &mut out);
    out
}

fn collect(node: &Node, depth: usize, sessions: &HashMap<PathBuf, Vec<SessionRow>>, out: &mut Vec<Row>) {
    if node.is_dir {
        out.push(Row {
            path: node.path.clone(),
            label: node.name.clone(),
            depth,
            kind: RowKind::Dir { expanded: node.expanded },
        });
        if let Some(sess) = sessions.get(&node.path) {
            for s in sess {
                out.push(Row {
                    path: node.path.clone(),
                    label: s.label.clone(),
                    depth: depth + 1,
                    kind: RowKind::Session { slug: s.slug.clone(), kind: s.kind },
                });
            }
        }
        if node.expanded {
            if let Some(children) = &node.children {
                for c in children {
                    collect(c, depth + 1, sessions, out);
                }
            }
        }
    } else {
        out.push(Row {
            path: node.path.clone(),
            label: node.name.clone(),
            depth,
            kind: RowKind::File,
        });
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib rows 2>&1 | tail -10`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/rows.rs src/lib.rs
git commit -m "feat: add typed rows and build_rows flattening"
```

---

### Task 5: Rewire app, ui, run to v3 (and remove the old row/registry)

This is the coordinated change that switches `app.rs`, `ui.rs`, and `run.rs` from the old `tree::Row`/`SessionRegistry`/badge model to the new `rows::Row`/`SessionStore`/viewer/popup model, and deletes the now-dead code. It lands in one commit and the crate is green at the end (`cargo test`, `cargo clippy`).

**Files:**
- Rewrite: `src/app.rs`
- Rewrite: `src/ui.rs`
- Rewrite: `src/run.rs`
- Modify: `src/tree.rs` (remove `Row` struct and `Tree::visible_rows`; keep `Node`, `Tree::new`, `Tree::node_at_mut`, and the node tests)
- Modify: `src/session.rs` (remove `SessionRegistry` and its two tests)

**Interfaces produced (consumed across these files):**
- `app::Focus { Tree, Right }`, `app::Popup { None, Help, Chooser { dir: PathBuf, selected: usize } }`.
- `App<R>` public fields: `tree: Tree`, `store: SessionStore`, `tmux: Tmux<R>`, `root: PathBuf`, `selected: usize`, `rows: Vec<rows::Row>`, `host_tty: Option<String>`, `viewer: Option<viewer::FileView>`, `focus: Focus`, `popup: Popup`, `status: String`.
- `App` methods used by `run.rs`: `new(root, tmux)`, `rebuild_rows()`, `selected_row() -> Option<&rows::Row>`, `up()`, `down()`, `activate()`, `open_chooser()`, `chooser_move(delta: i32)`, `chooser_confirm()`, `chooser_cancel()`, `open_file(path: &Path)`, `viewer_scroll(delta: i32, page: bool)`, `sync()`, `toggle_focus()`, `host_client_ready()`.
- `ui`: `render(f, area, &app) -> Layout` (takes `&App`), `resolve_pane_click`, `PaneHit`, `Hit`, `ListLayout`, `Pane`, `render_help`, `chooser_options() -> [&str; 2]`, `centered_rect`.

- [ ] **Step 1: Rewrite `src/app.rs`** with this exact content:

```rust
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::rows::{build_rows, Row, RowKind};
use crate::session::{SessionKind, SessionStore};
use crate::tmux::{CommandRunner, Tmux};
use crate::tree::Tree;
use crate::viewer::FileView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    None,
    Help,
    Chooser { dir: PathBuf, selected: usize },
}

/// The kinds offered by the chooser, in display order.
pub const CHOOSER_KINDS: [SessionKind; 2] = [SessionKind::Shell, SessionKind::Claude];

pub struct App<R: CommandRunner> {
    pub tree: Tree,
    pub store: SessionStore,
    pub tmux: Tmux<R>,
    pub root: PathBuf,
    pub selected: usize,
    pub rows: Vec<Row>,
    pub host_tty: Option<String>,
    pub viewer: Option<FileView>,
    pub focus: Focus,
    pub popup: Popup,
    pub status: String,
}

impl<R: CommandRunner> App<R> {
    pub fn new(root: PathBuf, tmux: Tmux<R>) -> Self {
        let tree = Tree::new(root.clone());
        let mut app = Self {
            tree,
            store: SessionStore::new(),
            tmux,
            root,
            selected: 0,
            rows: Vec::new(),
            host_tty: None,
            viewer: None,
            focus: Focus::Tree,
            popup: Popup::None,
            status: String::new(),
        };
        app.rebuild_rows();
        app
    }

    pub fn rebuild_rows(&mut self) {
        self.rows = build_rows(&self.tree.root, &self.store.by_dir());
        if !self.rows.is_empty() && self.selected >= self.rows.len() {
            self.selected = self.rows.len() - 1;
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    /// Enter/click on the selected row, dispatched by kind.
    pub fn activate(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        match row.kind {
            RowKind::Dir { .. } => {
                if let Some(node) = self.tree.node_at_mut(&row.path) {
                    node.toggle();
                }
                self.rebuild_rows();
            }
            RowKind::Session { slug, .. } => {
                self.switch_to(&slug)?;
            }
            RowKind::File => {
                self.open_file(&row.path);
            }
        }
        Ok(())
    }

    /// Open the shell/claude chooser for the selected directory row.
    pub fn open_chooser(&mut self) {
        if let Some(row) = self.selected_row() {
            if matches!(row.kind, RowKind::Dir { .. }) {
                self.popup = Popup::Chooser {
                    dir: row.path.clone(),
                    selected: 0,
                };
            }
        }
    }

    pub fn chooser_move(&mut self, delta: i32) {
        if let Popup::Chooser { selected, .. } = &mut self.popup {
            let n = CHOOSER_KINDS.len() as i32;
            *selected = (((*selected as i32 + delta) % n + n) % n) as usize;
        }
    }

    pub fn chooser_confirm(&mut self) -> io::Result<()> {
        if let Popup::Chooser { dir, selected } = self.popup.clone() {
            let kind = CHOOSER_KINDS[selected];
            self.popup = Popup::None;
            self.create_session(&dir, kind)?;
        }
        Ok(())
    }

    pub fn chooser_cancel(&mut self) {
        self.popup = Popup::None;
    }

    fn create_session(&mut self, dir: &Path, kind: SessionKind) -> io::Result<()> {
        let slug = self.store.create(dir, &self.root, kind);
        self.tmux.new_session(&slug, dir, kind.command())?;
        self.rebuild_rows();
        self.switch_to(&slug)?;
        self.status = format!("started {}", kind.label_base());
        Ok(())
    }

    fn switch_to(&mut self, slug: &str) -> io::Result<()> {
        self.viewer = None;
        if let Some(tty) = self.ensure_host_tty()? {
            self.tmux.switch_client(&tty, slug)?;
            self.status = format!("switched to {slug}");
        } else {
            self.status = "no host client to switch".to_string();
        }
        Ok(())
    }

    pub fn open_file(&mut self, path: &Path) {
        self.viewer = Some(FileView::load(path));
        self.status = format!("viewing {}", path.display());
    }

    pub fn viewer_scroll(&mut self, delta: i32, page: bool) {
        if let Some(v) = &mut self.viewer {
            let step = if page { 10 } else { 1 };
            if delta < 0 {
                v.scroll_up(step);
            } else {
                v.scroll_down(step);
            }
        }
    }

    pub fn sync(&mut self) -> io::Result<()> {
        let live: HashSet<String> = self.tmux.list_sessions()?.into_iter().collect();
        self.store.sync(&live);
        self.rebuild_rows();
        Ok(())
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Right,
            Focus::Right => Focus::Tree,
        };
    }

    pub fn host_client_ready(&mut self) -> bool {
        matches!(self.tmux.host_tty(), Ok(Some(_)))
    }

    fn ensure_host_tty(&mut self) -> io::Result<Option<String>> {
        self.host_tty = self.tmux.host_tty()?;
        Ok(self.host_tty.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{MockRunner, Tmux};
    use std::fs;
    use tempfile::tempdir;

    fn app_over_tempdir() -> (tempfile::TempDir, App<MockRunner>) {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("a.rs"), "x").unwrap();
        let tmux = Tmux::new("runner", MockRunner::new());
        let app = App::new(dir.path().to_path_buf(), tmux);
        (dir, app)
    }

    #[test]
    fn activate_dir_toggles_expand() {
        let (_d, mut app) = app_over_tempdir();
        // rows[0] = root dir (expanded). Select the 'src' dir row.
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.activate().unwrap();
        assert!(app.rows.iter().any(|r| r.label == "a.rs"));
        assert_eq!(app.tmux.runner.call_count(), 0); // no tmux for expand
    }

    #[test]
    fn chooser_confirm_creates_shell_and_switches() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        assert!(matches!(app.popup, Popup::Chooser { selected: 0, .. }));
        // shell (index 0): new-session (no command), then list-clients, then switch-client
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_confirm().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "new-session");
        assert!(!app.tmux.runner.nth_call(0).contains(&"claude".to_string()));
        assert_eq!(app.tmux.runner.nth_call(1)[2], "list-clients");
        assert_eq!(app.tmux.runner.nth_call(2)[2], "switch-client");
        // a 'shell' session row now exists under src
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. }) && r.label == "shell"));
    }

    #[test]
    fn chooser_confirm_claude_appends_command() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        app.chooser_move(1); // select claude (index 1)
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_confirm().unwrap();
        assert!(app.tmux.runner.nth_call(0).contains(&"claude".to_string()));
    }

    #[test]
    fn activate_file_opens_viewer_no_tmux() {
        let (_d, mut app) = app_over_tempdir();
        // expand src so a.rs is visible
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.activate().unwrap();
        let file_idx = app.rows.iter().position(|r| r.label == "a.rs").unwrap();
        app.selected = file_idx;
        app.activate().unwrap();
        assert!(app.viewer.is_some());
        assert_eq!(app.tmux.runner.call_count(), 0);
    }

    #[test]
    fn sync_prunes_dead_session_rows() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_confirm().unwrap();
        assert!(app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
        // sync with an empty live set -> the session is gone
        app.tmux.runner.push(true, ""); // list-sessions returns nothing
        app.sync().unwrap();
        assert!(!app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
    }
}
```

- [ ] **Step 2: Rewrite `src/ui.rs`** with this exact content:

```rust
use std::path::PathBuf;

use ratatui::layout::{Constraint, Direction, Layout as RtLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, CHOOSER_KINDS, Focus, Popup};
use crate::rows::{Row, RowKind};
use crate::tmux::CommandRunner;

pub struct ListLayout {
    pub origin_y: u16,
    pub button_col_start: u16,
    pub button_col_end: u16,
    pub row_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Row(usize),
    Button(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneHit {
    Tree(Option<Hit>),
    Right,
}

pub struct Layout {
    pub tree: ListLayout,
    pub split_col: u16,
    pub term_area: Rect,
}

pub fn resolve_click(col: u16, row: u16, layout: &ListLayout) -> Option<Hit> {
    if row < layout.origin_y {
        return None;
    }
    let idx = (row - layout.origin_y) as usize;
    if idx >= layout.row_count {
        return None;
    }
    if col >= layout.button_col_start && col <= layout.button_col_end {
        Some(Hit::Button(idx))
    } else {
        Some(Hit::Row(idx))
    }
}

pub fn resolve_pane_click(col: u16, row: u16, split_col: u16, tree_layout: &ListLayout) -> PaneHit {
    if col >= split_col {
        PaneHit::Right
    } else {
        PaneHit::Tree(resolve_click(col, row, tree_layout))
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn row_line(row: &Row, width: usize) -> String {
    let indent = "  ".repeat(row.depth);
    match &row.kind {
        RowKind::Dir { expanded } => {
            let icon = if *expanded { "▾ " } else { "▸ " };
            let left = format!("{indent}{icon}{}", row.label);
            let btn = "[+]";
            let pad = width.saturating_sub(left.chars().count() + btn.len());
            format!("{left}{}{btn}", " ".repeat(pad))
        }
        RowKind::Session { .. } => format!("{indent}• {}", row.label),
        RowKind::File => format!("{indent}  {}", row.label),
    }
}

pub fn render<R: CommandRunner>(
    f: &mut Frame,
    area: Rect,
    app: &App<R>,
    screen: Option<&vt100::Screen>,
) -> Layout {
    let chunks = RtLayout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);
    let tree_area = chunks[0];
    let right_area = chunks[1];

    // ---- left: tree ----
    let tree_block = Block::default()
        .title("tree")
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Tree));
    let inner = tree_block.inner(tree_area);
    f.render_widget(tree_block, tree_area);

    let width = inner.width as usize;
    let items: Vec<ListItem> = app.rows.iter().map(|r| ListItem::new(row_line(r, width))).collect();
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !app.rows.is_empty() {
        state.select(Some(app.selected.min(app.rows.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);

    let tree_layout = ListLayout {
        origin_y: inner.y,
        button_col_start: inner.x + inner.width.saturating_sub(3),
        button_col_end: inner.x + inner.width.saturating_sub(1),
        row_count: app.rows.len(),
    };

    // ---- right: terminal or viewer ----
    // The embedded PTY's vt100 parser is owned by run.rs, so the screen is
    // passed in: Some when the terminal is shown, None when the viewer is.
    let right_focused = app.focus == Focus::Right;
    let right_inner = Block::default().borders(Borders::ALL).inner(right_area);
    if let Some(view) = &app.viewer {
        let title = view
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style(right_focused));
        let body = view.lines.join("\n");
        let para = Paragraph::new(body).block(block).scroll((view.scroll as u16, 0));
        f.render_widget(para, right_area);
    } else {
        let block = Block::default()
            .title("terminal")
            .borders(Borders::ALL)
            .border_style(border_style(right_focused));
        f.render_widget(block, right_area);
        let screen = screen.expect("terminal screen present when viewer is None");
        f.render_widget(PseudoTerminal::new(screen), right_inner);
    }

    Layout {
        tree: tree_layout,
        split_col: right_area.x,
        term_area: right_inner,
    }
}
```

Then append the popup + helper rendering and tests to `ui.rs`:

```rust
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let v = RtLayout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let h = RtLayout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1]);
    h[1]
}

pub fn render_help(f: &mut Frame, area: Rect) {
    let lines = [
        "j / ↓      move down",
        "k / ↑      move up",
        "Enter      expand dir / switch session / view file",
        "a / [+]    new session (shell or claude) on a dir",
        "h / ?      this help",
        "Ctrl-q     toggle focus (tree / right pane)",
        "q          quit",
        "",
        "right pane focused: type into the shell, or",
        "j/k/PgUp/PgDn to scroll a file view",
        "",
        "— press any key to close —",
    ];
    let popup = centered_rect(64, 70, area);
    let block = Block::default().title("Keys").borders(Borders::ALL);
    let para = Paragraph::new(lines.join("\n")).block(block);
    f.render_widget(Clear, popup);
    f.render_widget(para, popup);
}

pub fn render_chooser(f: &mut Frame, area: Rect, selected: usize) -> Rect {
    let popup = centered_rect(40, 30, area);
    let block = Block::default().title("New session").borders(Borders::ALL);
    let items: Vec<ListItem> = CHOOSER_KINDS
        .iter()
        .map(|k| ListItem::new(format!("  {}", k.label_base())))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(selected.min(CHOOSER_KINDS.len() - 1)));
    f.render_widget(Clear, popup);
    f.render_stateful_widget(list, popup, &mut state);
    popup
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_click_distinguishes_row_and_button() {
        let layout = ListLayout { origin_y: 1, button_col_start: 20, button_col_end: 22, row_count: 3 };
        assert_eq!(resolve_click(5, 1, &layout), Some(Hit::Row(0)));
        assert_eq!(resolve_click(21, 2, &layout), Some(Hit::Button(1)));
        assert_eq!(resolve_click(5, 0, &layout), None);
        assert_eq!(resolve_click(5, 10, &layout), None);
    }

    #[test]
    fn resolve_pane_click_splits_on_column() {
        let layout = ListLayout { origin_y: 1, button_col_start: 38, button_col_end: 40, row_count: 3 };
        assert_eq!(resolve_pane_click(5, 2, 50, &layout), PaneHit::Tree(Some(Hit::Row(1))));
        assert_eq!(resolve_pane_click(50, 2, 50, &layout), PaneHit::Right);
    }

    #[test]
    fn centered_rect_is_centered() {
        let area = Rect { x: 0, y: 0, width: 100, height: 100 };
        assert_eq!(centered_rect(50, 50, area), Rect { x: 25, y: 25, width: 50, height: 50 });
    }
}
```

`render` reads `CHOOSER_KINDS[i].label_base()`, so add `use crate::session::SessionKind;` if needed (already pulled transitively via `app`/`rows`; import explicitly if the compiler asks). `CommandRunner` import is needed for the `render`/`App<R>` generic bound.

- [ ] **Step 3: Rewrite `src/run.rs`** with this exact content:

```rust
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, Focus, Popup};
use crate::keys::encode_key;
use crate::pty::Pty;
use crate::tmux::{SystemRunner, Tmux};
use crate::ui::{self, Hit, PaneHit};

pub fn run(root: PathBuf, socket: String, _editor: String) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let pty_args = ["tmux", "-L", socket.as_str(), "new-session", "-A", "-s", "scratch"];
    let mut pty = match Pty::spawn(&pty_args, 24, 80) {
        Ok(p) => p,
        Err(e) => {
            let _ = disable_raw_mode();
            let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
            return Err(e);
        }
    };
    let parser = pty.parser();

    let tmux = Tmux::new(socket, SystemRunner);
    let mut app = App::new(root, tmux);

    for _ in 0..20 {
        if app.host_client_ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = app.tmux.set_global_option("detach-on-destroy", "off");
    let _ = app.sync();

    let mut last_term_size: (u16, u16) = (0, 0);
    let mut last_sync = Instant::now();

    let result = loop {
        let mut captured: Option<ui::Layout> = None;
        let draw_res = terminal.draw(|f| {
            let screen_guard = if app.viewer.is_none() {
                Some(parser.read().unwrap())
            } else {
                None
            };
            let screen = screen_guard.as_ref().map(|g| g.screen());
            captured = Some(ui::render(f, f.area(), &app, screen));
            drop(screen_guard);
            match &app.popup {
                Popup::Help => ui::render_help(f, f.area()),
                Popup::Chooser { selected, .. } => {
                    let _ = ui::render_chooser(f, f.area(), *selected);
                }
                Popup::None => {}
            }
        });
        if let Err(e) = draw_res {
            break Err(e);
        }
        let layout = captured.expect("render returns a Layout");

        if app.viewer.is_none() {
            let term_size = (layout.term_area.height, layout.term_area.width);
            if term_size != last_term_size && term_size.0 > 0 && term_size.1 > 0 {
                let _ = pty.resize(term_size.0, term_size.1);
                last_term_size = term_size;
            }
        }

        if last_sync.elapsed() >= Duration::from_millis(1000) {
            let _ = app.sync();
            last_sync = Instant::now();
        }

        if !event::poll(Duration::from_millis(33)).unwrap_or(false) {
            continue;
        }

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                match app.popup.clone() {
                    Popup::Help => {
                        app.popup = Popup::None;
                    }
                    Popup::Chooser { .. } => match key.code {
                        KeyCode::Esc => app.chooser_cancel(),
                        KeyCode::Enter => {
                            let _ = app.chooser_confirm();
                        }
                        KeyCode::Down | KeyCode::Char('j') => app.chooser_move(1),
                        KeyCode::Up | KeyCode::Char('k') => app.chooser_move(-1),
                        _ => {}
                    },
                    Popup::None => {
                        if key.code == KeyCode::Char('q')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            app.toggle_focus();
                        } else {
                            match app.focus {
                                Focus::Tree => match key.code {
                                    KeyCode::Char('q') => break Ok(()),
                                    KeyCode::Char('h') | KeyCode::Char('?') => {
                                        app.popup = Popup::Help;
                                    }
                                    KeyCode::Char('a') => app.open_chooser(),
                                    KeyCode::Char('j') | KeyCode::Down => app.down(),
                                    KeyCode::Char('k') | KeyCode::Up => app.up(),
                                    KeyCode::Enter => {
                                        let _ = app.activate();
                                    }
                                    _ => {}
                                },
                                Focus::Right => {
                                    if app.viewer.is_some() {
                                        match key.code {
                                            KeyCode::Char('j') | KeyCode::Down => {
                                                app.viewer_scroll(1, false)
                                            }
                                            KeyCode::Char('k') | KeyCode::Up => {
                                                app.viewer_scroll(-1, false)
                                            }
                                            KeyCode::PageDown => app.viewer_scroll(1, true),
                                            KeyCode::PageUp => app.viewer_scroll(-1, true),
                                            _ => {}
                                        }
                                    } else {
                                        let _ = pty.write_input(&encode_key(key));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Mouse(m)) => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    match app.popup.clone() {
                        Popup::Help => app.popup = Popup::None,
                        Popup::Chooser { .. } => app.chooser_cancel(),
                        Popup::None => {
                            match ui::resolve_pane_click(m.column, m.row, layout.split_col, &layout.tree) {
                                PaneHit::Right => app.focus = Focus::Right,
                                PaneHit::Tree(hit) => {
                                    app.focus = Focus::Tree;
                                    match hit {
                                        Some(Hit::Row(idx)) => {
                                            app.selected = idx;
                                            let _ = app.activate();
                                        }
                                        Some(Hit::Button(idx)) => {
                                            app.selected = idx;
                                            app.open_chooser();
                                        }
                                        None => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };

    let restore_raw = disable_raw_mode();
    let restore_screen = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
    result.and(restore_raw).and(restore_screen)
}
```

> Note: `main.rs` calls `run::run(root, "runner".into(), editor)`. The `editor` arg is now unused (`_editor`); leave `main.rs` as-is (it still passes it). `app.tmux.set_global_option` and `Pty`/`encode_key` are unchanged from v2.

- [ ] **Step 4: Remove dead code**
  - In `src/tree.rs`: delete the `pub struct Row { … }` definition and the `Tree::visible_rows` method and its private `collect` helper. Keep `Node`, `Tree::new`, `Tree::node_at_mut`/`find_mut`, and the existing node tests (`root_expands_…`, `toggle_…`, `depth_…`). If a tree test references the removed `Row`/`visible_rows`, update it to use `crate::rows::build_rows(&tree.root, &std::collections::HashMap::new())` instead, asserting on `Row.label`/`Row.kind`.
  - In `src/session.rs`: delete `pub struct SessionRegistry` + its `impl` + the two tests `registry_is_stable_per_path` and `registry_disambiguates_collisions`. Keep `slugify` (+ its tests), `SessionKind`, `SessionRow`, `SessionStore`.

- [ ] **Step 5: Build, test, clippy**

Run: `cargo build && cargo test 2>&1 | tail -8 && cargo clippy --all-targets 2>&1 | tail -20`
Expected: builds; all tests pass (app: 5 new; ui: 3; plus tasks 1–4); clippy clean. Fix any compile errors from the rewrite (most likely: imports, the `render` screen-passing note in Step 2, or a leftover reference to `app.editor`/`active`/`should_quit`/`map_key` — all removed). Do NOT reintroduce removed concepts.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs src/ui.rs src/run.rs src/tree.rs src/session.rs
git commit -m "feat: multi-session tree, chooser, and file viewer (v3 rewire)"
```

---

### Task 6: README + manual verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update `README.md`** — replace the keybinding table and behavior text to match v3:

```markdown
# runner-manager

A standalone terminal UI: a NERDTree-style file tree (left) plus a live
embedded terminal or read-only file viewer (right). Each directory can hold
multiple tmux sessions (shell or claude), shown as rows under it.

## How it works

- Runs directly in your terminal (NOT inside tmux); draws a fixed two-pane
  layout on the alternate screen.
- `a` (or clicking `[+]`) on a directory opens a chooser to start a **shell**
  or **claude** session in that directory, on the `tmux -L runner` server.
  Sessions appear as rows under the directory and disappear when their shell
  exits.
- Selecting a session row shows it in the right pane (embedded terminal).
  Selecting a file shows it in a read-only viewer in the right pane.

## Usage

Run from the directory you want as the tree root (not inside tmux):

```bash
runner-manager
```

| Key            | Action                                              |
|----------------|-----------------------------------------------------|
| `j` / `down`   | move down (tree focus)                              |
| `k` / `up`     | move up (tree focus)                                |
| `Enter`        | expand/collapse dir · switch to session · view file |
| `a` / `[+]`    | new session (shell/claude) on a directory           |
| `h` / `?`      | help popup                                          |
| `q`            | quit (tree focus)                                   |
| `Ctrl-q`       | toggle focus between tree and the right pane        |
| left-click     | focus a pane; in the tree, act on the clicked row   |

In the chooser popup: `↑`/`↓`/`j`/`k` to move, `Enter` to start, `Esc` to
cancel. When the right pane is focused: keys go to the shell, or — if a file
is shown — `j`/`k`/`PgUp`/`PgDn` scroll it (read-only).
```

- [ ] **Step 2: Final verification**

Run: `cargo test && cargo build --release && cargo clippy --all-targets 2>&1 | tail -20`
Expected: all tests pass; release builds; clippy clean.

- [ ] **Step 3: Manual smoke test** (requires tmux; not automated)

```bash
tmux -L runner kill-server 2>/dev/null
cargo run   # from a project dir, NOT inside tmux
```

Verify: `a`/`[+]` on a directory opens the shell/claude chooser; choosing one
starts a session that appears as a row under the directory and shows in the
right pane; `Enter`/click on a file shows it in a read-only viewer (Ctrl-q then
`j`/`k`/`PgUp`/`PgDn` scroll it); exiting a session's shell removes its row;
there is no `●` badge; `q` (tree focus) quits.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: update README for v3 multi-session + viewer"
```

---

## Self-Review Notes

- **Spec coverage:** chooser shell/claude (Task 5 `open_chooser`/`chooser_*` + `SessionKind::command`, Task 1 cmd arg); multiple sessions per dir + slug/label (Task 2); sessions as tree rows always-visible + auto-prune (Task 4 `build_rows`, Task 5 `sync`); badge removed (Task 5 `ui::row_line` has no badge); inline read-only viewer (Task 3 + Task 5 right-pane branch); focus Tree/Right + viewer scroll (Task 5 `run.rs`); no kill key (removed in Task 5); help popup updated (Task 5 `render_help`); README (Task 6). All spec sections map to tasks.
- **Placeholder scan:** none. The one inline directive is the `render` screen-passing NOTE in Task 5 Step 2, which gives the exact replacement signature and is resolved within the same step (and `run.rs` in Step 3 already calls the resolved signature).
- **Type consistency:** `Focus { Tree, Right }`, `Popup`, `CHOOSER_KINDS`, `App` fields/methods are used identically across `app.rs` (def), `ui.rs`, and `run.rs`. `rows::{Row, RowKind}` and `session::{SessionKind, SessionRow, SessionStore}` names match across producer/consumer tasks. `ui::render(f, area, &app, screen)` (resolved signature) matches the `run.rs` call site.
- **Build-green ordering:** Tasks 1–4 keep the crate compiling and tested. Task 5 is a single coordinated rewrite that is green at its end (it changes `Row`'s type and all three consumers together, which is why they share one task). Task 6 is docs.
