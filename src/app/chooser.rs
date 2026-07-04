//! The new-session chooser: a small radio-form state machine opened on a
//! directory row. All form state — the chosen dir/kind/permission, the resume
//! picker and its discovered sessions, and the focus position — lives in one
//! [`ChooserForm`] carried by `Popup::Chooser`, so the form cannot outlive or
//! drift from its popup. The form is navigated in two axes — Up/Down moves
//! between selection groups, Left/Right changes the option within the focused
//! group — and which groups exist depends on the chosen kind (Perm/Resume are
//! claude-only). [`launch_command`] maps the final selections to the launch
//! command for the new session.

use std::io;
use std::path::PathBuf;

use crate::app::rows::RowKind;
use crate::project::claude::{self, ResumeId, ResumeSession};
use crate::tmux::session::{ClaudePerm, SessionKind};
use crate::tmux::CommandRunner;

use super::{App, Popup};

/// One focusable option in the new-session form; the visible set is derived
/// per-frame by [`ChooserForm::rows`] (Perm/Resume rows only exist for claude).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserRow {
    /// Kind radio: plain shell.
    KindShell,
    /// Kind radio: Claude Code.
    KindClaude,
    /// Kind radio: Codex.
    KindCodex,
    /// Permission radio: normal prompts.
    PermNormal,
    /// Permission radio: `--dangerously-skip-permissions`.
    PermSkip,
    /// Start a fresh claude session (the default when resumes are offered).
    ResumeNew,
    /// Resume the i-th discovered session in [`ChooserForm::resumes`].
    Resume(usize),
    /// The `[ Cancel ]` button.
    Cancel,
    /// The `[ Create ]` button.
    Create,
}

/// A selection group in the new-session form. The form is navigated in two
/// axes: **Up/Down** moves between groups, **Left/Right** changes the option
/// within the focused group. Which groups are present depends on the chosen
/// kind (Perm/Resume only show for claude) — see [`ChooserForm::groups`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserGroup {
    /// shell · claude · codex
    Kind,
    /// normal · skip (claude only)
    Perm,
    /// new session · one entry per resumable transcript (claude only)
    Resume,
    /// `[ Cancel ]` · `[ Create ]`
    Actions,
}

/// The complete state of one open new-session form. Owned by
/// `Popup::Chooser`, so it exists exactly as long as the popup is shown and
/// the resume list can never go stale against a different directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChooserForm {
    /// The directory the session will be created in.
    pub dir: PathBuf,
    /// Selected session kind (the Kind radio group).
    pub kind: SessionKind,
    /// Selected Claude permission mode (the Perm radio group; claude-only).
    pub perm: ClaudePerm,
    /// Which existing Claude session to resume: `None` = start fresh,
    /// `Some(i)` = resume `resumes[i]`.
    pub resume: Option<usize>,
    /// Which selection group has focus (moved by Up/Down). The selected
    /// option within each radio group lives in `kind`/`perm`/`resume`; the
    /// focused button within `Actions` is `action`.
    pub group: ChooserGroup,
    /// Focused action button: `false` = Cancel, `true` = Create.
    pub action: bool,
    /// Resumable Claude sessions discovered for `dir` when the form opened.
    /// Indexed by `ChooserRow::Resume`.
    pub resumes: Vec<ResumeSession>,
}

impl ChooserForm {
    /// A fresh form for `dir` with the defaults selected: a plain shell,
    /// normal permissions, no resume, focus on the Kind group.
    pub fn new(dir: PathBuf, resumes: Vec<ResumeSession>) -> Self {
        Self {
            dir,
            kind: SessionKind::Shell,
            perm: ClaudePerm::Normal,
            resume: None,
            group: ChooserGroup::Kind,
            action: true,
            resumes,
        }
    }

    /// Visible focusable rows for the current kind.
    pub fn rows(&self) -> Vec<ChooserRow> {
        let mut rows = vec![
            ChooserRow::KindShell,
            ChooserRow::KindClaude,
            ChooserRow::KindCodex,
        ];
        if self.kind == SessionKind::Claude {
            rows.push(ChooserRow::PermNormal);
            rows.push(ChooserRow::PermSkip);
            // Offer the resume picker only when there is history to resume.
            if !self.resumes.is_empty() {
                rows.push(ChooserRow::ResumeNew);
                for i in 0..self.resumes.len() {
                    rows.push(ChooserRow::Resume(i));
                }
            }
        }
        rows.push(ChooserRow::Cancel);
        rows.push(ChooserRow::Create);
        rows
    }

    /// The selection groups present for the current kind, in display order.
    /// Perm and Resume only exist for claude (and Resume only when this
    /// directory has history). Up/Down navigation walks this list.
    pub fn groups(&self) -> Vec<ChooserGroup> {
        let mut groups = vec![ChooserGroup::Kind];
        if self.kind == SessionKind::Claude {
            groups.push(ChooserGroup::Perm);
            if !self.resumes.is_empty() {
                groups.push(ChooserGroup::Resume);
            }
        }
        groups.push(ChooserGroup::Actions);
        groups
    }

    /// The `ChooserRow` that currently has focus: the selected option of the
    /// focused radio group, or the focused button in `Actions`. Drives both
    /// the render highlight and what `Enter`/`Space` act on.
    pub fn focus_row(&self) -> ChooserRow {
        match self.group {
            ChooserGroup::Kind => match self.kind {
                SessionKind::Shell => ChooserRow::KindShell,
                SessionKind::Claude => ChooserRow::KindClaude,
                SessionKind::Codex => ChooserRow::KindCodex,
            },
            ChooserGroup::Perm => match self.perm {
                ClaudePerm::Normal => ChooserRow::PermNormal,
                ClaudePerm::Skip => ChooserRow::PermSkip,
            },
            ChooserGroup::Resume => match self.resume {
                None => ChooserRow::ResumeNew,
                Some(i) => ChooserRow::Resume(i),
            },
            ChooserGroup::Actions => {
                if self.action {
                    ChooserRow::Create
                } else {
                    ChooserRow::Cancel
                }
            }
        }
    }

    /// Move focus between groups by `delta` (Up/Down — clamps at the ends).
    pub fn group_move(&mut self, delta: i32) {
        self.group_step(delta, false);
    }

    /// Cycle focus between groups by `delta` (Tab/Shift-Tab — wraps past the
    /// ends so every group is reachable with one key).
    pub fn group_cycle(&mut self, delta: i32) {
        self.group_step(delta, true);
    }

    fn group_step(&mut self, delta: i32, wrap: bool) {
        let groups = self.groups();
        let Some(pos) = groups.iter().position(|g| *g == self.group) else {
            return;
        };
        let len = groups.len() as i32;
        let next = if wrap {
            (((pos as i32 + delta) % len) + len) % len
        } else {
            (pos as i32 + delta).clamp(0, len - 1)
        } as usize;
        self.group = groups[next];
    }

    /// Change the selected option within the focused group by `delta`
    /// (Left/Right — clamps within the group). Switching `kind` here may add
    /// or remove the Perm/Resume groups, which is fine: focus stays on `Kind`.
    pub fn option_move(&mut self, delta: i32) {
        match self.group {
            ChooserGroup::Kind => {
                // shell · claude · codex (clamped).
                let cur = match self.kind {
                    SessionKind::Shell => 0,
                    SessionKind::Claude => 1,
                    SessionKind::Codex => 2,
                };
                self.kind = match (cur + delta).clamp(0, 2) {
                    0 => SessionKind::Shell,
                    1 => SessionKind::Claude,
                    _ => SessionKind::Codex,
                };
            }
            ChooserGroup::Perm => {
                if delta > 0 {
                    self.perm = ClaudePerm::Skip;
                } else if delta < 0 {
                    self.perm = ClaudePerm::Normal;
                }
            }
            ChooserGroup::Resume => {
                // Option index: 0 = new (None), 1..=n = Some(i-1).
                let cur = match self.resume {
                    None => 0,
                    Some(i) => i as i32 + 1,
                };
                let next = (cur + delta).clamp(0, self.resumes.len() as i32);
                self.resume = if next == 0 {
                    None
                } else {
                    Some((next - 1) as usize)
                };
            }
            ChooserGroup::Actions => {
                if delta > 0 {
                    self.action = true;
                } else if delta < 0 {
                    self.action = false;
                }
            }
        }
    }

    /// Select the option `row` and focus its group — the state half of a
    /// mouse click. (Acting on the `Cancel`/`Create` buttons needs the app
    /// and lives in `App::chooser_click`.)
    pub fn select(&mut self, row: ChooserRow) {
        match row {
            ChooserRow::KindShell => {
                self.kind = SessionKind::Shell;
                self.group = ChooserGroup::Kind;
            }
            ChooserRow::KindClaude => {
                self.kind = SessionKind::Claude;
                self.group = ChooserGroup::Kind;
            }
            ChooserRow::KindCodex => {
                self.kind = SessionKind::Codex;
                self.group = ChooserGroup::Kind;
            }
            ChooserRow::PermNormal => {
                self.perm = ClaudePerm::Normal;
                self.group = ChooserGroup::Perm;
            }
            ChooserRow::PermSkip => {
                self.perm = ClaudePerm::Skip;
                self.group = ChooserGroup::Perm;
            }
            ChooserRow::ResumeNew => {
                self.resume = None;
                self.group = ChooserGroup::Resume;
            }
            ChooserRow::Resume(i) => {
                self.resume = Some(i);
                self.group = ChooserGroup::Resume;
            }
            ChooserRow::Cancel => {
                self.action = false;
                self.group = ChooserGroup::Actions;
            }
            ChooserRow::Create => {
                self.action = true;
                self.group = ChooserGroup::Actions;
            }
        }
    }

    /// The launch command for the form's current selections (see
    /// [`launch_command`]).
    pub fn command(&self) -> Option<String> {
        let resume_id = self.resume.and_then(|i| self.resumes.get(i)).map(|s| &s.id);
        launch_command(self.kind, self.perm, resume_id)
    }
}

/// Build the launch command for a session. Shell sessions run the default
/// shell (`None`). Claude sessions run `claude`, optionally resuming an
/// existing session (`--resume <id>`) and/or skipping permission prompts.
/// Codex sessions run `codex` (the perm/resume inputs are claude-only and
/// ignored). The command string is handed to a shell by tmux; splicing the
/// resume id is safe because a [`ResumeId`] is shell-safe by construction.
pub fn launch_command(
    kind: SessionKind,
    perm: ClaudePerm,
    resume_id: Option<&ResumeId>,
) -> Option<String> {
    match kind {
        SessionKind::Shell => None,
        SessionKind::Codex => Some(String::from("codex")),
        SessionKind::Claude => {
            let mut cmd = String::from("claude");
            if let Some(id) = resume_id {
                cmd.push_str(" --resume ");
                cmd.push_str(id.as_str());
            }
            if perm == ClaudePerm::Skip {
                cmd.push_str(" --dangerously-skip-permissions");
            }
            Some(cmd)
        }
    }
}

impl<R: CommandRunner> App<R> {
    /// Open the new-session chooser for the selected directory row.
    pub fn open_chooser(&mut self) {
        if let Some(row) = self.selected_row() {
            if matches!(row.kind, RowKind::Dir { .. }) {
                let dir = row.path.clone();
                // Discover any resumable Claude sessions for this directory now,
                // so the chooser can offer them once the user picks "claude".
                let resumes = claude::projects_base()
                    .map(|base| claude::list_sessions(&base, &dir))
                    .unwrap_or_default();
                self.popup = Popup::Chooser(ChooserForm::new(dir, resumes));
            }
        }
    }

    /// Clicking a row focuses its group, selects that option, and — for the
    /// `Cancel`/`Create` buttons — acts immediately.
    pub fn chooser_click(&mut self, row: ChooserRow) -> io::Result<()> {
        if let Popup::Chooser(form) = &mut self.popup {
            form.select(row);
        }
        if matches!(row, ChooserRow::Cancel | ChooserRow::Create) {
            self.chooser_activate()?;
        }
        Ok(())
    }

    /// Act on the focused row (Space / click): the `Cancel`/`Create` buttons
    /// fire; radio options are no-ops here (Left/Right already selected them).
    /// For "Enter anywhere creates", see [`App::chooser_commit`].
    pub fn chooser_activate(&mut self) -> io::Result<()> {
        let Popup::Chooser(form) = &self.popup else {
            return Ok(());
        };
        match form.focus_row() {
            ChooserRow::Cancel => self.popup = Popup::None,
            ChooserRow::Create => self.create_from_form()?,
            _ => {}
        }
        Ok(())
    }

    /// Commit the form (Enter): create the session with the current selections
    /// no matter which group has focus, so the user need not travel to
    /// `[ Create ]`. Enter while parked on `Cancel` still cancels — least surprise.
    pub fn chooser_commit(&mut self) -> io::Result<()> {
        let Popup::Chooser(form) = &self.popup else {
            return Ok(());
        };
        if form.focus_row() == ChooserRow::Cancel {
            self.popup = Popup::None;
            return Ok(());
        }
        self.create_from_form()
    }

    /// Close the popup, taking its form, and start the session it describes.
    /// Shared by [`App::chooser_activate`] (Create button) and
    /// [`App::chooser_commit`] (Enter).
    fn create_from_form(&mut self) -> io::Result<()> {
        let Popup::Chooser(form) = std::mem::replace(&mut self.popup, Popup::None) else {
            return Ok(());
        };
        let cmd = form.command();
        self.create_session(&form.dir, form.kind, cmd.as_deref())
    }

    /// Dismiss the chooser without creating anything.
    pub fn chooser_cancel(&mut self) {
        self.popup = Popup::None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{
        app_over_tempdir, fake_resume, focus_create, open_dir_chooser, push_create_seq,
    };
    use crate::app::Focus;
    use crate::tmux::MockRunner;

    /// The open chooser form, or a panic when no chooser is open.
    fn form(app: &App<MockRunner>) -> &ChooserForm {
        match &app.popup {
            Popup::Chooser(form) => form,
            other => panic!("expected an open chooser, got {other:?}"),
        }
    }

    fn form_mut(app: &mut App<MockRunner>) -> &mut ChooserForm {
        match &mut app.popup {
            Popup::Chooser(form) => form,
            other => panic!("expected an open chooser, got {other:?}"),
        }
    }

    #[test]
    fn chooser_create_makes_shell_and_switches() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        assert_eq!(form(&app).group, ChooserGroup::Kind);
        assert_eq!(form(&app).kind, SessionKind::Shell);
        push_create_seq(&mut app);
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
        open_dir_chooser(&mut app);
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
        open_dir_chooser(&mut app);
        form_mut(&mut app).option_move(1); // Kind group: shell -> claude
        push_create_seq(&mut app);
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        assert!(app.tmux.runner.nth_call(0).contains(&"claude".to_string()));
    }

    #[test]
    fn chooser_commit_creates_without_focusing_create() {
        // NEC-29: Enter creates from any group — here focus stays on the Kind
        // group (shell selected), no travelling down to [ Create ].
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        assert_eq!(form(&app).group, ChooserGroup::Kind);
        push_create_seq(&mut app);
        app.chooser_commit().unwrap();
        assert!(matches!(app.popup, Popup::None));
        assert_eq!(app.tmux.runner.nth_call(0)[2], "new-session");
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. }) && r.label == "shell"));
    }

    #[test]
    fn chooser_commit_on_cancel_row_cancels() {
        // Enter while parked on Cancel must cancel, not create.
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        // Focus the Actions group on the Cancel button.
        let form = form_mut(&mut app);
        form.group = ChooserGroup::Actions;
        form.action = false;
        app.chooser_commit().unwrap();
        assert!(matches!(app.popup, Popup::None));
        assert_eq!(app.tmux.runner.call_count(), 0); // no session created
    }

    #[test]
    fn form_group_move_navigates_groups_and_clamps() {
        // claude reveals Perm; Up/Down walk groups (Kind -> Perm -> Actions),
        // and Down at the last group clamps.
        let mut form = ChooserForm::new(PathBuf::from("/p"), Vec::new());
        form.option_move(1); // Kind -> claude (reveals Perm group)
        assert_eq!(form.group, ChooserGroup::Kind);
        form.group_move(1); // -> Perm
        assert_eq!(form.group, ChooserGroup::Perm);
        form.group_move(1); // -> Actions (no resumes here)
        assert_eq!(form.group, ChooserGroup::Actions);
        form.group_move(1); // clamp at the end
        assert_eq!(form.group, ChooserGroup::Actions);
        form.group_move(-10); // clamp at the start
        assert_eq!(form.group, ChooserGroup::Kind);
    }

    #[test]
    fn form_option_move_changes_selection_within_group() {
        let mut form = ChooserForm::new(PathBuf::from("/p"), Vec::new());
        // Kind group: Right -> claude, Left -> shell (clamps).
        form.option_move(1);
        assert_eq!(form.kind, SessionKind::Claude);
        form.option_move(-5);
        assert_eq!(form.kind, SessionKind::Shell);
        // Back to claude, then walk into Perm and toggle normal -> skip.
        form.option_move(1);
        form.group_move(1); // -> Perm
        form.option_move(1); // normal -> skip
        assert_eq!(form.perm, ClaudePerm::Skip);
        form.option_move(-1); // skip -> normal
        assert_eq!(form.perm, ClaudePerm::Normal);
    }

    #[test]
    fn form_group_cycle_wraps_around_ends() {
        // shell: groups are [Kind, Actions].
        let mut form = ChooserForm::new(PathBuf::from("/p"), Vec::new());
        // Shift-Tab from the first group wraps to the last (Actions).
        form.group_cycle(-1);
        assert_eq!(form.group, ChooserGroup::Actions);
        // Tab from the last group wraps back to the first (Kind).
        form.group_cycle(1);
        assert_eq!(form.group, ChooserGroup::Kind);
    }

    #[test]
    fn form_defaults_to_shell_with_no_perm_rows() {
        let form = ChooserForm::new(PathBuf::from("/p"), Vec::new());
        assert_eq!(
            form.rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::KindCodex,
                ChooserRow::Cancel,
                ChooserRow::Create
            ]
        );
    }

    #[test]
    fn focusing_claude_reveals_perm_rows_and_selects_it() {
        let mut form = ChooserForm::new(PathBuf::from("/p"), Vec::new());
        form.option_move(1); // Kind group: shell -> claude
        assert_eq!(form.kind, SessionKind::Claude);
        assert_eq!(
            form.rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::KindCodex,
                ChooserRow::PermNormal,
                ChooserRow::PermSkip,
                ChooserRow::Cancel,
                ChooserRow::Create
            ]
        );
    }

    #[test]
    fn switching_back_to_shell_drops_perm_group() {
        let mut form = ChooserForm::new(PathBuf::from("/p"), Vec::new());
        form.option_move(1); // Kind -> claude
        form.group_move(1); // -> Perm group (normal)
        form.option_move(1); // normal -> skip
        assert_eq!(form.perm, ClaudePerm::Skip);
        // Back on Kind, switch to shell: the Perm group disappears and the
        // focused group stays valid (no stale index to reclamp).
        form.group_move(-1); // -> Kind
        form.option_move(-1); // claude -> shell
        assert_eq!(form.kind, SessionKind::Shell);
        assert!(!form.groups().contains(&ChooserGroup::Perm));
        assert!(form.groups().contains(&form.group));
    }

    #[test]
    fn claude_form_lists_resume_rows_when_history_exists() {
        let resumes = vec![
            fake_resume("aaa", "do a thing"),
            fake_resume("bbb", "do another"),
        ];
        let mut form = ChooserForm::new(PathBuf::from("/p"), resumes);
        form.option_move(1); // Kind group: shell -> claude
        assert_eq!(
            form.rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::KindCodex,
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
        form.select(ChooserRow::KindShell);
        assert_eq!(
            form.rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::KindCodex,
                ChooserRow::Cancel,
                ChooserRow::Create
            ]
        );
    }

    #[test]
    fn chooser_create_claude_resume_appends_resume_flag() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        form_mut(&mut app).resumes = vec![fake_resume("sess-xyz", "fix the parser")];
        app.chooser_click(ChooserRow::KindClaude).unwrap();
        app.chooser_click(ChooserRow::Resume(0)).unwrap();
        assert_eq!(form(&app).resume, Some(0));
        push_create_seq(&mut app);
        app.chooser_click(ChooserRow::Create).unwrap();
        assert!(app
            .tmux
            .runner
            .nth_call(0)
            .contains(&"claude --resume sess-xyz".to_string()));
    }

    #[test]
    fn launch_command_maps_kind_and_perm() {
        let id = ResumeId::new("abc-123").expect("test id is shell-safe");
        assert_eq!(
            launch_command(SessionKind::Shell, ClaudePerm::Normal, None),
            None
        );
        assert_eq!(
            launch_command(SessionKind::Claude, ClaudePerm::Normal, None).as_deref(),
            Some("claude")
        );
        assert_eq!(
            launch_command(SessionKind::Claude, ClaudePerm::Skip, None).as_deref(),
            Some("claude --dangerously-skip-permissions")
        );
        // Resuming an existing session injects --resume <id>, before the perm flag.
        assert_eq!(
            launch_command(SessionKind::Claude, ClaudePerm::Normal, Some(&id)).as_deref(),
            Some("claude --resume abc-123")
        );
        assert_eq!(
            launch_command(SessionKind::Claude, ClaudePerm::Skip, Some(&id)).as_deref(),
            Some("claude --resume abc-123 --dangerously-skip-permissions")
        );
        // Codex ignores the claude-only perm/resume inputs and just runs `codex`.
        assert_eq!(
            launch_command(SessionKind::Codex, ClaudePerm::Skip, Some(&id)).as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn form_command_resolves_the_selected_resume() {
        let mut form = ChooserForm::new(
            PathBuf::from("/p"),
            vec![fake_resume("sess-abc", "earlier work")],
        );
        form.kind = SessionKind::Claude;
        assert_eq!(form.command().as_deref(), Some("claude"));
        form.resume = Some(0);
        assert_eq!(form.command().as_deref(), Some("claude --resume sess-abc"));
        // An out-of-range index (impossible via navigation) falls back to fresh.
        form.resume = Some(9);
        assert_eq!(form.command().as_deref(), Some("claude"));
    }

    #[test]
    fn focusing_codex_selects_it_without_perm_rows() {
        let mut form = ChooserForm::new(PathBuf::from("/p"), Vec::new());
        form.option_move(2); // Kind group: shell -> claude -> codex
        assert_eq!(form.kind, SessionKind::Codex);
        // Codex offers no permission/resume rows (those are claude-only).
        assert_eq!(
            form.rows(),
            vec![
                ChooserRow::KindShell,
                ChooserRow::KindClaude,
                ChooserRow::KindCodex,
                ChooserRow::Cancel,
                ChooserRow::Create
            ]
        );
    }

    #[test]
    fn chooser_create_codex_runs_codex_and_tags_session() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        form_mut(&mut app).option_move(2); // Kind group: shell -> claude -> codex
        push_create_seq(&mut app);
        focus_create(&mut app);
        app.chooser_activate().unwrap();
        assert!(app.tmux.runner.nth_call(0).contains(&"codex".to_string()));
        // The @rm tag records the codex kind so a later run re-adopts it.
        let tag = app.tmux.runner.nth_call(1);
        assert_eq!(tag[2], "set-option");
        assert!(tag.iter().any(|a| a.starts_with("codex ")));
    }

    #[test]
    fn chooser_activate_create_starts_claude_skip() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        let form = form_mut(&mut app);
        form.option_move(1); // Kind -> claude
        form.group_move(1); // -> Perm group
        form.option_move(1); // normal -> skip
        focus_create(&mut app);
        push_create_seq(&mut app);
        app.chooser_activate().unwrap();
        let call = app.tmux.runner.nth_call(0);
        assert_eq!(call[2], "new-session");
        assert!(call.contains(&"claude --dangerously-skip-permissions".to_string()));
        assert_eq!(app.popup, Popup::None);
    }

    #[test]
    fn chooser_activate_cancel_closes_without_tmux() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        // Focus the Actions group on the Cancel button.
        let form = form_mut(&mut app);
        form.group = ChooserGroup::Actions;
        form.action = false;
        app.chooser_activate().unwrap();
        assert_eq!(app.popup, Popup::None);
        assert_eq!(app.tmux.runner.call_count(), 0);
    }
}
