//! The new-session chooser: a small radio-form state machine opened on a
//! directory row (`Popup::Chooser`). The form is navigated in two axes —
//! Up/Down moves between selection groups, Left/Right changes the option
//! within the focused group — and which groups exist depends on the chosen
//! kind (Perm/Resume are claude-only). `chooser_command` maps the final
//! selections to the launch command for the new session.

use std::io;

use crate::app::rows::RowKind;
use crate::project::claude;
use crate::tmux::session::{ClaudePerm, SessionKind};
use crate::tmux::CommandRunner;

use super::{App, Popup};

/// One focusable option in the new-session form; the visible set is derived
/// per-frame by `App::chooser_rows` (Perm/Resume rows only exist for claude).
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
    /// Resume the i-th discovered session in `App::chooser_resumes`.
    Resume(usize),
    /// The `[ Cancel ]` button.
    Cancel,
    /// The `[ Create ]` button.
    Create,
}

/// A selection group in the new-session form. The form is navigated in two
/// axes: **Up/Down** moves between groups, **Left/Right** changes the option
/// within the focused group. Which groups are present depends on `kind`
/// (Perm/Resume only show for claude) — see `App::chooser_groups`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserGroup {
    /// shell · claude
    Kind,
    /// normal · skip (claude only)
    Perm,
    /// new session · one entry per resumable transcript (claude only)
    Resume,
    /// `[ Cancel ]` · `[ Create ]`
    Actions,
}

impl<R: CommandRunner> App<R> {
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
                    group: ChooserGroup::Kind,
                    action: true,
                };
            }
        }
    }

    /// Visible focusable rows for the current chooser kind.
    pub fn chooser_rows(&self) -> Vec<ChooserRow> {
        let mut rows = vec![
            ChooserRow::KindShell,
            ChooserRow::KindClaude,
            ChooserRow::KindCodex,
        ];
        if let Popup::Chooser {
            kind: SessionKind::Claude,
            ..
        } = self.popup
        {
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

    /// The selection groups present for the current kind, in display order.
    /// Perm and Resume only exist for claude (and Resume only when this
    /// directory has history). Up/Down navigation walks this list.
    pub fn chooser_groups(&self) -> Vec<ChooserGroup> {
        let mut groups = vec![ChooserGroup::Kind];
        if let Popup::Chooser {
            kind: SessionKind::Claude,
            ..
        } = self.popup
        {
            groups.push(ChooserGroup::Perm);
            if !self.chooser_resumes.is_empty() {
                groups.push(ChooserGroup::Resume);
            }
        }
        groups.push(ChooserGroup::Actions);
        groups
    }

    /// The currently focused group (defaults to `Kind` when no chooser is open).
    fn chooser_group(&self) -> ChooserGroup {
        match self.popup {
            Popup::Chooser { group, .. } => group,
            _ => ChooserGroup::Kind,
        }
    }

    /// The `ChooserRow` that currently has focus: the selected option of the
    /// focused radio group, or the focused button in `Actions`. Drives both the
    /// render highlight and what `Enter`/`Space` act on.
    pub fn chooser_focus_row(&self) -> ChooserRow {
        let Popup::Chooser {
            kind,
            perm,
            resume,
            group,
            action,
            ..
        } = self.popup
        else {
            return ChooserRow::KindShell;
        };
        match group {
            ChooserGroup::Kind => match kind {
                SessionKind::Shell => ChooserRow::KindShell,
                SessionKind::Claude => ChooserRow::KindClaude,
                SessionKind::Codex => ChooserRow::KindCodex,
            },
            ChooserGroup::Perm => match perm {
                ClaudePerm::Normal => ChooserRow::PermNormal,
                ClaudePerm::Skip => ChooserRow::PermSkip,
            },
            ChooserGroup::Resume => match resume {
                None => ChooserRow::ResumeNew,
                Some(i) => ChooserRow::Resume(i),
            },
            ChooserGroup::Actions => {
                if action {
                    ChooserRow::Create
                } else {
                    ChooserRow::Cancel
                }
            }
        }
    }

    /// Move focus between groups by `delta` (Up/Down — clamps at the ends).
    pub fn chooser_group_move(&mut self, delta: i32) {
        self.chooser_group_step(delta, false);
    }

    /// Cycle focus between groups by `delta` (Tab/Shift-Tab — wraps past the
    /// ends so every group is reachable with one key).
    pub fn chooser_group_cycle(&mut self, delta: i32) {
        self.chooser_group_step(delta, true);
    }

    fn chooser_group_step(&mut self, delta: i32, wrap: bool) {
        let groups = self.chooser_groups();
        let cur = self.chooser_group();
        let Some(pos) = groups.iter().position(|g| *g == cur) else {
            return;
        };
        let len = groups.len() as i32;
        let next = if wrap {
            (((pos as i32 + delta) % len) + len) % len
        } else {
            (pos as i32 + delta).clamp(0, len - 1)
        } as usize;
        if let Popup::Chooser { group, .. } = &mut self.popup {
            *group = groups[next];
        }
    }

    /// Change the selected option within the focused group by `delta`
    /// (Left/Right — clamps within the group). Switching `kind` here may add or
    /// remove the Perm/Resume groups, which is fine: focus stays on `Kind`.
    pub fn chooser_option_move(&mut self, delta: i32) {
        let resume_count = self.chooser_resumes.len() as i32;
        if let Popup::Chooser {
            kind,
            perm,
            resume,
            group,
            action,
            ..
        } = &mut self.popup
        {
            match group {
                ChooserGroup::Kind => {
                    // shell · claude · codex (clamped).
                    let cur = match *kind {
                        SessionKind::Shell => 0,
                        SessionKind::Claude => 1,
                        SessionKind::Codex => 2,
                    };
                    *kind = match (cur + delta).clamp(0, 2) {
                        0 => SessionKind::Shell,
                        1 => SessionKind::Claude,
                        _ => SessionKind::Codex,
                    };
                }
                ChooserGroup::Perm => {
                    if delta > 0 {
                        *perm = ClaudePerm::Skip;
                    } else if delta < 0 {
                        *perm = ClaudePerm::Normal;
                    }
                }
                ChooserGroup::Resume => {
                    // Option index: 0 = new (None), 1..=n = Some(i-1).
                    let cur = match resume {
                        None => 0,
                        Some(i) => *i as i32 + 1,
                    };
                    let next = (cur + delta).clamp(0, resume_count);
                    *resume = if next == 0 {
                        None
                    } else {
                        Some((next - 1) as usize)
                    };
                }
                ChooserGroup::Actions => {
                    if delta > 0 {
                        *action = true;
                    } else if delta < 0 {
                        *action = false;
                    }
                }
            }
        }
    }

    /// Clicking a row focuses its group, selects that option, and — for the
    /// `Cancel`/`Create` buttons — acts immediately.
    pub fn chooser_click(&mut self, row: ChooserRow) -> io::Result<()> {
        if let Popup::Chooser {
            kind,
            perm,
            resume,
            group,
            action,
            ..
        } = &mut self.popup
        {
            match row {
                ChooserRow::KindShell => {
                    *kind = SessionKind::Shell;
                    *group = ChooserGroup::Kind;
                }
                ChooserRow::KindClaude => {
                    *kind = SessionKind::Claude;
                    *group = ChooserGroup::Kind;
                }
                ChooserRow::KindCodex => {
                    *kind = SessionKind::Codex;
                    *group = ChooserGroup::Kind;
                }
                ChooserRow::PermNormal => {
                    *perm = ClaudePerm::Normal;
                    *group = ChooserGroup::Perm;
                }
                ChooserRow::PermSkip => {
                    *perm = ClaudePerm::Skip;
                    *group = ChooserGroup::Perm;
                }
                ChooserRow::ResumeNew => {
                    *resume = None;
                    *group = ChooserGroup::Resume;
                }
                ChooserRow::Resume(i) => {
                    *resume = Some(i);
                    *group = ChooserGroup::Resume;
                }
                ChooserRow::Cancel => {
                    *action = false;
                    *group = ChooserGroup::Actions;
                }
                ChooserRow::Create => {
                    *action = true;
                    *group = ChooserGroup::Actions;
                }
            }
        }
        if matches!(row, ChooserRow::Cancel | ChooserRow::Create) {
            self.chooser_activate()?;
        }
        Ok(())
    }

    /// Act on the focused row (Space / click): the `Cancel`/`Create` buttons
    /// fire; radio options are no-ops here (Left/Right already selected them).
    /// For "Enter anywhere creates", see `chooser_commit`.
    pub fn chooser_activate(&mut self) -> io::Result<()> {
        if !matches!(self.popup, Popup::Chooser { .. }) {
            return Ok(());
        }
        match self.chooser_focus_row() {
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
        if !matches!(self.popup, Popup::Chooser { .. }) {
            return Ok(());
        }
        if matches!(self.chooser_focus_row(), ChooserRow::Cancel) {
            self.popup = Popup::None;
            return Ok(());
        }
        self.create_from_form()
    }

    /// Build the launch command from the open chooser's selections, close the
    /// popup, and start the session. Shared by `chooser_activate` (Create button)
    /// and `chooser_commit` (Enter).
    fn create_from_form(&mut self) -> io::Result<()> {
        let Popup::Chooser {
            dir,
            kind,
            perm,
            resume,
            ..
        } = self.popup.clone()
        else {
            return Ok(());
        };
        let resume_id = resume
            .and_then(|i| self.chooser_resumes.get(i))
            .map(|s| s.id.as_str());
        let cmd = Self::chooser_command(kind, perm, resume_id);
        self.popup = Popup::None;
        self.create_session(&dir, kind, cmd.as_deref())
    }

    /// Dismiss the chooser without creating anything.
    pub fn chooser_cancel(&mut self) {
        self.popup = Popup::None;
    }

    /// Build the launch command for a session. Shell sessions run the default
    /// shell (`None`). Claude sessions run `claude`, optionally resuming an
    /// existing session (`--resume <id>`) and/or skipping permission prompts.
    /// Codex sessions run `codex`.
    pub fn chooser_command(
        kind: SessionKind,
        perm: ClaudePerm,
        resume_id: Option<&str>,
    ) -> Option<String> {
        match kind {
            SessionKind::Shell => None,
            SessionKind::Codex => Some(String::from("codex")),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{
        app_over_tempdir, fake_resume, focus_create, open_dir_chooser, push_create_seq,
    };
    use crate::app::Focus;
    use crate::tmux::MockRunner;

    #[test]
    fn chooser_create_makes_shell_and_switches() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        assert!(matches!(
            app.popup,
            Popup::Chooser {
                group: ChooserGroup::Kind,
                kind: SessionKind::Shell,
                ..
            }
        ));
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
        app.chooser_option_move(1); // Kind group: shell -> claude
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
        assert!(matches!(
            app.popup,
            Popup::Chooser {
                group: ChooserGroup::Kind,
                ..
            }
        ));
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
        if let Popup::Chooser { group, action, .. } = &mut app.popup {
            *group = ChooserGroup::Actions;
            *action = false;
        }
        app.chooser_commit().unwrap();
        assert!(matches!(app.popup, Popup::None));
        assert_eq!(app.tmux.runner.call_count(), 0); // no session created
    }

    #[test]
    fn chooser_group_move_navigates_groups_and_clamps() {
        // claude reveals Perm; Up/Down walk groups (Kind -> Perm -> Actions),
        // and Down at the last group clamps.
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_option_move(1); // Kind -> claude (reveals Perm group)
        assert_eq!(app.chooser_group(), ChooserGroup::Kind);
        app.chooser_group_move(1); // -> Perm
        assert_eq!(app.chooser_group(), ChooserGroup::Perm);
        app.chooser_group_move(1); // -> Actions (no resumes here)
        assert_eq!(app.chooser_group(), ChooserGroup::Actions);
        app.chooser_group_move(1); // clamp at the end
        assert_eq!(app.chooser_group(), ChooserGroup::Actions);
        app.chooser_group_move(-10); // clamp at the start
        assert_eq!(app.chooser_group(), ChooserGroup::Kind);
    }

    #[test]
    fn chooser_option_move_changes_selection_within_group() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        // Kind group: Right -> claude, Left -> shell (clamps).
        app.chooser_option_move(1);
        assert!(matches!(
            app.popup,
            Popup::Chooser {
                kind: SessionKind::Claude,
                ..
            }
        ));
        app.chooser_option_move(-5);
        assert!(matches!(
            app.popup,
            Popup::Chooser {
                kind: SessionKind::Shell,
                ..
            }
        ));
        // Back to claude, then walk into Perm and toggle normal -> skip.
        app.chooser_option_move(1);
        app.chooser_group_move(1); // -> Perm
        app.chooser_option_move(1); // normal -> skip
        assert!(matches!(
            app.popup,
            Popup::Chooser {
                perm: ClaudePerm::Skip,
                ..
            }
        ));
        app.chooser_option_move(-1); // skip -> normal
        assert!(matches!(
            app.popup,
            Popup::Chooser {
                perm: ClaudePerm::Normal,
                ..
            }
        ));
    }

    #[test]
    fn chooser_group_cycle_wraps_around_ends() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app); // shell: groups are [Kind, Actions]
                                    // Shift-Tab from the first group wraps to the last (Actions).
        app.chooser_group_cycle(-1);
        assert_eq!(app.chooser_group(), ChooserGroup::Actions);
        // Tab from the last group wraps back to the first (Kind).
        app.chooser_group_cycle(1);
        assert_eq!(app.chooser_group(), ChooserGroup::Kind);
    }

    #[test]
    fn chooser_defaults_to_shell_with_no_perm_rows() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        assert_eq!(
            app.chooser_rows(),
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
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_option_move(1); // Kind group: shell -> claude
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
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_option_move(1); // Kind -> claude
        app.chooser_group_move(1); // -> Perm group (normal)
        app.chooser_option_move(1); // normal -> skip
        if let Popup::Chooser { perm, .. } = app.popup {
            assert_eq!(perm, ClaudePerm::Skip);
        } else {
            panic!();
        }
        // Back on Kind, switch to shell: the Perm group disappears and the
        // focused group stays valid (no stale index to reclamp).
        app.chooser_group_move(-1); // -> Kind
        app.chooser_option_move(-1); // claude -> shell
        if let Popup::Chooser { kind, .. } = app.popup {
            assert_eq!(kind, SessionKind::Shell);
        } else {
            panic!();
        }
        assert!(!app.chooser_groups().contains(&ChooserGroup::Perm));
        assert!(app.chooser_groups().contains(&app.chooser_group()));
    }

    #[test]
    fn claude_chooser_lists_resume_rows_when_history_exists() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        // Inject discovered sessions (open_chooser found none for the tempdir).
        app.chooser_resumes = vec![
            fake_resume("aaa", "do a thing"),
            fake_resume("bbb", "do another"),
        ];
        app.chooser_option_move(1); // Kind group: shell -> claude
        assert_eq!(
            app.chooser_rows(),
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
        app.chooser_click(ChooserRow::KindShell).unwrap();
        assert_eq!(
            app.chooser_rows(),
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
        app.chooser_resumes = vec![fake_resume("sess-xyz", "fix the parser")];
        app.chooser_click(ChooserRow::KindClaude).unwrap();
        app.chooser_click(ChooserRow::Resume(0)).unwrap();
        if let Popup::Chooser { resume, .. } = app.popup {
            assert_eq!(resume, Some(0));
        } else {
            panic!("expected chooser");
        }
        push_create_seq(&mut app);
        app.chooser_click(ChooserRow::Create).unwrap();
        assert!(app
            .tmux
            .runner
            .nth_call(0)
            .contains(&"claude --resume sess-xyz".to_string()));
    }

    #[test]
    fn chooser_command_maps_kind_and_perm() {
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Shell, ClaudePerm::Normal, None),
            None
        );
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Normal, None)
                .as_deref(),
            Some("claude")
        );
        assert_eq!(
            App::<MockRunner>::chooser_command(SessionKind::Claude, ClaudePerm::Skip, None)
                .as_deref(),
            Some("claude --dangerously-skip-permissions")
        );
        // Resuming an existing session injects --resume <id>, before the perm flag.
        assert_eq!(
            App::<MockRunner>::chooser_command(
                SessionKind::Claude,
                ClaudePerm::Normal,
                Some("abc-123")
            )
            .as_deref(),
            Some("claude --resume abc-123")
        );
        assert_eq!(
            App::<MockRunner>::chooser_command(
                SessionKind::Claude,
                ClaudePerm::Skip,
                Some("abc-123")
            )
            .as_deref(),
            Some("claude --resume abc-123 --dangerously-skip-permissions")
        );
        // Codex ignores the claude-only perm/resume inputs and just runs `codex`.
        assert_eq!(
            App::<MockRunner>::chooser_command(
                SessionKind::Codex,
                ClaudePerm::Skip,
                Some("abc-123")
            )
            .as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn focusing_codex_selects_it_without_perm_rows() {
        let (_d, mut app) = app_over_tempdir();
        open_dir_chooser(&mut app);
        app.chooser_option_move(2); // Kind group: shell -> claude -> codex
        if let Popup::Chooser { kind, .. } = app.popup {
            assert_eq!(kind, SessionKind::Codex);
        } else {
            panic!("expected chooser");
        }
        // Codex offers no permission/resume rows (those are claude-only).
        assert_eq!(
            app.chooser_rows(),
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
        app.chooser_option_move(2); // Kind group: shell -> claude -> codex
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
        app.chooser_option_move(1); // Kind -> claude
        app.chooser_group_move(1); // -> Perm group
        app.chooser_option_move(1); // normal -> skip
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
        if let Popup::Chooser { group, action, .. } = &mut app.popup {
            *group = ChooserGroup::Actions;
            *action = false;
        }
        app.chooser_activate().unwrap();
        assert_eq!(app.popup, Popup::None);
        assert_eq!(app.tmux.runner.call_count(), 0);
    }
}
