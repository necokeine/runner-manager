use std::env;
use std::io;
use std::process::Command;

use runner_manager::run;

fn main() -> io::Result<()> {
    if env::var_os("TMUX").is_some() {
        eprintln!(
            "runner-manager must not be run inside tmux; tmux is used for the inner task sessions."
        );
        std::process::exit(1);
    }
    if !tmux_available() {
        eprintln!(
            "runner-manager requires tmux, but `tmux` was not found on your PATH. \
             Install tmux and try again."
        );
        std::process::exit(1);
    }
    let root = env::current_dir()?;
    let socket = env::var("RM_SOCKET").unwrap_or_else(|_| "runner".to_string());
    run::run(root, socket)
}

/// Probe for a usable `tmux` binary before we touch the terminal. tmux is the
/// engine behind both the embedded client and every task session, so without it
/// there is nothing to render. Probing here (rather than letting `Pty::spawn`
/// fail later) keeps the failure off the alternate screen: the user gets a plain
/// stderr message and a clean exit instead of a flicker into a broken TUI.
fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}
