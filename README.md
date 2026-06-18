# runner-manager

A terminal UI that pairs a NERDTree-style file tree (left) with a per-directory
tmux session (right), using nested tmux.

## How it works

- An outer tmux window splits into two panes: the left runs the `runner-manager`
  TUI, the right hosts a client attached to a dedicated inner tmux server
  (`tmux -L runner`).
- Each directory you open becomes its own session on the inner server, rooted at
  that directory. Selecting a directory creates-or-switches to its session; the
  tree stays pinned on the left.
- Files open in `$EDITOR` (default `vi`) inside their directory's session.

## Usage

Run from the directory you want as the tree root:

```bash
runner-manager
```

This bootstraps the outer split and attaches you. Inside the left pane:

| Key            | Action                                   |
|----------------|------------------------------------------|
| `j` / `down`   | move down                                |
| `k` / `up`     | move up                                  |
| `Enter`        | expand/collapse a directory; open a file |
| `a`            | open/switch the session for a directory  |
| `x`            | kill a directory's session               |
| `q`            | quit (inner sessions keep running)       |
| left-click     | select + activate a row                  |
| click `[+]`    | open the session for that directory      |

The inner server uses prefix `C-a` to avoid clashing with your normal tmux.
