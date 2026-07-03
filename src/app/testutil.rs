//! Shared helpers for the `app` and `app::chooser` unit tests: a ready-made
//! `App<MockRunner>` over a tempdir with a `src/` dir, plus shortcuts for the
//! common "open the chooser on `src` and create a session" choreography.

use std::fs;

use tempfile::TempDir;

use crate::claude::ResumeSession;
use crate::tmux::{MockRunner, Tmux};

use super::{App, ChooserGroup, Popup};

/// An `App` over a fresh tempdir containing `src/a.rs`, backed by a
/// `MockRunner` so no real tmux is touched.
pub(crate) fn app_over_tempdir() -> (TempDir, App<MockRunner>) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src").join("a.rs"), "x").unwrap();
    let tmux = Tmux::new("runner", MockRunner::new());
    let app = App::new(dir.path().to_path_buf(), tmux);
    (dir, app)
}

/// Select the `src` dir row and open the new-session chooser on it.
pub(crate) fn open_dir_chooser(app: &mut App<MockRunner>) {
    let src_idx = app.rows.iter().position(|r| r.label == "src").unwrap();
    app.selected = src_idx;
    app.open_chooser();
}

/// Move focus to the `[ Create ]` button (Actions group, Create selected).
pub(crate) fn focus_create(app: &mut App<MockRunner>) {
    if let Popup::Chooser { group, action, .. } = &mut app.popup {
        *group = ChooserGroup::Actions;
        *action = true;
    }
}

/// Queue the canned tmux responses for a successful create-and-switch:
/// new-session → set-option (@rm tag) → list-clients (a host tty) →
/// switch-client. This is the sequence `create_session` issues.
pub(crate) fn push_create_seq(app: &mut App<MockRunner>) {
    app.tmux.runner.push(true, ""); // new-session
    app.tmux.runner.push(true, ""); // set-option (@rm tag)
    app.tmux.runner.push(true, "/dev/ttys009\n"); // list-clients
    app.tmux.runner.push(true, ""); // switch-client
}

/// Create a shell session on the `src` dir via the chooser, with the canned
/// tmux responses queued. Leaves the app with one `src-shell` session row.
pub(crate) fn create_src_shell(app: &mut App<MockRunner>) {
    open_dir_chooser(app);
    push_create_seq(app);
    focus_create(app);
    app.chooser_activate().unwrap();
}

pub(crate) fn fake_resume(id: &str, last: &str) -> ResumeSession {
    ResumeSession {
        id: id.to_string(),
        last_command: last.to_string(),
        modified: std::time::SystemTime::UNIX_EPOCH,
    }
}
