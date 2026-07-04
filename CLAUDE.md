# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Style guide

**Always follow the Rust style guide in [`docs/policy/rust.md`](docs/policy/rust.md)** for every code change: naming, doc comments, error handling, testing, and the pre-commit checklist (`cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`) defined there.

## Commands

- Build: `cargo build` (release: `cargo build --release`)
- Run: `cargo run` — **must NOT be run inside tmux** (`main.rs` exits if `$TMUX` is set, since tmux is reserved for the inner task sessions). The tree root is the current working directory.
- Test all: `cargo test`
- Single test: `cargo test <name>` (e.g. `cargo test chooser_create_makes_shell`)
- Tests for one module: `cargo test --lib tmux::session::` etc.

There is no separate lint config; use `cargo clippy` and `cargo fmt`, and follow `docs/policy/rust.md` (see above).

## Architecture

A ratatui/crossterm TUI with a NERDTree-style file tree (left) and a right pane that is **either** an embedded terminal **or** a read-only file viewer. The non-obvious design is how the terminal pane works:

- `runner-manager` runs on the alternate screen *outside* tmux. Per-project state lives in a **config directory** `<root>/.pjma/` (`project/config.rs`, name `DIR_NAME`), created at startup in `run/mod.rs`; it holds the tmux socket and the saved tree state. All tmux interaction uses a **project-local socket file** `<root>/.pjma/pjma.sock` (passed as `tmux -S <root>/.pjma/pjma.sock`), not a shared named `-L` socket, so each project's sessions are isolated. `RM_SOCKET` overrides the path. At startup (`run/mod.rs`) it queries that socket for the most recently active session (`Tmux::latest_session`) and, if one exists, spawns one PTY (`term/pty.rs`) attached to it (`tmux -S <root>/.pjma/pjma.sock new-session -A -s <latest>`) — recovery into whatever the user was last working in. If there are **no** sessions (fresh start), no PTY is spawned: the right pane shows a hint and the `EmbeddedTerm` stays detached until the first session is created, which the run loop then attaches to via `pending_respawn`. There is no dedicated "scratch" session. That single embedded tmux **client** is what the right pane renders, via `tui-term`'s vt100 parser.
- Task sessions are **separate** detached tmux sessions on the same `<root>/.pjma/pjma.sock` socket. Selecting a session row does **not** spawn a new pane — it calls `tmux switch-client -c <host_tty> -t =<slug>` (the `=` forces an exact session-name match — bare targets prefix-match, which is dangerous with sibling slugs like `src-shell` / `src-shell-2`) so the one embedded client *switches* to display that session. `host_tty` is discovered via `tmux list-clients` (`Tmux::host_tty`); without it, switching can't happen.
- `set -g detach-on-destroy off` is set globally so the embedded client survives when a task session's shell exits.

### Module layout

Six top-level modules, grouped by architectural role (bottom-up):

- `project/` — the project directory on disk: persisted state (`config.rs`, `lock.rs`), the lazy file tree (`tree.rs`), git-status colouring (`git.rs`), and Claude history discovery (`claude.rs`).
- `tmux/` — tmux server interaction (`mod.rs`) and in-memory session bookkeeping (`session.rs`).
- `term/` — the embedded terminal: the PTY running the tmux client (`pty.rs`), keystroke encoding (`keys.rs`), and the `EmbeddedTerm` unit owning the PTY + parser + size across its lifecycle (`embedded.rs`).
- `app/` — all state and actions (`mod.rs`), plus the chooser form (`chooser.rs`), the derived row list (`rows.rs`), and the file viewer (`viewer.rs`).
- `ui/` — pure rendering + hit-testing (`mod.rs`, `popups.rs`).
- `run/` — the composition root: real-terminal setup/teardown and the event loop (`mod.rs`), plus the pure input-routing layer (`input.rs`).

### Layers

- `tmux/mod.rs` — all tmux interaction goes through the `CommandRunner` trait (`SystemRunner` in prod, `MockRunner` in tests). `Tmux<R>` prefixes every call with `-S <socket-path>`. Everything downstream is generic over `R: CommandRunner`, which is what makes `App` unit-testable without a real tmux.
- `tmux/session.rs` — `SessionStore` is the in-memory source of truth for which sessions exist. `create()` generates unique slugs (`<dirslug>-<kind>[-N]`); `slugify()` maps a path relative to root into a tmux-safe name. `by_dir()` groups sessions under their directory with display labels. `sync(live)` reconciles the store against the live set from `tmux list-sessions` — the run loop calls `App::sync` ~once/second, which prunes rows for sessions whose shell exited.
- `project/claude.rs` — discovery of resumable Claude Code sessions. Claude keeps one JSONL transcript per session under `~/.claude/projects/<encoded cwd>/<uuid>.jsonl`; those files *are* the record of resume ids, so there's nothing extra to persist. `encode_project_dir()` reproduces Claude's folder naming (every non-ASCII-alphanumeric character → `-`), `projects_base()` resolves `~/.claude/projects` (overridable via `RM_CLAUDE_PROJECTS`), and `list_sessions(base, dir)` returns the newest few transcripts as `ResumeSession { id, last_command, modified }`, parsing each transcript's last genuine user prompt. The `id` is a `ResumeId` — a newtype that is **shell-safe by construction** (its only constructor rejects anything but ASCII alphanumerics and `-`), because it is later spliced verbatim into the `claude --resume <id>` command that tmux hands to a shell. This is the **only** module that uses `serde_json` (to read Claude's own format); our own state files stay serde-free.
- `project/config.rs` — the per-project config dir `<root>/.pjma/` (`Config`). `ensure_dir()` creates it at startup; `save_expanded`/`load_expanded` persist the expanded-directory set to `<root>/.pjma/expanded` as root-relative paths (one per line, no serde dependency). `RM_SOCKET` aside, the tmux socket also lives in this dir.
- `project/tree.rs` — lazy filesystem tree; `Node::load_children` reads a dir on first expand, dirs sorted before files. `expanded_dirs()` collects the visible expanded subtree (for persistence) and `apply_expanded()` re-expands a saved set shallow-to-deep, loading children lazily.
- `project/git.rs` — the `git status` scan (`GitStatuses::load`) that colours tree rows: parses porcelain `-z` output per repo (the root's containing repo plus nested repos), staged → green, dirty/untracked → red, ignored → grey. Always run on a worker thread (see `run/mod.rs`).
- `project/lock.rs` — the single-instance guard: an advisory `flock` on `<root>/.pjma/pjma.lock` held for the process lifetime, so two runner-managers can never share one socket and fight over the embedded client.
- `term/pty.rs` / `term/keys.rs` / `term/embedded.rs` — the embedded terminal: one PTY running the tmux client with a reader thread feeding a shared vt100 parser (dropping the `Pty` kills the child), the crossterm→bytes key/wheel encoding the run loop writes to it, and `EmbeddedTerm` — the single owner of PTY + parser + last-pushed size, whose `spawn_attached`/`respawn_if_dead`/`resize_to`/`write_input` methods are the only way the run loop touches the terminal (a respawn request while the client is still alive is deliberately dropped; the next `sync` reconciles).
- `app/rows.rs` — flattens the tree + per-dir sessions into a single `Vec<Row>` (the visible list). `RowKind` is `Dir | Session | File`; `selected` indexes into this vec. **Rebuild rows (`App::rebuild_rows`) after any tree expand/collapse or session change** — the row vec is derived state.
- `app/mod.rs` — `App<R>` holds all state and the action methods (`activate`, `switch_to`, `sync`, split sizing). `restore_expanded()` (called once at startup) re-expands the saved dirs; `persist_expanded()` is called after every dir toggle in `activate` to save the new state. No I/O event handling here. The chooser lives in the `app/chooser.rs` submodule: `Popup::Chooser(ChooserForm)` carries **all** form state — selections, focus, and the resume list discovered by `open_chooser` (via `claude::list_sessions`) — so nothing chooser-related outlives the popup. `ChooserForm` is a small radio-form state machine: `rows()` derives the visible focusable rows (the claude permission rows only appear when kind == Claude; a `Resume` group — `ResumeNew` + one `Resume(i)` per discovered session — appears when kind == Claude and `resumes` is non-empty), selection follows focus, and pure navigation (`group_move`/`option_move`/`select`) needs no `App`. The free fn `launch_command(kind, perm, resume_id)` builds the launch command per kind: shell → none (default shell), claude → `claude [--resume <id>] [--dangerously-skip-permissions]`, codex → `codex` (the perm/resume inputs are claude-only and ignored). Shared unit-test helpers (a tempdir `App<MockRunner>`, the canned create-session response sequence) live in `app/testutil.rs`.
- `run/mod.rs` — owns the terminal setup/teardown, the PTY, and the event loop: draws via `ui::render`, drives PTY resize from the rendered terminal area, and runs the periodic sync. All key/mouse dispatch lives in `run/input.rs`: `Router::route_key`/`route_mouse` mutate `App` directly (dispatching on `app.popup` and `app.focus`, resolving mouse events against the last frame's `Geometry`) and return the few `Action`s only the loop can perform — `Quit`, `WriteToPty` (keystrokes/wheel encoded via `term/keys.rs`), and `SpawnGitScan`. This keeps the entire keymap unit-testable over `App<MockRunner>` — add routing tests there, not in `mod.rs`. **Git colouring is computed off the UI thread**: `spawn_git_scan` runs `GitStatuses::load` on a worker thread and the loop applies each result via `App::apply_git` (one scan in flight at a time, re-triggered ~1 s after the previous finished). A full `git status` of a large tree takes seconds, so it must never run inline — neither `App::new` nor `App::sync` touch git.
- `ui/mod.rs` — pure rendering + hit-testing. `render` returns a `Layout` (tree list geometry, split column, terminal rect) that `run/input.rs` uses to resolve mouse clicks (`resolve_pane_click` → tree row / `[+]` button / right pane) and to detect splitter drags. Popup rendering (help overlay, chooser form, close confirmation) lives in the `ui/popups.rs` submodule; each popup renderer returns its clickable geometry, re-exported through `ui`.
- `app/viewer.rs` — when `app.viewer` is `Some`, the right pane shows a file (capped at 5000 lines, binary-safe) and the PTY parser is *not* read that frame.

### Conventions

- The split between panes is a percent (`split_pct`, 15–80) adjusted by `<`/`>` keys or by dragging the border; `col_to_split_pct` clamps and is divide-by-zero safe.
- When adding tmux behavior, add the method on `Tmux<R>` and assert the exact arg vector in a `MockRunner` test (see the `tmux/mod.rs` tests for the pattern). When adding app behavior, drive it through `App<MockRunner>` and push canned responses in the order the code issues them (new-session → list-clients → switch-client is the common sequence).

## Reference

Design specs and implementation plans live in `docs/superpowers/specs/` and `docs/superpowers/plans/` (v1–v4), useful for the rationale behind each iteration. `README.md` documents the end-user keybindings.
