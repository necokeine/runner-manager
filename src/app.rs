use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::rows::{build_rows, Row, RowKind};
use crate::session::{SessionKind, SessionStore};
use crate::tmux::{CommandRunner, Tmux};
use crate::tree::Tree;
use crate::viewer::FileView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    None,
    Help,
    Chooser { dir: PathBuf, selected: usize },
}

/// The kinds offered by the chooser, in display order.
pub const CHOOSER_KINDS: [SessionKind; 2] = [SessionKind::Shell, SessionKind::Claude];

pub struct App<R: CommandRunner> {
    pub tree: Tree,
    pub store: SessionStore,
    pub tmux: Tmux<R>,
    pub root: PathBuf,
    pub selected: usize,
    pub rows: Vec<Row>,
    pub host_tty: Option<String>,
    pub viewer: Option<FileView>,
    pub focus: Focus,
    pub popup: Popup,
    pub status: String,
}

impl<R: CommandRunner> App<R> {
    pub fn new(root: PathBuf, tmux: Tmux<R>) -> Self {
        let tree = Tree::new(root.clone());
        let mut app = Self {
            tree,
            store: SessionStore::new(),
            tmux,
            root,
            selected: 0,
            rows: Vec::new(),
            host_tty: None,
            viewer: None,
            focus: Focus::Tree,
            popup: Popup::None,
            status: String::new(),
        };
        app.rebuild_rows();
        app
    }

    pub fn rebuild_rows(&mut self) {
        self.rows = build_rows(&self.tree.root, &self.store.by_dir());
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

    /// Enter/click on the selected row, dispatched by kind.
    pub fn activate(&mut self) -> io::Result<()> {
        let Some(row) = self.selected_row().cloned() else {
            return Ok(());
        };
        match row.kind {
            RowKind::Dir { .. } => {
                if let Some(node) = self.tree.node_at_mut(&row.path) {
                    node.toggle();
                }
                self.rebuild_rows();
            }
            RowKind::Session { slug, .. } => {
                self.switch_to(&slug)?;
            }
            RowKind::File => {
                self.open_file(&row.path);
            }
        }
        Ok(())
    }

    /// Open the shell/claude chooser for the selected directory row.
    pub fn open_chooser(&mut self) {
        if let Some(row) = self.selected_row() {
            if matches!(row.kind, RowKind::Dir { .. }) {
                self.popup = Popup::Chooser {
                    dir: row.path.clone(),
                    selected: 0,
                };
            }
        }
    }

    pub fn chooser_move(&mut self, delta: i32) {
        if let Popup::Chooser { selected, .. } = &mut self.popup {
            let n = CHOOSER_KINDS.len() as i32;
            *selected = (((*selected as i32 + delta) % n + n) % n) as usize;
        }
    }

    pub fn chooser_confirm(&mut self) -> io::Result<()> {
        if let Popup::Chooser { dir, selected } = self.popup.clone() {
            let kind = CHOOSER_KINDS[selected];
            self.popup = Popup::None;
            self.create_session(&dir, kind)?;
        }
        Ok(())
    }

    pub fn chooser_cancel(&mut self) {
        self.popup = Popup::None;
    }

    fn create_session(&mut self, dir: &Path, kind: SessionKind) -> io::Result<()> {
        let slug = self.store.create(dir, &self.root, kind);
        self.tmux.new_session(&slug, dir, kind.command())?;
        self.rebuild_rows();
        self.switch_to(&slug)?;
        self.status = format!("started {}", kind.label_base());
        Ok(())
    }

    fn switch_to(&mut self, slug: &str) -> io::Result<()> {
        self.viewer = None;
        if let Some(tty) = self.ensure_host_tty()? {
            self.tmux.switch_client(&tty, slug)?;
            self.status = format!("switched to {slug}");
        } else {
            self.status = "no host client to switch".to_string();
        }
        Ok(())
    }

    pub fn open_file(&mut self, path: &Path) {
        self.viewer = Some(FileView::load(path));
        self.status = format!("viewing {}", path.display());
    }

    pub fn viewer_scroll(&mut self, delta: i32, page: bool) {
        if let Some(v) = &mut self.viewer {
            let step = if page { 10 } else { 1 };
            if delta < 0 {
                v.scroll_up(step);
            } else {
                v.scroll_down(step);
            }
        }
    }

    pub fn sync(&mut self) -> io::Result<()> {
        let live: HashSet<String> = self.tmux.list_sessions()?.into_iter().collect();
        self.store.sync(&live);
        self.rebuild_rows();
        Ok(())
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Right,
            Focus::Right => Focus::Tree,
        };
    }

    pub fn host_client_ready(&mut self) -> bool {
        matches!(self.tmux.host_tty(), Ok(Some(_)))
    }

    fn ensure_host_tty(&mut self) -> io::Result<Option<String>> {
        self.host_tty = self.tmux.host_tty()?;
        Ok(self.host_tty.clone())
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
        let app = App::new(dir.path().to_path_buf(), tmux);
        (dir, app)
    }

    #[test]
    fn activate_dir_toggles_expand() {
        let (_d, mut app) = app_over_tempdir();
        // rows[0] = root dir (expanded). Select the 'src' dir row.
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.activate().unwrap();
        assert!(app.rows.iter().any(|r| r.label == "a.rs"));
        assert_eq!(app.tmux.runner.call_count(), 0); // no tmux for expand
    }

    #[test]
    fn chooser_confirm_creates_shell_and_switches() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        assert!(matches!(app.popup, Popup::Chooser { selected: 0, .. }));
        // shell (index 0): new-session (no command), then list-clients, then switch-client
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_confirm().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "new-session");
        assert!(!app.tmux.runner.nth_call(0).contains(&"claude".to_string()));
        assert_eq!(app.tmux.runner.nth_call(1)[2], "list-clients");
        assert_eq!(app.tmux.runner.nth_call(2)[2], "switch-client");
        // a 'shell' session row now exists under src
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. }) && r.label == "shell"));
    }

    #[test]
    fn chooser_confirm_claude_appends_command() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        app.chooser_move(1); // select claude (index 1)
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_confirm().unwrap();
        assert!(app.tmux.runner.nth_call(0).contains(&"claude".to_string()));
    }

    #[test]
    fn activate_file_opens_viewer_no_tmux() {
        let (_d, mut app) = app_over_tempdir();
        // expand src so a.rs is visible
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.activate().unwrap();
        let file_idx = app.rows.iter().position(|r| r.label == "a.rs").unwrap();
        app.selected = file_idx;
        app.activate().unwrap();
        assert!(app.viewer.is_some());
        assert_eq!(app.tmux.runner.call_count(), 0);
    }

    #[test]
    fn sync_prunes_dead_session_rows() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_confirm().unwrap();
        assert!(app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
        // sync with an empty live set -> the session is gone
        app.tmux.runner.push(true, ""); // list-sessions returns nothing
        app.sync().unwrap();
        assert!(!app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
    }
}
