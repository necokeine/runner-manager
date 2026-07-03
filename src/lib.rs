//! runner-manager: a NERDTree-style TUI that pairs a lazy filesystem tree with
//! an embedded tmux client, so shell/agent task sessions can be started,
//! switched, and resumed per directory. See `CLAUDE.md`/`README.md` for the
//! architecture and keybindings.
//!
//! Module layout, bottom-up: [`project`] (the project dir on disk), [`tmux`]
//! (server interaction + session bookkeeping), and [`term`] (the embedded
//! terminal) are the leaves; [`app`] holds all state and actions over them;
//! [`ui`] renders that state; [`run`] is the composition root that owns the
//! real terminal and the event loop.

pub mod app;
pub mod project;
pub mod run;
pub mod term;
pub mod tmux;
pub mod ui;
