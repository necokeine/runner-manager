use std::collections::HashMap;
use std::path::PathBuf;

use crate::session::{SessionKind, SessionRow};
use crate::tree::Node;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Dir { expanded: bool },
    Session { slug: String, kind: SessionKind },
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub label: String,
    pub depth: usize,
    pub kind: RowKind,
}

pub fn build_rows(root: &Node, sessions: &HashMap<PathBuf, Vec<SessionRow>>) -> Vec<Row> {
    let mut out = Vec::new();
    collect(root, 0, sessions, &mut out);
    out
}

fn collect(node: &Node, depth: usize, sessions: &HashMap<PathBuf, Vec<SessionRow>>, out: &mut Vec<Row>) {
    if node.is_dir {
        out.push(Row {
            path: node.path.clone(),
            label: node.name.clone(),
            depth,
            kind: RowKind::Dir { expanded: node.expanded },
        });
        if let Some(sess) = sessions.get(&node.path) {
            for s in sess {
                out.push(Row {
                    path: node.path.clone(),
                    label: s.label.clone(),
                    depth: depth + 1,
                    kind: RowKind::Session { slug: s.slug.clone(), kind: s.kind },
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
            vec![SessionRow { slug: "root-shell".into(), kind: SessionKind::Shell, label: "shell".into() }],
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
    fn sessions_show_even_when_dir_collapsed() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("a.txt"), "x").unwrap();
        let tree = Tree::new(dir.path().to_path_buf());
        // 'sub' is collapsed (not expanded), but give it a session
        let mut sessions: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        sessions.insert(
            dir.path().join("sub"),
            vec![SessionRow { slug: "sub-shell".into(), kind: SessionKind::Shell, label: "shell".into() }],
        );
        let rows = build_rows(&tree.root, &sessions);
        // 'sub' dir row is present, immediately followed by its session row,
        // and a.txt is NOT present (sub is collapsed)
        let sub_idx = rows.iter().position(|r| r.label == "sub").unwrap();
        assert!(matches!(rows[sub_idx + 1].kind, RowKind::Session { .. }));
        assert!(!rows.iter().any(|r| r.label == "a.txt"));
    }
}
