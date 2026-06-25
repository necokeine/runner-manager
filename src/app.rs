use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::claude::{self, ResumeSession};
use crate::config::Config;
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
    /// Start a fresh claude session (the default when resumes are offered).
    ResumeNew,
    /// Resume the i-th discovered session in `App::chooser_resumes`.
    Resume(usize),
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
        /// Which existing Claude session to resume: `None` = start fresh,
        /// `Some(i)` = resume `App::chooser_resumes[i]`.
        resume: Option<usize>,
        focus: usize,
    },
    /// "Really close this session?" — opened by `x`/`[×]`, resolved by
    /// `confirm_close`/`cancel_close`. Keyed off the slug (not a row index) so a
    /// periodic `sync` between opening and confirming can't redirect the kill.
    ConfirmClose {
        slug: String,
    },
}

pub struct App<R: CommandRunner> {
    pub tree: Tree,
    pub store: SessionStore,
    pub tmux: Tmux<R>,
    pub config: Config,
    pub root: PathBuf,
    pub selected: usize,
    pub rows: Vec<Row>,
    pub host_tty: Option<String>,
    pub viewer: Option<FileView>,
    pub focus: Focus,
    pub popup: Popup,
    /// Resumable Claude sessions for the directory the chooser is open on,
    /// discovered when the chooser opens. Indexed by `ChooserRow::Resume`.
    pub chooser_resumes: Vec<ResumeSession>,
    pub status: String,
    pub split_pct: u16,
    /// Index of the first tree row shown (scroll position). Reconciled against
    /// the real viewport height in `ui::render`; driven by the mouse wheel.
    pub tree_offset: usize,
    /// Set to a session slug when a switch found no live embedded client (the
    /// terminal PTY died after all sessions exited). The run loop respawns the
    /// PTY attached to this slug so the right pane shows the new session.
    pub pending_respawn: Option<String>,
    /// Slug of the session the embedded terminal client is currently showing.
    /// Set when switching/recovering and reconciled against tmux in `sync`;
    /// used to label the terminal pane with that session's directory.
    pub current_session: Option<String>,
}

impl<R: CommandRunner> App<R> {
    pub fn new(root: PathBuf, tmux: Tmux<R>) -> Self {
        let tree = Tree::new(root.clone());
        let config = Config::new(root.clone());
        let mut app = Self {
            tree,
            store: SessionStore::new(),
            tmux,
            config,
            root,
            selected: 0,
            rows: Vec::new(),
            host_tty: None,
            viewer: None,
            focus: Focus::Tree,
            popup: Popup::None,
            chooser_resumes: Vec::new(),
            status: String::new(),
            split_pct: DEFAULT_SPLIT,
            tree_offset: 0,
            pending_respawn: None,
            current_session: None,
        };
        app.rebuild_rows();
        app
    }

    /// Re-expand the directories saved in the config dir from a previous run.
    /// Called once at startup, after the tree is built.
    pub fn restore_expanded(&mut self) {
        let dirs = self.config.load_expanded();
        self.tree.apply_expanded(&dirs);
        self.rebuild_rows();
    }

    /// Persist the current set of expanded directories to the config dir.
    /// Best-effort: a write failure leaves the tree usable, just not saved.
    pub fn persist_expanded(&self) {
        let _ = self.config.save_expanded(&self.tree.expanded_dirs());
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
                self.persist_expanded();
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
                let dir = row.path.clone();
                // Discover any resumable Claude sessions for this directory now,
                // so the chooser can offer them once the user picks "claude".
                self.chooser_resumes = claude::projects_base()
                    .map(|base| claude::list_sessions(&base, &dir))
                    .unwrap_or_default();
                self.popup = Popup::Chooser {
                    dir,
                    kind: SessionKind::Shell,
                    perm: ClaudePerm::Normal,
                    resume: None,
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
            // Offer the resume picker only when there is history to resume.
            if !self.chooser_resumes.is_empty() {
                rows.push(ChooserRow::ResumeNew);
                for i in 0..self.chooser_resumes.len() {
                    rows.push(ChooserRow::Resume(i));
                }
            }
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
        if let Popup::Chooser { kind, perm, resume, .. } = &mut self.popup {
            match row {
                ChooserRow::KindShell => *kind = SessionKind::Shell,
                ChooserRow::KindClaude => *kind = SessionKind::Claude,
                ChooserRow::PermNormal => *perm = ClaudePerm::Normal,
                ChooserRow::PermSkip => *perm = ClaudePerm::Skip,
                ChooserRow::ResumeNew => *resume = None,
                ChooserRow::Resume(i) => *resume = Some(i),
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
        let Popup::Chooser { dir, kind, perm, resume, focus } = self.popup.clone() else {
            return Ok(());
        };
        let rows = self.chooser_rows();
        match rows.get(focus) {
            Some(ChooserRow::Cancel) => {
                self.popup = Popup::None;
            }
            Some(ChooserRow::Create) => {
                let resume_id = resume
                    .and_then(|i| self.chooser_resumes.get(i))
                    .map(|s| s.id.as_str());
                let cmd = Self::chooser_command(kind, perm, resume_id);
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

    /// Build the launch command for a session. Shell sessions run the default
    /// shell (`None`). Claude sessions run `claude`, optionally resuming an
    /// existing session (`--resume <id>`) and/or skipping permission prompts.
    pub fn chooser_command(
        kind: SessionKind,
        perm: ClaudePerm,
        resume_id: Option<&str>,
    ) -> Option<String> {
        match kind {
            SessionKind::Shell => None,
            SessionKind::Claude => {
                let mut cmd = String::from("claude");
                if let Some(id) = resume_id {
                    cmd.push_str(" --resume ");
                    cmd.push_str(id);
                }
                if perm == ClaudePerm::Skip {
                    cmd.push_str(" --dangerously-skip-permissions");
                }
                Some(cmd)
            }
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
        // The embedded client is about to show this session (either via
        // switch-client below or after a respawn), so it becomes the one the
        // terminal pane is labelled with.
        self.current_session = Some(slug.to_string());
        // Selecting a session means the user wants to work in it, so hand
        // keyboard focus to the terminal pane right away (bug: focus used to
        // stay on the tree after picking a session).
        self.focus = Focus::Right;
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

    /// Close (kill) the session at row `idx`, if that row is a session. As long
    /// as another session remains, the embedded client survives —
    /// `detach-on-destroy off` makes tmux switch it to one of them rather than
    /// detaching, and the next `sync` reconciles `current_session`. The row is
    /// dropped from the store immediately for instant feedback; the periodic
    /// `sync` would otherwise prune it a beat later.
    pub fn close_session(&mut self, idx: usize) -> io::Result<()> {
        let Some(row) = self.rows.get(idx) else {
            return Ok(());
        };
        let RowKind::Session { slug, .. } = &row.kind else {
            return Ok(());
        };
        let slug = slug.clone();
        self.tmux.kill_session(&slug)?;
        self.store.remove(&slug);
        self.status = format!("closed {slug}");
        self.rebuild_rows();
        Ok(())
    }

    /// Ask before closing: if `idx` is a session row, open the confirmation
    /// popup keyed off its slug. Non-session rows are a no-op, mirroring
    /// `close_session`, so no popup ever appears for a dir or file row.
    pub fn request_close(&mut self, idx: usize) {
        let Some(row) = self.rows.get(idx) else {
            return;
        };
        let RowKind::Session { slug, .. } = &row.kind else {
            return;
        };
        self.popup = Popup::ConfirmClose { slug: slug.clone() };
    }

    /// Confirm the pending close: dismiss the popup and kill the session named
    /// by it. The slug is re-resolved to a current row index rather than trusted
    /// as-is, so a `sync` that reshuffled rows can't make us kill the wrong one
    /// (and a session that already exited just no-ops). Not a `ConfirmClose`
    /// popup -> nothing happens.
    pub fn confirm_close(&mut self) -> io::Result<()> {
        let Popup::ConfirmClose { slug } = self.popup.clone() else {
            return Ok(());
        };
        self.popup = Popup::None;
        let idx = self
            .rows
            .iter()
            .position(|r| matches!(&r.kind, RowKind::Session { slug: s, .. } if *s == slug));
        match idx {
            Some(idx) => self.close_session(idx),
            None => Ok(()),
        }
    }

    /// Dismiss the confirmation popup without closing anything.
    pub fn cancel_close(&mut self) {
        self.popup = Popup::None;
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
        // directory). Untagged ones — any hand-made sessions — are left out of
        // the tree.
        let adoptable: Vec<(String, PathBuf, SessionKind)> = infos
            .iter()
            .filter(|i| !i.dir.is_empty())
            .map(|i| (i.name.clone(), PathBuf::from(&i.dir), SessionKind::from_tag(&i.kind)))
            .collect();
        self.store.adopt(&adoptable);
        let live: HashSet<String> = infos.into_iter().map(|i| i.name).collect();
        self.store.sync(&live);
        // Track which session the embedded client actually shows. It can change
        // without a `switch_to` — when the viewed session's shell exits,
        // `detach-on-destroy off` switches the client to another session — so
        // querying tmux keeps the terminal title honest. Keep the last known
        // slug when no client is attached (e.g. during a respawn window).
        if let Some(slug) = self.tmux.client_session()? {
            self.current_session = Some(slug);
        }
        self.rebuild_rows();
        Ok(())
    }

    /// Title for the terminal pane: the literal "terminal" plus the directory
    /// of the session the embedded client is showing, relative to the tree root
    /// (e.g. "terminal — foo/bar" for a session opened on `<root>/foo/bar`).
    /// Falls back to plain "terminal" when no session is shown, the session is
    /// rooted at the tree root, or its directory isn't tracked.
    pub fn terminal_title(&self) -> String {
        let rel = self
            .current_session
            .as_deref()
            .and_then(|slug| self.store.dir_of(slug))
            .map(|dir| dir.strip_prefix(&self.root).unwrap_or(dir).to_string_lossy().into_owned());
        match rel {
            Some(rel) if !rel.is_empty() && rel != "." => format!("terminal — {rel}"),
            _ => "terminal".to_string(),
        }
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
    fn expand_state_persists_across_app_instances() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("a.rs"), "x").unwrap();
        let root = dir.path().to_path_buf();

        // First instance: expanding src/ persists to <root>/.pjma/expanded
        // (activate calls persist_expanded).
        {
            let mut app = App::new(root.clone(), Tmux::new("runner", MockRunner::new()));
            let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
            app.selected = src_idx;
            app.activate().unwrap();
            assert!(app.rows.iter().any(|r| r.label == "a.rs"));
        }

        // A fresh instance over the same root starts collapsed, then restores
        // the saved expand state on startup.
        let mut app2 = App::new(root.clone(), Tmux::new("runner", MockRunner::new()));
        assert!(!app2.rows.iter().any(|r| r.label == "a.rs"));
        app2.restore_expanded();
        assert!(app2.rows.iter().any(|r| r.label == "a.rs"));
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
        // focus starts on the tree before the session is created
        assert_eq!(app.focus, Focus::Tree);
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
        // NEC-13: focus moves to the new session (right pane), not the tree
        assert_eq!(app.focus, Focus::Right);
    }

    #[test]
    fn chooser_create_when_no_client_focuses_new_session() {
        // Fresh start: no embedded client attached yet (list-clients empty), so
        // create falls into the respawn path. Focus must still move to the new
        // session rather than staying on the tree (NEC-13).
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        assert_eq!(app.focus, Focus::Tree);
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, ""); // list-clients -> no host tty
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        // no switch-client issued; the run loop will respawn the PTY into this slug
        assert!(app.pending_respawn.is_some());
        assert_eq!(app.focus, Focus::Right);
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
        // an untagged hand-made session must stay out of the tree.
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

    fn fake_resume(id: &str, last: &str) -> ResumeSession {
        ResumeSession {
            id: id.to_string(),
            last_command: last.to_string(),
            modified: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn claude_chooser_lists_resume_rows_when_history_exists() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        // Inject discovered sessions (open_chooser found none for the tempdir).
        app.chooser_resumes = vec![fake_resume("aaa", "do a thing"), fake_resume("bbb", "do another")];
        app.chooser_move(1); // focus -> claude
        assert_eq!(
            app.chooser_rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::PermNormal,
                ChooserRow::PermSkip,
                ChooserRow::ResumeNew,
                ChooserRow::Resume(0),
                ChooserRow::Resume(1),
                ChooserRow::Cancel,
                ChooserRow::Create,
            ]
        );
        // Switching back to shell hides the resume rows again.
        app.chooser_click(ChooserRow::KindShell).unwrap();
        assert_eq!(
            app.chooser_rows(),
            vec![ChooserRow::KindShell, ChooserRow::KindClaude, ChooserRow::Cancel, ChooserRow::Create]
        );
    }

    #[test]
    fn chooser_create_claude_resume_appends_resume_flag() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_resumes = vec![fake_resume("sess-xyz", "fix the parser")];
        app.chooser_click(ChooserRow::KindClaude).unwrap();
        app.chooser_click(ChooserRow::Resume(0)).unwrap();
        if let Popup::Chooser { resume, .. } = app.popup {
            assert_eq!(resume, Some(0));
        } else {
            panic!("expected chooser");
        }
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_click(ChooserRow::Create).unwrap();
        assert!(app
            .tmux
            .runner
            .nth_call(0)
            .contains(&"claude --resume sess-xyz".to_string()));
    }

    #[test]
    fn chooser_command_maps_kind_and_perm() {
        assert_eq!(App::<MockRunner>::chooser_command(SessionKind::Shell, ClaudePerm::Normal, None), None);
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Normal, None).as_deref(),
            Some("claude")
        );
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Skip, None).as_deref(),
            Some("claude --dangerously-skip-permissions")
        );
        // Resuming an existing session injects --resume <id>, before the perm flag.
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Normal, Some("abc-123")).as_deref(),
            Some("claude --resume abc-123")
        );
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Skip, Some("abc-123")).as_deref(),
            Some("claude --resume abc-123 --dangerously-skip-permissions")
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
    fn activating_a_session_row_moves_focus_to_terminal() {
        // Bug: after selecting a session in the tree, focus must jump to the
        // right (terminal) pane so the user can type into it immediately.
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        focus_create(&mut app);
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_activate().unwrap();
        app.focus = Focus::Tree;
        let sess_idx = app
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session { .. }))
            .unwrap();
        app.selected = sess_idx;
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.activate().unwrap();
        assert_eq!(app.focus, Focus::Right);
    }

    #[test]
    fn activating_a_dir_or_file_keeps_focus_on_tree() {
        // Focus only moves for sessions; expanding a dir or opening a file must
        // leave focus on the tree so the user can keep navigating.
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.activate().unwrap(); // expand dir
        assert_eq!(app.focus, Focus::Tree);
        let file_idx = app.rows.iter().position(|r| r.label == "a.rs").unwrap();
        app.selected = file_idx;
        app.activate().unwrap(); // open file in viewer
        assert_eq!(app.focus, Focus::Tree);
    }

    #[test]
    fn close_session_kills_tmux_and_drops_the_row() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        focus_create(&mut app);
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_activate().unwrap();
        let sess_idx = app
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session { .. }))
            .unwrap();
        let calls_before = app.tmux.runner.call_count();
        app.tmux.runner.push(true, ""); // kill-session
        app.close_session(sess_idx).unwrap();
        // the kill-session call targets the session's slug
        let kill = app.tmux.runner.nth_call(calls_before);
        assert_eq!(kill[2], "kill-session");
        assert_eq!(kill[4], "src-shell");
        // and the session row is gone immediately
        assert!(!app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn request_close_opens_a_confirm_popup_then_confirm_kills() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        focus_create(&mut app);
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_activate().unwrap();
        let sess_idx = app
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session { .. }))
            .unwrap();

        // Requesting a close only opens the popup — no tmux call yet.
        let calls_before = app.tmux.runner.call_count();
        app.request_close(sess_idx);
        assert!(matches!(app.popup, Popup::ConfirmClose { ref slug } if slug == "src-shell"));
        assert_eq!(app.tmux.runner.call_count(), calls_before);
        assert!(app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));

        // Confirming dismisses the popup and kills the session.
        app.tmux.runner.push(true, ""); // kill-session
        app.confirm_close().unwrap();
        let kill = app.tmux.runner.nth_call(calls_before);
        assert_eq!(kill[2], "kill-session");
        assert_eq!(kill[4], "src-shell");
        assert!(matches!(app.popup, Popup::None));
        assert!(!app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn cancel_close_dismisses_without_killing() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        focus_create(&mut app);
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        app.chooser_activate().unwrap();
        let sess_idx = app
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session { .. }))
            .unwrap();

        app.request_close(sess_idx);
        let calls_before = app.tmux.runner.call_count();
        app.cancel_close();
        // Popup gone, session untouched, no tmux call issued.
        assert!(matches!(app.popup, Popup::None));
        assert_eq!(app.tmux.runner.call_count(), calls_before);
        assert!(app.rows.iter().any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn request_close_on_a_non_session_row_opens_no_popup() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.request_close(src_idx);
        assert!(matches!(app.popup, Popup::None));
        assert_eq!(app.tmux.runner.call_count(), 0);
    }

    #[test]
    fn close_session_on_a_non_session_row_is_a_noop() {
        let (_d, mut app) = app_over_tempdir();
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        // src is a directory row, not a session -> nothing happens, no tmux call
        app.close_session(src_idx).unwrap();
        assert_eq!(app.tmux.runner.call_count(), 0);
    }

    #[test]
    fn terminal_title_shows_session_dir_relative_to_root() {
        let (_d, mut app) = app_over_tempdir();
        // Nothing shown yet -> plain title.
        assert_eq!(app.terminal_title(), "terminal");
        // Create a shell session under src/ -> title carries the relative dir.
        let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
        app.selected = src_idx;
        app.open_chooser();
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        assert_eq!(app.terminal_title(), "terminal — src");
    }

    #[test]
    fn terminal_title_is_plain_for_a_root_session() {
        // A session opened on the tree root has an empty relative path, so the
        // title stays the bare "terminal".
        let (_d, mut app) = app_over_tempdir();
        app.selected = 0; // root dir row
        app.open_chooser();
        app.tmux.runner.push(true, ""); // new-session
        app.tmux.runner.push(true, ""); // set-option (@rm tag)
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(true, ""); // switch-client
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        assert_eq!(app.terminal_title(), "terminal");
    }

    #[test]
    fn sync_updates_current_session_from_client() {
        // The embedded client can switch sessions on its own (a viewed shell
        // exits -> detach-on-destroy off moves it elsewhere); sync must adopt
        // the client's real session so the title follows.
        let (_d, mut app) = app_over_tempdir();
        let root = app.root.to_str().unwrap().to_string();
        // list-sessions-full adopts a session under src/, then client_session
        // reports the client is attached to it.
        app.tmux.runner.push(true, &format!("src-shell\tshell {root}/src\n"));
        app.tmux.runner.push(true, "src-shell\n"); // list-clients (client_session)
        app.sync().unwrap();
        assert_eq!(app.current_session.as_deref(), Some("src-shell"));
        assert_eq!(app.terminal_title(), "terminal — src");
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
