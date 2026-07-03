//! The project directory on disk: its persisted state under `<root>/.pjma/`
//! ([`config`], [`lock`]), its lazily-loaded file tree ([`tree`]), its git
//! status colouring ([`git`]), and the Claude Code history discovered for its
//! directories ([`claude`]).

pub mod claude;
pub mod config;
pub mod git;
pub mod lock;
pub mod tree;
