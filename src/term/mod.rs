//! The embedded terminal: the PTY running the tmux client ([`pty`]), the
//! translation of crossterm input events into the bytes it expects
//! ([`keys`]), and the [`embedded::EmbeddedTerm`] unit that owns the PTY,
//! its parser, and its size across the terminal's whole lifecycle.

pub mod embedded;
pub mod keys;
pub mod pty;
