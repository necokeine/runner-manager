# runner-manager

A standalone terminal UI: a NERDTree-style file tree (left) plus a live
embedded terminal or read-only file viewer (right). Each directory can hold
multiple tmux sessions (shell, claude, or codex), shown as rows under it.

## How it works

- Runs directly in your terminal (NOT inside tmux); draws a fixed two-pane
  layout on the alternate screen.
- `a` (or clicking the `[+]` next to a directory name) opens a chooser to start
  a **shell**, **claude**, or **codex** session in that directory, on a
  project-local `pjma.sock` tmux server (`tmux -S <root>/.pjma/pjma.sock`).
  Sessions appear as rows under the directory (prefixed `$` for shell, `✦` for
  claude, `⌬` for codex) and disappear when their shell exits.
- `x` (or clicking the `[×]` next to a session name) closes that session,
  killing it on the `pjma.sock` server and removing its row.
- Quitting the tool does **not** close your tmux sessions — they keep running on
  the `<root>/.pjma/pjma.sock` socket. Reopen runner-manager and they are listed
  again under their directories.
- Selecting a session row shows it in the right pane (embedded terminal).
  Selecting a file shows it in a read-only viewer in the right pane.
- Git-changed files (and the directories containing them) can be coloured
  following `git status`'s own policy: staged changes ("Changes to be
  committed") in green, modified-but-unstaged and untracked paths in red, and
  git-ignored files and directories in grey. This works whether the tree root is
  itself a repository or just a parent folder holding several separate
  checkouts — each directory with its own `.git` is coloured from its own
  status. The colouring refreshes as sessions edit files. **This feature is off
  by default** — a full `git status` of a large tree can be expensive — and is
  toggled at runtime with `g` (the choice is persisted). It can also be enabled
  up front by writing a truthy value (`on`/`1`/`true`) to `<root>/.pjma/git`, or
  for one run via the `RM_GIT_STATUS` environment variable (which overrides the
  file).
- The left pane has two tabs, switched with `Tab` (or by clicking a tab):
  the **directory** view (the file tree above) and the **project** view, a flat
  list of every open session showing its type, directory, and a short brief
  (the command running in it). Selecting, switching, and closing sessions work
  the same in either view.
- Per-project state lives in a `<root>/.pjma/` config directory (the tmux socket
  plus the saved tree state). Which directories you have expanded is remembered
  across runs.
- Only **one** runner-manager may run per root directory at a time. Launching a
  second one in the same tree (both would share the one `pjma.sock` and embedded
  client) is rejected with an error instead of starting. The guard is an
  advisory lock on `<root>/.pjma/pjma.lock`, released automatically when the
  process exits — even on a crash — so there is no stale lock to clean up.

## Usage

Run from the directory you want as the tree root (not inside tmux):

```bash
runner-manager
```

| Key            | Action                                              |
|----------------|-----------------------------------------------------|
| `j` / `down`   | move down (tree focus)                              |
| `k` / `up`     | move up (tree focus)                                |
| `Tab`          | switch left pane between directory / project view   |
| `Enter`        | expand/collapse dir · switch to session · view file |
| `a` / `[+]`    | new session form (shell/claude/codex) on a directory |
| `x` / `[×]`    | close the selected session (tree focus)             |
| `<` / `>`      | narrow / widen the tree pane (tree focus)           |
| `g`            | toggle git-status colouring on/off (off by default) |
| `h` / `?`      | help popup                                          |
| `q`            | quit (tree focus)                                   |
| `Ctrl-q`       | toggle focus between tree and the right pane        |
| left-click     | focus a pane; in the tree, act on the clicked row   |
| scroll wheel   | over the tree: scroll it · over the terminal: scroll its history (old logs) |
| drag in terminal | select text; releasing copies it to the system clipboard |
| drag border    | resize the tree/terminal split                      |

The new-session form is laid out as labelled groups (Kind, Permission, Resume,
and the Cancel/Create buttons) and navigated in two axes:

| Key                        | Action                                              |
|----------------------------|-----------------------------------------------------|
| `↑`/`↓` (`j`/`k`)          | move between groups (Kind → Permission → Resume → buttons) |
| `←`/`→` (`h`/`l`)          | change the selected option within the focused group |
| `Tab` / `Shift-Tab`        | cycle between groups (wraps around)                 |
| `Enter`                    | create the session from **any** group (`Cancel` cancels) |
| `Space`                    | activate the focused `Cancel`/`Create` button       |
| `Esc`                      | cancel                                              |

The Kind group offers `shell`, `claude`, and `codex`. Selecting `claude` reveals
the **Permission** group — `normal` or `skip` = `--dangerously-skip-permissions`
(`shell` and `codex` have no extra options). When that directory already has past
Claude sessions, a **Resume** group appears too: pick `new session` to start
fresh, or an existing session (shown with the last prompt it was working on) to
launch `claude --resume <id>` and continue where it left off. Every option is
also clickable. The split between the tree and the right pane is adjustable with
`<`/`>` or by dragging the border.
