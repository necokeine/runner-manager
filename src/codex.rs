//! Discovery of resumable Codex CLI sessions.
//!
//! Codex stores interactive session transcripts under
//! `~/.codex/sessions/YYYY/MM/DD/*.jsonl`. Unlike Claude's per-project folder
//! layout, Codex histories are date-bucketed, so this module scans recent JSONL
//! transcripts and filters them by the `session_meta.payload.cwd` recorded in
//! each file.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::resume::ResumeSession;

/// Longest `last_command` we retain; the UI truncates further to fit the popup.
const MAX_COMMAND_LEN: usize = 200;
/// Cap on how many transcript files we inspect when opening the chooser. Codex
/// histories are global rather than per-project, so keep the scan bounded.
const MAX_SCAN_FILES: usize = 500;
/// Cap on how many matching sessions are returned for a directory.
const MAX_SESSIONS: usize = 10;
/// Cap on how many resumable Codex sessions the project tab shows.
const MAX_PROJECT_SESSIONS: usize = 30;

/// One Codex transcript that can be resumed from the project tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSession {
    pub dir: PathBuf,
    pub session: ResumeSession,
}

/// Base directory holding Codex transcripts (`~/.codex/sessions`).
/// `RM_CODEX_SESSIONS` overrides it for tests and non-standard installs.
pub fn sessions_base() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RM_CODEX_SESSIONS") {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("CODEX_HOME") {
        return Some(Path::new(&p).join("sessions"));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".codex").join("sessions"))
}

/// List resumable Codex sessions for `dir`, most recently modified first.
/// Missing or unreadable history yields an empty list.
pub fn list_sessions(base: &Path, dir: &Path) -> Vec<ResumeSession> {
    let target = comparable_path(dir);
    let mut files = Vec::new();
    collect_jsonl(base, &mut files);
    files.sort_by_key(|f| std::cmp::Reverse(f.1));

    let mut out = Vec::new();
    for (path, modified) in files.into_iter().take(MAX_SCAN_FILES) {
        if let Some(parsed) = read_session(&path, modified) {
            if comparable_path(&parsed.dir) != target {
                continue;
            }
            out.push(parsed.session);
            if out.len() >= MAX_SESSIONS {
                break;
            }
        }
    }
    out
}

/// List resumable Codex sessions whose recorded cwd is inside `root`, newest
/// first. This powers the project tab's "local Codex history" rows.
pub fn list_sessions_under(base: &Path, root: &Path) -> Vec<ProjectSession> {
    let target_root = comparable_path(root);
    let mut files = Vec::new();
    collect_jsonl(base, &mut files);
    files.sort_by_key(|f| std::cmp::Reverse(f.1));

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (path, modified) in files.into_iter().take(MAX_SCAN_FILES) {
        let Some(parsed) = read_session(&path, modified) else {
            continue;
        };
        let dir = comparable_path(&parsed.dir);
        if !dir.starts_with(&target_root) || !seen.insert(parsed.session.id.clone()) {
            continue;
        }
        out.push(ProjectSession {
            dir,
            session: parsed.session,
        });
        if out.len() >= MAX_PROJECT_SESSIONS {
            break;
        }
    }
    out
}

fn collect_jsonl(dir: &Path, out: &mut Vec<(PathBuf, SystemTime)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((path, mtime));
        }
    }
}

struct ParsedSession {
    dir: PathBuf,
    session: ResumeSession,
}

fn read_session(path: &Path, modified: SystemTime) -> Option<ParsedSession> {
    let content = fs::read_to_string(path).ok()?;
    let mut id: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut last: Option<String> = None;

    for line in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) == Some("session_meta") {
            let payload = value.get("payload");
            if id.is_none() {
                id = payload
                    .and_then(|p| p.get("session_id").or_else(|| p.get("id")))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
            if cwd.is_none() {
                cwd = payload
                    .and_then(|p| p.get("cwd"))
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
            }
            continue;
        }
        if let Some(text) = user_message_text(&value) {
            let text = text.trim();
            if !text.is_empty() && !text.starts_with('<') {
                last = Some(collapse(text));
            }
        }
    }

    let dir = cwd?;
    Some(ParsedSession {
        dir,
        session: ResumeSession {
            id: id.or_else(|| id_from_filename(path))?,
            last_command: last.unwrap_or_default(),
            modified,
        },
    })
}

fn user_message_text(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("response_item") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(|t| t.as_str()) != Some("message") {
        return None;
    }
    if payload.get("role").and_then(|r| r.as_str()) != Some("user") {
        return None;
    }
    Some(message_text(payload.get("content")))
}

fn message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter(|b| {
                matches!(
                    b.get("type").and_then(|t| t.as_str()),
                    Some("input_text" | "text")
                )
            })
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn collapse(text: &str) -> String {
    let mut out: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > MAX_COMMAND_LEN {
        out = out.chars().take(MAX_COMMAND_LEN).collect();
        out.push_str("...");
    }
    out
}

fn comparable_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let tail: String = stem
        .chars()
        .rev()
        .take(36)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    (tail.len() == 36).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn write_session(base: &Path, day: &str, id: &str, cwd: &Path, lines: &[&str]) -> PathBuf {
        let dir = base.join(day);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-06-30T00-00-00-{id}.jsonl"));
        let mut all = vec![format!(
            r#"{{"type":"session_meta","payload":{{"session_id":"{id}","cwd":"{}"}}}}"#,
            cwd.display()
        )];
        all.extend(lines.iter().map(|s| s.to_string()));
        fs::write(&path, all.join("\n")).unwrap();
        path
    }

    #[test]
    fn lists_sessions_for_matching_cwd_and_extracts_last_user_prompt() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("sessions");
        let cwd = d.path().join("proj");
        fs::create_dir(&cwd).unwrap();
        write_session(
            &base,
            "2026/06/30",
            "11111111-1111-1111-1111-111111111111",
            &cwd,
            &[
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"first thing"}]}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix   codex\nresume"}]}}"#,
            ],
        );

        let sessions = list_sessions(&base, &cwd);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(sessions[0].last_command, "fix codex resume");
    }

    #[test]
    fn filters_sessions_by_cwd() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("sessions");
        let wanted = d.path().join("wanted");
        let other = d.path().join("other");
        fs::create_dir(&wanted).unwrap();
        fs::create_dir(&other).unwrap();
        write_session(
            &base,
            "2026/06/30",
            "11111111-1111-1111-1111-111111111111",
            &wanted,
            &[
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":"keep me"}}"#,
            ],
        );
        write_session(
            &base,
            "2026/06/29",
            "22222222-2222-2222-2222-222222222222",
            &other,
            &[
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":"skip me"}}"#,
            ],
        );

        let sessions = list_sessions(&base, &wanted);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn project_listing_finds_sessions_under_root() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("sessions");
        let root = d.path().join("work");
        let inside = root.join("runner-manager");
        let outside = d.path().join("elsewhere");
        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&outside).unwrap();

        write_session(
            &base,
            "2026/06/29",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            &inside,
            &[
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":"inside root"}}"#,
            ],
        );
        write_session(
            &base,
            "2026/06/30",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            &outside,
            &[
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":"outside root"}}"#,
            ],
        );

        let sessions = list_sessions_under(&base, &root);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].dir, inside);
        assert_eq!(
            sessions[0].session.id,
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        );
        assert_eq!(sessions[0].session.last_command, "inside root");
    }

    #[test]
    fn missing_history_yields_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(list_sessions(&d.path().join("missing"), d.path()).is_empty());
    }
}
