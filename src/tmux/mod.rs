//! All tmux server interaction. [`Tmux<R>`] prefixes every call with this
//! project's socket (`-S <path>`) and routes it through the [`CommandRunner`]
//! trait — [`SystemRunner`] in production, a mock in tests — which is what
//! makes everything downstream unit-testable without a real tmux. The
//! in-memory session bookkeeping lives in [`session`].

pub mod session;

use std::io;
use std::path::Path;
use std::process::Command;

/// Result of one tmux invocation: its exit status and captured stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    /// Whether tmux exited with status 0.
    pub success: bool,
    /// Captured standard output, lossily decoded as UTF-8.
    pub stdout: String,
}

/// A live tmux session plus the metadata this tool tags it with at creation
/// (`@rm` = "<kind> <dir>"). `kind`/`dir` are empty for sessions we did not
/// create (the embedded `scratch` client, or anything made by hand). `command`
/// is the foreground command of the session's active pane (`pane_current_command`),
/// used as a one-word "session brief" in the project view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub name: String,
    pub dir: String,
    pub kind: String,
    pub command: String,
}

/// How tmux is actually invoked. Everything downstream is generic over this so
/// tests can substitute a mock and assert the exact argument vectors.
pub trait CommandRunner {
    /// Run `tmux <args>` and report its exit status and stdout.
    fn run(&self, args: &[&str]) -> io::Result<CmdOutput>;
}

/// The production [`CommandRunner`]: spawns the real `tmux` binary.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, args: &[&str]) -> io::Result<CmdOutput> {
        let out = Command::new("tmux").args(args).output()?;
        Ok(CmdOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }
}

pub struct Tmux<R: CommandRunner> {
    /// Filesystem path of the tmux socket, passed as `-S <path>`. This is a
    /// project-local socket (`<root>/pjma.sock`), not a named `-L` socket in
    /// tmux's shared per-user tmpdir, so each project's sessions are isolated.
    socket: String,
    pub runner: R,
}

impl<R: CommandRunner> Tmux<R> {
    /// Wrap a runner with the socket path every call is prefixed with.
    pub fn new(socket: impl Into<String>, runner: R) -> Self {
        Self {
            socket: socket.into(),
            runner,
        }
    }

    fn run(&self, extra: &[&str]) -> io::Result<CmdOutput> {
        let mut args: Vec<&str> = vec!["-S", &self.socket];
        args.extend_from_slice(extra);
        self.runner.run(&args)
    }

    /// Like [`Tmux::run`] but for mutating commands where a non-zero tmux exit
    /// status is a real failure (e.g. `new-session` on a duplicate name), not a
    /// "no server yet" condition to be tolerated. `what` names the operation in
    /// the error message.
    fn run_ok(&self, extra: &[&str], what: &str) -> io::Result<()> {
        if self.run(extra)?.success {
            Ok(())
        } else {
            Err(io::Error::other(format!("tmux {what} failed")))
        }
    }

    /// Create a detached session named `slug` in `dir`, running `command` (or
    /// the default shell when `None`).
    ///
    /// # Errors
    ///
    /// Fails if `dir` is not valid UTF-8 (tmux takes `-c` as a string), if tmux
    /// could not be spawned, or if tmux itself rejects the command — most
    /// notably `duplicate session` when the slug is already taken on the socket.
    pub fn new_session(&self, slug: &str, dir: &Path, command: Option<&str>) -> io::Result<()> {
        let dir = path_str(dir)?;
        let mut args: Vec<&str> = vec!["new-session", "-d", "-s", slug, "-c", dir];
        if let Some(cmd) = command {
            args.push(cmd);
        }
        self.run_ok(&args, "new-session")
    }

    /// Point the client on `tty` at the session named exactly `slug` (the `=`
    /// prefix disables tmux's prefix-matching, so `src-shell` can never resolve
    /// to its sibling `src-shell-2` after the exact target is gone).
    ///
    /// # Errors
    ///
    /// Fails if tmux could not be spawned or exited non-zero (no such session,
    /// no such client).
    pub fn switch_client(&self, tty: &str, slug: &str) -> io::Result<()> {
        let target = exact(slug);
        self.run_ok(
            &["switch-client", "-c", tty, "-t", &target],
            "switch-client",
        )
    }

    /// Detach the given client (by its tty) from the server without destroying
    /// any session. Used on quit so the server and its sessions outlive us.
    ///
    /// # Errors
    ///
    /// Fails if tmux could not be spawned or exited non-zero (no such client).
    pub fn detach_client(&self, tty: &str) -> io::Result<()> {
        self.run_ok(&["detach-client", "-t", tty], "detach-client")
    }

    /// The live session most recently active (highest `session_activity`). Used
    /// at startup to attach the embedded client to whatever the user was last
    /// working in, instead of a throwaway scratch session. `None` if no sessions
    /// exist (a fresh start with nothing to recover).
    pub fn latest_session(&self) -> io::Result<Option<String>> {
        let out = self.run(&["list-sessions", "-F", "#{session_activity} #{session_name}"])?;
        if !out.success {
            return Ok(None);
        }
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| {
                let mut cols = l.trim().splitn(2, ' ');
                let activity: i64 = cols.next()?.trim().parse().ok()?;
                let name = cols.next()?.trim().to_string();
                (!name.is_empty()).then_some((activity, name))
            })
            .max_by_key(|(activity, _)| *activity)
            .map(|(_, name)| name))
    }

    /// Names of all live sessions on the socket; empty when the server is not
    /// running (a failed `list-sessions` is a normal fresh-start condition).
    pub fn list_sessions(&self) -> io::Result<Vec<String>> {
        let out = self.run(&["list-sessions", "-F", "#{session_name}"])?;
        if !out.success {
            return Ok(Vec::new());
        }
        Ok(out
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// Tag a session with its directory and kind so it can be re-adopted into
    /// the tree on a later run. Stored as one user option `@rm` = "<kind> <dir>"
    /// (kind has no spaces, so the dir is everything after the first space).
    /// A dir containing a tab or newline would corrupt the tab-delimited
    /// `list_sessions_full` format, so such a session is left untagged (it
    /// still works; it just won't be re-adopted on a later run).
    ///
    /// # Errors
    ///
    /// Fails if `dir` is not valid UTF-8, or if tmux could not be spawned or
    /// exited non-zero (no such session).
    pub fn tag_session(&self, slug: &str, dir: &Path, kind: &str) -> io::Result<()> {
        let dir = path_str(dir)?;
        if dir.contains(['\t', '\n']) {
            return Ok(());
        }
        let value = format!("{kind} {dir}");
        let target = exact(slug);
        self.run_ok(&["set-option", "-t", &target, "@rm", &value], "set-option")
    }

    /// List live sessions with their `@rm` tag split back into kind + dir, plus
    /// the active pane's foreground command. The three fields are tab-delimited;
    /// the `@rm` tag itself is "<kind> <dir>" (space-separated, and `tag_session`
    /// refuses tab-containing dirs) and a command never contains a tab, so a
    /// three-way `splitn` is unambiguous.
    pub fn list_sessions_full(&self) -> io::Result<Vec<SessionInfo>> {
        let out = self.run(&[
            "list-sessions",
            "-F",
            "#{session_name}\t#{@rm}\t#{pane_current_command}",
        ])?;
        if !out.success {
            return Ok(Vec::new());
        }
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| {
                let mut cols = l.splitn(3, '\t');
                let name = cols.next()?.trim().to_string();
                if name.is_empty() {
                    return None;
                }
                let tag = cols.next().unwrap_or("").trim();
                let command = cols.next().unwrap_or("").trim().to_string();
                let (kind, dir) = match tag.split_once(' ') {
                    Some((k, d)) => (k.to_string(), d.to_string()),
                    None => (String::new(), String::new()),
                };
                Some(SessionInfo {
                    name,
                    dir,
                    kind,
                    command,
                })
            })
            .collect())
    }

    /// The tty of the (first) client attached to the socket — the embedded
    /// client's tty, used as the `-c` target of `switch-client`. `None` when no
    /// client is attached or the server is not running.
    pub fn host_tty(&self) -> io::Result<Option<String>> {
        let out = self.run(&["list-clients", "-F", "#{client_tty}"])?;
        if !out.success {
            return Ok(None);
        }
        Ok(out
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .find(|l| !l.is_empty()))
    }

    /// The session the (first) embedded client is currently attached to. Unlike
    /// the slug we set when issuing `switch-client`, this reflects tmux's real
    /// state — including an auto-switch to another session when the viewed one
    /// is destroyed (`detach-on-destroy off`). `None` if no client is attached.
    pub fn client_session(&self) -> io::Result<Option<String>> {
        let out = self.run(&["list-clients", "-F", "#{client_session}"])?;
        if !out.success {
            return Ok(None);
        }
        Ok(out
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .find(|l| !l.is_empty()))
    }

    /// Destroy the session named exactly `slug` (see [`Tmux::switch_client`]
    /// for why the target is exact-matched).
    ///
    /// # Errors
    ///
    /// Fails if tmux could not be spawned or exited non-zero — most commonly
    /// because the session already exited on its own.
    pub fn kill_session(&self, slug: &str) -> io::Result<()> {
        let target = exact(slug);
        self.run_ok(&["kill-session", "-t", &target], "kill-session")
    }

    /// Set a global server option (`tmux set -g <name> <value>`), e.g.
    /// `detach-on-destroy off` at startup.
    ///
    /// # Errors
    ///
    /// Fails if tmux could not be spawned or exited non-zero (unknown option,
    /// no server).
    pub fn set_global_option(&self, name: &str, value: &str) -> io::Result<()> {
        self.run_ok(&["set", "-g", name, value], "set")
    }
}

/// An exact-match tmux target for a session name: tmux resolves bare `-t`
/// targets by prefix when no exact match exists, which is dangerous with our
/// deliberately prefix-shaped sibling slugs (`src-shell`, `src-shell-2`).
fn exact(slug: &str) -> String {
    format!("={slug}")
}

/// `path` as UTF-8, or an error naming the problem — tmux arguments are
/// strings, and silently substituting another directory would be worse.
fn path_str(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other(format!("non-UTF-8 path: {}", path.display())))
}

#[cfg(test)]
#[derive(Default)]
pub struct MockRunner {
    pub calls: std::cell::RefCell<Vec<Vec<String>>>,
    pub responses: std::cell::RefCell<std::collections::VecDeque<CmdOutput>>,
}

#[cfg(test)]
impl MockRunner {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&self, success: bool, stdout: &str) {
        self.responses.borrow_mut().push_back(CmdOutput {
            success,
            stdout: stdout.to_string(),
        });
    }
    pub fn nth_call(&self, i: usize) -> Vec<String> {
        self.calls.borrow()[i].clone()
    }
    pub fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

#[cfg(test)]
impl CommandRunner for MockRunner {
    fn run(&self, args: &[&str]) -> io::Result<CmdOutput> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        Ok(self
            .responses
            .borrow_mut()
            .pop_front()
            .unwrap_or(CmdOutput {
                success: true,
                stdout: String::new(),
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn set_global_option_builds_set_g() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.set_global_option("detach-on-destroy", "off").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-S", "runner", "set", "-g", "detach-on-destroy", "off"]
        );
    }

    #[test]
    fn new_session_builds_detached_with_dir() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.new_session("src", Path::new("/tmp/proj/src"), None)
            .unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec![
                "-S",
                "runner",
                "new-session",
                "-d",
                "-s",
                "src",
                "-c",
                "/tmp/proj/src"
            ]
        );
    }

    #[test]
    fn new_session_with_command_appends_it() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.new_session("src", Path::new("/tmp/proj/src"), Some("claude"))
            .unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec![
                "-S",
                "runner",
                "new-session",
                "-d",
                "-s",
                "src",
                "-c",
                "/tmp/proj/src",
                "claude"
            ]
        );
    }

    #[test]
    fn switch_client_targets_tty_and_session() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.switch_client("/dev/ttys003", "src").unwrap();
        // `=src` forces an exact session-name match; a bare `src` would
        // prefix-match `src-2` once `src` itself is gone.
        assert_eq!(
            tmux.runner.nth_call(0),
            vec![
                "-S",
                "runner",
                "switch-client",
                "-c",
                "/dev/ttys003",
                "-t",
                "=src"
            ]
        );
    }

    #[test]
    fn kill_session_targets_exact_name() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.kill_session("src-shell").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-S", "runner", "kill-session", "-t", "=src-shell"]
        );
    }

    #[test]
    fn mutating_commands_surface_tmux_failure() {
        // A tmux exit status of 1 (e.g. "duplicate session") must become an
        // Err, not a silent Ok.
        let runner = MockRunner::new();
        runner.push(false, "");
        let tmux = Tmux::new("runner", runner);
        assert!(tmux
            .new_session("src", Path::new("/tmp/proj/src"), None)
            .is_err());

        let runner = MockRunner::new();
        runner.push(false, "");
        let tmux = Tmux::new("runner", runner);
        assert!(tmux.kill_session("src").is_err());
    }

    #[test]
    fn detach_client_targets_tty() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.detach_client("/dev/ttys003").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-S", "runner", "detach-client", "-t", "/dev/ttys003"]
        );
    }

    #[test]
    fn list_sessions_parses_lines_and_empty_on_failure() {
        let runner = MockRunner::new();
        runner.push(true, "src\ntests\n");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(tmux.list_sessions().unwrap(), vec!["src", "tests"]);

        let runner = MockRunner::new();
        runner.push(false, "no server running");
        let tmux = Tmux::new("runner", runner);
        assert!(tmux.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn latest_session_picks_highest_activity_and_none_on_failure() {
        let runner = MockRunner::new();
        // tests-shell has the newest activity, so it's the one to recover into.
        runner.push(true, "100 src-claude\n145 tests-shell\n90 root-shell\n");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(
            tmux.latest_session().unwrap(),
            Some("tests-shell".to_string())
        );
        assert_eq!(
            tmux.runner.nth_call(0),
            vec![
                "-S",
                "runner",
                "list-sessions",
                "-F",
                "#{session_activity} #{session_name}"
            ]
        );

        // No server / no sessions -> None so the caller spawns nothing.
        let runner = MockRunner::new();
        runner.push(false, "no server running");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(tmux.latest_session().unwrap(), None);

        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(tmux.latest_session().unwrap(), None);
    }

    #[test]
    fn host_tty_returns_first_nonempty() {
        let runner = MockRunner::new();
        runner.push(true, "/dev/ttys005\n");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(tmux.host_tty().unwrap(), Some("/dev/ttys005".to_string()));
    }

    #[test]
    fn tag_session_sets_rm_option() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.tag_session("src-shell", Path::new("/tmp/proj/src"), "shell")
            .unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec![
                "-S",
                "runner",
                "set-option",
                "-t",
                "=src-shell",
                "@rm",
                "shell /tmp/proj/src"
            ]
        );
    }

    #[test]
    fn tag_session_skips_dirs_that_would_corrupt_the_tag() {
        let runner = MockRunner::new();
        let tmux = Tmux::new("runner", runner);
        tmux.tag_session("x-shell", Path::new("/tmp/a\tb"), "shell")
            .unwrap();
        assert_eq!(tmux.runner.call_count(), 0);
    }

    #[test]
    fn list_sessions_full_splits_tag_into_kind_and_dir() {
        let runner = MockRunner::new();
        // a tagged claude session, a tagged shell session, and the untagged scratch client
        runner.push(true, "src-claude\tclaude /tmp/proj/src\tnode\nroot-shell\tshell /tmp/proj\tvim\nscratch\t\tzsh\n");
        let tmux = Tmux::new("runner", runner);
        let infos = tmux.list_sessions_full().unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec![
                "-S",
                "runner",
                "list-sessions",
                "-F",
                "#{session_name}\t#{@rm}\t#{pane_current_command}"
            ]
        );
        assert_eq!(infos.len(), 3);
        assert_eq!(
            infos[0],
            SessionInfo {
                name: "src-claude".into(),
                dir: "/tmp/proj/src".into(),
                kind: "claude".into(),
                command: "node".into()
            }
        );
        assert_eq!(
            infos[1],
            SessionInfo {
                name: "root-shell".into(),
                dir: "/tmp/proj".into(),
                kind: "shell".into(),
                command: "vim".into()
            }
        );
        // scratch (no tag) -> empty dir/kind so it won't be adopted into the tree
        assert_eq!(
            infos[2],
            SessionInfo {
                name: "scratch".into(),
                dir: String::new(),
                kind: String::new(),
                command: "zsh".into()
            }
        );
    }

    #[test]
    fn list_sessions_full_tolerates_missing_command_column() {
        // Older tags / mocked rows without the command column still parse, just
        // with an empty brief.
        let runner = MockRunner::new();
        runner.push(true, "root-shell\tshell /tmp/proj\n");
        let tmux = Tmux::new("runner", runner);
        let infos = tmux.list_sessions_full().unwrap();
        assert_eq!(
            infos[0],
            SessionInfo {
                name: "root-shell".into(),
                dir: "/tmp/proj".into(),
                kind: "shell".into(),
                command: String::new()
            }
        );
    }

    #[test]
    fn client_session_returns_first_nonempty() {
        let runner = MockRunner::new();
        runner.push(true, "src-shell\n");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(
            tmux.client_session().unwrap(),
            Some("src-shell".to_string())
        );
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-S", "runner", "list-clients", "-F", "#{client_session}"]
        );

        // No client attached / no server -> None.
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        assert_eq!(tmux.client_session().unwrap(), None);
    }
}
