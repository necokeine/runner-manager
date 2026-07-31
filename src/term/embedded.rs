//! The embedded terminal as a single unit: the PTY running the tmux client,
//! the shared vt100 parser its reader thread feeds, and the last size pushed
//! to it. These three used to live as separate locals in the run loop, which
//! smeared the terminal's lifecycle (spawn, respawn, resize, death) across
//! the loop body; owning them together makes the state machine — detached →
//! attached → dead → respawned — explicit and testable.

use std::io;

use crate::term::pty::{ParserHandle, Pty};

/// The one embedded tmux client and everything the run loop needs to drive
/// it. `None` states are real: a fresh start has no sessions to attach to, so
/// no PTY exists until the first session is created.
pub struct EmbeddedTerm {
    /// Path of the project's tmux socket; every (re)spawn attaches through it,
    /// so a respawn can never land on a different server than the original.
    socket: String,
    /// The live PTY, or `None` when no embedded client exists (fresh start,
    /// or dropped after the client died and before a respawn).
    pty: Option<Pty>,
    /// Shared handle to the PTY's vt100 parser, kept alongside so the render
    /// loop can read the screen without borrowing the PTY itself.
    parser: Option<ParserHandle>,
    /// Last `(rows, cols)` pushed to the PTY; [`EmbeddedTerm::resize_to`]
    /// skips repeats so the child only sees a SIGWINCH on real changes.
    last_size: (u16, u16),
}

impl EmbeddedTerm {
    /// A detached terminal for `socket`: no PTY, no parser, nothing to render.
    pub fn new(socket: String) -> Self {
        Self {
            socket,
            pty: None,
            parser: None,
            last_size: (0, 0),
        }
    }

    /// Spawn the tmux client on a fresh PTY attached to `session`
    /// (`new-session -A` attaches if the session exists, creates it
    /// otherwise), replacing any previous PTY. The remembered size is reset
    /// so the next [`EmbeddedTerm::resize_to`] always reaches the new client.
    ///
    /// # Errors
    ///
    /// Fails if the PTY cannot be opened or tmux cannot be spawned.
    pub fn spawn_attached(&mut self, session: &str) -> io::Result<()> {
        let pty = Pty::spawn(
            &[
                "tmux",
                "-S",
                &self.socket,
                "new-session",
                "-A",
                "-s",
                session,
            ],
            24,
            80,
        )?;
        self.parser = Some(pty.parser());
        self.pty = Some(pty);
        self.last_size = (0, 0);
        Ok(())
    }

    /// Whether a PTY exists at all — even one whose client has since exited.
    /// Used at startup to decide if there is a tmux server to configure.
    pub fn is_attached(&self) -> bool {
        self.pty.is_some()
    }

    /// Whether the embedded tmux client is still running (a PTY exists and
    /// has not hit EOF).
    pub fn is_alive(&self) -> bool {
        self.pty.as_ref().is_some_and(Pty::is_alive)
    }

    /// Respawn attached to `session` when the PTY is dead or absent; a live
    /// client is left alone (it is already showing something, and yanking it
    /// would drop keystrokes). Returns whether a spawn actually happened —
    /// the caller must then re-apply the tmux global options, because a
    /// brand-new server starts from the user's config. A failed spawn also
    /// returns `false`; the next selection retries via the same path.
    pub fn respawn_if_dead(&mut self, session: &str) -> bool {
        if self.is_alive() {
            return false;
        }
        self.spawn_attached(session).is_ok()
    }

    /// Whether the reader thread fed new output (or blanked the screen on
    /// EOF) since the last call, clearing the flag; `false` when detached.
    /// The run loop polls this to skip re-rendering an idle terminal.
    pub fn take_dirty(&self) -> bool {
        self.pty.as_ref().is_some_and(Pty::take_dirty)
    }

    /// The shared vt100 parser to render the pane from, when a PTY exists.
    pub fn parser(&self) -> Option<&ParserHandle> {
        self.parser.as_ref()
    }

    /// Forward encoded input bytes (keystrokes, wheel reports) to the client.
    /// Dropped when detached or on a write error — there is nowhere to type
    /// into, and the status line is not an input-error surface.
    pub fn write_input(&mut self, bytes: &[u8]) {
        if let Some(p) = &mut self.pty {
            let _ = p.write_input(bytes);
        }
    }

    /// Resize the PTY (and its parser screen) to `rows` × `cols`, skipping
    /// repeats of the last pushed size and degenerate zero sizes. No-op when
    /// detached; the size is only remembered when it actually reached a PTY,
    /// so a spawn that follows still gets its first resize.
    pub fn resize_to(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || (rows, cols) == self.last_size {
            return;
        }
        if let Some(p) = &mut self.pty {
            let _ = p.resize(rows, cols);
            self.last_size = (rows, cols);
        }
    }
}

#[cfg(test)]
impl EmbeddedTerm {
    /// Attach an arbitrary already-spawned PTY. Test-only: exercises the
    /// attached-state machinery with a harmless child (e.g. `cat`) instead of
    /// launching a real tmux server.
    pub(crate) fn attach_for_test(&mut self, pty: Pty) {
        self.parser = Some(pty.parser());
        self.pty = Some(pty);
        self.last_size = (0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::pty::read_screen;
    use std::time::{Duration, Instant};

    fn attached_to_cat() -> EmbeddedTerm {
        let mut term = EmbeddedTerm::new("unused-socket".to_string());
        let pty = Pty::spawn(&["cat"], 24, 80).expect("spawn cat on a pty");
        term.attach_for_test(pty);
        term
    }

    #[test]
    fn detached_term_is_inert() {
        let mut term = EmbeddedTerm::new("sock".to_string());
        assert!(!term.is_attached());
        assert!(!term.is_alive());
        assert!(!term.take_dirty());
        assert!(term.parser().is_none());
        // Nothing to type into or resize — both must be safe no-ops.
        term.write_input(b"ignored");
        term.resize_to(30, 90);
        assert!(!term.is_attached());
    }

    #[test]
    fn respawn_leaves_a_live_client_alone() {
        let mut term = attached_to_cat();
        assert!(term.is_attached());
        assert!(term.is_alive());
        // Alive -> no respawn. Returning false is what keeps the run loop
        // from yanking a healthy client (and keeps tmux out of this test).
        assert!(!term.respawn_if_dead("some-session"));
        assert!(term.is_alive());
    }

    #[test]
    fn resize_reaches_the_parser_and_skips_degenerate_sizes() {
        let mut term = attached_to_cat();
        term.resize_to(30, 90);
        let parser = term.parser().expect("attached term has a parser");
        assert_eq!(read_screen(parser).screen().size(), (30, 90));
        // Zero dimensions are ignored rather than pushed to the child.
        term.resize_to(0, 90);
        term.resize_to(30, 0);
        let parser = term.parser().expect("attached term has a parser");
        assert_eq!(read_screen(parser).screen().size(), (30, 90));
    }

    #[test]
    fn written_input_comes_back_through_the_parser() {
        // `cat` echoes the PTY input, so a write must eventually surface on
        // the shared parser screen and raise the dirty flag.
        let mut term = attached_to_cat();
        term.write_input(b"ping\r");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if term.take_dirty() {
                let parser = term.parser().expect("attached term has a parser");
                if read_screen(parser).screen().contents().contains("ping") {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "echoed input never reached the parser"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
