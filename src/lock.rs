//! Single-instance guard: at most one `runner-manager` per root directory.
//!
//! Two `runner-manager` processes sharing a root would also share that root's
//! tmux socket (`<root>/.pjma/pjma.sock`) and config dir. They would then race
//! on the *one* embedded client — both issuing `switch-client`/`new-session`
//! against the same host tty, both rewriting `<root>/.pjma/expanded`, both
//! reconciling the session store against the same live set. The outcome is a
//! flickering, fighting UI and a corrupted expanded-state file. The clean fix
//! is to forbid the second process from starting at all.
//!
//! The guard is an advisory `flock(2)` on `<root>/.pjma/pjma.lock`, held for the
//! whole lifetime of the process. `flock` is the right primitive here because
//! the kernel releases it when the holding *process* dies — normal exit, panic,
//! or even `SIGKILL` — so there is never a stale lock to detect or clean up,
//! which is exactly the trap a PID file would fall into (PID reuse, leftover
//! file after a crash). The lock file itself is intentionally left on disk
//! between runs; only the kernel lock state matters.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// Lock file under the config dir. Lives next to `pjma.sock`.
const LOCK_FILE: &str = "pjma.lock";

/// Why acquiring the single-instance lock failed.
#[derive(Debug)]
pub enum LockError {
    /// Another `runner-manager` already holds the lock for this root.
    Held,
    /// The lock file could not be opened or locked for some other reason
    /// (permissions, a full disk, etc.).
    Io(io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held => write!(f, "another runner-manager is already running here"),
            LockError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// An exclusive, advisory lock held for the lifetime of the process. Dropping
/// it closes the file descriptor, which releases the underlying `flock`.
#[derive(Debug)]
pub struct InstanceLock {
    // The held file: as long as this fd stays open, we hold the flock. The
    // field is never read; it exists solely to keep the descriptor alive.
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Try to take the single-instance lock for the config directory `dir`
    /// (`<root>/.pjma`). The directory must already exist. Returns
    /// [`LockError::Held`] if another process holds it, without blocking.
    pub fn try_acquire(dir: &Path) -> Result<Self, LockError> {
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(LockError::Io)?;
        flock_nonblocking(&file)?;
        Ok(Self { _file: file, path })
    }

    /// Path of the lock file (`<root>/.pjma/pjma.lock`).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Take a non-blocking exclusive `flock` on `file`. Maps a would-block result
/// to [`LockError::Held`] and any other failure to [`LockError::Io`].
#[cfg(unix)]
fn flock_nonblocking(file: &File) -> Result<(), LockError> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file` owns a valid fd for the duration of the call; `flock` only
    // reads it and never retains it past return.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // A held lock surfaces as EWOULDBLOCK (aliased to EAGAIN on some systems).
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Err(LockError::Held),
        _ => Err(LockError::Io(err)),
    }
}

/// On non-Unix targets there is no `flock` and no tmux either, so the guard is a
/// no-op: opening the file succeeded, which is all we can portably promise.
#[cfg(not(unix))]
fn flock_nonblocking(_file: &File) -> Result<(), LockError> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn acquire_succeeds_and_creates_lock_file() {
        let d = tempdir().unwrap();
        let lock = InstanceLock::try_acquire(d.path()).unwrap();
        assert_eq!(lock.path(), d.path().join("pjma.lock"));
        assert!(lock.path().exists());
    }

    #[test]
    fn second_acquire_while_held_is_rejected() {
        let d = tempdir().unwrap();
        let _held = InstanceLock::try_acquire(d.path()).unwrap();
        // A different open file description for the same path conflicts with the
        // flock we already hold, so the second attempt reports the lock as held.
        match InstanceLock::try_acquire(d.path()) {
            Err(LockError::Held) => {}
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn lock_released_on_drop_allows_reacquire() {
        let d = tempdir().unwrap();
        let first = InstanceLock::try_acquire(d.path()).unwrap();
        drop(first);
        // Once the holder is dropped the fd closes and the flock is released, so
        // a fresh acquire on the same dir succeeds. Retry briefly on `Held`:
        // other tests in this binary fork children (git, PTY spawns), and a
        // fork between our acquire and drop duplicates the locked fd into the
        // child until its exec closes it (CLOEXEC), transiently keeping the
        // flock alive from another process.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match InstanceLock::try_acquire(d.path()) {
                Ok(_) => break,
                Err(LockError::Held) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("reacquire after drop should succeed: {e:?}"),
            }
        }
    }

    #[test]
    fn missing_dir_is_an_io_error_not_held() {
        let d = tempdir().unwrap();
        let missing = d.path().join("does-not-exist");
        match InstanceLock::try_acquire(&missing) {
            Err(LockError::Io(_)) => {}
            other => panic!("expected Io error for missing dir, got {other:?}"),
        }
    }
}
