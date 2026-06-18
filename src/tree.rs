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

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
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

    pub fn visible_rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        Self::collect(&self.root, 0, &mut out);
        out
    }

    fn collect(node: &Node, depth: usize, out: &mut Vec<Row>) {
        out.push(Row {
            path: node.path.clone(),
            name: node.name.clone(),
            is_dir: node.is_dir,
            depth,
            expanded: node.expanded,
        });
        if node.is_dir && node.expanded {
            if let Some(children) = &node.children {
                for c in children {
                    Self::collect(c, depth + 1, out);
                }
            }
        }
    }

    pub fn node_at_mut(&mut self, path: &Path) -> Option<&mut Node> {
        Self::find_mut(&mut self.root, path)
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

    #[test]
    fn root_expands_and_orders_dirs_first_then_alpha() {
        let dir = setup();
        let tree = Tree::new(dir.path().to_path_buf());
        let rows = tree.visible_rows();
        // row 0 is the root itself; children follow
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let child_names = &names[1..];
        assert_eq!(child_names, &["asub", "zsub", "readme.md"]);
        assert!(rows[1].is_dir);
        assert!(!rows.iter().any(|r| r.name == "inner.txt")); // not expanded yet
    }

    #[test]
    fn toggle_expands_and_collapses_lazily() {
        let dir = setup();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let zsub = dir.path().join("zsub");
        tree.node_at_mut(&zsub).unwrap().toggle(); // expand
        assert!(tree.visible_rows().iter().any(|r| r.name == "inner.txt"));
        tree.node_at_mut(&zsub).unwrap().toggle(); // collapse
        assert!(!tree.visible_rows().iter().any(|r| r.name == "inner.txt"));
    }

    #[test]
    fn depth_increases_for_children() {
        let dir = setup();
        let mut tree = Tree::new(dir.path().to_path_buf());
        let zsub = dir.path().join("zsub");
        tree.node_at_mut(&zsub).unwrap().toggle();
        let inner = tree.visible_rows().into_iter().find(|r| r.name == "inner.txt").unwrap();
        assert_eq!(inner.depth, 2);
    }
}
