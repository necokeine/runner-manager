# runner-manager v4 — Enriched chooser form + adjustable splitter

**Date:** 2026-06-18
**Status:** Approved for planning
**Builds on:** v3 (`2026-06-18-runner-manager-v3-multisession-viewer-design.md`). Two independent UI enhancements bundled in one round.

## 1. Summary

1. **Enriched "new session" chooser.** The chooser popup becomes a small form: a kind radio (`shell` / `claude`); when `claude` is selected, a permission radio (`normal` / `skip`) appears; and a `[ Cancel ]` / `[ Create ]` button bar. `skip` maps to `claude --dangerously-skip-permissions`.
2. **Adjustable tree/terminal splitter.** The fixed 35/65 split becomes resizable via keyboard (`<`/`>` in tree focus) and mouse-drag on the border, persisting for the run with min clamps.

## 2. Chooser form

### State
`Popup::Chooser` carries:
- `dir: PathBuf` — the target directory.
- `kind: SessionKind` (`Shell` | `Claude`), default `Shell`.
- `perm: ClaudePerm` (`Normal` | `Skip`), default `Normal` — only meaningful when `kind == Claude`.
- `focus: usize` — index into the form's currently-focusable rows.

`ClaudePerm` is a new enum in `session.rs`.

### Focusable rows (top to bottom)
- Always: `shell` (row), `claude` (row).
- When `kind == Claude`: `normal` (row), `skip` (row).
- Always last: `Cancel` (button), `Create` (button).

So the focus order is `[shell, claude, Cancel, Create]` when shell is selected, and `[shell, claude, normal, skip, Cancel, Create]` when claude is selected.

### Interaction
- `↑`/`↓` (or `k`/`j`) move `focus` through the visible rows (clamped; no wrap).
- **Radios follow focus:** moving focus onto `shell`/`claude` sets `kind` to it (selecting `claude` reveals the perm rows and keeps `perm` at its current value; selecting `shell` hides them). Moving focus onto `normal`/`skip` sets `perm`.
- **Buttons:** `Enter` or `Space` on `Create` creates the session and switches the right pane to it; on `Cancel` closes with no action. `Esc` cancels from anywhere.
- **Mouse:** a left-click on a radio row selects it (and moves focus there); a click on `[ Cancel ]` cancels; a click on `[ Create ]` creates. A click outside the popup cancels.

### Command mapping (at Create)
- `shell` → no command (default `$SHELL`).
- `claude` + `normal` → `claude`.
- `claude` + `skip` → `claude --dangerously-skip-permissions`.

The command string is computed at confirm time and passed to `tmux.new_session(slug, dir, Some(cmd))` (v3 already supports the optional command). The session **label/slug stay keyed by kind** (`shell`/`claude`), unchanged from v3 — permission choice does not change the label.

## 3. Adjustable splitter

### State
`App` gains `split_pct: u16` — the tree pane width as a percent of the full area width, default `35`, clamped to `[15, 80]`.

### Rendering
`ui::render` uses `Constraint::Percentage(split_pct)` for the tree pane and `Percentage(100 - split_pct)` for the right pane (replacing the hard-coded 35/65). The returned `Layout.split_col` (= right pane's `x`) continues to drive click resolution and is the draggable border column.

### Keyboard (tree focus, no popup)
- `<` → `narrow_split()` (tree narrower, `split_pct -= 5`, clamped).
- `>` → `widen_split()` (tree wider, `split_pct += 5`, clamped).

Handled only in tree focus so they never reach the PTY.

### Mouse drag
`run.rs` tracks `dragging_split: bool`.
- On `MouseEventKind::Down(Left)` whose column is within `split_col ± 1`: set `dragging_split = true` (do not treat as a pane/row click).
- On `MouseEventKind::Drag(Left)` while `dragging_split`: set `split_pct` from the cursor column as `round(col * 100 / area_width)`, clamped to `[15, 80]`.
- On `MouseEventKind::Up(Left)`: set `dragging_split = false`.
- A `Down(Left)` that is **not** on the border behaves exactly as in v3 (focus pane / act on row).

### Window resize
Because the split is a percent, it stays proportional across terminal resizes; the clamp keeps both panes usable at small widths.

## 4. Components / files

- `session.rs` — add `ClaudePerm { Normal, Skip }` (with a helper to build the claude command suffix, or compute in app).
- `app.rs` — `Popup::Chooser` carries `kind`/`perm`/`focus`; `chooser_move(delta)`, `chooser_click(row)`, `chooser_confirm()`, `chooser_cancel()` updated for the form; `create_session(dir, kind, command: Option<String>)`; `split_pct` field + `widen_split()`/`narrow_split()`.
- `ui.rs` — `render_chooser` draws the form (kind radios, conditional perm radios, button bar) and returns the clickable regions/geometry needed for mouse routing; layout constraints use `split_pct`.
- `run.rs` — chooser key/mouse routing for the form; `<`/`>` split keys in tree focus; border-drag tracking in the mouse handler.

## 5. Data flow

- **Chooser open** (`a`/`[+]` on a Dir): `Popup::Chooser { dir, kind: Shell, perm: Normal, focus: 0 }`.
- **Navigate:** `chooser_move(±1)` clamps `focus` and sets `kind`/`perm` when focus lands on a radio row.
- **Create:** compute command from `kind`+`perm` → `create_session(dir, kind, cmd)` → `store.create` → `new_session` → `rebuild_rows` → `switch_to`; close popup.
- **Resize (keyboard):** `<`/`>` adjust `split_pct`; next render reflows.
- **Resize (drag):** border `Down` arms `dragging_split`; `Drag` updates `split_pct`; `Up` disarms.

## 6. Error handling

- Same as v3: session-create failures surface on the status line; the loop never panics.
- Split clamp guarantees `15 ≤ split_pct ≤ 80`, so neither pane can vanish; drag math guards against zero-width areas (no divide-by-zero).
- Chooser `focus` is always clamped to the visible-row count, which changes when `kind` toggles (selecting `shell` while focus was on a perm/button row re-clamps).

## 7. Testing

- **Unit-tested (pure):** chooser state machine — `chooser_move` clamping, radios-follow-focus (focus on `claude` sets kind and exposes perm rows; focus on `skip` sets perm), focus re-clamp when switching `claude`→`shell`, and `chooser_confirm`/command computation (`None` / `claude` / `claude --dangerously-skip-permissions`); splitter `widen_split`/`narrow_split` clamping at both bounds; drag column→percent conversion (a pure helper, clamped, divide-by-zero-safe).
- **Manual verification:** chooser rendering + mouse hit-regions, `<`/`>` resize, and border drag — exercised by running the app. A small TestBackend smoke test confirms the chooser draws the radios and both buttons.

## 8. Out of scope (v4, YAGNI)

- Persisting `split_pct` (or chooser defaults) to disk.
- Distinguishing skip vs normal in the session row label.
- Additional claude flags beyond `--dangerously-skip-permissions`.
- Multiple/nested splits or a horizontal split.
- Free-text custom command in the chooser.
