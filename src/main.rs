use std::env;
use std::io;

use runner_manager::run;

fn main() -> io::Result<()> {
    if env::var_os("TMUX").is_some() {
        eprintln!(
            "runner-manager must not be run inside tmux; tmux is used for the inner task sessions."
        );
        std::process::exit(1);
    }
    let root = env::current_dir()?;
    let socket = env::var("RM_SOCKET").unwrap_or_else(|_| "runner".to_string());
    run::run(root, socket)
}
