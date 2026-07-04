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

/// A validated, shell-safe Claude session id.
///
/// The id is spliced verbatim into the `claude --resume <id>` launch command,
/// which tmux hands to a shell — so the only way to obtain one is through
/// [`ResumeId::new`], which accepts nothing but ASCII alphanumerics and `-`
/// (real ids are UUIDs). Tying the guarantee to the type keeps the splice site
/// (`chooser_command`) safe no matter where an id comes from, instead of
/// relying on whichever caller happens to filter today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeId(String);

impl ResumeId {
    /// Wrap `id` if it is shell-safe: non-empty and built only from ASCII
    /// alphanumerics and `-`. Anything else — and thus anything a shell could
    /// interpret — yields `None`.
    pub fn new(id: impl Into<String>) -> Option<Self> {
        let id = id.into();
        let safe = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        safe.then_some(Self(id))
    }

    /// The id as a plain string slice, e.g. to splice into a launch command.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One resumable Claude session discovered on disk for a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSession {
    /// Session id — the JSONL filename without its extension. This is the value
    /// passed to `claude --resume <id>`.
    pub id: ResumeId,
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

/// Tail windows tried, smallest first, when scanning a transcript backwards
/// for the last user prompt. Transcripts grow to tens of MB and
/// `list_sessions` runs on the UI thread (the chooser opens synchronously), so
/// whole-file reads would freeze the TUI. Most sessions hit in the first
/// window; a long agentic turn can bury the prompt under megabytes of tool
/// output, so the scan deepens before giving up. The largest window bounds the
/// work per transcript.
const TAIL_READ_SIZES: [u64; 3] = [256 * 1024, 1024 * 1024, 4 * 1024 * 1024];

/// Encode a working directory the way Claude Code names its per-project folder:
/// every character that is not an ASCII letter or digit becomes `-`. e.g.
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
            // `ResumeId::new` refuses anything that isn't shell-safe, so a
            // transcript with a hostile filename is skipped rather than quoted.
            let id = ResumeId::new(path.file_stem()?.to_str()?)?;
            Some(ResumeSession {
                id,
                last_command: last_user_command(&path).unwrap_or_default(),
                modified,
            })
        })
        .collect()
}

/// Return a transcript's last genuine human prompt as a single line: skips
/// meta entries, tool results, and the angle-bracketed bookkeeping Claude
/// injects, so the result reads like something the user typed. Reads the file
/// tail in growing windows (see [`TAIL_READ_SIZES`]) — this runs on the UI
/// thread, so the whole file is never slurped.
fn last_user_command(path: &Path) -> Option<String> {
    let len = fs::metadata(path).ok()?.len();
    for size in TAIL_READ_SIZES {
        let (text, cut) = read_tail(path, size)?;
        if let Some(cmd) = scan_transcript(&text, cut) {
            return Some(cmd);
        }
        if size >= len {
            break; // the whole file was scanned; no deeper window exists
        }
    }
    None
}

/// Scan transcript JSONL text for the last genuine human prompt. `skip_first`
/// drops the first line — a fragment of a record when the text came from a
/// mid-file byte cut.
fn scan_transcript(text: &str, skip_first: bool) -> Option<String> {
    let mut lines = text.lines();
    if skip_first {
        lines.next();
    }
    let mut last: Option<String> = None;
    for line in lines {
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
        if text.is_empty() || is_wrapper(text) {
            continue;
        }
        last = Some(collapse(text));
    }
    last
}

/// True for the angle-bracketed bookkeeping bodies Claude injects as `user`
/// entries (`<tool_result>`, `<task-notification>`, `<local-command-stdout>`,
/// `<bash-input>`, …): they all open with a bare lowercase tag. A genuine
/// prompt that merely starts with pasted markup (`<div class=x>`, `<H1>`) has
/// attributes, digits, or uppercase in the tag and is kept.
fn is_wrapper(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    match rest.find('>') {
        Some(end) if end > 0 => rest[..end]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-'),
        _ => false,
    }
}

/// Read at most the last `max` bytes of `path` as UTF-8; the flag reports
/// whether the file was cut mid-record (the first line is then a fragment the
/// caller must skip). Decoding is zero-copy for valid UTF-8 — the lossy copy
/// only happens when the cut split a multibyte character or the file holds
/// genuine junk.
fn read_tail(path: &Path, max: u64) -> Option<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let skipped = len.saturating_sub(max);
    file.seek(SeekFrom::Start(skipped)).ok()?;
    let mut buf = Vec::with_capacity(len.min(max) as usize);
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    Some((text, skipped > 0))
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
        assert_eq!(
            sessions[0].id.as_str(),
            "11111111-1111-1111-1111-111111111111"
        );
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
    fn prompt_starting_with_pasted_markup_is_still_a_prompt() {
        // Only the known bookkeeping wrappers are skipped; a genuine prompt
        // that happens to start with pasted HTML must not be discarded.
        let d = tempfile::tempdir().unwrap();
        let base = d.path();
        let dir = Path::new("/proj/app");
        write_session(
            base,
            dir,
            "33333333-3333-3333-3333-333333333333",
            &[
                r#"{"type":"user","message":{"role":"user","content":"earlier prompt"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<div class=x> why does this overflow?"}}"#,
            ],
        );
        let sessions = list_sessions(base, dir);
        assert_eq!(
            sessions[0].last_command,
            "<div class=x> why does this overflow?"
        );
    }

    #[test]
    fn non_shell_safe_session_ids_are_skipped() {
        // The id is spliced into the `claude --resume <id>` command line, so a
        // transcript whose file stem isn't a plain UUID-ish name is refused.
        let d = tempfile::tempdir().unwrap();
        let base = d.path();
        let dir = Path::new("/proj/app");
        write_session(
            base,
            dir,
            "evil; touch pwned",
            &[r#"{"type":"user","message":{"role":"user","content":"hi"}}"#],
        );
        write_session(
            base,
            dir,
            "44444444-4444-4444-4444-444444444444",
            &[r#"{"type":"user","message":{"role":"user","content":"ok"}}"#],
        );
        let sessions = list_sessions(base, dir);
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].id.as_str(),
            "44444444-4444-4444-4444-444444444444"
        );
    }

    #[test]
    fn resume_id_accepts_uuids_and_rejects_shell_metacharacters() {
        assert!(ResumeId::new("11111111-1111-1111-1111-111111111111").is_some());
        assert!(ResumeId::new("abc-DEF-123").is_some());
        for bad in ["", "evil; touch pwned", "a b", "$(x)", "a'b", "id\n", "a/b"] {
            assert!(ResumeId::new(bad).is_none(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn huge_transcripts_are_read_from_the_tail() {
        // A transcript larger than the first tail window still yields its last
        // prompt (which lives near the end), and the partial first line the
        // byte cut leaves behind is ignored rather than parsed.
        let d = tempfile::tempdir().unwrap();
        let base = d.path();
        let dir = Path::new("/proj/app");
        let filler =
            r#"{"type":"assistant","message":{"role":"assistant","content":"xxxxxxxxxx"}}"#;
        let window = TAIL_READ_SIZES[0] as usize;
        let mut lines: Vec<&str> = vec![filler; (window / filler.len()) * 2];
        lines.push(r#"{"type":"user","message":{"role":"user","content":"the last prompt"}}"#);
        write_session(base, dir, "55555555-5555-5555-5555-555555555555", &lines);
        let sessions = list_sessions(base, dir);
        assert_eq!(sessions[0].last_command, "the last prompt");
    }

    #[test]
    fn prompt_buried_beyond_the_first_window_is_still_found() {
        // One long agentic turn: the user's last prompt is followed by more
        // tool/assistant output than the first tail window holds. The scan
        // must deepen to the next window rather than fall back to no label.
        let d = tempfile::tempdir().unwrap();
        let base = d.path();
        let dir = Path::new("/proj/app");
        let filler =
            r#"{"type":"assistant","message":{"role":"assistant","content":"xxxxxxxxxx"}}"#;
        let window = TAIL_READ_SIZES[0] as usize;
        let mut lines: Vec<&str> =
            vec![r#"{"type":"user","message":{"role":"user","content":"buried prompt"}}"#];
        lines.extend(vec![filler; (window / filler.len()) * 2]);
        write_session(base, dir, "66666666-6666-6666-6666-666666666666", &lines);
        let sessions = list_sessions(base, dir);
        assert_eq!(sessions[0].last_command, "buried prompt");
    }

    #[test]
    fn bare_lowercase_tags_are_wrappers_markup_prompts_are_not() {
        // The wrapper rule is structural, not a blocklist: any bare lowercase
        // tag opener is bookkeeping, anything else is a prompt.
        for wrapper in [
            "<bash-input>git status</bash-input>",
            "<user-memory-input>note</user-memory-input>",
            "<retrieval_status>done</retrieval_status>",
            "<task-notification>...</task-notification>",
        ] {
            assert!(is_wrapper(wrapper), "{wrapper} should be skipped");
        }
        for prompt in ["<div class=x> why?", "<H1> heading", "< 5 && > 3", "plain"] {
            assert!(!is_wrapper(prompt), "{prompt} should be kept");
        }
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
