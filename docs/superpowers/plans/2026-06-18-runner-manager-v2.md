# runner-manager v2 (Embedded Terminal) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert runner-manager from an outer-tmux split into a standalone TUI that embeds the right-pane terminal itself, with click/`Ctrl-q` focus switching and a fixed two-pane layout.

**Architecture:** The process runs directly in the user's terminal (refusing `$TMUX`). The right pane is a real embedded terminal: `portable-pty` spawns `tmux -L runner new-session -A -s scratch`, a reader thread feeds bytes into a `vt100` parser behind a shared lock, and `tui-term` renders it. Selecting a directory drives `switch-client` against the embedded PTY's client exactly as before. The tmux/session/tree layers are unchanged.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, portable-pty, tui-term, vt100, tempfile (dev).

## Global Constraints

- Reuse the existing `tmux.rs`, `session.rs`, `tree.rs` unchanged. Do not alter `App`'s tmux-driving methods (`open_dir`/`open_file`/`ensure_session`/`ensure_host_tty`/`kill_selected`/`sync_active`) — they already target the embedded PTY's client correctly.
- Inner tmux socket is `runner`. The embedded PTY runs `tmux -L runner new-session -A -s scratch`. Inner tmux prefix is left at default (no `-A`-prefix override).
- The app MUST refuse to run when `$TMUX` is set, exiting non-zero with: `runner-manager must not be run inside tmux; tmux is used for the inner task sessions.`
- Focus is purely visual highlight + input routing: the two-pane layout is FIXED; switching focus never resizes or hides a pane. The tree pane is always rendered.
- `Ctrl-q` toggles focus (intercepted in any focus). `q` quits only when focus = Tree. When focus = Terminal, every key except `Ctrl-q` is encoded to bytes and written to the PTY.
- `bootstrap.rs` and `cli.rs` are deleted; `main` takes no subcommand and always runs the TUI.
- Tests are inline `#[cfg(test)]`. TDD for pure units. PTY/vt100/tui-term/event-loop integration is verified by `cargo build` + `cargo clippy` + manual run (no unit tests for the live terminal).
- `vt100` version MUST match the one `tui-term` depends on, or the `Screen` types won't unify. Prefer the `tui_term::vt100` re-export; only add a standalone `vt100` dep pinned to tui-term's version if no re-export exists. When adding crates, verify the current API on docs.rs and adapt the reference code below to the actual signatures (as was done for ratatui in v1).

---

### Task 1: Dependencies + key-encoding module

**Files:**
- Modify: `Cargo.toml`
- Create: `src/keys.rs`
- Modify: `src/lib.rs` (add `pub mod keys;`)

**Interfaces:**
- Consumes: `crossterm::event::{KeyCode, KeyEvent, KeyModifiers}`.
- Produces: `pub fn encode_key(key: KeyEvent) -> Vec<u8>` — translates a keypress to the bytes written to a PTY. `Ctrl-q` is handled by the caller (focus toggle) and is not special-cased here.

- [ ] **Step 1: Add dependencies**

Run: `cargo add portable-pty tui-term` then inspect what versions resolved.
Then ensure `Cargo.toml`'s `[dependencies]` includes (adjust versions to what resolves against ratatui 0.29; these are the expected families):

```toml
ratatui = "0.29"
crossterm = "0.28"
portable-pty = "0.8"
tui-term = "0.2"
```

Do NOT add a separate `vt100` dependency yet — Task 4/5 will use `tui_term::vt100` if it is re-exported. Only add `vt100 = "<tui-term's version>"` if `tui_term::vt100` does not exist. Run `cargo build` to confirm the new crates resolve and compile.

- [ ] **Step 2: Write the failing tests** (create `src/keys.rs` with just the test module first)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn printable_chars_are_utf8() {
        assert_eq!(encode_key(k(KeyCode::Char('a'))), b"a");
        assert_eq!(encode_key(k(KeyCode::Char('Z'))), b"Z");
        assert_eq!(encode_key(k(KeyCode::Char('é'))), "é".as_bytes());
    }

    #[test]
    fn control_chars_map_to_control_bytes() {
        assert_eq!(encode_key(ctrl('c')), vec![0x03]);
        assert_eq!(encode_key(ctrl('a')), vec![0x01]);
        assert_eq!(encode_key(ctrl('z')), vec![0x1a]);
    }

    #[test]
    fn special_keys_map_to_sequences() {
        assert_eq!(encode_key(k(KeyCode::Enter)), vec![b'\r']);
        assert_eq!(encode_key(k(KeyCode::Tab)), vec![b'\t']);
        assert_eq!(encode_key(k(KeyCode::Backspace)), vec![0x7f]);
        assert_eq!(encode_key(k(KeyCode::Esc)), vec![0x1b]);
        assert_eq!(encode_key(k(KeyCode::Up)), b"\x1b[A".to_vec());
        assert_eq!(encode_key(k(KeyCode::Down)), b"\x1b[B".to_vec());
        assert_eq!(encode_key(k(KeyCode::Right)), b"\x1b[C".to_vec());
        assert_eq!(encode_key(k(KeyCode::Left)), b"\x1b[D".to_vec());
        assert_eq!(encode_key(k(KeyCode::Home)), b"\x1b[H".to_vec());
        assert_eq!(encode_key(k(KeyCode::End)), b"\x1b[F".to_vec());
        assert_eq!(encode_key(k(KeyCode::Delete)), b"\x1b[3~".to_vec());
        assert_eq!(encode_key(k(KeyCode::PageUp)), b"\x1b[5~".to_vec());
        assert_eq!(encode_key(k(KeyCode::PageDown)), b"\x1b[6~".to_vec());
    }

    #[test]
    fn unmapped_keys_produce_no_bytes() {
        assert!(encode_key(k(KeyCode::F(5))).is_empty());
        assert!(encode_key(k(KeyCode::Insert)).is_empty());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib keys`
Expected: FAIL — `encode_key` not found.

- [ ] **Step 4: Write the implementation** (prepend above the test module in `src/keys.rs`)

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Translate a key press into the bytes a PTY expects. Returns an empty vec
/// for keys we don't forward. `Ctrl-q` is intercepted by the caller (focus
/// toggle) before this is called, so it is not special-cased here.
pub fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if ctrl && c.is_ascii_alphabetic() => {
            vec![(c.to_ascii_lowercase() as u8) & 0x1f]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        _ => Vec::new(),
    }
}
```

- [ ] **Step 5: Add the module to `src/lib.rs`**

Add `pub mod keys;` to the module list (e.g. after `pub mod input;`).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib keys`
Expected: PASS (4 tests).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/keys.rs src/lib.rs
git commit -m "feat: add PTY key-encoding module and terminal deps"
```

---

### Task 2: Two-pane click resolution

**Files:**
- Modify: `src/ui.rs`

**Interfaces:**
- Consumes: existing `ListLayout`, `Hit`, `resolve_click` (unchanged).
- Produces:
  - `pub enum Pane { Tree, Terminal }` (derives `Debug, Clone, Copy, PartialEq, Eq`).
  - `pub enum PaneHit { Tree(Option<Hit>), Terminal }` (derives `Debug, Clone, PartialEq, Eq`).
  - `pub fn resolve_pane_click(col: u16, row: u16, split_col: u16, tree_layout: &ListLayout) -> PaneHit` — `split_col` is the first column of the terminal pane; clicks left of it resolve against the tree (delegating to `resolve_click`), clicks at/after it are `Terminal`.

- [ ] **Step 1: Write the failing tests** (append to the existing `#[cfg(test)] mod tests` in `src/ui.rs`)

```rust
    #[test]
    fn pane_click_left_of_split_resolves_tree() {
        let layout = ListLayout {
            origin_y: 1,
            button_col_start: 38,
            button_col_end: 40,
            row_count: 3,
        };
        // split at col 50; a click at col 5 row 2 is in the tree on row 1
        assert_eq!(
            resolve_pane_click(5, 2, 50, &layout),
            PaneHit::Tree(Some(Hit::Row(1)))
        );
        // a click on the [+] button column within the tree
        assert_eq!(
            resolve_pane_click(39, 1, 50, &layout),
            PaneHit::Tree(Some(Hit::Button(0)))
        );
        // a tree-region click below the rows resolves to Tree(None)
        assert_eq!(resolve_pane_click(5, 20, 50, &layout), PaneHit::Tree(None));
    }

    #[test]
    fn pane_click_at_or_after_split_is_terminal() {
        let layout = ListLayout {
            origin_y: 1,
            button_col_start: 38,
            button_col_end: 40,
            row_count: 3,
        };
        assert_eq!(resolve_pane_click(50, 2, 50, &layout), PaneHit::Terminal);
        assert_eq!(resolve_pane_click(70, 4, 50, &layout), PaneHit::Terminal);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ui::tests::pane_click`
Expected: FAIL — `resolve_pane_click` / `Pane` / `PaneHit` not found.

- [ ] **Step 3: Write the implementation** (add near `Hit` in `src/ui.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneHit {
    Tree(Option<Hit>),
    Terminal,
}

pub fn resolve_pane_click(
    col: u16,
    row: u16,
    split_col: u16,
    tree_layout: &ListLayout,
) -> PaneHit {
    if col >= split_col {
        PaneHit::Terminal
    } else {
        PaneHit::Tree(resolve_click(col, row, tree_layout))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ui`
Expected: PASS (existing ui tests + the 2 new ones).

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "feat: add two-pane click resolution"
```

---

### Task 3: Focus state on App

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: existing `App<R>`.
- Produces:
  - `pub enum Focus { Tree, Terminal }` (derives `Debug, Clone, Copy, PartialEq, Eq`) in `app.rs`.
  - New field `pub focus: Focus` on `App<R>`, initialized to `Focus::Tree` in `App::new`.
  - `pub fn toggle_focus(&mut self)` — swaps `Tree`↔`Terminal`.

- [ ] **Step 1: Write the failing test** (append to the `#[cfg(test)] mod tests` in `src/app.rs`)

```rust
    #[test]
    fn focus_starts_on_tree_and_toggles() {
        let (_dir, mut app) = app_over_tempdir();
        assert_eq!(app.focus, Focus::Tree);
        app.toggle_focus();
        assert_eq!(app.focus, Focus::Terminal);
        app.toggle_focus();
        assert_eq!(app.focus, Focus::Tree);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib app::tests::focus_starts`
Expected: FAIL — `Focus` / `focus` / `toggle_focus` not found.

- [ ] **Step 3: Write the implementation**

Add the enum near the top of `src/app.rs` (after the imports):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Terminal,
}
```

Add the field to the `App<R>` struct (after `should_quit`):

```rust
    pub focus: Focus,
```

Initialize it in `App::new` (in the `Self { ... }` literal, after `should_quit: false,`):

```rust
            focus: Focus::Tree,
```

Add the method inside `impl<R: CommandRunner> App<R>` (e.g. after `down`):

```rust
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Terminal,
            Focus::Terminal => Focus::Tree,
        };
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib app`
Expected: PASS (existing app tests + the new one).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat: add focus state to App"
```

---

### Task 4: Embedded terminal (PTY) module

**Files:**
- Create: `src/pty.rs`
- Modify: `src/lib.rs` (add `pub mod pty;`)

**Interfaces:**
- Consumes: `portable-pty`, `vt100` (via `tui_term::vt100` if re-exported).
- Produces:
  - `pub struct Pty` owning the master PTY, a writer, the shared `vt100` parser, and the spawned child.
  - `pub fn spawn(args: &[&str], rows: u16, cols: u16) -> std::io::Result<Pty>` — opens a PTY of the given size, spawns `tmux` (program `args[0]`, rest as arguments), and starts a reader thread feeding the parser.
  - `pub fn write_input(&mut self, bytes: &[u8]) -> std::io::Result<()>`.
  - `pub fn resize(&mut self, rows: u16, cols: u16) -> std::io::Result<()>` — resizes the PTY and the parser.
  - `pub fn parser(&self) -> std::sync::Arc<std::sync::RwLock<vt100::Parser>>` — clone of the shared parser, for the renderer to read `parser.read().unwrap().screen()`.

This is integration glue: verified by `cargo build` + `cargo clippy`, not unit tests.

- [ ] **Step 1: Confirm the crate API**

Before writing code, check docs.rs for the resolved versions of `portable-pty` and `tui-term`:
- `portable_pty`: `native_pty_system()`, `PtySize`, `CommandBuilder`, `PtyPair { master, slave }`, `master.try_clone_reader()`, `master.take_writer()`, `master.resize(PtySize)`, `slave.spawn_command(CommandBuilder)`.
- Whether `tui_term::vt100` is re-exported (preferred). If not, add `vt100 = "<tui-term's pinned version>"` to `Cargo.toml` and use that. Either way, `vt100::Parser::new(rows, cols, scrollback)`, `parser.process(&bytes)`, `parser.set_size(rows, cols)`, `parser.screen()`.

Adapt the reference code below to the actual signatures you find.

- [ ] **Step 2: Write `src/pty.rs`**

```rust
use std::io::{self, Read, Write};
use std::sync::{Arc, RwLock};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

// If tui-term re-exports vt100, prefer `use tui_term::vt100;` so the Screen
// type unifies with the renderer. Otherwise `use vt100;` with a pinned version.
use tui_term::vt100;

pub struct Pty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    parser: Arc<RwLock<vt100::Parser>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pty {
    pub fn spawn(args: &[&str], rows: u16, cols: u16) -> io::Result<Pty> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(size)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let mut cmd = CommandBuilder::new(args[0]);
        cmd.args(&args[1..]);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::other(e.to_string()))?;

        let parser = Arc::new(RwLock::new(vt100::Parser::new(rows, cols, 0)));
        let reader_parser = parser.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = reader_parser.write() {
                            p.process(&buf[..n]);
                        }
                    }
                }
            }
        });

        Ok(Pty {
            master: pair.master,
            writer,
            parser,
            _child: child,
        })
    }

    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
        if let Ok(mut p) = self.parser.write() {
            p.set_size(rows, cols);
        }
        Ok(())
    }

    pub fn parser(&self) -> Arc<RwLock<vt100::Parser>> {
        self.parser.clone()
    }
}
```

- [ ] **Step 3: Add the module to `src/lib.rs`**

Add `pub mod pty;` to the module list.

- [ ] **Step 4: Build + clippy**

Run: `cargo build && cargo clippy --all-targets 2>&1 | tail -20`
Expected: compiles; no warnings from `pty.rs`. If the crate API differs, adapt the calls (note any change in your report).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/pty.rs src/lib.rs
git commit -m "feat: add embedded terminal PTY module"
```

---

### Task 5: Two-pane render with embedded terminal

**Files:**
- Modify: `src/ui.rs`

**Interfaces:**
- Consumes: `crate::app::Focus`, `crate::tree::Row`, `vt100::Screen` (same vt100 as `pty.rs`), `tui_term::widget::PseudoTerminal`.
- Produces:
  - `pub struct Layout { pub tree: ListLayout, pub split_col: u16, pub term_area: Rect }`.
  - Rewritten `pub fn render(f: &mut Frame, area: Rect, rows: &[Row], selected: usize, active: &HashSet<PathBuf>, focus: Focus, screen: &vt100::Screen) -> Layout` — splits `area` into a left tree pane and a right terminal pane, draws both (focused pane gets a highlighted border), renders the `PseudoTerminal` over `screen` in the right pane, and returns the geometry needed for click routing and PTY resize.

The signature of `render` changes, so `run.rs` (Task 6) must be updated in lockstep; until then the build may not pass through `run.rs` — that's expected and resolved in Task 6. Commit this task even though `run.rs` won't compile yet (the next task fixes it). To keep the tree-drawing logic, keep the existing list-building code; only the outer framing (split, borders, terminal widget, return type) changes.

- [ ] **Step 1: Confirm the tui-term widget API**

On docs.rs for the resolved `tui-term`: confirm `tui_term::widget::PseudoTerminal::new(&screen)` implements `ratatui::widgets::Widget` and renders with `f.render_widget(pseudo_term, area)`. Adapt if the constructor or path differs.

- [ ] **Step 2: Rewrite `render` and add `Layout`** (replace the existing `render` function in `src/ui.rs`)

```rust
use ratatui::layout::{Constraint, Direction, Layout as RtLayout};
use ratatui::style::Color;
use tui_term::widget::PseudoTerminal;

use crate::app::Focus;

pub struct Layout {
    pub tree: ListLayout,
    pub split_col: u16,
    pub term_area: Rect,
}

fn pane_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

pub fn render(
    f: &mut Frame,
    area: Rect,
    rows: &[Row],
    selected: usize,
    active: &HashSet<PathBuf>,
    focus: Focus,
    screen: &vt100::Screen,
) -> Layout {
    let chunks = RtLayout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);
    let tree_area = chunks[0];
    let right_area = chunks[1];

    // ----- left: tree -----
    let tree_block = Block::default()
        .title("tree")
        .borders(Borders::ALL)
        .border_style(pane_border_style(focus == Focus::Tree));
    let inner = tree_block.inner(tree_area);
    f.render_widget(tree_block, tree_area);

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

    let tree_layout = ListLayout {
        origin_y: inner.y,
        button_col_start: inner.x + inner.width.saturating_sub(3),
        button_col_end: inner.x + inner.width.saturating_sub(1),
        row_count: rows.len(),
    };

    // ----- right: embedded terminal -----
    let term_block = Block::default()
        .title("terminal")
        .borders(Borders::ALL)
        .border_style(pane_border_style(focus == Focus::Terminal));
    let term_inner = term_block.inner(right_area);
    f.render_widget(term_block, right_area);
    let pseudo_term = PseudoTerminal::new(screen);
    f.render_widget(pseudo_term, term_inner);

    Layout {
        tree: tree_layout,
        split_col: right_area.x,
        term_area: term_inner,
    }
}
```

- [ ] **Step 3: Update the existing `render_draws_names_and_button` test**

The `render` signature changed (added `focus` and `screen`). Update the test to construct a parser/screen and pass focus. Replace the body of `render_draws_names_and_button` with:

```rust
    #[test]
    fn render_draws_names_and_button() {
        use crate::app::Focus;
        let rows = vec![
            Row { path: PathBuf::from("/p"), name: "p".into(), is_dir: true, depth: 0, expanded: true },
            Row { path: PathBuf::from("/p/src"), name: "src".into(), is_dir: true, depth: 1, expanded: false },
            Row { path: PathBuf::from("/p/r.md"), name: "r.md".into(), is_dir: false, depth: 1, expanded: false },
        ];
        let active: HashSet<PathBuf> = HashSet::new();
        let parser = tui_term::vt100::Parser::new(24, 80, 0);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let _ = render(f, f.area(), &rows, 0, &active, Focus::Tree, parser.screen());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("src"));
        assert!(content.contains("[+]"));
    }
```

(If `vt100` is a standalone dep rather than `tui_term::vt100`, use that path instead. The terminal width is widened to 60 so the 35% tree pane is wide enough to show `src` and `[+]`.)

- [ ] **Step 4: Run the ui tests**

Run: `cargo test --lib ui`
Expected: PASS. (`cargo build` of the whole crate will still fail in `run.rs` because it calls the old `render` signature — that is fixed in Task 6.)

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "feat: render fixed two-pane layout with embedded terminal"
```

---

### Task 6: Event loop with PTY, focus routing, and resize

**Files:**
- Modify: `src/run.rs`

**Interfaces:**
- Consumes: `App` + `Focus` + `toggle_focus`, `Pty`, `keys::encode_key`, `input::{map_key, Action}`, `ui::{render, Layout, resolve_pane_click, PaneHit, Hit}`.
- Produces: unchanged signature `pub fn run(root: PathBuf, socket: String, editor: String) -> std::io::Result<()>`.

Integration glue: verified by `cargo build` + `cargo clippy` + manual run.

- [ ] **Step 1: Rewrite `src/run.rs`**

```rust
use std::io;
use std::path::PathBuf;
use std::time::Duration;

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

use crate::app::{App, Focus};
use crate::input::{map_key, Action};
use crate::keys::encode_key;
use crate::pty::Pty;
use crate::tmux::{SystemRunner, Tmux};
use crate::ui::{self, Hit, PaneHit};

pub fn run(root: PathBuf, socket: String, editor: String) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Spawn the embedded terminal: a tmux client attached to (or creating) the
    // scratch session on the runner socket. Initial size is a placeholder; the
    // first resize after the initial draw corrects it.
    let pty_args = ["tmux", "-L", socket.as_str(), "new-session", "-A", "-s", "scratch"];
    let pty = Pty::spawn(&pty_args, 24, 80);
    let mut pty = match pty {
        Ok(p) => p,
        Err(e) => {
            let _ = disable_raw_mode();
            let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
            return Err(e);
        }
    };
    let parser = pty.parser();

    let tmux = Tmux::new(socket, SystemRunner);
    let mut app = App::new(root, tmux, editor);
    let _ = app.sync_active();

    let mut last_term_size: (u16, u16) = (0, 0);

    let result = loop {
        let draw_res = terminal.draw(|f| {
            let guard = parser.read().unwrap();
            let layout = ui::render(
                f,
                f.area(),
                &app.rows,
                app.selected,
                &app.active,
                app.focus,
                guard.screen(),
            );
            // Stash geometry for input handling after the closure.
            f.set_cursor_position((layout.split_col, 0)); // harmless; real handling below
            APP_LAYOUT.with(|c| *c.borrow_mut() = Some((layout.tree.clone_fields(), layout.split_col, layout.term_area)));
        });
        if let Err(e) = draw_res {
            break Err(e);
        }
        // (see Step 2 note: simpler to read layout from a returned value; this
        //  template uses a small refactor — adapt as described.)
        break Ok(());
    };

    let restore_raw = disable_raw_mode();
    let restore_screen = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    result.and(restore_raw).and(restore_screen)
}
```

The closure-capture pattern above is awkward because `terminal.draw` borrows the frame. Use this cleaner structure instead, which captures the returned `Layout` out of the closure:

```rust
    let result = loop {
        let mut captured: Option<ui::Layout> = None;
        let draw_res = terminal.draw(|f| {
            let guard = parser.read().unwrap();
            captured = Some(ui::render(
                f,
                f.area(),
                &app.rows,
                app.selected,
                &app.active,
                app.focus,
                guard.screen(),
            ));
        });
        if let Err(e) = draw_res {
            break Err(e);
        }
        let layout = captured.expect("render returns a Layout");

        // Resize the PTY to match the terminal pane's inner area.
        let term_size = (layout.term_area.height, layout.term_area.width);
        if term_size != last_term_size && term_size.0 > 0 && term_size.1 > 0 {
            let _ = pty.resize(term_size.0, term_size.1);
            last_term_size = term_size;
        }

        if !event::poll(Duration::from_millis(33)).unwrap_or(false) {
            continue; // tick: redraw to reflect new PTY output
        }

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                // Ctrl-q toggles focus regardless of current focus.
                if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.toggle_focus();
                } else {
                    match app.focus {
                        Focus::Tree => match map_key(key) {
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
                        Focus::Terminal => {
                            let _ = pty.write_input(&encode_key(key));
                        }
                    }
                }
            }
            Ok(Event::Mouse(m)) => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    match ui::resolve_pane_click(m.column, m.row, layout.split_col, &layout.tree) {
                        PaneHit::Terminal => app.focus = Focus::Terminal,
                        PaneHit::Tree(hit) => {
                            app.focus = Focus::Tree;
                            match hit {
                                Some(Hit::Row(idx)) => {
                                    app.selected = idx;
                                    let _ = app.activate();
                                }
                                Some(Hit::Button(idx)) => {
                                    app.selected = idx;
                                    let _ = app.open_session();
                                }
                                None => {}
                            }
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
```

Use the second structure (delete the first awkward `loop` body and the `APP_LAYOUT`/`set_cursor_position` lines — they are illustrative only). Keep the setup before the loop and the teardown after it.

- [ ] **Step 2: Build + clippy**

Run: `cargo build && cargo clippy --all-targets 2>&1 | tail -20`
Expected: compiles cleanly; no warnings from `run.rs`. Resolve any borrow issues by capturing `Layout` out of the `draw` closure as shown.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: all existing unit tests still pass (this task adds no unit tests).

- [ ] **Step 4: Commit**

```bash
git add src/run.rs
git commit -m "feat: event loop with embedded PTY, focus routing, and resize"
```

---

### Task 7: main entry + remove bootstrap.rs and cli.rs

**Files:**
- Modify: `src/main.rs`
- Delete: `src/bootstrap.rs`, `src/cli.rs`
- Modify: `src/lib.rs` (remove `pub mod bootstrap;` and `pub mod cli;`)

**Interfaces:**
- Consumes: `runner_manager::run`.
- Produces: `main` that refuses `$TMUX` and runs the TUI directly.

- [ ] **Step 1: Rewrite `src/main.rs`**

```rust
use std::env;
use std::io;

use runner_manager::run;

fn main() -> io::Result<()> {
    if env::var_os("TMUX").is_some() {
        eprintln!(
            "runner-manager must not be run inside tmux; tmux is used for the inner task sessions."
        );
        std::process::exit(1);
    }
    let root = env::current_dir()?;
    let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    run::run(root, "runner".to_string(), editor)
}
```

- [ ] **Step 2: Delete the obsolete modules**

```bash
git rm src/bootstrap.rs src/cli.rs
```

- [ ] **Step 3: Remove their declarations from `src/lib.rs`**

Delete the lines `pub mod bootstrap;` and `pub mod cli;`. The module list becomes:

```rust
pub mod tmux;
pub mod session;
pub mod tree;
pub mod input;
pub mod keys;
pub mod ui;
pub mod app;
pub mod pty;
pub mod run;
```

- [ ] **Step 4: Build, test, clippy**

Run: `cargo build && cargo test && cargo clippy --all-targets 2>&1 | tail -20`
Expected: builds; all tests pass; no warnings. There must be no remaining references to `bootstrap` or `cli` anywhere.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/lib.rs
git commit -m "feat: run TUI directly, refuse \$TMUX, drop bootstrap/cli"
```

---

### Task 8: README v2 update + manual verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Rewrite `README.md`**

```markdown
# runner-manager

A standalone terminal UI that pairs a NERDTree-style file tree (left) with a
live embedded terminal (right). Each directory maps to its own tmux session;
selecting a directory creates-or-switches to that session in the right pane,
with the tree always pinned on the left. Files open in `$EDITOR` inside their
directory's session.

## How it works

- runner-manager runs directly in your terminal (it must NOT be run inside
  tmux). It draws a fixed two-pane layout on the alternate screen.
- The right pane is a real embedded terminal: it spawns
  `tmux -L runner new-session -A -s scratch` in a PTY and renders it. Selecting
  a directory switches that terminal to the directory's session.
- tmux is used only for the inner per-directory sessions (socket `runner`),
  which persist across runs.

## Usage

Run from the directory you want as the tree root (not inside tmux):

```bash
runner-manager
```

| Key            | Action                                       |
|----------------|----------------------------------------------|
| `j` / `down`   | move down (tree focus)                       |
| `k` / `up`     | move up (tree focus)                         |
| `Enter`        | expand/collapse a directory; open a file     |
| `a`            | open/switch the session for a directory      |
| `x`            | kill a directory's session                   |
| `q`            | quit (tree focus only; inner sessions persist)|
| `Ctrl-q`       | toggle focus between tree and terminal       |
| left-click     | focus the clicked pane; in tree, select/act  |

When the terminal pane has focus, every key except `Ctrl-q` goes to the inner
tmux session (shell, vim, the tmux prefix `C-b`, etc.). The focused pane has a
highlighted border; the layout never changes.
```

- [ ] **Step 2: Final full verification**

Run: `cargo test && cargo build --release && cargo clippy --all-targets 2>&1 | tail -20`
Expected: all tests pass; release builds; no warnings.

- [ ] **Step 3: Manual smoke test** (requires tmux; not automated)

```bash
# Ensure not inside tmux, then:
tmux -L runner kill-server 2>/dev/null   # clean slate
cargo run
```

Verify: two panes render (tree left, a shell right); `Ctrl-q` moves the
highlighted border without changing the layout; with terminal focus you can
type into the shell; `a`/`[+]`/click on a directory switches the right pane to
that dir's session; `Enter` on a file opens it in `$EDITOR`; `q` (tree focus)
quits and `tmux -L runner ls` still lists the sessions. Also confirm running
`runner-manager` from inside a tmux session exits with the refusal message.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: update README for v2 embedded-terminal architecture"
```

---

## Self-Review Notes

- **Spec coverage:** standalone TUI / no outer tmux (Tasks 6–7), embedded terminal via portable-pty+vt100+tui-term (Tasks 4–5), `tmux -L runner new-session -A -s scratch` (Task 6), `switch-client` reuse (unchanged `app.rs`), fixed two-pane layout + always-visible tree (Task 5), `Ctrl-q`/click focus with highlight (Tasks 5–6), key-encoding layer (Task 1), `$TMUX` refusal (Task 7), removal of bootstrap.rs/cli.rs (Task 7), persistence (unchanged), README (Task 8). All spec sections map to a task.
- **Placeholder scan:** the only intentionally-illustrative code is the first awkward loop body in Task 6 Step 1, which is explicitly superseded by the second structure in the same step (with an instruction to delete the first). All other code is complete.
- **Type consistency:** `Focus` (app.rs) used by ui.rs and run.rs; `Layout`/`PaneHit`/`resolve_pane_click`/`Hit` (ui.rs) used by run.rs; `Pty::{spawn,write_input,resize,parser}` (pty.rs) used by run.rs; `encode_key` (keys.rs) used by run.rs; `render` new signature consistent between Task 5 (definition) and Task 6 (call). The `vt100` type is shared via `tui_term::vt100` to ensure `Screen` unifies between pty.rs and ui.rs.
- **Build-green ordering:** Tasks 1–4 keep the crate compiling. Task 5 changes `render`'s signature, so the whole-crate build is red between Task 5 and Task 6 (ui's own tests still pass); Task 6 restores it. This is called out in Task 5.
