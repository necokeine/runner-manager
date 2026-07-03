//! The embedded terminal: the PTY running the tmux client ([`pty`]) and the
//! translation of crossterm input events into the bytes it expects ([`keys`]).

pub mod keys;
pub mod pty;
