//! All application state and the action methods the run loop drives —
//! the view-model layer between the leaf modules ([`crate::project`],
//! [`crate::tmux`], [`crate::term`]) and the renderer ([`crate::ui`]).
//! Submodules: the new-session [`chooser`] form, the derived visible
//! [`rows`], and the right-pane file [`viewer`].

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::app::rows::{build_project_rows, build_rows, Row, RowKind};
use crate::app::viewer::FileView;
use crate::project::config::Config;
use crate::project::git::GitStatuses;
use crate::project::tree::Tree;
use crate::tmux::session::{SessionKind, SessionStore};
use crate::tmux::{CommandRunner, Tmux};

pub mod chooser;
pub mod rows;
#[cfg(test)]
pub(crate) mod testutil;
pub mod viewer;

pub use chooser::{ChooserForm, ChooserGroup, ChooserRow};

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

/// Which pane owns the keyboard: the tree list or the right (terminal/viewer)
/// pane. Toggled with Tab and by activating a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The left tree/session list receives navigation keys.
    Tree,
    /// The right pane receives keys (forwarded to the PTY, or viewer scrolling).
    Right,
}

/// The two views the left pane can show, switched by the tab bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeTab {
    /// The filesystem tree with sessions nested under their directories.
    Directory,
    /// A flat list of every open session (type, directory, brief).
    Project,
}

impl TreeTab {
    /// Human label shown on the tab bar.
    pub fn label(self) -> &'static str {
        match self {
            TreeTab::Directory => "directory",
            TreeTab::Project => "project",
        }
    }
}

/// The modal overlay currently shown, if any. While a popup is open it owns
/// all key and mouse input (see the routing in `run/input.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    /// No popup; input goes to the focused pane.
    None,
    /// The keybinding help overlay.
    Help,
    /// The new-session form. All of its state — the selections, the focus
    /// position, and the discovered resume list — lives in the carried
    /// [`ChooserForm`], so it exists exactly as long as the popup is open.
    Chooser(ChooserForm),
    /// "Really close this session?" — opened by `x`/`[×]`, resolved by
    /// `confirm_close`/`cancel_close`. Keyed off the slug (not a row index) so a
    /// periodic `sync` between opening and confirming can't redirect the kill.
    ConfirmClose { slug: String },
}

/// All application state plus the action methods the run loop drives. Generic
/// over the [`CommandRunner`] so tests can run against a mock tmux.
pub struct App<R: CommandRunner> {
    /// The lazy filesystem tree under `root`.
    pub tree: Tree,
    /// In-memory source of truth for which sessions exist (see `session.rs`).
    pub store: SessionStore,
    /// All tmux interaction, prefixed with this project's socket.
    pub tmux: Tmux<R>,
    /// The per-project config dir (`<root>/.pjma`) and its persisted state.
    pub config: Config,
    /// The tree root — the working directory the app was started in.
    pub root: PathBuf,
    /// Index of the selected row in `rows`.
    pub selected: usize,
    /// The flattened, visible row list — derived state; see [`App::rebuild_rows`].
    pub rows: Vec<Row>,
    /// Which left-pane view is active. The directory tree and the flat session
    /// list share the `rows`/`selected` machinery; `rebuild_rows` fills `rows`
    /// from whichever tab is current.
    pub tab: TreeTab,
    /// One-word brief per session slug (the active pane's foreground command),
    /// refreshed each `sync` and shown in the project view.
    pub briefs: HashMap<String, String>,
    /// `git status` snapshot for the tree root, refreshed each `sync`. Drives
    /// the per-row colour in the directory view; empty when the root is not in
    /// a git repo.
    pub git: GitStatuses,
    /// Whether git-status colouring is enabled. Off by default (see
    /// [`Config::git_status_enabled`]); toggled at runtime with `g` and
    /// persisted. When false the run loop runs no `git status` scans and `git`
    /// stays empty, so the tree renders in its default colours.
    ///
    /// [`Config::git_status_enabled`]: crate::project::config::Config::git_status_enabled
    pub git_enabled: bool,
    /// Cached tty of the embedded tmux client (the `switch-client` target),
    /// refreshed by `ensure_host_tty`. `None` until a client attaches.
    pub host_tty: Option<String>,
    /// When `Some`, the right pane shows this file instead of the terminal.
    pub viewer: Option<FileView>,
    /// Which pane currently receives keyboard input.
    pub focus: Focus,
    /// The modal overlay currently shown, if any.
    pub popup: Popup,
    /// One-line message shown in the status bar (last action or error).
    pub status: String,
    /// Width of the tree pane as a percent of the terminal, clamped 15–80.
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
    /// Every live session name on the socket as of the last `sync` — including
    /// untagged ones the store never adopts. `create_session` dedupes new
    /// slugs against this set, since a name only the tmux server knows about
    /// would make `new-session` fail on every retry.
    pub live_sessions: HashSet<String>,
}

impl<R: CommandRunner> App<R> {
    /// Build the initial state over `root`: tree loaded one level deep,
    /// persisted split width and git toggle restored, no sessions yet (the
    /// first `sync` adopts live ones). Performs no git or tmux I/O.
    pub fn new(root: PathBuf, tmux: Tmux<R>) -> Self {
        let tree = Tree::new(root.clone());
        let config = Config::new(root.clone());
        // Git colouring starts empty and is filled in by the first background
        // scan the run loop kicks off (see `apply_git`). Loading it here would
        // block startup behind a full `git status` of the whole tree — seconds
        // on a parent-of-many-repos root — before the first frame can paint.
        let git = GitStatuses::empty();
        // Git colouring is opt-in and off by default; read the persisted (or
        // env-overridden) toggle so the run loop knows whether to scan.
        let git_enabled = config.git_status_enabled();
        // Restore the saved tree-pane width, clamped into the legal range in
        // case the file was hand-edited; fall back to the default otherwise.
        let split_pct = config
            .load_split()
            .map(|p| p.clamp(MIN_SPLIT, MAX_SPLIT))
            .unwrap_or(DEFAULT_SPLIT);
        let mut app = Self {
            tree,
            store: SessionStore::new(),
            tmux,
            config,
            root,
            selected: 0,
            rows: Vec::new(),
            tab: TreeTab::Directory,
            briefs: HashMap::new(),
            git,
            git_enabled,
            host_tty: None,
            viewer: None,
            focus: Focus::Tree,
            popup: Popup::None,
            status: String::new(),
            split_pct,
            tree_offset: 0,
            pending_respawn: None,
            current_session: None,
            live_sessions: HashSet::new(),
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

    /// Re-derive `rows` from the tree, sessions, and active tab, and clamp the
    /// selection into range. Must be called after any tree expand/collapse or
    /// session change — `rows` is derived state.
    pub fn rebuild_rows(&mut self) {
        self.rows = match self.tab {
            TreeTab::Directory => build_rows(&self.tree.root, &self.store.by_dir()),
            TreeTab::Project => build_project_rows(&self.store.by_dir(), &self.root, &self.briefs),
        };
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    /// Switch the left pane to `tab`, resetting the selection and scroll since
    /// the row set changes meaning between views.
    pub fn set_tab(&mut self, tab: TreeTab) {
        if self.tab != tab {
            self.tab = tab;
            self.selected = 0;
            self.tree_offset = 0;
            self.rebuild_rows();
        }
    }

    /// Cycle between the directory and project views.
    pub fn toggle_tab(&mut self) {
        let next = match self.tab {
            TreeTab::Directory => TreeTab::Project,
            TreeTab::Project => TreeTab::Directory,
        };
        self.set_tab(next);
    }

    /// The currently selected row, if any rows exist.
    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    /// Move the selection up one row, stopping at the top.
    pub fn up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move the selection down one row, stopping at the bottom.
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

    fn create_session(
        &mut self,
        dir: &Path,
        kind: SessionKind,
        command: Option<&str>,
    ) -> io::Result<()> {
        let slug = self
            .store
            .create(dir, &self.root, kind, &self.live_sessions);
        if let Err(e) = self.tmux.new_session(&slug, dir, command) {
            // Roll back the speculative store entry (it would otherwise occupy
            // the slug until the next sync prunes it) and tell the user —
            // callers discard the Err, so the status line is the only feedback.
            self.store.remove(&slug);
            self.status = format!("could not start {}: {e}", kind.label_base());
            return Err(e);
        }
        // Tag the session so a later run can re-adopt it into the tree.
        let _ = self.tmux.tag_session(&slug, dir, kind.label_base());
        self.rebuild_rows();
        self.switch_to(&slug)?;
        self.status = format!("started {}", kind.label_base());
        Ok(())
    }

    fn switch_to(&mut self, slug: &str) -> io::Result<()> {
        // Talk to tmux first and only commit UI state once the client is (or is
        // about to be) showing the session — otherwise a failed switch leaves
        // the pane labelled with a session the client never moved to.
        let switched = self.ensure_host_tty().and_then(|tty| match tty {
            Some(tty) => self.tmux.switch_client(&tty, slug).map(|()| true),
            None => Ok(false),
        });
        match switched {
            Ok(true) => self.status = format!("switched to {slug}"),
            Ok(false) => {
                // No client attached means the embedded terminal PTY exited
                // after the last session was destroyed. Ask the run loop to
                // respawn it attached to this session.
                self.pending_respawn = Some(slug.to_string());
                self.status = "reopening terminal".to_string();
            }
            Err(e) => {
                self.status = format!("switch to {slug} failed: {e}");
                return Err(e);
            }
        }
        self.viewer = None;
        // The embedded client now shows (or will show, after the respawn) this
        // session, so it becomes the one the terminal pane is labelled with.
        self.current_session = Some(slug.to_string());
        // Selecting a session means the user wants to work in it, so hand
        // keyboard focus to the terminal pane right away.
        self.focus = Focus::Right;
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
        // Drop the row whether or not the kill succeeded: the usual failure is
        // "session already exited" (its shell quit inside the confirm window),
        // where removing matches reality; if it was somehow alive, the next
        // sync re-adopts it. Either way the user gets immediate feedback —
        // callers discard the Result, so the status line is the only channel.
        match self.tmux.kill_session(&slug) {
            Ok(()) => self.status = format!("closed {slug}"),
            Err(e) => self.status = format!("close {slug} failed: {e}"),
        }
        self.store.remove(&slug);
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

    /// Show `path` in the right-pane file viewer (replacing the terminal view
    /// until the next session switch).
    pub fn open_file(&mut self, path: &Path) {
        self.viewer = Some(FileView::load(path));
        self.status = format!("viewing {}", path.display());
    }

    /// Scroll the file viewer by one step (or a 10-row page): `delta < 0` is
    /// up, anything else down. No-op when no viewer is open.
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

    /// Reconcile against live tmux state; called ~once/second by the run loop.
    /// Re-adopts tagged sessions from a prior run, prunes rows whose session
    /// exited, refreshes the per-session briefs, and follows the embedded
    /// client's real session (it can move on its own via `detach-on-destroy
    /// off`). Deliberately does **no** git work — see [`App::apply_git`].
    pub fn sync(&mut self) -> io::Result<()> {
        let infos = self.tmux.list_sessions_full()?;
        // Re-adopt sessions this tool created on a prior run (those tagged with a
        // directory). Untagged ones — any hand-made sessions — are left out of
        // the tree.
        let adoptable: Vec<(String, PathBuf, SessionKind)> = infos
            .iter()
            .filter(|i| !i.dir.is_empty())
            .map(|i| {
                (
                    i.name.clone(),
                    PathBuf::from(&i.dir),
                    SessionKind::from_tag(&i.kind),
                )
            })
            .collect();
        self.store.adopt(&adoptable);
        // Refresh the per-session briefs shown in the project view from the
        // live foreground commands.
        self.briefs = infos
            .iter()
            .filter(|i| !i.command.is_empty())
            .map(|i| (i.name.clone(), i.command.clone()))
            .collect();
        let live: HashSet<String> = infos.into_iter().map(|i| i.name).collect();
        self.store.sync(&live);
        // Remember every live name — untagged ones included — so create_session
        // never picks a slug the server already has (see `live_sessions`).
        self.live_sessions = live;
        // Track which session the embedded client actually shows. It can change
        // without a `switch_to` — when the viewed session's shell exits,
        // `detach-on-destroy off` switches the client to another session — so
        // querying tmux keeps the terminal title honest. Keep the last known
        // slug when no client is attached (e.g. during a respawn window).
        if let Some(slug) = self.tmux.client_session()? {
            self.current_session = Some(slug);
        }
        // Git colouring is refreshed separately, off the UI thread (see
        // `apply_git`): a full `git status` scan can take seconds on a large
        // tree, so doing it here would stall the per-second sync and freeze
        // input. `sync` only reconciles sessions.
        self.rebuild_rows();
        Ok(())
    }

    /// Install a git-status snapshot computed off the UI thread; the colours
    /// show on the next frame (`ui::render` styles each row from `self.git`,
    /// so no row rebuild is needed). The run loop calls this when a background
    /// `GitStatuses::load` finishes; the scan never runs inline because it can
    /// take seconds on a parent-of-many-repos tree.
    pub fn apply_git(&mut self, git: GitStatuses) {
        self.git = git;
    }

    /// Toggle git-status colouring, persist the new state, and return whether it
    /// is now enabled. Turning it **off** clears the current colours immediately
    /// (so the tree redraws plain); turning it **on** leaves `git` empty until
    /// the run loop's next background scan lands via [`apply_git`]. Persistence
    /// is best-effort — a write failure still flips the in-memory flag.
    ///
    /// [`apply_git`]: App::apply_git
    pub fn toggle_git_status(&mut self) -> bool {
        self.git_enabled = !self.git_enabled;
        let _ = self.config.save_git_status(self.git_enabled);
        if !self.git_enabled {
            self.git = GitStatuses::empty();
        }
        self.status = if self.git_enabled {
            "git status: on".into()
        } else {
            "git status: off".into()
        };
        self.git_enabled
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
            .map(|dir| {
                dir.strip_prefix(&self.root)
                    .unwrap_or(dir)
                    .to_string_lossy()
                    .into_owned()
            });
        match rel {
            Some(rel) if !rel.is_empty() && rel != "." => format!("terminal — {rel}"),
            _ => "terminal".to_string(),
        }
    }

    /// Move keyboard focus to the other pane.
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

    /// Widen the tree pane by one step, clamped and persisted.
    pub fn widen_split(&mut self) {
        self.split_pct = (self.split_pct + SPLIT_STEP).min(MAX_SPLIT);
        self.persist_split();
    }

    /// Narrow the tree pane by one step, clamped and persisted.
    pub fn narrow_split(&mut self) {
        self.split_pct = self.split_pct.saturating_sub(SPLIT_STEP).max(MIN_SPLIT);
        self.persist_split();
    }

    /// Persist the current tree-pane width to the config dir. Best-effort: a
    /// write failure leaves the layout usable, just not saved.
    pub fn persist_split(&self) {
        let _ = self.config.save_split(self.split_pct);
    }

    /// Whether an embedded tmux client is attached and addressable. A tmux
    /// failure counts as "not ready" — this is a readiness probe used to gate
    /// the respawn path, not an error surface.
    pub fn host_client_ready(&self) -> bool {
        matches!(self.tmux.host_tty(), Ok(Some(_)))
    }

    fn ensure_host_tty(&mut self) -> io::Result<Option<String>> {
        self.host_tty = self.tmux.host_tty()?;
        Ok(self.host_tty.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{
        app_over_tempdir, create_src_shell, focus_create, open_dir_chooser, push_create_seq,
    };
    use super::*;
    use crate::tmux::{MockRunner, Tmux};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn startup_does_not_scan_git_and_apply_git_colours_rows() {
        use crate::project::git::GitStatus;
        use std::process::Command;
        // A real repo with an untracked file: a `git status` scan WOULD colour
        // it. We assert `App::new` does NOT — the scan is deferred to a
        // background thread (run loop) so startup never blocks on `git status`.
        let dir = tempdir().unwrap();
        let root = dir.path();
        let git_cmd = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .map(|o| o.status.success())
        };
        if !matches!(git_cmd(&["init", "-q"]), Ok(true)) {
            return; // git unavailable in this environment — nothing to assert.
        }
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src").join("loose.rs"), "x").unwrap();

        let mut app = App::new(root.to_path_buf(), Tmux::new("runner", MockRunner::new()));
        // Startup left git colouring empty even though the worktree is dirty.
        assert_eq!(app.git.get(&root.join("src")), None);

        // A background scan's result is installed via `apply_git`, which colours
        // the matching rows.
        let src = root.join("src");
        app.apply_git(GitStatuses::from_entries([(
            src.clone(),
            GitStatus::Untracked,
        )]));
        assert_eq!(app.git.get(&src), Some(GitStatus::Untracked));
    }

    #[test]
    fn git_status_off_by_default_and_toggle_persists_and_clears() {
        use crate::project::git::GitStatus;
        let (dir, mut app) = app_over_tempdir();
        // Off by default — no persisted `.pjma/git` file exists.
        assert!(!app.git_enabled);

        // Pretend a background scan had coloured a row.
        let src = dir.path().join("src");
        app.apply_git(GitStatuses::from_entries([(
            src.clone(),
            GitStatus::Untracked,
        )]));

        // Toggle on: the flag flips and the choice is persisted, but colours are
        // left to the next background scan (still present from before here).
        assert!(app.toggle_git_status());
        assert!(app.git_enabled);
        assert!(app.config.git_status_enabled());

        // Toggle off: colours are cleared immediately and the choice persists.
        assert!(!app.toggle_git_status());
        assert!(!app.git_enabled);
        assert_eq!(app.git.get(&src), None);
        assert!(!app.config.git_status_enabled());

        // A fresh App over the same root honours the persisted (off) state.
        let app2 = App::new(
            dir.path().to_path_buf(),
            Tmux::new("runner", MockRunner::new()),
        );
        assert!(!app2.git_enabled);
    }

    #[test]
    fn app_new_honours_persisted_git_status_on() {
        let (dir, app) = app_over_tempdir();
        app.config.save_git_status(true).unwrap();
        let app2 = App::new(
            dir.path().to_path_buf(),
            Tmux::new("runner", MockRunner::new()),
        );
        assert!(app2.git_enabled);
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
    fn split_width_persists_across_app_instances() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // First instance: a width change is saved to <root>/.pjma/split.
        {
            let mut app = App::new(root.clone(), Tmux::new("runner", MockRunner::new()));
            app.widen_split(); // 35 -> 40
            assert_eq!(app.split_pct, 40);
        }

        // A fresh instance over the same root restores the saved width.
        let app2 = App::new(root.clone(), Tmux::new("runner", MockRunner::new()));
        assert_eq!(app2.split_pct, 40);
    }

    #[test]
    fn split_width_falls_back_to_default_when_unsaved() {
        let (_d, app) = app_over_tempdir();
        assert_eq!(app.split_pct, 35);
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
        create_src_shell(&mut app);
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
        // sync with an empty live set -> the session is gone
        app.tmux.runner.push(true, ""); // list-sessions returns nothing
        app.sync().unwrap();
        assert!(!app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn sync_adopts_pre_existing_sessions_into_tree() {
        // Simulates reopening the tool: tmux still has sessions from a prior run.
        // They carry the `@rm` dir tag, so sync must re-adopt and list them, while
        // an untagged hand-made session must stay out of the tree.
        let (_d, mut app) = app_over_tempdir();
        let root = app.root.to_str().unwrap().to_string();
        assert!(!app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
        app.tmux
            .runner
            .push(true, &format!("root-shell\tshell {root}\nscratch\t\n"));
        app.sync().unwrap();
        let sessions: Vec<&Row> = app
            .rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Session { .. }))
            .collect();
        assert_eq!(sessions.len(), 1);
        assert!(matches!(
            sessions[0].kind,
            RowKind::Session {
                kind: SessionKind::Shell,
                ..
            }
        ));
        assert!(!app.rows.iter().any(|r| r.label == "scratch"));
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
    fn create_session_failure_rolls_back_and_reports() {
        // tmux rejecting new-session (e.g. "duplicate session") must not leave
        // a phantom store entry occupying the slug, and the status line must
        // say what happened (callers discard the Err).
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        focus_create(&mut app);
        app.tmux.runner.push(false, ""); // new-session fails
        assert!(app.chooser_activate().is_err());
        assert!(app.store.by_dir().is_empty(), "phantom entry must be gone");
        assert!(app.status.contains("could not start"));
        // A retry gets the base slug again, not a bumped `-2`.
        push_create_seq(&mut app);
        app.open_chooser();
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        assert_eq!(app.current_session.as_deref(), Some("src-shell"));
    }

    #[test]
    fn create_session_steps_over_live_untracked_names() {
        // An untagged session named like our slug lives on the socket (hand
        // made, or its @rm tag was lost). sync never adopts it, but it must
        // still be counted as taken or new-session would fail on every retry.
        let (_d, mut app) = app_over_tempdir();
        app.tmux.runner.push(true, "src-shell\t\tzsh\n"); // list-sessions-full: untagged
        app.tmux.runner.push(true, ""); // list-clients (client_session)
        app.sync().unwrap();
        assert!(app.store.by_dir().is_empty(), "untagged is not adopted");

        open_dir_chooser(&mut app);
        push_create_seq(&mut app);
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        // The new session took the bumped slug, not the occupied one.
        let new_session = app.tmux.runner.nth_call(2);
        assert_eq!(new_session[2], "new-session");
        assert_eq!(new_session[5], "src-shell-2");
    }

    #[test]
    fn switch_to_failure_leaves_ui_state_untouched() {
        // A failed switch-client must not relabel the terminal pane or move
        // focus — the embedded client never moved.
        let (_d, mut app) = app_over_tempdir();
        create_src_shell(&mut app);
        app.current_session = None;
        app.focus = Focus::Tree;
        let sess_idx = app
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session { .. }))
            .unwrap();
        app.selected = sess_idx;
        app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
        app.tmux.runner.push(false, ""); // switch-client fails
        assert!(app.activate().is_err());
        assert_eq!(app.current_session, None);
        assert_eq!(app.focus, Focus::Tree);
        assert!(app.status.contains("failed"));
    }

    #[test]
    fn switch_with_host_client_does_not_request_respawn() {
        let (_d, mut app) = app_over_tempdir();
        create_src_shell(&mut app);
        assert_eq!(app.pending_respawn, None);
    }

    #[test]
    fn activating_a_session_row_moves_focus_to_terminal() {
        // Bug: after selecting a session in the tree, focus must jump to the
        // right (terminal) pane so the user can type into it immediately.
        let (_d, mut app) = app_over_tempdir();
        create_src_shell(&mut app);
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
        create_src_shell(&mut app);
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
        assert_eq!(kill[4], "=src-shell");
        // and the session row is gone immediately
        assert!(!app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn request_close_opens_a_confirm_popup_then_confirm_kills() {
        let (_d, mut app) = app_over_tempdir();
        create_src_shell(&mut app);
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
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));

        // Confirming dismisses the popup and kills the session.
        app.tmux.runner.push(true, ""); // kill-session
        app.confirm_close().unwrap();
        let kill = app.tmux.runner.nth_call(calls_before);
        assert_eq!(kill[2], "kill-session");
        assert_eq!(kill[4], "=src-shell");
        assert!(matches!(app.popup, Popup::None));
        assert!(!app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn cancel_close_dismisses_without_killing() {
        let (_d, mut app) = app_over_tempdir();
        create_src_shell(&mut app);
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
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
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
        create_src_shell(&mut app);
        assert_eq!(app.terminal_title(), "terminal — src");
    }

    #[test]
    fn terminal_title_is_plain_for_a_root_session() {
        // A session opened on the tree root has an empty relative path, so the
        // title stays the bare "terminal".
        let (_d, mut app) = app_over_tempdir();
        app.selected = 0; // root dir row
        app.open_chooser();
        push_create_seq(&mut app);
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
        app.tmux
            .runner
            .push(true, &format!("src-shell\tshell {root}/src\n"));
        app.tmux.runner.push(true, "src-shell\n"); // list-clients (client_session)
        app.sync().unwrap();
        assert_eq!(app.current_session.as_deref(), Some("src-shell"));
        assert_eq!(app.terminal_title(), "terminal — src");
    }

    #[test]
    fn toggle_tab_switches_view_and_lists_open_sessions() {
        let (_d, mut app) = app_over_tempdir();
        // Open a shell session under src so the project view has something to show.
        create_src_shell(&mut app);
        // Give the session a brief, as a sync would.
        app.briefs.insert("src-shell".into(), "zsh".into());

        assert_eq!(app.tab, TreeTab::Directory);
        app.toggle_tab();
        assert_eq!(app.tab, TreeTab::Project);
        // The project view is a flat list of sessions, no directory rows.
        assert!(app
            .rows
            .iter()
            .all(|r| matches!(r.kind, RowKind::Session { .. })));
        let row = app
            .rows
            .iter()
            .find(|r| matches!(&r.kind, RowKind::Session { slug, .. } if slug == "src-shell"))
            .unwrap();
        assert_eq!(row.label, "shell  src  — zsh");
        // Selection resets to the top when switching tabs.
        assert_eq!(app.selected, 0);
        // Switching back restores the directory tree (root dir row present).
        app.toggle_tab();
        assert_eq!(app.tab, TreeTab::Directory);
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Dir { .. })));
    }

    #[test]
    fn project_view_session_row_can_be_closed() {
        let (_d, mut app) = app_over_tempdir();
        create_src_shell(&mut app);
        app.set_tab(TreeTab::Project);
        let sess_idx = app
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session { .. }))
            .unwrap();
        app.tmux.runner.push(true, ""); // kill-session
        app.close_session(sess_idx).unwrap();
        assert!(!app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn sync_populates_briefs_from_pane_command() {
        let (_d, mut app) = app_over_tempdir();
        let root = app.root.to_str().unwrap().to_string();
        // list-sessions-full: a tagged session whose active pane runs `vim`.
        app.tmux
            .runner
            .push(true, &format!("src-shell\tshell {root}/src\tvim\n"));
        app.tmux.runner.push(true, "src-shell\n"); // client_session
        app.sync().unwrap();
        assert_eq!(app.briefs.get("src-shell").map(String::as_str), Some("vim"));
    }
}
