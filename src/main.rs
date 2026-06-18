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
    // Use a project-local socket file inside the config dir
    // (`<root>/.pjma/pjma.sock`) so each project's tmux sessions live on their
    // own socket rather than a shared named one. `RM_SOCKET` can still override
    // with an explicit socket path. The config dir is created in `run`.
    let socket = env::var("RM_SOCKET").unwrap_or_else(|_| {
        root.join(runner_manager::config::DIR_NAME)
            .join("pjma.sock")
            .to_string_lossy()
            .into_owned()
    });
    run::run(root, socket)
}
