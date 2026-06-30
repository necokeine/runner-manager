use std::time::SystemTime;

/// One resumable agent session discovered on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSession {
    /// Session id passed to the agent's resume command.
    pub id: String,
    /// The last human prompt in the transcript, collapsed to a single line.
    /// Empty when the transcript has no plain user message yet.
    pub last_command: String,
    /// Transcript file mtime; sessions are listed most-recently-modified first.
    pub modified: SystemTime,
}
