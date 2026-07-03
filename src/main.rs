use std::env;
use std::io;
use std::process::Command;

use runner_manager::project::config::Config;
use runner_manager::project::lock::{InstanceLock, LockError};
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

    // The config dir (`<root>/.pjma`) holds the tmux socket, the saved tree
    // state, and the instance lock. Create it here — before the lock and before
    // `run` touches the terminal — so a failure prints to plain stderr rather
    // than flickering the alternate screen. `run` also calls `ensure_dir`, but
    // it is idempotent.
    let config = Config::new(&root);
    if let Err(e) = config.ensure_dir() {
        eprintln!(
            "runner-manager: could not create config dir {}: {e}",
            config.dir().display()
        );
        std::process::exit(1);
    }

    // Single-instance guard: refuse to start if another runner-manager already
    // owns this root. Both would otherwise share one tmux socket and one
    // embedded client, racing on switch-client/new-session and on the saved
    // tree state. Held until the process exits, then released by the kernel.
    let _lock = match InstanceLock::try_acquire(config.dir()) {
        Ok(lock) => lock,
        Err(LockError::Held) => {
            eprintln!(
                "runner-manager is already running in {}.\n\
                 Only one instance is allowed per root directory. \
                 Quit the other instance first.",
                root.display()
            );
            std::process::exit(1);
        }
        Err(LockError::Io(e)) => {
            eprintln!("runner-manager: could not acquire instance lock: {e}");
            std::process::exit(1);
        }
    };

    // Use a project-local socket file inside the config dir
    // (`<root>/.pjma/pjma.sock`) so each project's tmux sessions live on their
    // own socket rather than a shared named one. `RM_SOCKET` can still override
    // with an explicit socket path.
    let socket = env::var("RM_SOCKET").unwrap_or_else(|_| {
        root.join(runner_manager::project::config::DIR_NAME)
            .join("pjma.sock")
            .to_string_lossy()
            .into_owned()
    });
    // `_lock` stays alive until `main` returns, releasing the flock on exit.
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
