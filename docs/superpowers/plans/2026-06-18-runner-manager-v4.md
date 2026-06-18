# runner-manager v4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the tree/terminal split resizable (keyboard + mouse-drag), and turn the new-session chooser into a form (shell/claude kind, claude normal/skip permission, Cancel/Create buttons).

**Architecture:** `App` gains `split_pct` (clamped percent) driving the layout constraints, with `<`/`>` keys and border-drag in `run.rs`. The chooser `Popup` variant becomes a small form state machine (`kind`/`perm`/`focus`) with radios-follow-focus; `ui::render_chooser` draws it and reports per-row screen Y for click mapping.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, portable-pty 0.9, tui-term 0.2, tempfile (dev).

## Global Constraints

- Split percent is the **tree** pane width; default `35`, clamped to `[15, 80]`, step `5`. Per-run only (no disk persistence).
- `<` narrows the tree, `>` widens it (tree focus, no popup). Border drag sets `split_pct` from the cursor column. Window resize keeps it proportional (percent-based).
- Chooser kinds: `shell`, `claude`. When `claude` is focused/selected, a permission radio appears: `normal`, `skip`. Buttons: `Cancel`, `Create`.
- Command mapping at Create: `shell` → `None`; `claude`+`normal` → `claude`; `claude`+`skip` → `claude --dangerously-skip-permissions`. Session slug/label stay keyed by kind (unchanged) — permission does not affect the label.
- Radios follow focus: moving focus onto a radio row selects it. `Enter`/`Space` activates the focused button; `Esc` cancels; a click on a row selects/activates it; a click outside the popup cancels.
- Tests inline `#[cfg(test)]`; TDD for pure state. Rendering + mouse hit-regions are manual-verified (plus a TestBackend smoke test that the chooser draws the radios and both buttons).

## File Structure

- `src/app.rs` — `split_pct` + `widen_split`/`narrow_split` + pure `col_to_split_pct`; chooser form state (`ChooserRow`, reshaped `Popup::Chooser`, `chooser_move`/`chooser_click`/`chooser_activate`/`chooser_command`); `create_session` gains a command arg.
- `src/session.rs` — add `ClaudePerm { Normal, Skip }`.
- `src/ui.rs` — layout constraints use `split_pct`; `render_chooser` rewritten to draw the form and return per-row Y geometry.
- `src/run.rs` — `<`/`>` keys; border-drag tracking; chooser key/mouse routing for the form.

---

### Task 1: Adjustable splitter

**Files:**
- Modify: `src/app.rs` (field + methods + pure helper + tests)
- Modify: `src/ui.rs` (layout constraints)
- Modify: `src/run.rs` (keys + drag)

**Interfaces:**
- Produces: `App.split_pct: u16`; `App::widen_split(&mut self)`, `App::narrow_split(&mut self)`; free `pub fn col_to_split_pct(col: u16, width: u16) -> u16`.

- [ ] **Step 1: Write the failing tests** (append to `src/app.rs`'s `#[cfg(test)] mod tests`)

```rust
    #[test]
    fn split_widen_and_narrow_clamp() {
        let (_d, mut app) = app_over_tempdir();
        assert_eq!(app.split_pct, 35);
        app.widen_split(); // 40
        app.narrow_split(); // 35
        assert_eq!(app.split_pct, 35);
        for _ in 0..30 {
            app.widen_split();
        }
        assert_eq!(app.split_pct, 80); // clamped high
        for _ in 0..30 {
            app.narrow_split();
        }
        assert_eq!(app.split_pct, 15); // clamped low
    }

    #[test]
    fn col_to_split_pct_clamps_and_is_safe() {
        assert_eq!(col_to_split_pct(50, 100), 50);
        assert_eq!(col_to_split_pct(0, 100), 15); // clamp low
        assert_eq!(col_to_split_pct(99, 100), 80); // clamp high
        assert_eq!(col_to_split_pct(10, 0), 35); // zero width -> default, no panic
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib app 2>&1 | tail -20`
Expected: FAIL — `split_pct`/`widen_split`/`col_to_split_pct` not found.

- [ ] **Step 3: Implement in `src/app.rs`**

Add these consts and the pure helper near the top of the file (after the `use` lines):

```rust
const MIN_SPLIT: u16 = 15;
const MAX_SPLIT: u16 = 80;
const SPLIT_STEP: u16 = 5;
const DEFAULT_SPLIT: u16 = 35;

/// Convert a cursor column to a clamped tree-width percent. Zero width
/// returns the default (avoids divide-by-zero during a drag on a 0-wide area).
pub fn col_to_split_pct(col: u16, width: u16) -> u16 {
    if width == 0 {
        return DEFAULT_SPLIT;
    }
    let pct = (col as u32 * 100 / width as u32) as u16;
    pct.clamp(MIN_SPLIT, MAX_SPLIT)
}
```

Add the field to the `App<R>` struct (after `status: String,`):

```rust
    pub split_pct: u16,
```

Initialize it in `App::new` (in the `Self { ... }` literal, after `status: String::new(),`):

```rust
            split_pct: DEFAULT_SPLIT,
```

Add the two methods inside `impl<R: CommandRunner> App<R>` (e.g. after `toggle_focus`):

```rust
    pub fn widen_split(&mut self) {
        self.split_pct = (self.split_pct + SPLIT_STEP).min(MAX_SPLIT);
    }

    pub fn narrow_split(&mut self) {
        self.split_pct = self.split_pct.saturating_sub(SPLIT_STEP).max(MIN_SPLIT);
    }
```

- [ ] **Step 4: Run app tests**

Run: `cargo test --lib app 2>&1 | tail -10`
Expected: PASS (existing app tests + the 2 new).

- [ ] **Step 5: Use `split_pct` in the layout** — in `src/ui.rs`, the `render` function builds the horizontal split. Replace:

```rust
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
```

with:

```rust
        .constraints([
            Constraint::Percentage(app.split_pct),
            Constraint::Percentage(100 - app.split_pct),
        ])
```

(`app.split_pct ≤ 80`, so `100 - app.split_pct` never underflows.)

- [ ] **Step 6: Add resize keys + border drag in `src/run.rs`**

(a) Add the split keys to the `Focus::Tree` match (inside `Popup::None`), alongside the existing arms:

```rust
                                    KeyCode::Char('<') => app.narrow_split(),
                                    KeyCode::Char('>') => app.widen_split(),
```

(b) Capture the frame width during draw so the drag handler can convert columns to a percent. Add before the loop:

```rust
    let mut area_width: u16 = 0;
    let mut dragging_split = false;
```

Inside the `terminal.draw(|f| { ... })` closure, add as its first line:

```rust
            area_width = f.area().width;
```

(c) Replace the entire mouse arm — change the current `Ok(Event::Mouse(m)) => { if let MouseEventKind::Down(MouseButton::Left) = m.kind { ... } }` block with a `match m.kind` that also handles drag/up:

```rust
            Ok(Event::Mouse(m)) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => match app.popup.clone() {
                    Popup::Help => app.popup = Popup::None,
                    Popup::Chooser { .. } => app.chooser_cancel(),
                    Popup::None => {
                        let border = layout.split_col;
                        let on_border =
                            m.column + 1 >= border && m.column <= border.saturating_add(1);
                        if on_border {
                            dragging_split = true;
                        } else {
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
                },
                MouseEventKind::Drag(MouseButton::Left) => {
                    if dragging_split {
                        app.split_pct = crate::app::col_to_split_pct(m.column, area_width);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    dragging_split = false;
                }
                _ => {}
            },
```

Note: the `on_border` test treats columns `border-1 .. border+1` as the draggable border (`m.column + 1 >= border` is the underflow-safe form of `m.column >= border - 1`).

- [ ] **Step 7: Build, test, clippy**

Run: `cargo build && cargo test 2>&1 | tail -6 && cargo clippy --all-targets 2>&1 | tail -20`
Expected: builds; all tests pass; clippy clean. (The `Drag`/`Up` arms require no extra crossterm imports — `MouseEventKind` is already imported.)

- [ ] **Step 8: Commit**

```bash
git add src/app.rs src/ui.rs src/run.rs
git commit -m "feat: adjustable tree/terminal splitter (keys + drag)"
```

---

### Task 2: Enriched chooser form

**Files:**
- Modify: `src/session.rs` (add `ClaudePerm`)
- Modify: `src/app.rs` (reshape `Popup::Chooser`, form state machine, command, tests)
- Modify: `src/ui.rs` (rewrite `render_chooser`)
- Modify: `src/run.rs` (chooser key/mouse routing)

This is a coordinated change: the `Popup::Chooser` variant shape changes, so `app.rs`, `ui.rs`, and `run.rs` must change together. It lands green in one commit.

**Interfaces:**
- `session::ClaudePerm { Normal, Skip }` (derives `Debug, Clone, Copy, PartialEq, Eq`).
- `app::ChooserRow { KindShell, KindClaude, PermNormal, PermSkip, Cancel, Create }` (derives `Debug, Clone, Copy, PartialEq, Eq`).
- `Popup::Chooser { dir: PathBuf, kind: SessionKind, perm: ClaudePerm, focus: usize }`.
- `App::chooser_rows(&self) -> Vec<ChooserRow>` (the visible focusable rows for the current kind).
- `App::chooser_move(&mut self, delta: i32)`, `App::chooser_click(&mut self, row: ChooserRow) -> io::Result<()>`, `App::chooser_activate(&mut self) -> io::Result<()>`, `App::chooser_cancel(&mut self)`, `App::chooser_command(kind, perm) -> Option<String>`.
- `ui::render_chooser(f, area, kind, perm, focus_row) -> Vec<(u16, ChooserRow)>` — draws the form, returns each focusable row's screen Y.

- [ ] **Step 1: Add `ClaudePerm` to `src/session.rs`** (near `SessionKind`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudePerm {
    Normal,
    Skip,
}
```

- [ ] **Step 2: Write the failing tests** (append to `src/app.rs`'s `#[cfg(test)] mod tests`)

```rust
    use crate::session::ClaudePerm;

    fn open_dir_chooser(app: &mut App<MockRunner>) {
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
    }

    #[test]
    fn chooser_defaults_to_shell_with_no_perm_rows() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        assert_eq!(
            app.chooser_rows(),
            vec![ChooserRow::KindShell, ChooserRow::KindClaude, ChooserRow::Cancel, ChooserRow::Create]
        );
    }

    #[test]
    fn focusing_claude_reveals_perm_rows_and_selects_it() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_move(1); // focus -> claude
        if let Popup::Chooser { kind, .. } = app.popup {
            assert_eq!(kind, SessionKind::Claude);
        } else {
            panic!("expected chooser");
        }
        assert_eq!(
            app.chooser_rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::PermNormal,
                ChooserRow::PermSkip,
                ChooserRow::Cancel,
                ChooserRow::Create
            ]
        );
    }

    #[test]
    fn focusing_skip_sets_perm_then_back_to_shell_reclamps() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_move(1); // claude
        app.chooser_move(1); // normal
        app.chooser_move(1); // skip
        if let Popup::Chooser { perm, .. } = app.popup {
            assert_eq!(perm, ClaudePerm::Skip);
        } else {
            panic!();
        }
        // move focus up to shell -> kind becomes Shell, perm rows vanish, focus valid
        app.chooser_move(-3); // skip(3) -> shell(0)
        if let Popup::Chooser { kind, focus, .. } = app.popup {
            assert_eq!(kind, SessionKind::Shell);
            assert!(focus < app.chooser_rows().len());
        } else {
            panic!();
        }
    }

    #[test]
    fn chooser_command_maps_kind_and_perm() {
        assert_eq!(App::<MockRunner>::chooser_command(SessionKind::Shell, ClaudePerm::Normal), None);
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Normal).as_deref(),
            Some("claude")
        );
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Skip).as_deref(),
            Some("claude --dangerously-skip-permissions")
        );
    }

    #[test]
    fn chooser_activate_create_starts_claude_skip() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_move(1); // claude
        app.chooser_move(2); // skip
        // move focus to Create (rows: shell,claude,normal,skip,Cancel,Create => Create index 5)
        let create_idx = app.chooser_rows().iter().position(|r| *r == ChooserRow::Create).unwrap();
        if let Popup::Chooser { focus, .. } = &mut app.popup {
            *focus = create_idx;
        }
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_activate().unwrap();
        let call = app.tmux.runner.nth_call(0);
        assert_eq!(call[2], "new-session");
        assert!(call.contains(&"claude --dangerously-skip-permissions".to_string()));
        assert_eq!(app.popup, Popup::None);
    }

    #[test]
    fn chooser_activate_cancel_closes_without_tmux() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        let cancel_idx = app.chooser_rows().iter().position(|r| *r == ChooserRow::Cancel).unwrap();
        if let Popup::Chooser { focus, .. } = &mut app.popup {
            *focus = cancel_idx;
        }
        app.chooser_activate().unwrap();
        assert_eq!(app.popup, Popup::None);
        assert_eq!(app.tmux.runner.call_count(), 0);
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib app 2>&1 | tail -20`
Expected: FAIL — `ChooserRow`, reshaped `Popup::Chooser`, `chooser_rows`, `chooser_command`, `chooser_activate` not found.

- [ ] **Step 4: Reshape the chooser in `src/app.rs`**

(a) Update imports: change `use crate::session::{SessionKind, SessionStore};` to:

```rust
use crate::session::{ClaudePerm, SessionKind, SessionStore};
```

(b) Add the row enum (near `Popup`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserRow {
    KindShell,
    KindClaude,
    PermNormal,
    PermSkip,
    Cancel,
    Create,
}
```

(c) Replace the `Popup` enum's `Chooser` variant:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    None,
    Help,
    Chooser {
        dir: PathBuf,
        kind: SessionKind,
        perm: ClaudePerm,
        focus: usize,
    },
}
```

(The `CHOOSER_KINDS` const is no longer used by the chooser; remove its definition in `app.rs` and its `use` in `ui.rs` — Step 5.)

(d) Replace `open_chooser`, `chooser_move`, `chooser_confirm`, and `create_session` with:

```rust
    pub fn open_chooser(&mut self) {
        if let Some(row) = self.selected_row() {
            if matches!(row.kind, RowKind::Dir { .. }) {
                self.popup = Popup::Chooser {
                    dir: row.path.clone(),
                    kind: SessionKind::Shell,
                    perm: ClaudePerm::Normal,
                    focus: 0,
                };
            }
        }
    }

    /// Visible focusable rows for the current chooser kind.
    pub fn chooser_rows(&self) -> Vec<ChooserRow> {
        let mut rows = vec![ChooserRow::KindShell, ChooserRow::KindClaude];
        if let Popup::Chooser { kind: SessionKind::Claude, .. } = self.popup {
            rows.push(ChooserRow::PermNormal);
            rows.push(ChooserRow::PermSkip);
        }
        rows.push(ChooserRow::Cancel);
        rows.push(ChooserRow::Create);
        rows
    }

    pub fn chooser_move(&mut self, delta: i32) {
        let rows = self.chooser_rows();
        let Popup::Chooser { focus, .. } = &mut self.popup else {
            return;
        };
        let max = rows.len() as i32 - 1;
        let new_focus = (*focus as i32 + delta).clamp(0, max) as usize;
        *focus = new_focus;
        self.chooser_apply_focus(rows[new_focus]);
    }

    /// Clicking (or focusing) a row selects radios; clicking a button acts.
    pub fn chooser_click(&mut self, row: ChooserRow) -> io::Result<()> {
        if let Some(idx) = self.chooser_rows().iter().position(|r| *r == row) {
            if let Popup::Chooser { focus, .. } = &mut self.popup {
                *focus = idx;
            }
        }
        self.chooser_apply_focus(row);
        if matches!(row, ChooserRow::Cancel | ChooserRow::Create) {
            self.chooser_activate()?;
        }
        Ok(())
    }

    /// Apply the radio-follows-focus rule for the given row.
    fn chooser_apply_focus(&mut self, row: ChooserRow) {
        if let Popup::Chooser { kind, perm, focus, .. } = &mut self.popup {
            match row {
                ChooserRow::KindShell => *kind = SessionKind::Shell,
                ChooserRow::KindClaude => *kind = SessionKind::Claude,
                ChooserRow::PermNormal => *perm = ClaudePerm::Normal,
                ChooserRow::PermSkip => *perm = ClaudePerm::Skip,
                _ => {}
            }
            // Switching to Shell removes the perm rows; re-clamp focus.
            let row_count = if *kind == SessionKind::Claude { 6 } else { 4 };
            if *focus >= row_count {
                *focus = row_count - 1;
            }
        }
    }

    pub fn chooser_activate(&mut self) -> io::Result<()> {
        let Popup::Chooser { dir, kind, perm, focus } = self.popup.clone() else {
            return Ok(());
        };
        let rows = self.chooser_rows();
        match rows.get(focus) {
            Some(ChooserRow::Cancel) => {
                self.popup = Popup::None;
            }
            Some(ChooserRow::Create) => {
                let cmd = Self::chooser_command(kind, perm);
                self.popup = Popup::None;
                self.create_session(&dir, kind, cmd.as_deref())?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn chooser_cancel(&mut self) {
        self.popup = Popup::None;
    }

    pub fn chooser_command(kind: SessionKind, perm: ClaudePerm) -> Option<String> {
        match kind {
            SessionKind::Shell => None,
            SessionKind::Claude => match perm {
                ClaudePerm::Normal => Some("claude".to_string()),
                ClaudePerm::Skip => Some("claude --dangerously-skip-permissions".to_string()),
            },
        }
    }

    fn create_session(&mut self, dir: &Path, kind: SessionKind, command: Option<&str>) -> io::Result<()> {
        let slug = self.store.create(dir, &self.root, kind);
        self.tmux.new_session(&slug, dir, command)?;
        self.rebuild_rows();
        self.switch_to(&slug)?;
        self.status = format!("started {}", kind.label_base());
        Ok(())
    }
```

(Remove the old `chooser_confirm`; `chooser_activate` replaces it.)

- [ ] **Step 5: Rewrite `render_chooser` in `src/ui.rs`**

(a) Update the app import — remove `CHOOSER_KINDS`, add the form types. Change:

```rust
use crate::app::{App, Focus, CHOOSER_KINDS};
```

to:

```rust
use crate::app::{App, ChooserRow, Focus};
use crate::session::{ClaudePerm, SessionKind};
```

Also add `use ratatui::text::Line;` near the other ratatui imports if it is not already present.

(b) Replace the whole `render_chooser` function with this form renderer (draws section labels + one focusable row per line, returns each focusable row's screen Y):

```rust
pub fn render_chooser(
    f: &mut Frame,
    area: Rect,
    kind: SessionKind,
    perm: ClaudePerm,
    focus_row: ChooserRow,
) -> Vec<(u16, ChooserRow)> {
    let popup = centered_rect(50, 60, area);
    f.render_widget(Clear, popup);
    let block = Block::default().title("New session").borders(Borders::ALL);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    let mut row_ys: Vec<(u16, ChooserRow)> = Vec::new();
    let mut y = inner.y;

    let radio = |selected: bool| if selected { "(•)" } else { "( )" };
    let arrow = |row: ChooserRow| if row == focus_row { "> " } else { "  " };

    // Kind:
    lines.push(Line::from("Kind:".to_string()));
    y += 1;
    lines.push(Line::from(format!("{}{} shell", arrow(ChooserRow::KindShell), radio(kind == SessionKind::Shell))));
    row_ys.push((y, ChooserRow::KindShell));
    y += 1;
    lines.push(Line::from(format!("{}{} claude", arrow(ChooserRow::KindClaude), radio(kind == SessionKind::Claude))));
    row_ys.push((y, ChooserRow::KindClaude));
    y += 1;

    if kind == SessionKind::Claude {
        lines.push(Line::from("Permission:".to_string()));
        y += 1;
        lines.push(Line::from(format!("{}{} normal", arrow(ChooserRow::PermNormal), radio(perm == ClaudePerm::Normal))));
        row_ys.push((y, ChooserRow::PermNormal));
        y += 1;
        lines.push(Line::from(format!("{}{} skip (--dangerously-skip-permissions)", arrow(ChooserRow::PermSkip), radio(perm == ClaudePerm::Skip))));
        row_ys.push((y, ChooserRow::PermSkip));
        y += 1;
    }

    lines.push(Line::from(String::new()));
    y += 1;
    lines.push(Line::from(format!("{}[ Cancel ]", arrow(ChooserRow::Cancel))));
    row_ys.push((y, ChooserRow::Cancel));
    y += 1;
    lines.push(Line::from(format!("{}[ Create ]", arrow(ChooserRow::Create))));
    row_ys.push((y, ChooserRow::Create));

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
    row_ys
}
```

- [ ] **Step 6: Update chooser routing in `src/run.rs`**

(a) Add before the loop: `let mut chooser_row_ys: Vec<(u16, crate::app::ChooserRow)> = Vec::new();`

(b) In the `terminal.draw(|f| { ... })` closure, change the popup match's chooser arm from:

```rust
                Popup::Chooser { selected, .. } => {
                    let _ = ui::render_chooser(f, f.area(), *selected);
                }
```

to:

```rust
                Popup::Chooser { kind, perm, focus, .. } => {
                    let focus_row = app
                        .chooser_rows()
                        .get(*focus)
                        .copied()
                        .unwrap_or(crate::app::ChooserRow::KindShell);
                    chooser_row_ys = ui::render_chooser(f, f.area(), *kind, *perm, focus_row);
                }
```

> Borrow note: `app.chooser_rows()` and the `kind/perm/focus` bindings all borrow `app` immutably, alongside the immutable `&app` already passed to `ui::render` — all immutable, so this compiles. If the borrow checker objects, compute `focus_row` and the chooser fields into locals just before `terminal.draw` and capture them by value.

(c) Replace the chooser key arm:

```rust
                    Popup::Chooser { .. } => match key.code {
                        KeyCode::Esc => app.chooser_cancel(),
                        KeyCode::Enter => {
                            let _ = app.chooser_confirm();
                        }
                        KeyCode::Down | KeyCode::Char('j') => app.chooser_move(1),
                        KeyCode::Up | KeyCode::Char('k') => app.chooser_move(-1),
                        _ => {}
                    },
```

with:

```rust
                    Popup::Chooser { .. } => match key.code {
                        KeyCode::Esc => app.chooser_cancel(),
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            let _ = app.chooser_activate();
                        }
                        KeyCode::Down | KeyCode::Char('j') => app.chooser_move(1),
                        KeyCode::Up | KeyCode::Char('k') => app.chooser_move(-1),
                        _ => {}
                    },
```

(d) In the mouse `Down(Left)` handler (as rewritten in Task 1), replace the chooser arm:

```rust
                    Popup::Chooser { .. } => app.chooser_cancel(),
```

with a y-lookup against the captured row geometry (click a row → select/act; click off any row → cancel):

```rust
                    Popup::Chooser { .. } => {
                        match chooser_row_ys.iter().find(|(y, _)| *y == m.row) {
                            Some((_, row)) => {
                                let _ = app.chooser_click(*row);
                            }
                            None => app.chooser_cancel(),
                        }
                    }
```

- [ ] **Step 7: Build, fix, test, clippy**

Run: `cargo build 2>&1 | tail -20`
Resolve compile errors from the variant reshape (most likely: a leftover `chooser_confirm` reference, the `CHOOSER_KINDS` removal, or imports). Then:

Run: `cargo test 2>&1 | tail -8 && cargo clippy --all-targets 2>&1 | tail -20`
Expected: all tests pass (the 6 new chooser tests + Task 1's); clippy clean. Do NOT reintroduce `chooser_confirm`/`CHOOSER_KINDS`.

- [ ] **Step 8: Add a chooser render smoke test** (append to `src/ui.rs`'s `#[cfg(test)] mod tests`)

```rust
    #[test]
    fn render_chooser_draws_radios_and_buttons() {
        use crate::app::ChooserRow;
        use crate::session::{ClaudePerm, SessionKind};
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let rows = render_chooser(
                    f,
                    f.area(),
                    SessionKind::Claude,
                    ClaudePerm::Skip,
                    ChooserRow::Create,
                );
                assert!(rows.iter().any(|(_, r)| *r == ChooserRow::Create));
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("shell"));
        assert!(content.contains("claude"));
        assert!(content.contains("skip"));
        assert!(content.contains("Cancel"));
        assert!(content.contains("Create"));
    }
```

Run: `cargo test --lib ui 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/session.rs src/app.rs src/ui.rs src/run.rs
git commit -m "feat: enriched chooser form (kind, claude permission, buttons)"
```

---

### Task 3: README + manual verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update `README.md`** — replace the existing key table with:

```markdown
| Key            | Action                                              |
|----------------|-----------------------------------------------------|
| `j` / `down`   | move down (tree focus)                              |
| `k` / `up`     | move up (tree focus)                                |
| `Enter`        | expand/collapse dir · switch to session · view file |
| `a` / `[+]`    | new session form (shell/claude) on a directory      |
| `<` / `>`      | narrow / widen the tree pane (tree focus)           |
| `h` / `?`      | help popup                                          |
| `q`            | quit (tree focus)                                   |
| `Ctrl-q`       | toggle focus between tree and the right pane        |
| left-click     | focus a pane; in the tree, act on the clicked row   |
| drag border    | resize the tree/terminal split                      |
```

And add, after the usage table:

```markdown
In the new-session form: `↑`/`↓`/`j`/`k` move between rows (selecting `claude`
reveals a permission choice: `normal` or `skip` = `--dangerously-skip-permissions`),
`Enter`/`Space` activates the focused `Cancel`/`Create` button, `Esc` cancels.
Click a row to select it, or click `Cancel`/`Create`. The split between the tree
and the right pane is adjustable with `<`/`>` or by dragging the border.
```

- [ ] **Step 2: Final verification**

Run: `cargo test && cargo build --release && cargo clippy --all-targets 2>&1 | tail -20`
Expected: all tests pass; release builds; clippy clean.

- [ ] **Step 3: Manual smoke test** (requires tmux; not automated)

```bash
tmux -L runner kill-server 2>/dev/null
cargo run   # from a project dir, NOT inside tmux
```

Verify: `a`/`[+]` on a directory opens the form; arrowing onto `claude` reveals
`normal`/`skip`; focusing `Create` and pressing Enter starts the session (skip →
`claude --dangerously-skip-permissions`); `Cancel`/`Esc` closes with no session;
`<`/`>` resize the tree pane and dragging the border resizes it too; the split
stays proportional when the terminal window is resized.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: update README for chooser form + adjustable splitter"
```

---

## Self-Review Notes

- **Spec coverage:** split_pct + clamp + default (Task 1 app); `<`/`>` keys + drag + percent-proportional (Task 1 run/ui); chooser kind/perm radios with radios-follow-focus + Cancel/Create + command mapping (Task 2 app); `ClaudePerm` (Task 2 session); form rendering + click mapping (Task 2 ui/run); README (Task 3). All spec sections map to a task.
- **Placeholder scan:** none — every code step is complete. The one judgment call (Cancel/Create rendered one-per-line rather than side-by-side) is stated explicitly and keeps click mapping a simple y-lookup; it still satisfies "two buttons under the popup."
- **Type consistency:** `Popup::Chooser { dir, kind, perm, focus }`, `ChooserRow`, `ClaudePerm`, `chooser_rows`/`chooser_move`/`chooser_click`/`chooser_activate`/`chooser_command`, and `render_chooser(f, area, kind, perm, focus_row) -> Vec<(u16, ChooserRow)>` are used identically across app.rs (def), ui.rs, and run.rs. `col_to_split_pct`/`split_pct`/`widen_split`/`narrow_split` match between app.rs and run.rs/ui.rs.
- **Build-green ordering:** Task 1 is self-contained and green. Task 2 reshapes the `Popup::Chooser` variant and updates all three consumers in one commit (green at its end). Task 3 is docs.