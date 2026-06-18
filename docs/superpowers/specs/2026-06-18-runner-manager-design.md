# runner-manager — Design Spec

**Date:** 2026-06-18
**Status:** Approved for planning

## 1. Concept

`runner-manager` is a Rust terminal UI that pairs a NERDTree-style **file tree** (left)
with a live **tmux session** (right). Each directory maps to its own tmux session rooted
at that directory. Selecting a directory creates-or-switches to its session while the tree
stays pinned on the left. Files open in `$EDITOR` inside their directory's session.

## 2. Architecture — nested tmux

```
┌─ outer tmux window (default tmux server) ───────────────────────┐
│  ┌─ left pane ────────────┐  ┌─ right "host" pane ────────────┐  │
│  │ runner-manager TUI     │  │ inner tmux client:             │  │
│  │ (ratatui file tree)    │  │   tmux -L runner attach         │  │
│  │  src/   [+] ●          │  │  ── shows the selected dir's   │  │
│  │  tests/ [+]            │  │     session, swapped in place  │  │
│  │  README.md            │  │                                │  │
│  └────────────────────────┘  └────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
        controls via `tmux -L runner …`  ─────────►  inner server (socket: runner)
                                                     ├─ session: src     (-c …/src)
                                                     ├─ session: tests   (-c …/tests)
                                                     └─ session: scratch (default placeholder)
```

- **Outer tmux** (the user's normal/default server) provides only the 2-pane split.
  Left pane runs the TUI; right "host" pane runs a client attached to the inner server.
- **Inner tmux server** uses a dedicated socket via `tmux -L runner`, with its own prefix
  (default `C-a`) so it never collides with the user's normal tmux (`C-b`). It holds one
  session per directory.
- The TUI **only renders into the left pane**. The right side is genuinely tmux, so we
  never reimplement a terminal emulator. The TUI drives the inner server by shelling out
  to `tmux -L runner …`.

### Rationale for nesting

A tmux pane can only display panes belonging to one window/session at a time, so a fixed
"tree on the left + arbitrary other session on the right" layout is impossible with a single
tmux server. Hosting an inner tmux client inside the right pane lets that client switch which
inner session it displays, achieving a pinned tree + swappable session view. Using a separate
socket isolates the runner sessions from the user's main tmux and avoids double-prefix
confusion.

## 3. Decisions (locked)

| Topic | Decision |
|-------|----------|
| Language / TUI | Rust + ratatui + crossterm |
| tmux relationship | Orchestrate real tmux (drive via CLI) |
| Right-pane model | Persistent split, tree always visible |
| Per-directory unit | A tmux **session** (create-or-switch) on inner socket `runner` |
| Tree contents | Directories **and** files |
| File action | Open in `$EDITOR` within the file's directory session |
| Input | Keyboard-first **and** mouse (clickable `[+]` button) |
| Tree root scope | Locked to the cwd subtree (cannot navigate above cwd) |
| On TUI quit | Leave inner sessions running (durable workspaces) |

## 4. Core interactions

- **Navigate:** `j`/`k`/arrows and mouse click to select a row; `Enter` or click on a
  directory toggles expand/collapse.
- **Open terminal for a directory** (the "+" action), via a dedicated key (`a`) or by
  clicking the `[+]` button rendered beside the directory row:
  - `slug = sanitize(path)`.
  - If `tmux -L runner has-session -t slug` succeeds → switch the host client to it.
  - Otherwise `tmux -L runner new-session -d -s slug -c <dir>`, then switch.
  - "switch" = `tmux -L runner switch-client -c <host-tty> -t slug`, where `<host-tty>`
    is obtained from `tmux -L runner list-clients -F '#{client_tty}'` (first client).
- **Open a file** (`Enter` or click on a file row):
  - Ensure the parent directory's session exists (create if needed).
  - `tmux -L runner send-keys -t slug "$EDITOR -- <file>" Enter`.
  - Switch the host client to that session.
- **Kill a session:** key `x` on a directory that currently has a session
  (`tmux -L runner kill-session -t slug`).
- **Badges:** directories with a live session show a `●` marker.
- **Quit:** `q`. Inner sessions are left running.

## 5. Modules

- `main.rs` — entry + subcommand dispatch:
  - bare `runner-manager` → **bootstrap** (set up outer split + inner scratch + attach).
  - `runner-manager tui` → run the ratatui app (assumes it is the left pane).
- `bootstrap.rs` — create the outer 2-pane layout; configure the inner server socket and
  prefix; ensure a `scratch` inner session exists so the host pane always has something to
  attach to; attach the user.
- `tmux.rs` — typed wrapper over `tmux -L runner` operations (`has_session`, `new_session`,
  `switch_client`, `list_sessions`, `list_clients`, `send_keys`, `kill_session`) behind a
  `CommandRunner` trait so it is mockable in tests.
- `tree.rs` — file-tree model: lazy directory reads, expand/collapse, rooted at cwd,
  ordering (dirs first, then files, case-insensitive). Pure and unit-testable.
- `session.rs` — slug derivation and path↔slug registry; sync badges from `list-sessions`.
- `app.rs` — application state and event loop tying input → tmux actions.
- `ui.rs` — ratatui rendering of the tree, `[+]` button hit-targets, and a status/help line.
- `input.rs` — key and mouse event mapping (click coordinates → tree row / button region).

## 6. Data flow on startup (`tui` mode)

1. Read cwd → build the root node (children loaded lazily on expand).
2. Query `tmux -L runner list-sessions` → mark directories that already have sessions.
3. Cache the host client tty (re-queried before each switch for robustness).
4. Enter the event loop.

## 7. Slug derivation

- Derived from the directory path relative to cwd.
- tmux session names cannot contain `.` or `:`; these and other unsafe characters are
  replaced (e.g. with `_`). Empty/root maps to a stable name (e.g. `root`).
- The registry guarantees uniqueness; on collision, append a disambiguating suffix.
- Edge cases covered by tests: spaces, dots, leading dots, unicode, deeply nested paths,
  paths that sanitize to the same slug.

## 8. Error handling

- `tmux` not installed → bootstrap fails with a clear, actionable message.
- The inner server auto-starts on first `tmux -L runner` command (standard tmux behavior).
- Unreadable directory → inline error indicator; skip its children.
- `$EDITOR` unset → fall back to `vi`.
- Switch/create/kill failures surface on the status line; the event loop never panics.

## 9. Persistence

Inner sessions persist across TUI restarts because the inner tmux server keeps running.
On relaunch, badges re-sync from `list-sessions`. Quitting the TUI leaves runner sessions
alive.

## 10. Testing (TDD)

- `tree.rs` — pure logic over temporary directories: expand/collapse, lazy load, ordering.
- `session.rs` — slug derivation edge cases and registry uniqueness.
- `tmux.rs` — unit tests via a mocked `CommandRunner`; optional integration tests against a
  real `tmux -L runner-test` socket guarded on tmux being present.
- `app.rs`/`input.rs` — input-to-action mapping tested with a mocked tmux layer.

## 11. Out of scope (v1, YAGNI)

- Navigating the tree root above cwd.
- Tiled/multi-pane simultaneous viewing of several sessions.
- Embedding a self-rendered terminal emulator (PTY) instead of tmux.
- Configurable layouts, themes, or a config file (sensible defaults only).
- File operations (create/rename/delete) in the tree.
