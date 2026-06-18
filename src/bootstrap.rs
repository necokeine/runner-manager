use std::io;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCmd {
    pub socket: Option<String>,
    pub args: Vec<String>,
}

fn svec(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

pub fn inner_setup_commands(socket: &str) -> Vec<TmuxCmd> {
    vec![
        TmuxCmd {
            socket: Some(socket.to_string()),
            args: svec(&["new-session", "-d", "-s", "scratch"]),
        },
        TmuxCmd {
            socket: Some(socket.to_string()),
            args: svec(&["set", "-g", "prefix", "C-a"]),
        },
        TmuxCmd {
            socket: Some(socket.to_string()),
            args: svec(&["set", "-g", "prefix2", "None"]),
        },
    ]
}

pub fn outer_layout_commands(outer: &str, socket: &str, self_exe: &str) -> Vec<TmuxCmd> {
    let tui = format!("{self_exe} tui");
    let attach = format!("tmux -L {socket} attach -t scratch");
    vec![
        TmuxCmd {
            socket: None,
            args: vec![
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                outer.into(),
                "-n".into(),
                "main".into(),
                tui,
            ],
        },
        TmuxCmd {
            socket: None,
            args: vec![
                "split-window".into(),
                "-h".into(),
                "-t".into(),
                format!("{outer}:main"),
                attach,
            ],
        },
        TmuxCmd {
            socket: None,
            args: vec![
                "select-pane".into(),
                "-t".into(),
                format!("{outer}:main.0"),
            ],
        },
    ]
}

fn execute(cmds: &[TmuxCmd]) -> io::Result<()> {
    for c in cmds {
        let mut command = Command::new("tmux");
        if let Some(sock) = &c.socket {
            command.arg("-L").arg(sock);
        }
        command.args(&c.args);
        let status = command.status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "tmux command failed ({:?}): {:?}",
                status.code(),
                c.args
            )));
        }
    }
    Ok(())
}

pub fn run_bootstrap(socket: &str, outer: &str) -> io::Result<()> {
    if Command::new("tmux").arg("-V").output().is_err() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "tmux not found in PATH; install tmux to use runner-manager",
        ));
    }
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    // Inner setup creates the scratch session (which would error as a duplicate on a
    // re-run) and sets the prefix; only do it when the inner server isn't already set
    // up. The scratch session's presence is our marker that setup has run.
    if !session_exists(Some(socket), "scratch") {
        execute(&inner_setup_commands(socket))?;
    }
    // Only build the outer layout the first time; on a re-run the session already
    // exists and we just re-attach (rebuilding would duplicate panes / error).
    if !session_exists(None, outer) {
        execute(&outer_layout_commands(outer, socket, &exe))?;
    }
    Command::new("tmux").args(["attach", "-t", outer]).status()?;
    Ok(())
}

/// Returns true if a session named `name` exists on the given tmux server
/// (`socket` = None means the default server).
fn session_exists(socket: Option<&str>, name: &str) -> bool {
    let mut command = Command::new("tmux");
    if let Some(sock) = socket {
        command.arg("-L").arg(sock);
    }
    command
        .args(["has-session", "-t", name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_setup_creates_scratch_and_sets_prefix() {
        let cmds = inner_setup_commands("runner");
        assert_eq!(cmds[0].socket.as_deref(), Some("runner"));
        assert_eq!(cmds[0].args, vec!["new-session", "-d", "-s", "scratch"]);
        assert!(cmds
            .iter()
            .any(|c| c.args == vec!["set", "-g", "prefix", "C-a"]));
    }

    #[test]
    fn outer_layout_splits_with_tui_and_inner_attach() {
        let cmds = outer_layout_commands("runner-manager", "runner", "/usr/bin/runner-manager");
        // first command starts the detached outer session running the tui in the left pane
        assert_eq!(cmds[0].socket, None);
        assert_eq!(cmds[0].args[0], "new-session");
        assert!(cmds[0].args.iter().any(|a| a == "/usr/bin/runner-manager tui"));
        // a split-window attaches the inner scratch session in the right pane
        let split = cmds.iter().find(|c| c.args[0] == "split-window").unwrap();
        assert!(split
            .args
            .iter()
            .any(|a| a == "tmux -L runner attach -t scratch"));
    }
}
