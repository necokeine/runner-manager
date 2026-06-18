# runner-manager

A standalone terminal UI: a NERDTree-style file tree (left) plus a live
embedded terminal or read-only file viewer (right). Each directory can hold
multiple tmux sessions (shell or claude), shown as rows under it.

## How it works

- Runs directly in your terminal (NOT inside tmux); draws a fixed two-pane
  layout on the alternate screen.
- `a` (or clicking the `[+]` next to a directory name) opens a chooser to start
  a **shell** or **claude** session in that directory, on a project-local
  `pjma.sock` tmux server (`tmux -S <root>/pjma.sock`). Sessions appear as rows
  under the directory (prefixed `$` for shell, `✦` for claude) and disappear when
  their shell exits.
- Quitting the tool does **not** close your tmux sessions — they keep running on
  the `<root>/pjma.sock` socket. Reopen runner-manager and they are listed again
  under their directories.
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
| `a` / `[+]`    | new session form (shell/claude) on a directory      |
| `<` / `>`      | narrow / widen the tree pane (tree focus)           |
| `h` / `?`      | help popup                                          |
| `q`            | quit (tree focus)                                   |
| `Ctrl-q`       | toggle focus between tree and the right pane        |
| left-click     | focus a pane; in the tree, act on the clicked row   |
| scroll wheel   | over the tree: scroll it · over the terminal: scroll its history (old logs) |
| drag border    | resize the tree/terminal split                      |

In the new-session form: `↑`/`↓`/`j`/`k` move between rows (selecting `claude`
reveals a permission choice: `normal` or `skip` = `--dangerously-skip-permissions`),
`Enter`/`Space` activates the focused `Cancel`/`Create` button, `Esc` cancels.
Click a row to select it, or click `Cancel`/`Create`. The split between the tree
and the right pane is adjustable with `<`/`>` or by dragging the border.
