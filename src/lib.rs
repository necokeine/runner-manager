//! runner-manager: a NERDTree-style TUI that pairs a lazy filesystem tree with
//! an embedded tmux client, so shell/agent task sessions can be started,
//! switched, and resumed per directory. See `CLAUDE.md`/`README.md` for the
//! architecture and keybindings.

pub mod app;
pub mod claude;
pub mod config;
pub mod git;
pub mod keys;
pub mod lock;
pub mod pty;
pub mod rows;
pub mod run;
pub mod session;
pub mod tmux;
pub mod tree;
pub mod ui;
pub mod viewer;
