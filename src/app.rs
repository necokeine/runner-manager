use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::rows::{build_rows, Row, RowKind};
use crate::session::{ClaudePerm, SessionKind, SessionStore};
use crate::tmux::{CommandRunner, Tmux};
use crate::tree::Tree;
use crate::viewer::FileView;

const MIN_SPLIT: u16 = 15;
const MAX_SPLIT: u16 = 80;
const SPLIT_STEP: u16 = 5;
const DEFAULT_SPLIT: u16 = 35;

/// Convert a cursor column to a clamped tree-width percent. Zero width
/// returns the default (avoids divide-by-zero during a drag on a 0-wide area).
pub fn col_to_split_pct(col: u16, width: u16) -> u16 {
    if width == 0 {
        return DEFAULT_SPLIT;
    }
    let pct = (col as u32 * 100 / width as u32) as u16;
    pct.clamp(MIN_SPLIT, MAX_SPLIT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserRow {
    KindShell,
    KindClaude,
    PermNormal,
    PermSkip,
    Cancel,
    Create,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    None,
    Help,
    Chooser {
        dir: PathBuf,
        kind: SessionKind,
        perm: ClaudePerm,
        focus: usize,
    },
}

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
    pub split_pct: u16,
    /// Index of the first tree row shown (scroll position). Reconciled against
    /// the real viewport height in `ui::render`; driven by the mouse wheel.
    pub tree_offset: usize,
    /// Set to a session slug when a switch found no live embedded client (the
    /// terminal PTY died after all sessions exited). The run loop respawns the
    /// PTY attached to this slug so the right pane shows the new session.
    pub pending_respawn: Option<String>,
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
            split_pct: DEFAULT_SPLIT,
            tree_offset: 0,
            pending_respawn: None,
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
                    kind: SessionKind::Shell,
                    perm: ClaudePerm::Normal,
                    focus: 0,
                };
            }
        }
    }

    /// Visible focusable rows for the current chooser kind.
    pub fn chooser_rows(&self) -> Vec<ChooserRow> {
        let mut rows = vec![ChooserRow::KindShell, ChooserRow::KindClaude];
        if let Popup::Chooser { kind: SessionKind::Claude, .. } = self.popup {
            rows.push(ChooserRow::PermNormal);
            rows.push(ChooserRow::PermSkip);
        }
        rows.push(ChooserRow::Cancel);
        rows.push(ChooserRow::Create);
        rows
    }

    pub fn chooser_move(&mut self, delta: i32) {
        let rows = self.chooser_rows();
        let Popup::Chooser { focus, .. } = &mut self.popup else {
            return;
        };
        let max = rows.len() as i32 - 1;
        let new_focus = (*focus as i32 + delta).clamp(0, max) as usize;
        *focus = new_focus;
        self.chooser_apply_focus(rows[new_focus]);
    }

    /// Clicking (or focusing) a row selects radios; clicking a button acts.
    pub fn chooser_click(&mut self, row: ChooserRow) -> io::Result<()> {
        if let Some(idx) = self.chooser_rows().iter().position(|r| *r == row) {
            if let Popup::Chooser { focus, .. } = &mut self.popup {
                *focus = idx;
            }
        }
        self.chooser_apply_focus(row);
        if matches!(row, ChooserRow::Cancel | ChooserRow::Create) {
            self.chooser_activate()?;
        }
        Ok(())
    }

    /// Apply the radio-follows-focus rule for the given row.
    fn chooser_apply_focus(&mut self, row: ChooserRow) {
        // Apply radio selection first (this may change `kind`, which changes the row set).
        if let Popup::Chooser { kind, perm, .. } = &mut self.popup {
            match row {
                ChooserRow::KindShell => *kind = SessionKind::Shell,
                ChooserRow::KindClaude => *kind = SessionKind::Claude,
                ChooserRow::PermNormal => *perm = ClaudePerm::Normal,
                ChooserRow::PermSkip => *perm = ClaudePerm::Skip,
                _ => {}
            }
        }
        // Re-clamp focus against the (possibly shrunken) row set.
        let row_count = self.chooser_rows().len();
        if let Popup::Chooser { focus, .. } = &mut self.popup {
            if *focus >= row_count {
                *focus = row_count - 1;
            }
        }
    }

    pub fn chooser_activate(&mut self) -> io::Result<()> {
        let Popup::Chooser { dir, kind, perm, focus } = self.popup.clone() else {
            return Ok(());
        };
        let rows = self.chooser_rows();
        match rows.get(focus) {
            Some(ChooserRow::Cancel) => {
                self.popup = Popup::None;
            }
            Some(ChooserRow::Create) => {
                let cmd = Self::chooser_command(kind, perm);
                self.popup = Popup::None;
                self.create_session(&dir, kind, cmd.as_deref())?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn chooser_cancel(&mut self) {
        self.popup = Popup::None;
    }

    pub fn chooser_command(kind: SessionKind, perm: ClaudePerm) -> Option<String> {
        match kind {
            SessionKind::Shell => None,
            SessionKind::Claude => match perm {
                ClaudePerm::Normal => Some("claude".to_string()),
                ClaudePerm::Skip => Some("claude --dangerously-skip-permissions".to_string()),
            },
        }
    }

    fn create_session(&mut self, dir: &Path, kind: SessionKind, command: Option<&str>) -> io::Result<()> {
        let slug = self.store.create(dir, &self.root, kind);
        self.tmux.new_session(&slug, dir, command)?;
        // Tag the session so a later run can re-adopt it into the tree.
        let _ = self.tmux.tag_session(&slug, dir, kind.label_base());
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
            // No client attached means the embedded terminal PTY exited after
            // the last session was destroyed. Ask the run loop to respawn it
            // attached to this session so the right pane fills again.
            self.pending_respawn = Some(slug.to_string());
            self.status = "reopening terminal".to_string();
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
        let infos = self.tmux.list_sessions_full()?;
        // Re-adopt sessions this tool created on a prior run (those tagged with a
        // directory). Untagged ones — the embedded `scratch` client and any
        // hand-made sessions — are left out of the tree.
        let adoptable: Vec<(String, PathBuf, SessionKind)> = infos
            .iter()
            .filter(|i| !i.dir.is_empty())
            .map(|i| (i.name.clone(), PathBuf::from(&i.dir), SessionKind::from_tag(&i.kind)))
            .collect();
        self.store.adopt(&adoptable);
        let live: HashSet<String> = infos.into_iter().map(|i| i.name).collect();
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

    /// Scroll the tree by `delta` rows within `view_h` visible rows. Keeps the
    /// selection inside the new visible window so the List widget does not snap
    /// the offset back to the selection on the next render.
    pub fn scroll_tree(&mut self, delta: i32, view_h: usize) {
        if self.rows.is_empty() || view_h == 0 {
            return;
        }
        let max_off = self.rows.len().saturating_sub(view_h);
        let new_off = (self.tree_offset as i32 + delta).clamp(0, max_off as i32) as usize;
        self.tree_offset = new_off;
        let last = self.rows.len() - 1;
        let bottom = (new_off + view_h - 1).min(last);
        self.selected = self.selected.clamp(new_off, bottom);
    }

    pub fn widen_split(&mut self) {
        self.split_pct = (self.split_pct + SPLIT_STEP).min(MAX_SPLIT);
    }

    pub fn narrow_split(&mut self) {
        self.split_pct = self.split_pct.saturating_sub(SPLIT_STEP).max(MIN_SPLIT);
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

    fn focus_create(app: &mut App<MockRunner>) {
        let create_idx = app
            .chooser_rows()
            .iter()
            .position(|r| *r == ChooserRow::Create)
            .unwrap();
        if let Popup::Chooser { focus, .. } = &mut app.popup {
            *focus = create_idx;
        }
    }

    #[test]
    fn chooser_create_makes_shell_and_switches() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        assert!(matches!(
            app.popup,
            Popup::Chooser { focus: 0, kind: SessionKind::Shell, .. }
        ));
        // shell: new-session, tag (set-option), list-clients, switch-client
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        assert_eq!(app.tmux.runner.nth_call(0)[2], "new-session");
        assert!(!app.tmux.runner.nth_call(0).contains(&"claude".to_string()));
        assert_eq!(app.tmux.runner.nth_call(1)[2], "set-option");
        assert_eq!(app.tmux.runner.nth_call(2)[2], "list-clients");
        assert_eq!(app.tmux.runner.nth_call(3)[2], "switch-client");
        // a 'shell' session row now exists under src
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. }) && r.label == "shell"));
    }

    #[test]
    fn chooser_create_claude_appends_command() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        app.chooser_move(1); // focus -> claude
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        focus_create(&mut app);
        app.chooser_activate().unwrap();
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
    fn split_widen_and_narrow_clamp() {
        let (_d, mut app) = app_over_tempdir();
        assert_eq!(app.split_pct, 35);
        app.widen_split(); // 40
        app.narrow_split(); // 35
        assert_eq!(app.split_pct, 35);
        for _ in 0..30 {
            app.widen_split();
        }
        assert_eq!(app.split_pct, 80); // clamped high
        for _ in 0..30 {
            app.narrow_split();
        }
        assert_eq!(app.split_pct, 15); // clamped low
    }

    #[test]
    fn scroll_tree_clamps_and_drags_selection_into_view() {
        let (_d, mut app) = app_over_tempdir();
        // 20 synthetic rows so content overflows a small viewport.
        app.rows = (0..20)
            .map(|i| Row {
                path: app.root.clone(),
                label: format!("f{i}"),
                depth: 0,
                kind: RowKind::File,
            })
            .collect();
        app.selected = 0;
        let view_h = 5;
        assert_eq!(app.tree_offset, 0);
        // scroll down past the selection: selection is dragged to the new top
        app.scroll_tree(3, view_h);
        assert_eq!(app.tree_offset, 3);
        assert!(app.selected >= 3 && app.selected < 3 + view_h);
        // can't scroll below the last full page
        app.scroll_tree(1000, view_h);
        assert_eq!(app.tree_offset, app.rows.len() - view_h);
        // can't scroll above the top
        app.scroll_tree(-1000, view_h);
        assert_eq!(app.tree_offset, 0);
        assert!(app.selected < view_h);
    }

    #[test]
    fn scroll_tree_noop_when_content_fits() {
        let (_d, mut app) = app_over_tempdir();
        let big_view = 100; // far taller than the few rows
        app.scroll_tree(5, big_view);
        assert_eq!(app.tree_offset, 0);
    }

    #[test]
    fn col_to_split_pct_clamps_and_is_safe() {
        assert_eq!(col_to_split_pct(50, 100), 50);
        assert_eq!(col_to_split_pct(0, 100), 15); // clamp low
        assert_eq!(col_to_split_pct(99, 100), 80); // clamp high
        assert_eq!(col_to_split_pct(10, 0), 35); // zero width -> default, no panic
    }

    #[test]
    fn sync_prunes_dead_session_rows() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        assert!(app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
        // sync with an empty live set -> the session is gone
        app.tmux.runner.push(true, ""); // list-sessions returns nothing
        app.sync().unwrap();
        assert!(!app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn sync_adopts_pre_existing_sessions_into_tree() {
        // Simulates reopening the tool: tmux still has sessions from a prior run.
        // They carry the `@rm` dir tag, so sync must re-adopt and list them, while
        // the untagged embedded `scratch` client must stay out of the tree.
        let (_d, mut app) = app_over_tempdir();
        let root = app.root.to_str().unwrap().to_string();
        assert!(!app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
        app.tmux.runner.push(true, &format!("root-shell\tshell {root}\nscratch\t\n"));
        app.sync().unwrap();
        let sessions: Vec<&Row> = app
            .rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Session { .. }))
            .collect();
        assert_eq!(sessions.len(), 1);
        assert!(matches!(sessions[0].kind, RowKind::Session { kind: SessionKind::Shell, .. }));
        assert!(!app.rows.iter().any(|r| r.label == "scratch"));
    }

    use crate::session::ClaudePerm;

    fn open_dir_chooser(app: &mut App<MockRunner>) {
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
    }

    #[test]
    fn chooser_defaults_to_shell_with_no_perm_rows() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        assert_eq!(
            app.chooser_rows(),
            vec![ChooserRow::KindShell, ChooserRow::KindClaude, ChooserRow::Cancel, ChooserRow::Create]
        );
    }

    #[test]
    fn focusing_claude_reveals_perm_rows_and_selects_it() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_move(1); // focus -> claude
        if let Popup::Chooser { kind, .. } = app.popup {
            assert_eq!(kind, SessionKind::Claude);
        } else {
            panic!("expected chooser");
        }
        assert_eq!(
            app.chooser_rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::PermNormal,
                ChooserRow::PermSkip,
                ChooserRow::Cancel,
                ChooserRow::Create
            ]
        );
    }

    #[test]
    fn focusing_skip_sets_perm_then_back_to_shell_reclamps() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_move(1); // claude
        app.chooser_move(1); // normal
        app.chooser_move(1); // skip
        if let Popup::Chooser { perm, .. } = app.popup {
            assert_eq!(perm, ClaudePerm::Skip);
        } else {
            panic!();
        }
        // move focus up to shell -> kind becomes Shell, perm rows vanish, focus valid
        app.chooser_move(-3); // skip(3) -> shell(0)
        if let Popup::Chooser { kind, focus, .. } = app.popup {
            assert_eq!(kind, SessionKind::Shell);
            assert!(focus < app.chooser_rows().len());
        } else {
            panic!();
        }
    }

    #[test]
    fn chooser_command_maps_kind_and_perm() {
        assert_eq!(App::<MockRunner>::chooser_command(SessionKind::Shell, ClaudePerm::Normal), None);
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Normal).as_deref(),
            Some("claude")
        );
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Skip).as_deref(),
            Some("claude --dangerously-skip-permissions")
        );
    }

    #[test]
    fn chooser_activate_create_starts_claude_skip() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_move(1); // claude
        app.chooser_move(2); // skip
        // move focus to Create (rows: shell,claude,normal,skip,Cancel,Create => Create index 5)
        let create_idx = app.chooser_rows().iter().position(|r| *r == ChooserRow::Create).unwrap();
        if let Popup::Chooser { focus, .. } = &mut app.popup {
            *focus = create_idx;
        }
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_activate().unwrap();
        let call = app.tmux.runner.nth_call(0);
        assert_eq!(call[2], "new-session");
        assert!(call.contains(&"claude --dangerously-skip-permissions".to_string()));
        assert_eq!(app.popup, Popup::None);
    }

    #[test]
    fn create_session_without_host_client_requests_respawn() {
        // Simulates "all sessions quit -> embedded client dead": list-clients
        // returns nothing, so switch_to finds no client and asks the run loop
        // to respawn the terminal PTY attached to the new session.
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        focus_create(&mut app);
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, ""); // list-clients -> empty (no host client)
        app.chooser_activate().unwrap();
        assert_eq!(app.pending_respawn.as_deref(), Some("src-shell"));
        assert_eq!(app.tmux.runner.nth_call(0)[2], "new-session");
        assert_eq!(app.tmux.runner.nth_call(1)[2], "set-option");
        assert_eq!(app.tmux.runner.nth_call(2)[2], "list-clients");
    }

    #[test]
    fn switch_with_host_client_does_not_request_respawn() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        focus_create(&mut app);
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients -> a client
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_activate().unwrap();
        assert_eq!(app.pending_respawn, None);
    }

    #[test]
    fn chooser_activate_cancel_closes_without_tmux() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        let cancel_idx = app.chooser_rows().iter().position(|r| *r == ChooserRow::Cancel).unwrap();
        if let Popup::Chooser { focus, .. } = &mut app.popup {
            *focus = cancel_idx;
        }
        app.chooser_activate().unwrap();
        assert_eq!(app.popup, Popup::None);
        assert_eq!(app.tmux.runner.call_count(), 0);
    }
}
