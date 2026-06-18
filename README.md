# runner-manager

A standalone terminal UI: a NERDTree-style file tree (left) plus a live
embedded terminal or read-only file viewer (right). Each directory can hold
multiple tmux sessions (shell or claude), shown as rows under it.

## How it works

- Runs directly in your terminal (NOT inside tmux); draws a fixed two-pane
  layout on the alternate screen.
- `a` (or clicking `[+]`) on a directory opens a chooser to start a **shell**
  or **claude** session in that directory, on the `tmux -L runner` server.
  Sessions appear as rows under the directory and disappear when their shell
  exits.
- Selecting a session row shows it in the right pane (embedded terminal).
  Selecting a file shows it in a read-only viewer in the right pane.

## Usage

Run from the directory you want as the tree root (not inside tmux):

```bash
runner-manager
```

| Key            | Action                                              |
|----------------|-----------------------------------------------------|
| `j` / `down`   | move down (tree focus)                              |
| `k` / `up`     | move up (tree focus)                                |
| `Enter`        | expand/collapse dir · switch to session · view file |
| `a` / `[+]`    | new session (shell/claude) on a directory           |
| `h` / `?`      | help popup                                          |
| `q`            | quit (tree focus)                                   |
| `Ctrl-q`       | toggle focus between tree and the right pane        |
| left-click     | focus a pane; in the tree, act on the clicked row   |

In the chooser popup: `↑`/`↓`/`j`/`k` to move, `Enter` to start, `Esc` to
cancel. When the right pane is focused: keys go to the shell, or — if a file
is shown — `j`/`k`/`PgUp`/`PgDn` scroll it (read-only).
