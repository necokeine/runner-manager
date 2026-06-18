# runner-manager v2 — Embedded Terminal Design Spec

**Date:** 2026-06-18
**Status:** Approved for planning
**Supersedes:** the outer-tmux split from `2026-06-18-runner-manager-design.md` (v1). The tree model, slug/registry, and tmux command layer carry over unchanged.

## 1. Why v2

Two new constraints invalidate v1's architecture:

1. **runner-manager must not run inside tmux.** tmux is only for the inner per-directory tasks.
2. **A mouse click switches focus between the tree and the terminal.**

v1 relied on an *outer* tmux to provide the two-pane split (TUI in the left pane, an inner tmux client in the right pane). With no outer tmux, the runner-manager process must render **both** panes itself. The right pane therefore becomes a **real embedded terminal**: a PTY whose output is parsed and drawn inside the TUI. tmux remains, but only as the inner per-directory sessions the embedded terminal attaches to.

## 2. Architecture

```
your terminal (you ran `runner-manager` here; $TMUX must be unset)
└─ runner-manager — full-window ratatui TUI on the alternate screen
   ├─ left pane:  file tree              (drawn by ratatui)
   └─ right pane: embedded terminal      (drawn by tui-term from a vt100 grid)
                  └─ PTY runs: tmux -L runner new-session -A -s scratch
                              → switched per directory via switch-client
                                         │
                                         ▼
                          inner tmux server (socket: runner)
                          ├─ scratch (initial placeholder)
                          ├─ src   (-c …/src)
                          └─ tests (-c …/tests)
```

- The TUI takes over the current terminal (alternate screen) and draws a **fixed** two-pane layout. No window is spawned; nothing is nested above runner-manager.
- The **only** PTY the app creates is the right-pane terminal. `portable-pty` spawns it running `tmux -L runner new-session -A -s scratch` (a real PTY, so attach-or-create works). A background reader thread reads PTY bytes into a `vt100::Parser` held behind a shared lock; `tui-term`'s `PseudoTerminal` widget renders that screen into the right pane. The main loop redraws on a short tick and on input.
- **Session switching reuses v1 logic.** The embedded PTY is the only client on the `runner` socket, so `host_tty()` (`list-clients -F '#{client_tty}'`) returns its tty, and `switch-client -c <pty-tty> -t <slug>` swaps the displayed session. Selecting a directory creates-or-switches its session exactly as in v1 — only the "host" is now the embedded PTY instead of an outer pane.
- **No outer tmux.** The outer-layout bootstrap is removed; `main` runs the TUI directly.

## 3. Decisions (locked)

| Topic | Decision |
|-------|----------|
| Outer tmux | Removed. App runs directly in the user's terminal. |
| Right pane | Real embedded terminal: `portable-pty` + `vt100` + `tui-term`. |
| Embedded PTY command | `tmux -L runner new-session -A -s scratch` (attach-or-create). |
| Inner tmux prefix | Left at default (`C-b`); no override (no outer tmux to collide with). |
| Run-inside-tmux | Refused: if `$TMUX` is set, exit with a clear message. |
| Focus model | `Focus { Tree, Terminal }`; purely visual highlight + input routing. |
| Focus switch | `Ctrl-q` toggles; left-pane click → Tree; right-pane click → Terminal. |
| Layout | **Fixed.** Tree pane is always rendered; focus never resizes or hides a pane. |
| Quit | `q` while focus = Tree. `q` while focus = Terminal goes to the PTY. |
| Terminal-pane mouse | v1: click sets focus only; mouse events are **not** forwarded to inner programs (keyboard-only interaction inside the terminal). Deferred. |
| New deps | `portable-pty`, `tui-term`, `vt100`. |

## 4. Focus & input routing

- State: `Focus { Tree, Terminal }`, starting at `Tree`.
- **Mouse:** a left-click whose column is in the tree region sets `Focus::Tree` and performs the tree action (select row / activate / `[+]`); a left-click in the terminal region sets `Focus::Terminal`. Region is decided by the pane split column.
- **Keyboard, intercepted regardless of focus:** `Ctrl-q` toggles `Focus`.
- **Focus = Tree:** keys drive the tree — `j`/`Down`, `k`/`Up`, `Enter` (toggle dir / open file), `a` (open/switch session), `x` (kill session), `q` (quit).
- **Focus = Terminal:** every key *except* `Ctrl-q` is encoded to its terminal byte sequence and written to the PTY master. `q` is forwarded (does not quit).
- The focused pane is shown with a highlighted border; the layout is otherwise constant.

## 5. Key encoding (PTY input)

A dedicated, unit-tested function maps a crossterm `KeyEvent` to the bytes written to the PTY:

- Printable `Char(c)` with no/Shift modifier → the character's UTF-8 bytes.
- `Char(c)` with `CONTROL` → the control byte: `Ctrl-a`=0x01 … `Ctrl-z`=0x1a (i.e. `(c.to_ascii_lowercase() as u8) & 0x1f`); `Ctrl-q` is never reached here (intercepted as focus toggle).
- `Enter` → `\r` (0x0d); `Tab` → `\t` (0x09); `Backspace` → 0x7f; `Esc` → 0x1b.
- `Up`/`Down`/`Right`/`Left` → `\x1b[A`/`B`/`C`/`D`; `Home` → `\x1b[H`; `End` → `\x1b[F`; `Delete` → `\x1b[3~`; `PageUp` → `\x1b[5~`; `PageDown` → `\x1b[6~`.
- Unmapped keys → no bytes (ignored).

## 6. Components / files

- **Reused unchanged:** `tmux.rs` (CommandRunner / `Tmux<R>` / mock), `session.rs` (slug + registry), `tree.rs` (lazy tree).
- **`app.rs` (changed):** add `focus: Focus`; add focus toggling and click routing helpers; keep `open_dir`/`open_file`/`ensure_session`/`ensure_host_tty`/`kill_selected`/`sync_active` (the switch path now targets the PTY client, transparently). The app does not own the PTY — it only issues tmux commands.
- **`pty.rs` (new):** owns the embedded terminal. Spawns the PTY (`portable-pty`), holds the `vt100` parser behind a shared lock, runs the reader thread, exposes `screen()` for rendering, `write_input(&[u8])` to the master, and `resize(rows, cols)`. Isolated so the event loop and rendering depend only on its interface.
- **`keys.rs` (new):** `encode_key(KeyEvent) -> Vec<u8>` per §5. Pure, fully unit-tested.
- **`ui.rs` (changed):** render the fixed two-pane split — tree (left) via the existing list rendering, terminal (right) via `tui_term::PseudoTerminal` over `pty.screen()`. Highlight the focused pane's border. Click resolution returns which pane was hit and, for the tree, the row/`[+]` (extends v1 `resolve_click` with the pane-split column).
- **`input.rs` (changed):** `Ctrl-q` → focus toggle; otherwise route by focus (tree action vs. `encode_key` → PTY).
- **`run.rs` (changed):** build `App` + `Pty`; enable raw mode / alternate screen / mouse capture; event loop with a redraw tick (e.g. ~33 ms) plus event-driven redraws; on resize, recompute layout and call `pty.resize`; teardown restores the terminal and drops the PTY.
- **`main.rs` (changed):** if `$TMUX` is set, print a clear error and exit non-zero; otherwise run the TUI directly (cwd as root, `$EDITOR` or `vi`, socket `runner`). Takes no subcommand.
- **`bootstrap.rs` and `cli.rs` (removed):** delete `outer_layout_commands`, the outer-session guard, `run_bootstrap`, and the `Mode`/`parse_mode` dispatch. The PTY's `new-session -A` starts the inner server, so there is no separate bootstrap step and no bootstrap-vs-tui mode to choose — `main` always runs the TUI. (The `session_exists` helper and `TmuxCmd` go away with `bootstrap.rs`; per-session checks already live in `Tmux::has_session`.)

## 7. Data flow

1. **Startup:** `main` checks `$TMUX` is unset → builds `App` (tree from cwd) and `Pty` (spawns `tmux -L runner new-session -A -s scratch`) → enables raw mode, alternate screen, mouse capture → enters the loop with `Focus::Tree`.
2. **Render tick:** draw tree (left) and `pty.screen()` (right); highlight the focused border.
3. **Tree focus, select a dir:** `ensure_session` (`has-session` → `new-session` if absent) → `host_tty()` (the PTY client) → `switch-client` → the embedded terminal now shows that session.
4. **Tree focus, select a file:** ensure parent dir session → `send-keys "<editor> -- '<quoted path>'"` → `switch-client`.
5. **Terminal focus:** each key (except `Ctrl-q`) → `encode_key` → `pty.write_input`.
6. **Resize:** recompute the split; `pty.resize(rows, cols)` for the right pane's inner area so tmux reflows.
7. **Quit (`q` in Tree focus):** restore terminal, drop the PTY (its tmux client detaches); inner sessions persist on the `runner` server.

## 8. Error handling

- `$TMUX` set → exit non-zero with: "runner-manager must not be run inside tmux; tmux is used for the inner task sessions."
- tmux missing → the PTY command fails; surface a clear startup error and exit cleanly (restore the terminal first).
- PTY spawn failure → clean teardown + error message.
- tmux switch/create/kill failures → shown on a status line; never panic the loop.
- `$EDITOR` unset → fall back to `vi`. File paths are shell-quoted before `send-keys` (carried over from v1).

## 9. Persistence

Inner sessions persist across runs on the `runner` server. Quitting drops the embedded PTY (detaching its client) but leaves sessions alive; relaunch re-attaches scratch and re-syncs badges from `list-sessions`.

## 10. Testing

- **Unit-tested (pure / mockable):** `keys::encode_key` (every mapping in §5), `Focus` transitions, two-pane `resolve_click` (pane selection + tree row/`[+]`), and `app` switch sequences via the mock `CommandRunner`.
- **Manual verification (integration):** PTY spawn, vt100 rendering, key forwarding into a live shell/vim, focus highlight, resize reflow, and `$TMUX` refusal — exercised by running the app (requires tmux installed).

## 11. Out of scope (v2, YAGNI)

- Forwarding mouse events into the embedded terminal (scroll, click-to-position in vim/tmux).
- More than two panes, or a resizable/draggable split.
- Scrollback UI for the terminal beyond what the inner tmux/program provides.
- Reattaching to a pre-existing external runner server layout; config files; theming.
