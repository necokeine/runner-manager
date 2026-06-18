use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::session::SessionRegistry;
use crate::tmux::{CommandRunner, Tmux};
use crate::tree::{Row, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Terminal,
}

pub struct App<R: CommandRunner> {
    pub tree: Tree,
    pub registry: SessionRegistry,
    pub tmux: Tmux<R>,
    pub root: PathBuf,
    pub selected: usize,
    pub rows: Vec<Row>,
    pub active: HashSet<PathBuf>,
    pub host_tty: Option<String>,
    pub editor: String,
    pub status: String,
    pub should_quit: bool,
    pub focus: Focus,
    pub show_help: bool,
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl<R: CommandRunner> App<R> {
    pub fn new(root: PathBuf, tmux: Tmux<R>, editor: String) -> Self {
        let tree = Tree::new(root.clone());
        let rows = tree.visible_rows();
        Self {
            tree,
            registry: SessionRegistry::new(),
            tmux,
            root,
            selected: 0,
            rows,
            active: HashSet::new(),
            host_tty: None,
            editor,
            status: String::new(),
            should_quit: false,
            focus: Focus::Tree,
            show_help: false,
        }
    }

    pub fn refresh_rows(&mut self) {
        self.rows = self.tree.visible_rows();
        if !self.rows.is_empty() && self.selected >= self.rows.len() {
            self.selected = self.rows.len() - 1;
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Terminal,
            Focus::Terminal => Focus::Tree,
        };
    }

    pub fn activate(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        if row.is_dir {
            if let Some(node) = self.tree.node_at_mut(&row.path) {
                node.toggle();
            }
            self.refresh_rows();
        } else {
            self.open_file(&row.path)?;
        }
        Ok(())
    }

    pub fn open_session(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        if row.is_dir {
            self.open_dir(&row.path)?;
        } else {
            self.open_file(&row.path)?;
        }
        Ok(())
    }

    pub fn kill_selected(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        if !row.is_dir {
            return Ok(());
        }
        let slug = self.registry.slug_for(&row.path, &self.root);
        self.tmux.kill_session(&slug)?;
        self.active.remove(&row.path);
        self.status = format!("killed {slug}");
        Ok(())
    }

    pub fn sync_active(&mut self) -> io::Result<()> {
        let sessions: HashSet<String> = self.tmux.list_sessions()?.into_iter().collect();
        let rows = self.rows.clone();
        let mut active = HashSet::new();
        for row in rows {
            if row.is_dir {
                let slug = self.registry.slug_for(&row.path, &self.root);
                if sessions.contains(&slug) {
                    active.insert(row.path.clone());
                }
            }
        }
        self.active = active;
        Ok(())
    }

    /// True once the inner tmux server reports at least one client (the embedded
    /// PTY). Used at startup to avoid switching before that client has attached.
    pub fn host_client_ready(&mut self) -> bool {
        matches!(self.tmux.host_tty(), Ok(Some(_)))
    }

    fn ensure_host_tty(&mut self) -> io::Result<Option<String>> {
        self.host_tty = self.tmux.host_tty()?;
        Ok(self.host_tty.clone())
    }

    fn ensure_session(&mut self, dir: &Path) -> io::Result<String> {
        let slug = self.registry.slug_for(dir, &self.root);
        if !self.tmux.has_session(&slug)? {
            self.tmux.new_session(&slug, dir, None)?;
        }
        self.active.insert(dir.to_path_buf());
        Ok(slug)
    }

    fn open_dir(&mut self, dir: &Path) -> io::Result<()> {
        let slug = self.ensure_session(dir)?;
        if let Some(tty) = self.ensure_host_tty()? {
            self.tmux.switch_client(&tty, &slug)?;
            self.status = format!("switched to {slug}");
        } else {
            self.status = "no host client to switch".to_string();
        }
        Ok(())
    }

    fn open_file(&mut self, file: &Path) -> io::Result<()> {
        let dir = file.parent().unwrap_or(&self.root).to_path_buf();
        let slug = self.ensure_session(&dir)?;
        let cmd = format!("{} -- {}", self.editor, shell_quote(&file.to_string_lossy()));
        self.tmux.send_keys(&slug, &cmd)?;
        if let Some(tty) = self.ensure_host_tty()? {
            self.tmux.switch_client(&tty, &slug)?;
        }
        self.status = format!("opened {}", file.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{MockRunner, Tmux};
    use std::fs;
    use tempfile::tempdir;

    fn app_over_tempdir() -> (tempfile::TempDir, App<MockRunner>) {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("a.rs"), "x").unwrap();
        let tmux = Tmux::new("runner", MockRunner::new());
        let app = App::new(dir.path().to_path_buf(), tmux, "vi".to_string());
        (dir, app)
    }

    #[test]
    fn open_session_creates_when_absent_then_switches() {
        let (_dir, mut app) = app_over_tempdir();
        // rows[0] = root, rows[1] = src
        app.selected = 1;
        app.tmux.runner.push(false, "");                 // has-session -> false
        app.tmux.runner.push(true, "");                  // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n");    // list-clients (host tty)
        app.tmux.runner.push(true, "");                  // switch-client
        app.open_session().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "has-session");
        assert_eq!(app.tmux.runner.nth_call(1)[2], "new-session");
        assert_eq!(app.tmux.runner.nth_call(2)[2], "list-clients");
        assert_eq!(app.tmux.runner.nth_call(3)[2], "switch-client");
    }

    #[test]
    fn open_session_skips_create_when_present() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1;
        app.tmux.runner.push(true, "");                  // has-session -> true
        app.tmux.runner.push(true, "/dev/ttys009\n");    // list-clients
        app.tmux.runner.push(true, "");                  // switch-client
        app.open_session().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "has-session");
        assert_eq!(app.tmux.runner.nth_call(1)[2], "list-clients");
        assert_eq!(app.tmux.runner.nth_call(2)[2], "switch-client");
        assert_eq!(app.tmux.runner.call_count(), 3);
    }

    #[test]
    fn activate_toggles_directory() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1; // src
        app.activate().unwrap();
        assert!(app.rows.iter().any(|r| r.name == "a.rs"));
        assert_eq!(app.tmux.runner.call_count(), 0); // no tmux for toggle
    }

    #[test]
    fn activate_on_file_opens_editor_in_session() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1;
        app.activate().unwrap(); // expand src, no tmux calls
        let file_idx = app.rows.iter().position(|r| r.name == "a.rs").unwrap();
        app.selected = file_idx;
        app.tmux.runner.push(false, "");                 // has-session(src) -> false
        app.tmux.runner.push(true, "");                  // new-session
        app.tmux.runner.push(true, "");                  // send-keys
        app.tmux.runner.push(true, "/dev/ttys009\n");    // list-clients
        app.tmux.runner.push(true, "");                  // switch-client
        app.activate().unwrap();
        let send = app.tmux.runner.nth_call(2);
        assert_eq!(send[2], "send-keys");
        assert!(send.iter().any(|a| a.contains("vi -- ")));
    }

    #[test]
    fn kill_selected_kills_dir_session() {
        let (_dir, mut app) = app_over_tempdir();
        app.selected = 1;
        app.tmux.runner.push(true, "");
        app.kill_selected().unwrap();
        let call = app.tmux.runner.nth_call(0);
        assert_eq!(call[2], "kill-session");
        assert!(call.contains(&"-t".to_string()), "kill-session must target a session");
    }

    #[test]
    fn host_client_ready_reflects_list_clients() {
        let (_dir, mut app) = app_over_tempdir();
        app.tmux.runner.push(false, "");                // list-clients fails -> no client
        assert!(!app.host_client_ready());
        app.tmux.runner.push(true, "/dev/ttys009\n");   // a client present
        assert!(app.host_client_ready());
    }

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote("a b.rs"), "'a b.rs'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn focus_starts_on_tree_and_toggles() {
        let (_dir, mut app) = app_over_tempdir();
        assert_eq!(app.focus, Focus::Tree);
        app.toggle_focus();
        assert_eq!(app.focus, Focus::Terminal);
        app.toggle_focus();
        assert_eq!(app.focus, Focus::Tree);
    }
}
