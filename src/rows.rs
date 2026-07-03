use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::session::{SessionKind, SessionRow};
use crate::tree::Node;

/// What a visible row represents; drives its rendering and what activating or
/// clicking it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// A directory (toggles expansion; carries the `[+]` new-session button).
    Dir { expanded: bool },
    /// A tmux session (switches the embedded client; carries the `[×]` button).
    Session { slug: String, kind: SessionKind },
    /// A regular file (opens in the read-only viewer).
    File,
}

/// One visible row of the left pane. The row list is derived state — rebuilt
/// by `App::rebuild_rows` after any tree or session change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The filesystem path this row refers to (the session's dir for sessions).
    pub path: PathBuf,
    /// Rendered text (without the trailing `[+]`/`[×]` button).
    pub label: String,
    /// Nesting depth for indentation (root = 0).
    pub depth: usize,
    /// What the row is, with its kind-specific payload.
    pub kind: RowKind,
}

/// Flatten the visible (expanded) tree into rows, nesting each directory's
/// sessions right under it.
pub fn build_rows(root: &Node, sessions: &HashMap<PathBuf, Vec<SessionRow>>) -> Vec<Row> {
    let mut out = Vec::new();
    collect(root, 0, sessions, &mut out);
    out
}

/// Flat list of every open session for the "project view" tab. Each session is
/// one depth-0 `Session` row whose label carries the session kind, its directory
/// relative to `root`, and a short brief (the active pane's foreground command,
/// looked up by slug in `briefs`). Reusing `RowKind::Session` keeps selection,
/// switching, and the `[×]` close button working exactly as in the directory
/// view. Sessions are grouped by directory (sorted) for a stable order.
pub fn build_project_rows(
    sessions: &HashMap<PathBuf, Vec<SessionRow>>,
    root: &Path,
    briefs: &HashMap<String, String>,
) -> Vec<Row> {
    let mut dirs: Vec<&PathBuf> = sessions.keys().collect();
    dirs.sort();
    let mut out = Vec::new();
    for dir in dirs {
        let rel = dir
            .strip_prefix(root)
            .unwrap_or(dir)
            .to_string_lossy()
            .into_owned();
        let rel = if rel.is_empty() { ".".to_string() } else { rel };
        for s in &sessions[dir] {
            let brief = briefs.get(&s.slug).map(String::as_str).unwrap_or("");
            let label = if brief.is_empty() {
                format!("{}  {rel}", s.label)
            } else {
                format!("{}  {rel}  — {brief}", s.label)
            };
            out.push(Row {
                path: dir.clone(),
                label,
                depth: 0,
                kind: RowKind::Session {
                    slug: s.slug.clone(),
                    kind: s.kind,
                },
            });
        }
    }
    out
}

fn collect(
    node: &Node,
    depth: usize,
    sessions: &HashMap<PathBuf, Vec<SessionRow>>,
    out: &mut Vec<Row>,
) {
    if node.is_dir {
        out.push(Row {
            path: node.path.clone(),
            label: node.name.clone(),
            depth,
            kind: RowKind::Dir {
                expanded: node.expanded,
            },
        });
        if let Some(sess) = sessions.get(&node.path) {
            for s in sess {
                out.push(Row {
                    path: node.path.clone(),
                    label: s.label.clone(),
                    depth: depth + 1,
                    kind: RowKind::Session {
                        slug: s.slug.clone(),
                        kind: s.kind,
                    },
                });
            }
        }
        if node.expanded {
            if let Some(children) = &node.children {
                for c in children {
                    collect(c, depth + 1, sessions, out);
                }
            }
        }
    } else {
        out.push(Row {
            path: node.path.clone(),
            label: node.name.clone(),
            depth,
            kind: RowKind::File,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionKind, SessionRow};
    use crate::tree::Tree;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn rows_show_dir_then_sessions_then_files_when_expanded() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("readme.md"), "x").unwrap();
        let tree = Tree::new(dir.path().to_path_buf()); // root expanded, children loaded
        let mut sessions: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        sessions.insert(
            dir.path().to_path_buf(),
            vec![SessionRow {
                slug: "root-shell".into(),
                kind: SessionKind::Shell,
                label: "shell".into(),
            }],
        );
        let rows = build_rows(&tree.root, &sessions);
        // row 0 = root dir; row 1 = its shell session (depth 1); row 2 = readme.md (depth 1)
        assert!(matches!(rows[0].kind, RowKind::Dir { .. }));
        assert!(matches!(rows[1].kind, RowKind::Session { .. }));
        assert_eq!(rows[1].label, "shell");
        assert_eq!(rows[1].depth, 1);
        assert!(matches!(rows[2].kind, RowKind::File));
        assert_eq!(rows[2].label, "readme.md");
    }

    #[test]
    fn project_rows_are_flat_sessions_with_dir_and_brief() {
        let root = PathBuf::from("/p");
        let mut sessions: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        sessions.insert(
            PathBuf::from("/p/src"),
            vec![SessionRow {
                slug: "src-claude".into(),
                kind: SessionKind::Claude,
                label: "claude".into(),
            }],
        );
        sessions.insert(
            PathBuf::from("/p"),
            vec![SessionRow {
                slug: "root-shell".into(),
                kind: SessionKind::Shell,
                label: "shell".into(),
            }],
        );
        let mut briefs: HashMap<String, String> = HashMap::new();
        briefs.insert("src-claude".into(), "node".into());
        // root-shell has no brief recorded -> its label omits the dash.
        let rows = build_project_rows(&sessions, &root, &briefs);
        // Sorted by dir: "/p" before "/p/src".
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "shell  .");
        assert!(matches!(
            rows[0].kind,
            RowKind::Session {
                kind: SessionKind::Shell,
                ..
            }
        ));
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].label, "claude  src  — node");
        assert!(matches!(
            rows[1].kind,
            RowKind::Session {
                kind: SessionKind::Claude,
                ..
            }
        ));
    }

    #[test]
    fn sessions_show_even_when_dir_collapsed() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("a.txt"), "x").unwrap();
        let tree = Tree::new(dir.path().to_path_buf());
        // 'sub' is collapsed (not expanded), but give it a session
        let mut sessions: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        sessions.insert(
            dir.path().join("sub"),
            vec![SessionRow {
                slug: "sub-shell".into(),
                kind: SessionKind::Shell,
                label: "shell".into(),
            }],
        );
        let rows = build_rows(&tree.root, &sessions);
        // 'sub' dir row is present, immediately followed by its session row,
        // and a.txt is NOT present (sub is collapsed)
        let sub_idx = rows.iter().position(|r| r.label == "sub").unwrap();
        assert!(matches!(rows[sub_idx + 1].kind, RowKind::Session { .. }));
        assert!(!rows.iter().any(|r| r.label == "a.txt"));
    }
}
