# runner-manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust TUI that pairs a NERDTree-style file tree (left pane) with a per-directory tmux session (right pane), orchestrated via nested tmux.

**Architecture:** A ratatui app renders only the left pane and drives a dedicated inner tmux server (`tmux -L runner`) by shelling out. Each directory maps to its own inner session (create-or-switch); the right "host" pane is an inner tmux client whose displayed session is swapped via `switch-client`. Files open in `$EDITOR` inside their directory's session.

**Tech Stack:** Rust (edition 2021), ratatui 0.29, crossterm 0.28, tempfile (dev). All tmux interaction goes through a `CommandRunner` trait so logic is unit-testable with a mock.

## Global Constraints

- Rust edition 2021; crate name `runner_manager` (binary `runner-manager`).
- Dependencies pinned: `ratatui = "0.29"`, `crossterm = "0.28"`, dev `tempfile = "3"`.
- All tmux calls for runner sessions use the socket name `runner` (i.e. `tmux -L runner …`); the outer layout uses the default tmux server.
- Inner server prefix is set to `C-a` (so it never collides with the user's `C-b`).
- Tests are inline `#[cfg(test)]` modules (idiomatic Rust); `MockRunner` lives in `tmux.rs` under `#[cfg(test)]` and is reused by other modules' tests.
- TDD: failing test first, minimal impl, passing test, commit. Run `cargo test` (and `cargo build` where noted) and confirm output before claiming done.
- tmux session names cannot contain `.` or `:`; slug derivation must sanitize.

## File Structure

- `Cargo.toml` — package + dependencies.
- `src/lib.rs` — declares all modules so unit tests compile crate-wide.
- `src/main.rs` — binary entry; dispatches bootstrap vs tui.
- `src/cli.rs` — argument parsing (`Mode::Bootstrap | Mode::Tui`).
- `src/tmux.rs` — `CommandRunner` trait, `SystemRunner`, `CmdOutput`, `Tmux<R>` wrapper, `MockRunner` (test).
- `src/session.rs` — `slugify` + `SessionRegistry` (path↔slug, uniqueness).
- `src/tree.rs` — `Node`, `Tree`, `Row`; lazy load, expand/collapse, flatten to visible rows.
- `src/input.rs` — `Action` enum + `map_key`.
- `src/ui.rs` — `render`, `ListLayout`, `Hit`, `resolve_click`.
- `src/app.rs` — `App<R>` state + actions (open dir/file, kill, sync, navigation).
- `src/run.rs` — crossterm terminal setup + event loop (`run`).
- `src/bootstrap.rs` — pure tmux command generation + execution (`run_bootstrap`).

---

### Task 1: Project scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: compiling crate `runner_manager` with empty module declarations; binary that prints a placeholder.

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "runner-manager"
version = "0.1.0"
edition = "2021"

[lib]
name = "runner_manager"
path = "src/lib.rs"

[[bin]]
name = "runner-manager"
path = "src/main.rs"

[dependencies]
ratatui = "0.29"
crossterm = "0.28"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write `src/lib.rs` with a smoke test**

```rust
pub mod tmux;
pub mod session;
pub mod tree;
pub mod input;
pub mod ui;
pub mod app;
pub mod run;
pub mod bootstrap;
pub mod cli;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 3: Create minimal module files so the crate compiles**

Create each of these as empty-but-valid files (they get filled in later tasks):

`src/tmux.rs`, `src/session.rs`, `src/tree.rs`, `src/input.rs`, `src/ui.rs`, `src/app.rs`, `src/run.rs`, `src/bootstrap.rs`, `src/cli.rs` — each containing a single line comment:

```rust
// implemented in a later task
```

- [ ] **Step 4: Write `src/main.rs`**

```rust
fn main() {
    println!("runner-manager: run with no args to bootstrap, or `tui` inside the left pane");
}
```

- [ ] **Step 5: Build and test**

Run: `cargo build && cargo test`
Expected: builds successfully; `crate_builds` test passes.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/
git commit -m "chore: scaffold runner-manager crate"
```

---

### Task 2: tmux command layer

**Files:**
- Modify: `src/tmux.rs` (replace placeholder)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `trait CommandRunner { fn run(&self, args: &[&str]) -> std::io::Result<CmdOutput>; }`
  - `struct CmdOutput { pub success: bool, pub stdout: String }`
  - `struct SystemRunner;` implementing `CommandRunner` via `tmux`.
  - `struct Tmux<R: CommandRunner>` with: `new(socket, runner)`, `has_session(&str)->io::Result<bool>`, `new_session(&str,&Path)->io::Result<()>`, `switch_client(tty,slug)->io::Result<()>`, `list_sessions()->io::Result<Vec<String>>`, `host_tty()->io::Result<Option<String>>`, `send_keys(slug,keys)->io::Result<()>`, `kill_session(slug)->io::Result<()>`.
  - `#[cfg(test)] struct MockRunner` with `new()`, `push(success,stdout)`, `nth_call(i)->Vec<String>`, `call_count()`.

- [ ] **Step 1: Write the failing tests** (append to `src/tmux.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn has_session_prefixes_socket_and_reads_success() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        assert!(tmux.has_session("src").unwrap());
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "has-session", "-t", "src"]
        );
    }

    #[test]
    fn new_session_builds_detached_with_dir() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.new_session("src", Path::new("/tmp/proj/src")).unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "new-session", "-d", "-s", "src", "-c", "/tmp/proj/src"]
        );
    }

    #[test]
    fn switch_client_targets_tty_and_session() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.switch_client("/dev/ttys003", "src").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "switch-client", "-c", "/dev/ttys003", "-t", "src"]
        );
    }

    #[test]
    fn list_sessions_parses_lines_and_empty_on_failure() {
        let runner = MockRunner::new();
        runner.push(true, "src\ntests\n");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(tmux.list_sessions().unwrap(), vec!["src", "tests"]);

        let runner = MockRunner::new();
        runner.push(false, "no server running");
        let tmux = Tmux::new("runner", runner);
        assert!(tmux.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn host_tty_returns_first_nonempty() {
        let runner = MockRunner::new();
        runner.push(true, "/dev/ttys005\n");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(tmux.host_tty().unwrap(), Some("/dev/ttys005".to_string()));
    }

    #[test]
    fn send_keys_appends_enter() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.send_keys("src", "vi -- a.rs").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "send-keys", "-t", "src", "vi -- a.rs", "Enter"]
        );
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib tmux`
Expected: FAIL — `Tmux`, `MockRunner`, etc. not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module in `src/tmux.rs`, replacing the placeholder comment)

```rust
use std::io;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub success: bool,
    pub stdout: String,
}

pub trait CommandRunner {
    fn run(&self, args: &[&str]) -> io::Result<CmdOutput>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, args: &[&str]) -> io::Result<CmdOutput> {
        let out = Command::new("tmux").args(args).output()?;
        Ok(CmdOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }
}

pub struct Tmux<R: CommandRunner> {
    socket: String,
    pub runner: R,
}

impl<R: CommandRunner> Tmux<R> {
    pub fn new(socket: impl Into<String>, runner: R) -> Self {
        Self { socket: socket.into(), runner }
    }

    fn run(&self, extra: &[&str]) -> io::Result<CmdOutput> {
        let mut args: Vec<&str> = vec!["-L", &self.socket];
        args.extend_from_slice(extra);
        self.runner.run(&args)
    }

    pub fn has_session(&self, slug: &str) -> io::Result<bool> {
        Ok(self.run(&["has-session", "-t", slug])?.success)
    }

    pub fn new_session(&self, slug: &str, dir: &Path) -> io::Result<()> {
        let dir = dir.to_str().unwrap_or(".");
        self.run(&["new-session", "-d", "-s", slug, "-c", dir])?;
        Ok(())
    }

    pub fn switch_client(&self, tty: &str, slug: &str) -> io::Result<()> {
        self.run(&["switch-client", "-c", tty, "-t", slug])?;
        Ok(())
    }

    pub fn list_sessions(&self) -> io::Result<Vec<String>> {
        let out = self.run(&["list-sessions", "-F", "#{session_name}"])?;
        if !out.success {
            return Ok(Vec::new());
        }
        Ok(out
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    pub fn host_tty(&self) -> io::Result<Option<String>> {
        let out = self.run(&["list-clients", "-F", "#{client_tty}"])?;
        if !out.success {
            return Ok(None);
        }
        Ok(out
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .find(|l| !l.is_empty()))
    }

    pub fn send_keys(&self, slug: &str, keys: &str) -> io::Result<()> {
        self.run(&["send-keys", "-t", slug, keys, "Enter"])?;
        Ok(())
    }

    pub fn kill_session(&self, slug: &str) -> io::Result<()> {
        self.run(&["kill-session", "-t", slug])?;
        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MockRunner {
    pub calls: std::cell::RefCell<Vec<Vec<String>>>,
    pub responses: std::cell::RefCell<std::collections::VecDeque<CmdOutput>>,
}

#[cfg(test)]
impl MockRunner {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&self, success: bool, stdout: &str) {
        self.responses
            .borrow_mut()
            .push_back(CmdOutput { success, stdout: stdout.to_string() });
    }
    pub fn nth_call(&self, i: usize) -> Vec<String> {
        self.calls.borrow()[i].clone()
    }
    pub fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

#[cfg(test)]
impl CommandRunner for MockRunner {
    fn run(&self, args: &[&str]) -> io::Result<CmdOutput> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        Ok(self
            .responses
            .borrow_mut()
            .pop_front()
            .unwrap_or(CmdOutput { success: true, stdout: String::new() }))
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib tmux`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add src/tmux.rs
git commit -m "feat: add tmux command layer with mockable runner"
```

---

### Task 3: session slug + registry

**Files:**
- Modify: `src/session.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `fn slugify(rel: &str) -> String`
  - `struct SessionRegistry` with `new()` and `slug_for(&mut self, path: &Path, root: &Path) -> String`.

- [ ] **Step 1: Write the failing tests** (append to `src/session.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn slugify_basic_and_separators() {
        assert_eq!(slugify("src"), "src");
        assert_eq!(slugify("src/proto"), "src-proto");
        assert_eq!(slugify("my dir"), "my_dir");
        assert_eq!(slugify(".config"), "_config");
        assert_eq!(slugify("a.b"), "a_b");
        assert_eq!(slugify(""), "root");
        assert_eq!(slugify("."), "root");
    }

    #[test]
    fn slugify_keeps_unicode_letters() {
        assert_eq!(slugify("café"), "café");
    }

    #[test]
    fn registry_is_stable_per_path() {
        let mut reg = SessionRegistry::new();
        let root = Path::new("/p");
        let a = reg.slug_for(Path::new("/p/src"), root);
        let b = reg.slug_for(Path::new("/p/src"), root);
        assert_eq!(a, b);
        assert_eq!(a, "src");
    }

    #[test]
    fn registry_disambiguates_collisions() {
        let mut reg = SessionRegistry::new();
        let root = Path::new("/p");
        let a = reg.slug_for(Path::new("/p/a.b"), root); // -> a_b
        let b = reg.slug_for(Path::new("/p/a:b"), root); // also -> a_b, must differ
        assert_eq!(a, "a_b");
        assert_eq!(b, "a_b-2");
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib session`
Expected: FAIL — `slugify` / `SessionRegistry` not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module, replacing the placeholder comment)

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn slugify(rel: &str) -> String {
    if rel.is_empty() || rel == "." {
        return "root".to_string();
    }
    let s: String = rel
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else if c == '/' {
                '-'
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "root".to_string()
    } else {
        s
    }
}

#[derive(Default)]
pub struct SessionRegistry {
    by_slug: HashMap<String, PathBuf>,
    by_path: HashMap<PathBuf, String>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn slug_for(&mut self, path: &Path, root: &Path) -> String {
        if let Some(existing) = self.by_path.get(path) {
            return existing.clone();
        }
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
        let base = slugify(&rel);
        let mut slug = base.clone();
        let mut n = 2;
        while self.by_slug.contains_key(&slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        self.by_slug.insert(slug.clone(), path.to_path_buf());
        self.by_path.insert(path.to_path_buf(), slug.clone());
        slug
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib session`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/session.rs
git commit -m "feat: add slug derivation and session registry"
```

---

### Task 4: file tree model

**Files:**
- Modify: `src/tree.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `struct Node { pub path: PathBuf, pub name: String, pub is_dir: bool, pub expanded: bool, pub children: Option<Vec<Node>> }` with `new(PathBuf, is_dir)`, `load_children(&mut self)`, `toggle(&mut self)`.
  - `struct Row { pub path: PathBuf, pub name: String, pub is_dir: bool, pub depth: usize, pub expanded: bool }` (derives `Debug, Clone, PartialEq`).
  - `struct Tree { pub root: Node }` with `new(PathBuf)`, `visible_rows(&self) -> Vec<Row>`, `node_at_mut(&mut self, &Path) -> Option<&mut Node>`.

- [ ] **Step 1: Write the failing tests** (append to `src/tree.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn setup() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("zsub")).unwrap();
        fs::create_dir(dir.path().join("asub")).unwrap();
        fs::write(dir.path().join("readme.md"), "x").unwrap();
        fs::write(dir.path().join("zsub").join("inner.txt"), "y").unwrap();
        dir
    }

    #[test]
    fn root_expands_and_orders_dirs_first_then_alpha() {
        let dir = setup();
        let tree = Tree::new(dir.path().to_path_buf());
        let rows = tree.visible_rows();
        // row 0 is the root itself; children follow
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let child_names = &names[1..];
        assert_eq!(child_names, &["asub", "zsub", "readme.md"]);
        assert!(rows[1].is_dir);
        assert!(!rows.iter().any(|r| r.name == "inner.txt")); // not expanded yet
    }

    #[test]
    fn toggle_expands_and_collapses_lazily() {
        let dir = setup();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let zsub = dir.path().join("zsub");
        tree.node_at_mut(&zsub).unwrap().toggle(); // expand
        assert!(tree.visible_rows().iter().any(|r| r.name == "inner.txt"));
        tree.node_at_mut(&zsub).unwrap().toggle(); // collapse
        assert!(!tree.visible_rows().iter().any(|r| r.name == "inner.txt"));
    }

    #[test]
    fn depth_increases_for_children() {
        let dir = setup();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let zsub = dir.path().join("zsub");
        tree.node_at_mut(&zsub).unwrap().toggle();
        let inner = tree.visible_rows().into_iter().find(|r| r.name == "inner.txt").unwrap();
        assert_eq!(inner.depth, 2);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib tree`
Expected: FAIL — `Tree` / `Node` not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module, replacing the placeholder comment)

```rust
use std::fs;
use std::path::{Path, PathBuf};

pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<Node>>,
}

impl Node {
    pub fn new(path: PathBuf, is_dir: bool) -> Self {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self { path, name, is_dir, expanded: false, children: None }
    }

    pub fn load_children(&mut self) {
        if !self.is_dir {
            return;
        }
        let mut entries: Vec<Node> = Vec::new();
        if let Ok(read) = fs::read_dir(&self.path) {
            for e in read.flatten() {
                let p = e.path();
                let is_dir = p.is_dir();
                entries.push(Node::new(p, is_dir));
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.children = Some(entries);
    }

    pub fn toggle(&mut self) {
        if !self.is_dir {
            return;
        }
        if self.expanded {
            self.expanded = false;
        } else {
            if self.children.is_none() {
                self.load_children();
            }
            self.expanded = true;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

pub struct Tree {
    pub root: Node,
}

impl Tree {
    pub fn new(root_path: PathBuf) -> Self {
        let mut root = Node::new(root_path, true);
        root.load_children();
        root.expanded = true;
        Self { root }
    }

    pub fn visible_rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        Self::collect(&self.root, 0, &mut out);
        out
    }

    fn collect(node: &Node, depth: usize, out: &mut Vec<Row>) {
        out.push(Row {
            path: node.path.clone(),
            name: node.name.clone(),
            is_dir: node.is_dir,
            depth,
            expanded: node.expanded,
        });
        if node.is_dir && node.expanded {
            if let Some(children) = &node.children {
                for c in children {
                    Self::collect(c, depth + 1, out);
                }
            }
        }
    }

    pub fn node_at_mut(&mut self, path: &Path) -> Option<&mut Node> {
        Self::find_mut(&mut self.root, path)
    }

    fn find_mut<'a>(node: &'a mut Node, path: &Path) -> Option<&'a mut Node> {
        if node.path == path {
            return Some(node);
        }
        if let Some(children) = node.children.as_mut() {
            for c in children.iter_mut() {
                if let Some(found) = Self::find_mut(c, path) {
                    return Some(found);
                }
            }
        }
        None
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib tree`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/tree.rs
git commit -m "feat: add lazy file tree model"
```

---

### Task 5: keyboard input mapping

**Files:**
- Modify: `src/input.rs`

**Interfaces:**
- Consumes: `crossterm::event::KeyEvent`.
- Produces:
  - `enum Action { Quit, Up, Down, Activate, OpenSession, Kill, Noop }` (derives `Debug, Clone, PartialEq, Eq`).
  - `fn map_key(key: KeyEvent) -> Action`.

Semantics for later tasks: `Activate` = Enter (dir → toggle, file → open). `OpenSession` = `a` (dir → open/switch session, file → open file). `Kill` = `x` (dir session).

- [ ] **Step 1: Write the failing tests** (append to `src/input.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn maps_navigation_and_commands() {
        assert_eq!(map_key(key(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(map_key(key(KeyCode::Char('j'))), Action::Down);
        assert_eq!(map_key(key(KeyCode::Down)), Action::Down);
        assert_eq!(map_key(key(KeyCode::Char('k'))), Action::Up);
        assert_eq!(map_key(key(KeyCode::Up)), Action::Up);
        assert_eq!(map_key(key(KeyCode::Enter)), Action::Activate);
        assert_eq!(map_key(key(KeyCode::Char('a'))), Action::OpenSession);
        assert_eq!(map_key(key(KeyCode::Char('x'))), Action::Kill);
        assert_eq!(map_key(key(KeyCode::Char('z'))), Action::Noop);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib input`
Expected: FAIL — `map_key` / `Action` not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module, replacing the placeholder comment)

```rust
use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    Up,
    Down,
    Activate,
    OpenSession,
    Kill,
    Noop,
}

pub fn map_key(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => Action::Down,
        KeyCode::Char('k') | KeyCode::Up => Action::Up,
        KeyCode::Enter => Action::Activate,
        KeyCode::Char('a') => Action::OpenSession,
        KeyCode::Char('x') => Action::Kill,
        _ => Action::Noop,
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib input`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add src/input.rs
git commit -m "feat: add keyboard input mapping"
```

---

### Task 6: UI rendering + click resolution

**Files:**
- Modify: `src/ui.rs`

**Interfaces:**
- Consumes: `crate::tree::Row`.
- Produces:
  - `struct ListLayout { pub origin_y: u16, pub button_col_start: u16, pub button_col_end: u16, pub row_count: usize }`
  - `enum Hit { Row(usize), Button(usize) }` (derives `Debug, Clone, PartialEq, Eq`).
  - `fn resolve_click(col: u16, row: u16, layout: &ListLayout) -> Option<Hit>`
  - `fn render(f: &mut ratatui::Frame, area: ratatui::layout::Rect, rows: &[Row], selected: usize, active: &std::collections::HashSet<std::path::PathBuf>) -> ListLayout`

Note (v1 limitation): `render` assumes the list is not vertically scrolled, so click mapping uses `row_idx = screen_y - origin_y`. Acceptable for small trees.

- [ ] **Step 1: Write the failing tests** (append to `src/ui.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Row;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[test]
    fn resolve_click_distinguishes_row_and_button() {
        let layout = ListLayout {
            origin_y: 1,
            button_col_start: 20,
            button_col_end: 22,
            row_count: 3,
        };
        assert_eq!(resolve_click(5, 1, &layout), Some(Hit::Row(0)));
        assert_eq!(resolve_click(21, 2, &layout), Some(Hit::Button(1)));
        assert_eq!(resolve_click(5, 0, &layout), None); // above list
        assert_eq!(resolve_click(5, 10, &layout), None); // below rows
    }

    #[test]
    fn render_draws_names_and_button() {
        let rows = vec![
            Row { path: PathBuf::from("/p"), name: "p".into(), is_dir: true, depth: 0, expanded: true },
            Row { path: PathBuf::from("/p/src"), name: "src".into(), is_dir: true, depth: 1, expanded: false },
            Row { path: PathBuf::from("/p/r.md"), name: "r.md".into(), is_dir: false, depth: 1, expanded: false },
        ];
        let active: HashSet<PathBuf> = HashSet::new();
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let _ = render(f, f.area(), &rows, 0, &active);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("src"));
        assert!(content.contains("[+]"));
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib ui`
Expected: FAIL — `render` / `ListLayout` not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module, replacing the placeholder comment)

```rust
use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::tree::Row;

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

pub fn render(
    f: &mut Frame,
    area: Rect,
    rows: &[Row],
    selected: usize,
    active: &HashSet<PathBuf>,
) -> ListLayout {
    let block = Block::default().title("runner-manager").borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = inner.width as usize;
    let mut items: Vec<ListItem> = Vec::new();
    for row in rows {
        let indent = "  ".repeat(row.depth);
        let icon = if row.is_dir {
            if row.expanded { "▾ " } else { "▸ " }
        } else {
            "  "
        };
        let badge = if active.contains(&row.path) { "● " } else { "" };
        let left = format!("{indent}{icon}{badge}{}", row.name);
        let line = if row.is_dir {
            let btn = "[+]";
            let pad = width.saturating_sub(left.chars().count() + btn.len());
            format!("{left}{}{btn}", " ".repeat(pad))
        } else {
            left
        };
        items.push(ListItem::new(line));
    }

    let list =
        List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(selected.min(rows.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);

    ListLayout {
        origin_y: inner.y,
        button_col_start: inner.x + inner.width.saturating_sub(3),
        button_col_end: inner.x + inner.width.saturating_sub(1),
        row_count: rows.len(),
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib ui`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "feat: add tree rendering and click resolution"
```

---

### Task 7: application state + actions

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `crate::tmux::{CommandRunner, Tmux}`, `crate::tree::{Tree, Row}`, `crate::session::SessionRegistry`.
- Produces:
  - `struct App<R: CommandRunner>` with public fields: `tree, registry, tmux, root: PathBuf, selected: usize, rows: Vec<Row>, active: HashSet<PathBuf>, host_tty: Option<String>, editor: String, status: String, should_quit: bool`.
  - Methods: `new(root: PathBuf, tmux: Tmux<R>, editor: String) -> Self`, `refresh_rows(&mut self)`, `selected_row(&self) -> Option<&Row>`, `up(&mut self)`, `down(&mut self)`, `activate(&mut self) -> io::Result<()>`, `open_session(&mut self) -> io::Result<()>`, `kill_selected(&mut self) -> io::Result<()>`, `sync_active(&mut self) -> io::Result<()>`.

- [ ] **Step 1: Write the failing tests** (append to `src/app.rs`)

```rust
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
        let mut app = App::new(dir.path().to_path_buf(), tmux, "vi".to_string());
        app.host_tty = Some("/dev/ttys009".to_string());
        (dir, app)
    }

    #[test]
    fn open_session_creates_when_absent_then_switches() {
        let (_dir, mut app) = app_over_tempdir();
        // rows[0] = root, rows[1] = src
        app.selected = 1;
        // has-session -> false, new-session -> ok, switch -> ok
        app.tmux.runner.push(false, "");
        app.tmux.runner.push(true, "");
        app.tmux.runner.push(true, "");
        app.open_session().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "has-session");
        assert_eq!(app.tmux.runner.nth_call(1)[2], "new-session");
        assert_eq!(app.tmux.runner.nth_call(2)[2], "switch-client");
    }

    #[test]
    fn open_session_skips_create_when_present() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1;
        app.tmux.runner.push(true, ""); // has-session -> true
        app.tmux.runner.push(true, ""); // switch
        app.open_session().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "has-session");
        assert_eq!(app.tmux.runner.nth_call(1)[2], "switch-client");
        assert_eq!(app.tmux.runner.call_count(), 2);
    }

    #[test]
    fn activate_toggles_directory() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1; // src
        app.activate().unwrap();
        assert!(app.rows.iter().any(|r| r.name == "a.rs"));
        assert_eq!(app.tmux.runner.call_count(), 0); // no tmux for toggle
    }

    #[test]
    fn activate_on_file_opens_editor_in_session() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1;
        app.activate().unwrap(); // expand src, no tmux calls
        let file_idx = app.rows.iter().position(|r| r.name == "a.rs").unwrap();
        app.selected = file_idx;
        app.tmux.runner.push(false, ""); // has-session(src) -> false
        app.tmux.runner.push(true, "");  // new-session
        app.tmux.runner.push(true, "");  // send-keys
        app.tmux.runner.push(true, "");  // switch
        app.activate().unwrap();
        let send = app.tmux.runner.nth_call(2);
        assert_eq!(send[2], "send-keys");
        assert!(send.iter().any(|a| a.contains("vi -- ")));
    }

    #[test]
    fn kill_selected_kills_dir_session() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1;
        app.tmux.runner.push(true, "");
        app.kill_selected().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "kill-session");
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib app`
Expected: FAIL — `App` not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module, replacing the placeholder comment)

```rust
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::session::SessionRegistry;
use crate::tmux::{CommandRunner, Tmux};
use crate::tree::{Row, Tree};

pub struct App<R: CommandRunner> {
    pub tree: Tree,
    pub registry: SessionRegistry,
    pub tmux: Tmux<R>,
    pub root: PathBuf,
    pub selected: usize,
    pub rows: Vec<Row>,
    pub active: HashSet<PathBuf>,
    pub host_tty: Option<String>,
    pub editor: String,
    pub status: String,
    pub should_quit: bool,
}

impl<R: CommandRunner> App<R> {
    pub fn new(root: PathBuf, tmux: Tmux<R>, editor: String) -> Self {
        let tree = Tree::new(root.clone());
        let rows = tree.visible_rows();
        Self {
            tree,
            registry: SessionRegistry::new(),
            tmux,
            root,
            selected: 0,
            rows,
            active: HashSet::new(),
            host_tty: None,
            editor,
            status: String::new(),
            should_quit: false,
        }
    }

    pub fn refresh_rows(&mut self) {
        self.rows = self.tree.visible_rows();
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

    pub fn activate(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        if row.is_dir {
            if let Some(node) = self.tree.node_at_mut(&row.path) {
                node.toggle();
            }
            self.refresh_rows();
        } else {
            self.open_file(&row.path)?;
        }
        Ok(())
    }

    pub fn open_session(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        if row.is_dir {
            self.open_dir(&row.path)?;
        } else {
            self.open_file(&row.path)?;
        }
        Ok(())
    }

    pub fn kill_selected(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        if !row.is_dir {
            return Ok(());
        }
        let slug = self.registry.slug_for(&row.path, &self.root);
        self.tmux.kill_session(&slug)?;
        self.active.remove(&row.path);
        self.status = format!("killed {slug}");
        Ok(())
    }

    pub fn sync_active(&mut self) -> io::Result<()> {
        let sessions: HashSet<String> = self.tmux.list_sessions()?.into_iter().collect();
        let rows = self.rows.clone();
        let mut active = HashSet::new();
        for row in rows {
            if row.is_dir {
                let slug = self.registry.slug_for(&row.path, &self.root);
                if sessions.contains(&slug) {
                    active.insert(row.path.clone());
                }
            }
        }
        self.active = active;
        Ok(())
    }

    fn ensure_host_tty(&mut self) -> io::Result<Option<String>> {
        if self.host_tty.is_none() {
            self.host_tty = self.tmux.host_tty()?;
        }
        Ok(self.host_tty.clone())
    }

    fn ensure_session(&mut self, dir: &Path) -> io::Result<String> {
        let slug = self.registry.slug_for(dir, &self.root);
        if !self.tmux.has_session(&slug)? {
            self.tmux.new_session(&slug, dir)?;
        }
        self.active.insert(dir.to_path_buf());
        Ok(slug)
    }

    fn open_dir(&mut self, dir: &Path) -> io::Result<()> {
        let slug = self.ensure_session(dir)?;
        if let Some(tty) = self.ensure_host_tty()? {
            self.tmux.switch_client(&tty, &slug)?;
            self.status = format!("switched to {slug}");
        } else {
            self.status = "no host client to switch".to_string();
        }
        Ok(())
    }

    fn open_file(&mut self, file: &Path) -> io::Result<()> {
        let dir = file.parent().unwrap_or(&self.root).to_path_buf();
        let slug = self.ensure_session(&dir)?;
        let cmd = format!("{} -- {}", self.editor, file.to_string_lossy());
        self.tmux.send_keys(&slug, &cmd)?;
        if let Some(tty) = self.ensure_host_tty()? {
            self.tmux.switch_client(&tty, &slug)?;
        }
        self.status = format!("opened {}", file.display());
        Ok(())
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib app`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat: add application state and tmux-backed actions"
```

---

### Task 8: event loop + terminal driver

**Files:**
- Modify: `src/run.rs`

**Interfaces:**
- Consumes: `crate::app::App`, `crate::tmux::{SystemRunner, Tmux}`, `crate::input::{map_key, Action}`, `crate::ui::{self, Hit, ListLayout}`.
- Produces: `fn run(root: PathBuf, socket: String, editor: String) -> std::io::Result<()>`.

This task is integration glue (terminal raw mode, alternate screen, mouse capture, event loop). It is verified by `cargo build` and manual run, not unit tests.

- [ ] **Step 1: Write the implementation** (replace the placeholder comment in `src/run.rs`)

```rust
use std::io;
use std::path::PathBuf;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;
use crate::input::{map_key, Action};
use crate::tmux::{SystemRunner, Tmux};
use crate::ui::{self, Hit, ListLayout};

pub fn run(root: PathBuf, socket: String, editor: String) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tmux = Tmux::new(socket, SystemRunner);
    let mut app = App::new(root, tmux, editor);
    let _ = app.sync_active();

    let mut layout = ListLayout {
        origin_y: 0,
        button_col_start: 0,
        button_col_end: 0,
        row_count: 0,
    };

    let result = loop {
        if let Err(e) = terminal.draw(|f| {
            layout = ui::render(f, f.area(), &app.rows, app.selected, &app.active);
        }) {
            break Err(e);
        }

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match map_key(key) {
                Action::Quit => break Ok(()),
                Action::Up => app.up(),
                Action::Down => app.down(),
                Action::Activate => {
                    let _ = app.activate();
                }
                Action::OpenSession => {
                    let _ = app.open_session();
                }
                Action::Kill => {
                    let _ = app.kill_selected();
                }
                Action::Noop => {}
            },
            Ok(Event::Mouse(m)) => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    if let Some(hit) = ui::resolve_click(m.column, m.row, &layout) {
                        match hit {
                            Hit::Row(idx) => {
                                app.selected = idx;
                                let _ = app.activate();
                            }
                            Hit::Button(idx) => {
                                app.selected = idx;
                                let _ = app.open_session();
                            }
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    result
}
```

- [ ] **Step 2: Build to confirm it compiles**

Run: `cargo build`
Expected: builds cleanly (warnings about unused `should_quit`/`status` are acceptable).

- [ ] **Step 3: Commit**

```bash
git add src/run.rs
git commit -m "feat: add crossterm event loop driving the app"
```

---

### Task 9: bootstrap (outer split + inner server)

**Files:**
- Modify: `src/bootstrap.rs`

**Interfaces:**
- Consumes: nothing (shells out to `tmux`).
- Produces:
  - `struct TmuxCmd { pub socket: Option<String>, pub args: Vec<String> }` (derives `Debug, Clone, PartialEq, Eq`).
  - `fn inner_setup_commands(socket: &str) -> Vec<TmuxCmd>`
  - `fn outer_layout_commands(outer: &str, socket: &str, self_exe: &str) -> Vec<TmuxCmd>`
  - `fn run_bootstrap(socket: &str, outer: &str) -> std::io::Result<()>`

- [ ] **Step 1: Write the failing tests** (append to `src/bootstrap.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_setup_creates_scratch_and_sets_prefix() {
        let cmds = inner_setup_commands("runner");
        assert_eq!(cmds[0].socket.as_deref(), Some("runner"));
        assert_eq!(cmds[0].args, vec!["new-session", "-d", "-s", "scratch"]);
        assert!(cmds
            .iter()
            .any(|c| c.args == vec!["set", "-g", "prefix", "C-a"]));
    }

    #[test]
    fn outer_layout_splits_with_tui_and_inner_attach() {
        let cmds = outer_layout_commands("runner-manager", "runner", "/usr/bin/runner-manager");
        // first command starts the detached outer session running the tui in the left pane
        assert_eq!(cmds[0].socket, None);
        assert_eq!(cmds[0].args[0], "new-session");
        assert!(cmds[0].args.iter().any(|a| a == "/usr/bin/runner-manager tui"));
        // a split-window attaches the inner scratch session in the right pane
        let split = cmds.iter().find(|c| c.args[0] == "split-window").unwrap();
        assert!(split
            .args
            .iter()
            .any(|a| a == "tmux -L runner attach -t scratch"));
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib bootstrap`
Expected: FAIL — `inner_setup_commands` etc. not found.

- [ ] **Step 3: Write the implementation** (prepend above the test module, replacing the placeholder comment)

```rust
use std::io;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCmd {
    pub socket: Option<String>,
    pub args: Vec<String>,
}

fn svec(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

pub fn inner_setup_commands(socket: &str) -> Vec<TmuxCmd> {
    vec![
        TmuxCmd {
            socket: Some(socket.to_string()),
            args: svec(&["new-session", "-d", "-s", "scratch"]),
        },
        TmuxCmd {
            socket: Some(socket.to_string()),
            args: svec(&["set", "-g", "prefix", "C-a"]),
        },
        TmuxCmd {
            socket: Some(socket.to_string()),
            args: svec(&["set", "-g", "prefix2", "None"]),
        },
    ]
}

pub fn outer_layout_commands(outer: &str, socket: &str, self_exe: &str) -> Vec<TmuxCmd> {
    let tui = format!("{self_exe} tui");
    let attach = format!("tmux -L {socket} attach -t scratch");
    vec![
        TmuxCmd {
            socket: None,
            args: vec![
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                outer.into(),
                "-n".into(),
                "main".into(),
                tui,
            ],
        },
        TmuxCmd {
            socket: None,
            args: vec![
                "split-window".into(),
                "-h".into(),
                "-t".into(),
                format!("{outer}:main"),
                attach,
            ],
        },
        TmuxCmd {
            socket: None,
            args: vec![
                "select-pane".into(),
                "-t".into(),
                format!("{outer}:main.0"),
            ],
        },
    ]
}

fn execute(cmds: &[TmuxCmd]) -> io::Result<()> {
    for c in cmds {
        let mut command = Command::new("tmux");
        if let Some(sock) = &c.socket {
            command.arg("-L").arg(sock);
        }
        command.args(&c.args);
        command.status()?;
    }
    Ok(())
}

pub fn run_bootstrap(socket: &str, outer: &str) -> io::Result<()> {
    if Command::new("tmux").arg("-V").output().is_err() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "tmux not found in PATH; install tmux to use runner-manager",
        ));
    }
    let exe = std::env::current_exe()?
        .to_string_lossy()
        .into_owned();
    execute(&inner_setup_commands(socket))?;
    execute(&outer_layout_commands(outer, socket, &exe))?;
    Command::new("tmux").args(["attach", "-t", outer]).status()?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib bootstrap`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/bootstrap.rs
git commit -m "feat: add nested-tmux bootstrap"
```

---

### Task 10: CLI dispatch + main wiring

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `crate::run::run`, `crate::bootstrap::run_bootstrap`.
- Produces:
  - `enum Mode { Bootstrap, Tui }` (derives `Debug, PartialEq, Eq`).
  - `fn parse_mode(args: &[String]) -> Mode`.

- [ ] **Step 1: Write the failing tests** (append to `src/cli.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_is_bootstrap() {
        assert_eq!(parse_mode(&[]), Mode::Bootstrap);
    }

    #[test]
    fn tui_arg_selects_tui() {
        assert_eq!(parse_mode(&["tui".to_string()]), Mode::Tui);
    }

    #[test]
    fn unknown_arg_is_bootstrap() {
        assert_eq!(parse_mode(&["wat".to_string()]), Mode::Bootstrap);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test --lib cli`
Expected: FAIL — `parse_mode` / `Mode` not found.

- [ ] **Step 3: Write `src/cli.rs`** (replace the placeholder comment)

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Bootstrap,
    Tui,
}

pub fn parse_mode(args: &[String]) -> Mode {
    match args.first().map(|s| s.as_str()) {
        Some("tui") => Mode::Tui,
        _ => Mode::Bootstrap,
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test --lib cli`
Expected: PASS (3 tests).

- [ ] **Step 5: Wire `src/main.rs`** (replace its contents)

```rust
use std::env;

use runner_manager::bootstrap;
use runner_manager::cli::{parse_mode, Mode};
use runner_manager::run;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_mode(&args) {
        Mode::Tui => {
            let root = env::current_dir()?;
            let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
            run::run(root, "runner".to_string(), editor)
        }
        Mode::Bootstrap => bootstrap::run_bootstrap("runner", "runner-manager"),
    }
}
```

- [ ] **Step 6: Full build + test**

Run: `cargo build && cargo test`
Expected: builds; all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat: wire CLI dispatch and main entry"
```

---

### Task 11: README + manual verification

**Files:**
- Create: `README.md`

**Interfaces:**
- Consumes: the finished binary.
- Produces: usage docs; a manual smoke checklist.

- [ ] **Step 1: Write `README.md`**

```markdown
# runner-manager

A terminal UI that pairs a NERDTree-style file tree (left) with a per-directory
tmux session (right), using nested tmux.

## How it works

- An outer tmux window splits into two panes: the left runs the `runner-manager`
  TUI, the right hosts a client attached to a dedicated inner tmux server
  (`tmux -L runner`).
- Each directory you open becomes its own session on the inner server, rooted at
  that directory. Selecting a directory creates-or-switches to its session; the
  tree stays pinned on the left.
- Files open in `$EDITOR` (default `vi`) inside their directory's session.

## Usage

Run from the directory you want as the tree root:

```bash
runner-manager
```

This bootstraps the outer split and attaches you. Inside the left pane:

| Key            | Action                                   |
|----------------|------------------------------------------|
| `j` / `down`   | move down                                |
| `k` / `up`     | move up                                  |
| `Enter`        | expand/collapse a directory; open a file |
| `a`            | open/switch the session for a directory  |
| `x`            | kill a directory's session               |
| `q`            | quit (inner sessions keep running)       |
| left-click     | select + activate a row                  |
| click `[+]`    | open the session for that directory      |

The inner server uses prefix `C-a` to avoid clashing with your normal tmux.
```

- [ ] **Step 2: Run the full test suite one last time**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 3: Manual smoke test** (requires tmux installed; not automated)

```bash
cargo build --release
cd /some/project
/path/to/target/release/runner-manager
```

Verify: tree renders left, a scratch shell on the right; pressing `a` on a
directory opens a shell there on the right; `Enter` on a file opens it in
`$EDITOR`; `q` quits leaving `tmux -L runner ls` sessions alive.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: add usage and manual verification checklist"
```

---

## Self-Review Notes

- **Spec coverage:** nested-tmux architecture (Tasks 8–9), per-dir create-or-switch sessions (Task 7), tree of dirs+files with lazy load and ordering (Task 4), file→`$EDITOR` in dir session (Task 7), keyboard + mouse with `[+]` button (Tasks 5,6,8), slug sanitization for `.`/`:` (Task 3), badges via `list-sessions` sync (Tasks 6,7), persistence on quit / dedicated socket / `C-a` prefix (Tasks 8,9), error handling — tmux missing (Task 9), `$EDITOR` fallback (Task 10), unreadable dir skipped (Task 4 `load_children` ignores read errors). Root locked to cwd subtree: tree is rooted at cwd and never ascends (Task 4) — covered.
- **Placeholder scan:** none — every code step is complete and compilable.
- **Type consistency:** `Tmux<R>`, `MockRunner`, `CmdOutput`, `Row`, `Node`, `Action`, `Hit`, `ListLayout`, `App<R>`, `TmuxCmd`, `Mode` names/signatures are consistent across producing and consuming tasks.
