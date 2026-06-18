# runner-manager v3 — Multi-session tree, chooser, file viewer

**Date:** 2026-06-18
**Status:** Approved for planning
**Builds on:** v2 (embedded terminal, `2026-06-18-runner-manager-v2-embedded-terminal-design.md`). The standalone TUI, embedded PTY, focus model, and `tmux -L runner` inner server carry over.

## 1. Summary

Three connected changes to how directories, sessions, and files behave:

1. **Multiple sessions per directory.** Clicking `[+]` (or pressing `a`) on a directory opens a chooser popup offering **shell** or **claude**; confirming starts that command in a new tmux session under the directory. A directory may have many sessions.
2. **Sessions listed in the tree (badge removed).** The old `●` active-session badge is gone. Instead, each directory's live sessions are shown as indented rows beneath it (always visible, even when the directory is collapsed), labeled by kind (`shell`, `shell 2`, `claude`). A session row vanishes automatically when its shell exits.
3. **Inline read-only file viewer.** Selecting a file no longer opens `$EDITOR` in tmux; it opens a read-only, scrollable viewer that the app renders in the right pane. Selecting a session or another file replaces it (only one at a time).

## 2. Session model

- `SessionKind { Shell, Claude }`.
- A `SessionStore` tracks sessions created during the current run: each entry is `{ dir: PathBuf, kind: SessionKind, slug: String, label: String }`.
- **Slug** is unique on the `runner` socket: `<dir-slug>-<kind>`, with `-2`, `-3`, … appended on collision (`<dir-slug>` reuses v2's `slugify`). 
- **Label** is the kind plus an index when a directory has more than one of that kind: `shell`, `shell 2`, `claude`.
- **Creating a session** (chooser confirm) runs, then switches the right pane to it:
  - Shell → `tmux -L runner new-session -d -s <slug> -c <dir>` (default `$SHELL`).
  - Claude → `tmux -L runner new-session -d -s <slug> -c <dir> claude`.
- **Sync / auto-removal:** the store reconciles against `tmux -L runner list-sessions` on a throttle (~once per second) and immediately after creating a session. Entries whose slug is no longer live are dropped, so when a shell (or `claude`) exits and tmux destroys the session, its row disappears. There is no manual kill key.
- **v1 limitation (documented):** only sessions created in the current run are listed. Sessions persisted from a previous run remain on the socket but are not enumerated as rows, because a slug cannot be reversed to a directory path. (Follow-up option: store the dir in a tmux session user-option to recover it.)
- The `scratch` placeholder session is never shown as a row.

## 3. Tree rows

`Row` becomes typed:

```
enum RowKind { Dir { expanded: bool }, Session { slug, kind }, File }
struct Row { path: PathBuf, label: String, depth: usize, kind: RowKind }
```

For a `Session` row, `path` is the owning directory. Flattening order, per directory, top-down:

1. the **Dir** row;
2. its **Session** rows (always, depth+1, in store order);
3. if the directory is expanded, its **file/subdir children** (depth+1), recursing.

```
▾ project            [+]
  shell
  claude
  ▸ src              [+]
    shell
  README.md
```

`build_rows(root_node, sessions_by_dir)` is a pure function (in `tree.rs`) that produces the `Vec<Row>`; it takes the filesystem node tree (unchanged expand/lazy-load behavior) and the store's `dir → Vec<SessionEntry>` map.

## 4. Right pane: terminal or viewer

`App` holds `viewer: Option<FileView>`:
- `None` → render the embedded PTY (`tui_term::PseudoTerminal`), as in v2.
- `Some(view)` → render the read-only file viewer.

`FileView` (new `viewer.rs`): `{ path, lines: Vec<String>, scroll: usize }`.
- `load(path)` reads the file to a `Vec<String>` of lines. Caps: at most 5000 lines kept; lines longer than the pane are truncated at render time (not stored truncated). If the bytes aren't valid UTF-8, `lines` is a single entry `<binary file: NAME>`. Read errors yield `<unable to read: NAME>`.
- Scrolling: `scroll_down(n)`, `scroll_up(n)`, clamped so `scroll <= max(0, lines.len() - 1)`.
- Read-only; no editing.

Opening a file sets `viewer = Some(load(path))`. Selecting/creating a session sets `viewer = None`. Only one viewer exists at a time.

## 5. Focus & input

Focus is `Focus { Tree, Right }` (Right = the right pane, whichever content it shows). `Ctrl-q` toggles; a left-click focuses the clicked pane. **Selecting anything in the tree keeps Tree focus** — you `Ctrl-q` or click to interact with the right pane. This is uniform for sessions and files.

Routing when **no popup** is open:
- **Tree focus:** `j`/`Down` and `k`/`Up` move the selection; on the selected row by kind:
  - `Dir`: `Enter`/click toggles expand/collapse; `a` or click on the `[+]` button opens the chooser.
  - `Session`: `Enter`/click switches the right pane to that session (`viewer = None`, `switch-client -c <host-tty> -t <slug>`).
  - `File`: `Enter`/click opens its viewer (`viewer = Some(load(path))`).
  - `h`/`?` opens the help popup; `q` quits.
- **Right focus, terminal (`viewer == None`):** every key except `Ctrl-q` is encoded to PTY bytes (v2 `keys::encode_key`).
- **Right focus, viewer (`viewer == Some`):** `j`/`Down`/`k`/`Up` scroll one line; `PgUp`/`PgDn` page; other keys ignored (no edit). `Ctrl-q` returns to Tree.

`x` (kill) is removed.

## 6. Popups

`popup: Popup { None, Help, Chooser { dir: PathBuf, selected: usize } }`.

- **Help** (unchanged from the prior feature): `h`/`?` opens it in tree focus; any key or click closes it (swallowed); lists the tree keys. Its key list is updated to reflect v3 (chooser, session/file behavior; no `x`).
- **Chooser:** opened by `a`/`[+]` on a Dir row. A small centered popup listing `shell` and `claude`.
  - `↑`/`Down`/`j`/`k` move `selected`; `Enter` confirms → create a session of the selected kind in `dir`, switch the right pane to it, close the popup; `Esc` cancels (close, no action).
  - A left-click on an option row confirms it; a click outside the popup cancels.

Only one popup is open at a time; opening the chooser while help is open is not possible (help swallows the key).

## 7. Components / files

- `session.rs` — `SessionKind`, `SessionEntry`, `SessionStore` (`create`, `sync`, `sessions_by_dir`, label/slug logic). Replaces the v2 single-slug `SessionRegistry`.
- `tree.rs` — typed `Row`/`RowKind`; pure `build_rows(root, sessions_by_dir)`; node expand/lazy-load unchanged.
- `viewer.rs` (new) — `FileView` load + scroll + caps.
- `tmux.rs` — `new_session` gains an optional command argument (used for `claude`); everything else unchanged.
- `app.rs` — holds `SessionStore`, `viewer`, `popup`, `focus`; methods for navigation, open-chooser, create-session, switch-session, open-file, scroll-viewer, sync, rebuild-rows. Badge/kill/`active` removed.
- `ui.rs` — render typed rows (no badge; `[+]` on Dir rows; indented session rows), the right pane (terminal vs viewer), the chooser popup; keep the help popup; click resolution returns the row kind.
- `run.rs` — popup routing (chooser nav + help), tree actions by row kind, right-pane input (PTY vs viewer scroll), throttled `list-sessions` sync (via `std::time::Instant`).
- `keys.rs`, `pty.rs` — unchanged.

## 8. Data flow

- **Create session:** chooser confirm → `SessionStore::create(dir, kind)` computes slug+label → `tmux new-session …` (with `claude` arg for Claude) → `viewer = None` → `switch-client` to the new slug → rebuild rows. Focus stays Tree.
- **Switch session:** Session row activated → `viewer = None` → `switch-client -c <host-tty> -t <slug>`.
- **Open file:** File row activated → `viewer = Some(FileView::load(path))`. No tmux involvement.
- **Scroll viewer:** Right focus + viewer → scroll methods adjust `FileView::scroll`.
- **Tick:** redraw; throttled (~1s) `SessionStore::sync(list_sessions())` → prune dead sessions → rebuild rows (dead session rows disappear). PTY resize as in v2.

## 9. Error handling

- Session create failure (e.g. tmux error) → status line message; no row added; loop never panics.
- `host_tty()` returns `None` → status "no host client to switch" (as v2); the startup poll still applies.
- File read failure / binary → viewer shows a one-line placeholder; never panics.
- `$SHELL` unset → tmux uses its default; `claude` not installed → the session starts and immediately exits, so its row appears briefly then is pruned (acceptable; status unaffected).

## 10. Testing

- **Unit-tested (pure / mockable):** `SessionStore` slug uniqueness, label indexing (`shell`, `shell 2`), and `sync` pruning; `build_rows` interleaving (dir → sessions → files, depth, collapsed-but-sessions-shown); `FileView` load (utf8, binary placeholder, line cap) and scroll clamping; chooser `selected` movement; click-to-row-kind resolution; `tmux.new_session` argv with and without a command.
- **Manual verification:** the chooser popup, right-pane terminal-vs-viewer rendering and scrolling, session auto-removal on shell exit, and focus routing — exercised by running the app (requires tmux).

## 11. Out of scope (v3, YAGNI)

- Editing files in the viewer (read-only only).
- Listing sessions persisted from previous runs (dir-from-slug recovery).
- A custom-command chooser entry (only shell/claude).
- Syntax highlighting or large-file streaming in the viewer.
- Reordering/renaming sessions.
