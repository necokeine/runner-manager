use std::io;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub success: bool,
    pub stdout: String,
}

/// A live tmux session plus the metadata this tool tags it with at creation
/// (`@rm` = "<kind> <dir>"). `kind`/`dir` are empty for sessions we did not
/// create (the embedded `scratch` client, or anything made by hand).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub name: String,
    pub dir: String,
    pub kind: String,
}

pub trait CommandRunner {
    fn run(&self, args: &[&str]) -> io::Result<CmdOutput>;
}

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
    socket: String,
    pub runner: R,
}

impl<R: CommandRunner> Tmux<R> {
    pub fn new(socket: impl Into<String>, runner: R) -> Self {
        Self { socket: socket.into(), runner }
    }

    fn run(&self, extra: &[&str]) -> io::Result<CmdOutput> {
        let mut args: Vec<&str> = vec!["-L", &self.socket];
        args.extend_from_slice(extra);
        self.runner.run(&args)
    }

    pub fn has_session(&self, slug: &str) -> io::Result<bool> {
        Ok(self.run(&["has-session", "-t", slug])?.success)
    }

    pub fn new_session(&self, slug: &str, dir: &Path, command: Option<&str>) -> io::Result<()> {
        let dir = dir.to_str().unwrap_or(".");
        let mut args: Vec<&str> = vec!["new-session", "-d", "-s", slug, "-c", dir];
        if let Some(cmd) = command {
            args.push(cmd);
        }
        self.run(&args)?;
        Ok(())
    }

    pub fn switch_client(&self, tty: &str, slug: &str) -> io::Result<()> {
        self.run(&["switch-client", "-c", tty, "-t", slug])?;
        Ok(())
    }

    /// Detach the given client (by its tty) from the server without destroying
    /// any session. Used on quit so the server and its sessions outlive us.
    pub fn detach_client(&self, tty: &str) -> io::Result<()> {
        self.run(&["detach-client", "-t", tty])?;
        Ok(())
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
    pub fn tag_session(&self, slug: &str, dir: &Path, kind: &str) -> io::Result<()> {
        let dir = dir.to_str().unwrap_or(".");
        let value = format!("{kind} {dir}");
        self.run(&["set-option", "-t", slug, "@rm", &value])?;
        Ok(())
    }

    /// List live sessions with their `@rm` tag split back into kind + dir.
    pub fn list_sessions_full(&self) -> io::Result<Vec<SessionInfo>> {
        let out = self.run(&["list-sessions", "-F", "#{session_name}\t#{@rm}"])?;
        if !out.success {
            return Ok(Vec::new());
        }
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| {
                let mut cols = l.splitn(2, '\t');
                let name = cols.next()?.trim().to_string();
                if name.is_empty() {
                    return None;
                }
                let tag = cols.next().unwrap_or("").trim();
                let (kind, dir) = match tag.split_once(' ') {
                    Some((k, d)) => (k.to_string(), d.to_string()),
                    None => (String::new(), String::new()),
                };
                Some(SessionInfo { name, dir, kind })
            })
            .collect())
    }

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

    pub fn send_keys(&self, slug: &str, keys: &str) -> io::Result<()> {
        self.run(&["send-keys", "-t", slug, keys, "Enter"])?;
        Ok(())
    }

    pub fn kill_session(&self, slug: &str) -> io::Result<()> {
        self.run(&["kill-session", "-t", slug])?;
        Ok(())
    }

    pub fn set_global_option(&self, name: &str, value: &str) -> io::Result<()> {
        self.run(&["set", "-g", name, value])?;
        Ok(())
    }
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
        self.responses
            .borrow_mut()
            .push_back(CmdOutput { success, stdout: stdout.to_string() });
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
            .unwrap_or(CmdOutput { success: true, stdout: String::new() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn has_session_prefixes_socket_and_reads_success() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        assert!(tmux.has_session("src").unwrap());
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "has-session", "-t", "src"]
        );
    }

    #[test]
    fn set_global_option_builds_set_g() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.set_global_option("detach-on-destroy", "off").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "set", "-g", "detach-on-destroy", "off"]
        );
    }

    #[test]
    fn new_session_builds_detached_with_dir() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.new_session("src", Path::new("/tmp/proj/src"), None).unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "new-session", "-d", "-s", "src", "-c", "/tmp/proj/src"]
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
            vec!["-L", "runner", "new-session", "-d", "-s", "src", "-c", "/tmp/proj/src", "claude"]
        );
    }

    #[test]
    fn switch_client_targets_tty_and_session() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.switch_client("/dev/ttys003", "src").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "switch-client", "-c", "/dev/ttys003", "-t", "src"]
        );
    }

    #[test]
    fn detach_client_targets_tty() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.detach_client("/dev/ttys003").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "detach-client", "-t", "/dev/ttys003"]
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
        assert_eq!(tmux.latest_session().unwrap(), Some("tests-shell".to_string()));
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "list-sessions", "-F", "#{session_activity} #{session_name}"]
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
        tmux.tag_session("src-shell", Path::new("/tmp/proj/src"), "shell").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "set-option", "-t", "src-shell", "@rm", "shell /tmp/proj/src"]
        );
    }

    #[test]
    fn list_sessions_full_splits_tag_into_kind_and_dir() {
        let runner = MockRunner::new();
        // a tagged claude session, a tagged shell session, and the untagged scratch client
        runner.push(true, "src-claude\tclaude /tmp/proj/src\nroot-shell\tshell /tmp/proj\nscratch\t\n");
        let tmux = Tmux::new("runner", runner);
        let infos = tmux.list_sessions_full().unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "list-sessions", "-F", "#{session_name}\t#{@rm}"]
        );
        assert_eq!(infos.len(), 3);
        assert_eq!(infos[0], SessionInfo { name: "src-claude".into(), dir: "/tmp/proj/src".into(), kind: "claude".into() });
        assert_eq!(infos[1], SessionInfo { name: "root-shell".into(), dir: "/tmp/proj".into(), kind: "shell".into() });
        // scratch (no tag) -> empty dir/kind so it won't be adopted into the tree
        assert_eq!(infos[2], SessionInfo { name: "scratch".into(), dir: String::new(), kind: String::new() });
    }

    #[test]
    fn send_keys_appends_enter() {
        let runner = MockRunner::new();
        runner.push(true, "");
        let tmux = Tmux::new("runner", runner);
        tmux.send_keys("src", "vi -- a.rs").unwrap();
        assert_eq!(
            tmux.runner.nth_call(0),
            vec!["-L", "runner", "send-keys", "-t", "src", "vi -- a.rs", "Enter"]
        );
    }
}
