//! Integration tests that drive a **real** tmux server on a throwaway socket.
//!
//! The unit tests in `src/tmux/mod.rs` assert the argument vectors we build;
//! only tmux itself can tell us whether it *accepts* them. That gap is what let
//! `tag_session` regress: `set-option -t =<slug>` is a perfectly plausible
//! target that tmux rejects outright, so every session went untagged and none
//! could be re-adopted into the tree on a later run.

use std::path::{Path, PathBuf};
use std::process::Command;

use runner_manager::app::rows::RowKind;
use runner_manager::app::App;
use runner_manager::tmux::session::SessionKind;
use runner_manager::tmux::{SystemRunner, Tmux};
use tempfile::TempDir;

/// A tmux server on a private socket, killed when the test ends.
struct Server {
    tmux: Tmux<SystemRunner>,
    socket: String,
    _dir: TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-S", &self.socket, "kill-server"])
            .output();
    }
}

/// Start a server on a fresh socket, or `None` when tmux is not installed —
/// these tests are skipped rather than failed on such a machine.
fn server() -> Option<Server> {
    Command::new("tmux").arg("-V").output().ok()?;
    let dir = TempDir::new().expect("tempdir");
    let socket = dir.path().join("pjma.sock").to_string_lossy().into_owned();
    Some(Server {
        tmux: Tmux::new(socket.clone(), SystemRunner),
        socket,
        _dir: dir,
    })
}

#[test]
fn tag_session_round_trips_and_never_tags_a_prefix_sibling() {
    let Some(server) = server() else {
        return;
    };
    let tmux = &server.tmux;
    let dir = Path::new("/tmp");

    // Sibling slugs of the shape `create` generates: a bare (prefix-matched)
    // target for `src-shell` would resolve to `src-shell-2` when only the
    // sibling exists, so the target has to be exact.
    tmux.new_session("src-shell-2", dir, None).expect("sibling");
    tmux.new_session("src-shell", dir, None).expect("session");

    tmux.tag_session("src-shell", dir, "claude").expect("tag");

    let infos = tmux.list_sessions_full().expect("list");
    let tagged = infos
        .iter()
        .find(|i| i.name == "src-shell")
        .expect("tagged session listed");
    assert_eq!(tagged.kind, "claude");
    assert_eq!(tagged.dir, "/tmp");
    let sibling = infos
        .iter()
        .find(|i| i.name == "src-shell-2")
        .expect("sibling listed");
    assert_eq!(sibling.kind, "", "the sibling must be left untagged");
    assert_eq!(sibling.dir, "");
}

#[test]
fn sessions_from_a_previous_run_are_recovered_into_the_tree() {
    let Some(server) = server() else {
        return;
    };
    // A previous run of the tool: two tagged sessions, one at the tree root and
    // one in a subdirectory, left behind on the socket.
    let root_dir = TempDir::new().expect("root");
    let root: PathBuf = root_dir.path().canonicalize().expect("canonical root");
    let sub = root.join("src");
    std::fs::create_dir(&sub).expect("subdir");
    let previous = &server.tmux;
    previous.new_session("root-shell", &root, None).expect("a");
    previous
        .tag_session("root-shell", &root, "shell")
        .expect("tag a");
    previous.new_session("src-claude", &sub, None).expect("b");
    previous
        .tag_session("src-claude", &sub, "claude")
        .expect("tag b");

    // This run starts with an empty store and reconciles against tmux.
    let mut app = App::new(root.clone(), Tmux::new(server.socket.clone(), SystemRunner));
    app.sync().expect("sync");

    let by_dir = app.store.by_dir();
    let root_rows = by_dir.get(&root).expect("root sessions recovered");
    assert_eq!(root_rows[0].slug, "root-shell");
    assert_eq!(root_rows[0].kind, SessionKind::Shell);
    let sub_rows = by_dir.get(&sub).expect("subdir sessions recovered");
    assert_eq!(sub_rows[0].slug, "src-claude");
    assert_eq!(sub_rows[0].kind, SessionKind::Claude);

    // And both are visible from the first frame: each sits under its directory
    // row, which for `src/` is already in the one-level-deep initial tree.
    let recovered: Vec<&str> = app
        .rows
        .iter()
        .filter_map(|r| match &r.kind {
            RowKind::Session { slug, .. } => Some(slug.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(recovered, vec!["root-shell", "src-claude"]);
}

#[test]
fn tag_session_fails_when_the_exact_session_is_absent() {
    let Some(server) = server() else {
        return;
    };
    let tmux = &server.tmux;
    let dir = Path::new("/tmp");
    tmux.new_session("src-shell-2", dir, None).expect("sibling");

    // Only the sibling exists: tagging `src-shell` must be an error, not a
    // silent write onto `src-shell-2`.
    assert!(tmux.tag_session("src-shell", dir, "shell").is_err());
    let infos = tmux.list_sessions_full().expect("list");
    assert_eq!(infos[0].name, "src-shell-2");
    assert_eq!(infos[0].kind, "");
}
