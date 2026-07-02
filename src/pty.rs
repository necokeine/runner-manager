use std::cell::Cell;
use std::io::{self, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, RwLock, RwLockReadGuard};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

// tui_term re-exports vt100, so Screen types unify with the renderer (Task 5).
use tui_term::vt100;

/// Shared handle to the embedded terminal's vt100 parser. Held by both the
/// reader thread (which feeds bytes into it) and the render loop (which reads
/// the screen out). Optional in `run.rs`: `None` when no embedded client exists.
pub type ParserHandle = Arc<RwLock<vt100::Parser>>;

pub struct Pty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    parser: Arc<RwLock<vt100::Parser>>,
    alive: Arc<AtomicBool>,
    /// Set by the reader thread whenever it feeds fresh bytes into the parser
    /// (or blanks the screen on EOF). The run loop swaps it back to `false`
    /// each pass via [`take_dirty`]; a `true` means the embedded terminal's
    /// screen changed and the pane needs re-rendering. This is what lets the
    /// loop skip the expensive per-frame redraw while the terminal is idle.
    ///
    /// [`take_dirty`]: Pty::take_dirty
    dirty: Arc<AtomicBool>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Acquire the parser for reading, recovering the guard if a previous panic
/// poisoned the lock. vt100 has been known to panic on some escape sequences
/// tmux emits during rapid PTY resizes (e.g. dragging the pane splitter); if
/// that ever poisons the lock, the screen it left behind is still renderable,
/// so the UI keeps drawing instead of crashing.
pub fn read_screen(parser: &ParserHandle) -> RwLockReadGuard<'_, vt100::Parser> {
    parser.read().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    /// Set only for the duration of the `Parser::process` call inside [`feed`].
    /// While it is set, the panic hook installed by [`install_panic_filter`]
    /// stays silent, because any panic there is one we deliberately catch.
    static SILENCE_VT100_PANIC: Cell<bool> = const { Cell::new(false) };
}

/// RAII flag that marks the current thread as inside a guarded `process` call.
/// Restoring on drop keeps the flag correct whether `process` returns normally
/// or unwinds (the unwind is stopped by the surrounding `catch_unwind`).
struct SilenceGuard;

impl SilenceGuard {
    fn new() -> Self {
        SILENCE_VT100_PANIC.with(|c| c.set(true));
        SilenceGuard
    }
}

impl Drop for SilenceGuard {
    fn drop(&mut self) {
        SILENCE_VT100_PANIC.with(|c| c.set(false));
    }
}

static PANIC_FILTER: Once = Once::new();

/// Install a process-wide panic hook that suppresses the panic *message* for
/// the vt100 parser panics we deliberately catch in [`feed`], while leaving the
/// previous hook in place for every other panic.
///
/// [`b648388`] made the reader thread survive a vt100 panic (catch_unwind +
/// poison recovery), but `catch_unwind` does not stop the panic hook from
/// running: the default hook still printed `thread '…' panicked at vt100…` to
/// the tty, and that text bled onto the alternate screen during a rapid
/// splitter drag ("a panic information appear"). Silencing the hook for exactly
/// those caught panics removes the stray message without hiding real crashes —
/// the flag is only ever set on the reader thread around `Parser::process`, so
/// a genuine panic on any other thread (or path) still prints normally.
///
/// Idempotent; safe to call more than once.
pub fn install_panic_filter() {
    PANIC_FILTER.call_once(|| {
        let prev = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if SILENCE_VT100_PANIC.with(|c| c.get()) {
                return;
            }
            prev(info);
        }));
    });
}

/// Feed bytes to the parser, recovering a poisoned lock and swallowing any
/// panic from vt100 itself. A malformed or resize-time escape sequence that
/// makes the parser panic must not kill the reader thread or poison the lock —
/// the bad chunk is dropped and parsing resumes on the next read. The panic
/// message is silenced too (see [`install_panic_filter`]) so it never bleeds
/// onto the TUI.
fn feed(parser: &Arc<RwLock<vt100::Parser>>, bytes: &[u8]) {
    let mut p = parser.write().unwrap_or_else(|e| e.into_inner());
    // `p` lives in this frame, not the closure, so catch_unwind stops the
    // unwind before the guard drops — the lock is never marked poisoned.
    let _guard = SilenceGuard::new();
    let _ = panic::catch_unwind(AssertUnwindSafe(|| p.process(bytes)));
}

impl Pty {
    pub fn spawn(args: &[&str], rows: u16, cols: u16) -> io::Result<Self> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(size)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let mut cmd = CommandBuilder::new(args[0]);
        cmd.args(&args[1..]);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::other(e.to_string()))?;

        let parser = Arc::new(RwLock::new(vt100::Parser::new(rows, cols, 0)));
        let alive = Arc::new(AtomicBool::new(true));
        // Starts `true` so the first frame after spawn always renders the
        // freshly-attached session, even before any output arrives.
        let dirty = Arc::new(AtomicBool::new(true));
        let reader_parser = Arc::clone(&parser);
        let reader_alive = Arc::clone(&alive);
        let reader_dirty = Arc::clone(&dirty);
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        // The embedded tmux client exited (e.g. the last session
                        // was destroyed). Blank the screen so the pane shows an
                        // empty terminal instead of a stale frame, and mark the
                        // PTY dead so the caller can respawn it for a new session.
                        feed(&reader_parser, b"\x1b[2J\x1b[H");
                        // The blanked screen is a change too — force one redraw.
                        reader_dirty.store(true, Ordering::SeqCst);
                        reader_alive.store(false, Ordering::SeqCst);
                        break;
                    }
                    Ok(n) => {
                        feed(&reader_parser, &buf[..n]);
                        // Signal the run loop that the screen changed so it
                        // redraws this frame instead of skipping the render.
                        reader_dirty.store(true, Ordering::SeqCst);
                    }
                }
            }
        });

        Ok(Pty {
            master: pair.master,
            writer,
            parser,
            alive,
            dirty,
            _child: child,
        })
    }

    /// False once the embedded tmux client has exited (PTY hit EOF). When dead,
    /// the right pane is blank and a new session needs a freshly spawned PTY.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Whether the reader thread has fed new bytes (or blanked the screen)
    /// since the last call, clearing the flag as a side effect. The run loop
    /// polls this each pass and only re-renders the terminal pane when it
    /// returns `true`, keeping idle CPU near zero instead of redrawing at the
    /// poll rate.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }

    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut p = self.parser.write().unwrap_or_else(|e| e.into_inner());
        p.set_size(rows, cols);
        Ok(())
    }

    pub fn parser(&self) -> ParserHandle {
        Arc::clone(&self.parser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_parser() -> Arc<RwLock<vt100::Parser>> {
        Arc::new(RwLock::new(vt100::Parser::new(24, 80, 0)))
    }

    /// Panic while holding the write guard to deliberately poison the lock,
    /// mirroring what a vt100 panic in the reader thread used to do.
    fn poison(parser: &Arc<RwLock<vt100::Parser>>) {
        let p = Arc::clone(parser);
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = p.write().unwrap();
            panic!("poison the lock");
        }));
        assert!(parser.is_poisoned(), "lock should be poisoned for the test");
    }

    #[test]
    fn read_screen_recovers_from_poisoned_lock() {
        let parser = new_parser();
        feed(&parser, b"hello");
        poison(&parser);
        // The reported crash was `parser.read().unwrap()` panicking here; the
        // recovering read must hand back the (still valid) screen instead.
        let guard = read_screen(&parser);
        assert!(guard.screen().contents().contains("hello"));
    }

    #[test]
    fn feed_recovers_from_poisoned_lock_and_keeps_parsing() {
        let parser = new_parser();
        poison(&parser);
        // A poisoned lock must not stop the reader thread from advancing the
        // screen — feed recovers the guard and processes the new bytes.
        feed(&parser, b"after-poison");
        let contents = read_screen(&parser).screen().contents();
        assert!(contents.contains("after-poison"));
    }

    #[test]
    fn silence_guard_toggles_and_clears_on_unwind() {
        assert!(!SILENCE_VT100_PANIC.with(|c| c.get()));
        {
            let _g = SilenceGuard::new();
            assert!(SILENCE_VT100_PANIC.with(|c| c.get()));
        }
        assert!(!SILENCE_VT100_PANIC.with(|c| c.get()));
        // Unwinding through the guard still clears the flag (RAII on drop).
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            let _g = SilenceGuard::new();
            panic!("unwind past the guard");
        }));
        assert!(
            !SILENCE_VT100_PANIC.with(|c| c.get()),
            "flag must be cleared after an unwind"
        );
    }

    #[test]
    fn feed_swallows_vt100_panic_and_leaves_lock_usable() {
        install_panic_filter();
        // A wide (2-column) grapheme cannot fit a 1-column pane, which makes
        // vt100 0.15.2 panic inside `process` — exactly what a splitter drag to
        // a sliver-width pane provokes. feed must swallow it (the panic message
        // is silenced by the filter), leave the flag clear, and keep the lock
        // usable so the next chunk still parses.
        let parser = Arc::new(RwLock::new(vt100::Parser::new(20, 1, 100)));
        feed(&parser, "世界".as_bytes());
        assert!(!SILENCE_VT100_PANIC.with(|c| c.get()));
        assert!(!parser.is_poisoned());
        feed(&parser, b"ok");
        assert_eq!(read_screen(&parser).screen().size(), (20, 1));
    }
}
