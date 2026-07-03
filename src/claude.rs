//! Discovery of resumable Claude Code sessions.
//!
//! Claude Code keeps one JSONL transcript per session under
//! `~/.claude/projects/<encoded cwd>/<session-uuid>.jsonl`. It does not need a
//! separate "record" of resume ids — the transcripts on disk *are* the record.
//! This module reads them so the new-session chooser can offer to resume an
//! existing session (`claude --resume <id>`) and show what each one was last
//! working on. Reading Claude's own JSONL format is why this is the one place we
//! pull in `serde_json`; our own state files (see `config.rs`) stay serde-free.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One resumable Claude session discovered on disk for a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSession {
    /// Session id — the JSONL filename without its extension. This is the value
    /// passed to `claude --resume <id>`.
    pub id: String,
    /// The last human prompt in the transcript, collapsed to a single line.
    /// Empty when the transcript has no plain user message yet.
    pub last_command: String,
    /// Transcript file mtime; sessions are listed most-recently-modified first.
    pub modified: SystemTime,
}

/// Longest `last_command` we retain; the UI truncates further to fit the popup.
const MAX_COMMAND_LEN: usize = 200;
/// Cap on how many transcripts we read per directory so opening the chooser
/// stays snappy even in a project with a long Claude history.
const MAX_SESSIONS: usize = 10;

/// Encode a working directory the way Claude Code names its per-project folder:
/// every byte that is not an ASCII letter or digit becomes `-`. e.g.
/// `/Users/x/Projects/runner-manager` → `-Users-x-Projects-runner-manager`.
pub fn encode_project_dir(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Base directory holding Claude's per-project transcripts
/// (`~/.claude/projects`). `RM_CLAUDE_PROJECTS` overrides it (used by tests and
/// for non-standard installs). `None` when no home directory can be resolved.
pub fn projects_base() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RM_CLAUDE_PROJECTS") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".claude").join("projects"))
}

/// List resumable Claude sessions for `dir`, most recently modified first.
/// A missing project folder (no Claude history yet) yields an empty list, as
/// does any directory we can't read.
pub fn list_sessions(base: &Path, dir: &Path) -> Vec<ResumeSession> {
    let proj = base.join(encode_project_dir(dir));
    let Ok(entries) = fs::read_dir(&proj) else {
        return Vec::new();
    };

    let mut files: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        files.push((path, mtime));
    }
    // Newest first, then keep only the freshest handful.
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files.truncate(MAX_SESSIONS);

    files
        .into_iter()
        .filter_map(|(path, modified)| {
            let id = path.file_stem()?.to_str()?.to_string();
            Some(ResumeSession {
                id,
                last_command: last_user_command(&path).unwrap_or_default(),
                modified,
            })
        })
        .collect()
}

/// Scan a transcript and return its last genuine human prompt as a single line.
/// Skips meta entries, tool results, and the angle-bracketed local-command
/// bookkeeping Claude injects, so the result reads like something the user typed.
fn last_user_command(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let mut last: Option<String> = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        if value.get("isMeta").and_then(|m| m.as_bool()) == Some(true) {
            continue;
        }
        let text = message_text(value.get("message").and_then(|m| m.get("content")));
        let text = text.trim();
        // Angle-bracket-prefixed bodies are tool results / local-command wrappers,
        // not a prompt the user would recognise.
        if text.is_empty() || text.starts_with('<') {
            continue;
        }
        last = Some(collapse(text));
    }
    last
}

/// Flatten a message `content` field into plain text. Claude stores it either as
/// a bare string or as an array of typed blocks; we keep only the text blocks.
fn message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Collapse whitespace/newlines into a single trimmed line and cap its length so
/// one giant pasted prompt can't blow up memory or the chooser layout.
fn collapse(text: &str) -> String {
    let mut out: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > MAX_COMMAND_LEN {
        out = out.chars().take(MAX_COMMAND_LEN).collect();
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn encodes_dir_replacing_non_alnum_with_dash() {
        assert_eq!(
            encode_project_dir(Path::new("/Users/x/Projects/runner-manager")),
            "-Users-x-Projects-runner-manager"
        );
        // dots and other separators collapse to dashes too
        assert_eq!(
            encode_project_dir(Path::new("/a/.config/sub")),
            "-a--config-sub"
        );
    }

    #[test]
    fn missing_project_dir_yields_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(list_sessions(d.path(), Path::new("/no/such/dir")).is_empty());
    }

    fn write_session(base: &Path, dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
        let proj = base.join(encode_project_dir(dir));
        fs::create_dir_all(&proj).unwrap();
        let path = proj.join(format!("{id}.jsonl"));
        fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn lists_sessions_and_extracts_last_user_command() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path();
        let dir = Path::new("/proj/app");
        write_session(
            base,
            dir,
            "11111111-1111-1111-1111-111111111111",
            &[
                r#"{"type":"user","message":{"role":"user","content":"first thing"}}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":"ok"}}"#,
                r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"meta noise"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<tool_result>ignore me</tool_result>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"fix the   bug\nplease"}]}}"#,
            ],
        );

        let sessions = list_sessions(base, dir);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "11111111-1111-1111-1111-111111111111");
        // newlines/extra spaces collapsed; meta + tool-result lines skipped
        assert_eq!(sessions[0].last_command, "fix the bug please");
    }

    #[test]
    fn orders_sessions_newest_first() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path();
        let dir = Path::new("/proj/app");
        let old = write_session(
            base,
            dir,
            "00000000-0000-0000-0000-000000000000",
            &[r#"{"type":"user","message":{"role":"user","content":"older"}}"#],
        );
        let new = write_session(
            base,
            dir,
            "99999999-9999-9999-9999-999999999999",
            &[r#"{"type":"user","message":{"role":"user","content":"newer"}}"#],
        );
        // Force a definite mtime ordering regardless of filesystem timestamp
        // resolution: old at the epoch, new well after it.
        let epoch = SystemTime::UNIX_EPOCH;
        fs::File::open(&old).unwrap().set_modified(epoch).unwrap();
        fs::File::open(&new)
            .unwrap()
            .set_modified(epoch + std::time::Duration::from_secs(10))
            .unwrap();

        let sessions = list_sessions(base, dir);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].last_command, "newer");
        assert_eq!(sessions[1].last_command, "older");
    }

    #[test]
    fn transcript_without_plain_prompt_has_empty_command() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path();
        let dir = Path::new("/proj/app");
        write_session(
            base,
            dir,
            "22222222-2222-2222-2222-222222222222",
            &[
                r#"{"type":"summary","summary":"x"}"#,
                r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>bye</local-command-stdout>"}}"#,
            ],
        );
        let sessions = list_sessions(base, dir);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].last_command, "");
    }
}
