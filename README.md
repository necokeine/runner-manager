# runner-manager

A standalone terminal UI that pairs a NERDTree-style file tree (left) with a
live embedded terminal (right). Each directory maps to its own tmux session;
selecting a directory creates-or-switches to that session in the right pane,
with the tree always pinned on the left. Files open in `$EDITOR` inside their
directory's session.

## How it works

- runner-manager runs directly in your terminal (it must NOT be run inside
  tmux). It draws a fixed two-pane layout on the alternate screen.
- The right pane is a real embedded terminal: it spawns
  `tmux -L runner new-session -A -s scratch` in a PTY and renders it. Selecting
  a directory switches that terminal to the directory's session.
- tmux is used only for the inner per-directory sessions (socket `runner`),
  which persist across runs.

## Usage

Run from the directory you want as the tree root (not inside tmux):

```bash
runner-manager
```

| Key            | Action                                       |
|----------------|----------------------------------------------|
| `j` / `down`   | move down (tree focus)                       |
| `k` / `up`     | move up (tree focus)                         |
| `Enter`        | expand/collapse a directory; open a file     |
| `a`            | open/switch the session for a directory      |
| `x`            | kill a directory's session                   |
| `q`            | quit (tree focus only; inner sessions persist)|
| `Ctrl-q`       | toggle focus between tree and terminal       |
| left-click     | focus the clicked pane; in tree, select/act  |

When the terminal pane has focus, every key except `Ctrl-q` goes to the inner
tmux session (shell, vim, the tmux prefix `C-b`, etc.). The focused pane has a
highlighted border; the layout never changes.
