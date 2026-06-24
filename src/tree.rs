use std::fs;
use std::path::{Path, PathBuf};

pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub children: Option<Vec<Node>>,
}

impl Node {
    pub fn new(path: PathBuf, is_dir: bool) -> Self {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self { path, name, is_dir, expanded: false, children: None }
    }

    pub fn load_children(&mut self) {
        if !self.is_dir {
            return;
        }
        let mut entries: Vec<Node> = Vec::new();
        if let Ok(read) = fs::read_dir(&self.path) {
            for e in read.flatten() {
                let p = e.path();
                let is_dir = p.is_dir();
                entries.push(Node::new(p, is_dir));
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.children = Some(entries);
    }

    pub fn toggle(&mut self) {
        if !self.is_dir {
            return;
        }
        if self.expanded {
            self.expanded = false;
        } else {
            if self.children.is_none() {
                self.load_children();
            }
            self.expanded = true;
        }
    }
}

pub struct Tree {
    pub root: Node,
}

impl Tree {
    pub fn new(root_path: PathBuf) -> Self {
        let mut root = Node::new(root_path, true);
        root.load_children();
        root.expanded = true;
        Self { root }
    }

    pub fn node_at_mut(&mut self, path: &Path) -> Option<&mut Node> {
        Self::find_mut(&mut self.root, path)
    }

    /// The paths of every currently-expanded directory (absolute), excluding
    /// the root itself (which is always expanded). Only the visible expanded
    /// subtree is walked — a dir hidden under a collapsed ancestor is not
    /// reported — so the set always re-applies cleanly via `apply_expanded`.
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        Self::collect_expanded(&self.root, true, &mut out);
        out
    }

    fn collect_expanded(node: &Node, is_root: bool, out: &mut Vec<PathBuf>) {
        if !node.expanded {
            return;
        }
        if node.is_dir && !is_root {
            out.push(node.path.clone());
        }
        if let Some(children) = &node.children {
            for c in children {
                Self::collect_expanded(c, false, out);
            }
        }
    }

    /// Re-expand the given directories (absolute paths), loading children
    /// lazily as needed. Applied shallow-to-deep so each directory's ancestors
    /// are expanded (and their children loaded) before it is looked up; paths
    /// no longer present are skipped.
    pub fn apply_expanded(&mut self, dirs: &[PathBuf]) {
        let mut dirs: Vec<&PathBuf> = dirs.iter().collect();
        dirs.sort_by_key(|p| p.components().count());
        for p in dirs {
            if let Some(node) = self.node_at_mut(p) {
                if node.is_dir && !node.expanded {
                    if node.children.is_none() {
                        node.load_children();
                    }
                    node.expanded = true;
                }
            }
        }
    }

    fn find_mut<'a>(node: &'a mut Node, path: &Path) -> Option<&'a mut Node> {
        if node.path == path {
            return Some(node);
        }
        if let Some(children) = node.children.as_mut() {
            for c in children.iter_mut() {
                if let Some(found) = Self::find_mut(c, path) {
                    return Some(found);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn setup() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("zsub")).unwrap();
        fs::create_dir(dir.path().join("asub")).unwrap();
        fs::write(dir.path().join("readme.md"), "x").unwrap();
        fs::write(dir.path().join("zsub").join("inner.txt"), "y").unwrap();
        dir
    }

    fn rows_of(tree: &Tree) -> Vec<crate::rows::Row> {
        crate::rows::build_rows(&tree.root, &std::collections::HashMap::new())
    }

    #[test]
    fn root_expands_and_orders_dirs_first_then_alpha() {
        let dir = setup();
        let tree = Tree::new(dir.path().to_path_buf());
        let rows = rows_of(&tree);
        // row 0 is the root itself; children follow
        let names: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        let child_names = &names[1..];
        assert_eq!(child_names, &["asub", "zsub", "readme.md"]);
        assert!(matches!(rows[1].kind, crate::rows::RowKind::Dir { .. }));
        assert!(!rows.iter().any(|r| r.label == "inner.txt")); // not expanded yet
    }

    #[test]
    fn toggle_expands_and_collapses_lazily() {
        let dir = setup();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let zsub = dir.path().join("zsub");
        tree.node_at_mut(&zsub).unwrap().toggle(); // expand
        assert!(rows_of(&tree).iter().any(|r| r.label == "inner.txt"));
        tree.node_at_mut(&zsub).unwrap().toggle(); // collapse
        assert!(!rows_of(&tree).iter().any(|r| r.label == "inner.txt"));
    }

    #[test]
    fn expanded_dirs_then_apply_roundtrips() {
        let dir = setup();
        fs::create_dir(dir.path().join("zsub").join("deep")).unwrap();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let zsub = dir.path().join("zsub");
        let deep = zsub.join("deep");
        tree.node_at_mut(&zsub).unwrap().toggle(); // expand zsub
        tree.node_at_mut(&deep).unwrap().toggle(); // expand zsub/deep

        let mut saved = tree.expanded_dirs();
        saved.sort();
        assert_eq!(saved, vec![zsub.clone(), deep.clone()]);
        // The root is always expanded but never reported.
        assert!(!saved.contains(&dir.path().to_path_buf()));

        // A fresh tree starts collapsed; applying the saved set (in any order)
        // re-expands both, loading children lazily for the deep one.
        let mut fresh = Tree::new(dir.path().to_path_buf());
        assert!(fresh.expanded_dirs().is_empty());
        fresh.apply_expanded(&[deep.clone(), zsub.clone()]);
        assert!(fresh.node_at_mut(&zsub).unwrap().expanded);
        assert!(fresh.node_at_mut(&deep).unwrap().expanded);
    }

    #[test]
    fn apply_expanded_skips_missing_paths() {
        let dir = setup();
        let mut tree = Tree::new(dir.path().to_path_buf());
        tree.apply_expanded(&[dir.path().join("does-not-exist")]);
        assert!(tree.expanded_dirs().is_empty());
    }

    #[test]
    fn depth_increases_for_children() {
        let dir = setup();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let zsub = dir.path().join("zsub");
        tree.node_at_mut(&zsub).unwrap().toggle();
        let inner = rows_of(&tree).into_iter().find(|r| r.label == "inner.txt").unwrap();
        assert_eq!(inner.depth, 2);
    }
}
