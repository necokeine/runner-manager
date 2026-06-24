# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

- Build: `cargo build` (release: `cargo build --release`)
- Run: `cargo run` — **must NOT be run inside tmux** (`main.rs` exits if `$TMUX` is set, since tmux is reserved for the inner task sessions). The tree root is the current working directory.
- Test all: `cargo test`
- Single test: `cargo test <name>` (e.g. `cargo test chooser_create_makes_shell`)
- Tests for one module: `cargo test --lib session::` etc.

There is no separate lint config; use `cargo clippy` and `cargo fmt`.

## Architecture

A ratatui/crossterm TUI with a NERDTree-style file tree (left) and a right pane that is **either** an embedded terminal **or** a read-only file viewer. The non-obvious design is how the terminal pane works:

- `runner-manager` runs on the alternate screen *outside* tmux. Per-project state lives in a **config directory** `<root>/.pjma/` (`config.rs`, name `DIR_NAME`), created at startup in `run.rs`; it holds the tmux socket and the saved tree state. All tmux interaction uses a **project-local socket file** `<root>/.pjma/pjma.sock` (passed as `tmux -S <root>/.pjma/pjma.sock`), not a shared named `-L` socket, so each project's sessions are isolated. `RM_SOCKET` overrides the path. At startup (`run.rs`) it queries that socket for the most recently active session (`Tmux::latest_session`) and, if one exists, spawns one PTY (`pty.rs`) attached to it (`tmux -S <root>/.pjma/pjma.sock new-session -A -s <latest>`) — recovery into whatever the user was last working in. If there are **no** sessions (fresh start), no PTY is spawned: the right pane shows a hint and `pty`/`parser` stay `None` until the first session is created, which the run loop then attaches to via `pending_respawn`. There is no dedicated "scratch" session. That single embedded tmux **client** is what the right pane renders, via `tui-term`'s vt100 parser.
- Task sessions are **separate** detached tmux sessions on the same `<root>/.pjma/pjma.sock` socket. Selecting a session row does **not** spawn a new pane — it calls `tmux switch-client -c <host_tty> -t <slug>` so the one embedded client *switches* to display that session. `host_tty` is discovered via `tmux list-clients` (`Tmux::host_tty`); without it, switching can't happen.
- `set -g detach-on-destroy off` is set globally so the embedded client survives when a task session's shell exits.

### Layers

- `tmux.rs` — all tmux interaction goes through the `CommandRunner` trait (`SystemRunner` in prod, `MockRunner` in tests). `Tmux<R>` prefixes every call with `-S <socket-path>`. Everything downstream is generic over `R: CommandRunner`, which is what makes `App` unit-testable without a real tmux.
- `session.rs` — `SessionStore` is the in-memory source of truth for which sessions exist. `create()` generates unique slugs (`<dirslug>-<kind>[-N]`); `slugify()` maps a path relative to root into a tmux-safe name. `by_dir()` groups sessions under their directory with display labels. `sync(live)` reconciles the store against the live set from `tmux list-sessions` — the run loop calls `App::sync` ~once/second, which prunes rows for sessions whose shell exited.
- `config.rs` — the per-project config dir `<root>/.pjma/` (`Config`). `ensure_dir()` creates it at startup; `save_expanded`/`load_expanded` persist the expanded-directory set to `<root>/.pjma/expanded` as root-relative paths (one per line, no serde dependency). `RM_SOCKET` aside, the tmux socket also lives in this dir.
- `tree.rs` — lazy filesystem tree; `Node::load_children` reads a dir on first expand, dirs sorted before files. `expanded_dirs()` collects the visible expanded subtree (for persistence) and `apply_expanded()` re-expands a saved set shallow-to-deep, loading children lazily.
- `rows.rs` — flattens the tree + per-dir sessions into a single `Vec<Row>` (the visible list). `RowKind` is `Dir | Session | File`; `selected` indexes into this vec. **Rebuild rows (`App::rebuild_rows`) after any tree expand/collapse or session change** — the row vec is derived state.
- `app.rs` — `App<R>` holds all state and the action methods (`activate`, `open_chooser`, chooser state machine, `switch_to`, `sync`, split sizing). `restore_expanded()` (called once at startup) re-expands the saved dirs; `persist_expanded()` is called after every dir toggle in `activate` to save the new state. No I/O event handling here. The chooser (`Popup::Chooser`) is a small radio-form state machine: `chooser_rows()` derives the visible focusable rows (the claude permission rows only appear when kind == Claude), and selection follows focus.
- `run.rs` — owns the terminal setup/teardown, the PTY, and the event loop: draws via `ui::render`, routes key/mouse events depending on `app.popup` and `app.focus`, drives PTY resize from the rendered terminal area, and runs the periodic sync. Keystrokes to the terminal pane are translated by `keys.rs::encode_key` and written to the PTY.
- `ui.rs` — pure rendering + hit-testing. `render` returns a `Layout` (tree list geometry, split column, terminal rect) that `run.rs` uses to resolve mouse clicks (`resolve_pane_click` → tree row / `[+]` button / right pane) and to detect splitter drags.
- `viewer.rs` — when `app.viewer` is `Some`, the right pane shows a file (capped at 5000 lines, binary-safe) and the PTY parser is *not* read that frame.

### Conventions

- The split between panes is a percent (`split_pct`, 15–80) adjusted by `<`/`>` keys or by dragging the border; `col_to_split_pct` clamps and is divide-by-zero safe.
- When adding tmux behavior, add the method on `Tmux<R>` and assert the exact arg vector in a `MockRunner` test (see `tmux.rs` tests for the pattern). When adding app behavior, drive it through `App<MockRunner>` and push canned responses in the order the code issues them (new-session → list-clients → switch-client is the common sequence).

## Reference

Design specs and implementation plans live in `docs/superpowers/specs/` and `docs/superpowers/plans/` (v1–v4), useful for the rationale behind each iteration. `README.md` documents the end-user keybindings.
