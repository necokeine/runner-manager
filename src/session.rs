use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn slugify(rel: &str) -> String {
    if rel.is_empty() || rel == "." {
        return "root".to_string();
    }
    let s: String = rel
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else if c == '/' {
                '-'
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "root".to_string()
    } else {
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKind {
    Shell,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudePerm {
    Normal,
    Skip,
}

impl SessionKind {
    pub fn label_base(&self) -> &'static str {
        match self {
            SessionKind::Shell => "shell",
            SessionKind::Claude => "claude",
        }
    }

    /// Parse the kind from a session's `@rm` tag; anything but "claude" (including
    /// an empty/missing tag) is treated as a shell.
    pub fn from_tag(tag: &str) -> SessionKind {
        match tag {
            "claude" => SessionKind::Claude,
            _ => SessionKind::Shell,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    dir: PathBuf,
    kind: SessionKind,
    slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub slug: String,
    pub kind: SessionKind,
    pub label: String,
}

#[derive(Default)]
pub struct SessionStore {
    entries: Vec<Entry>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, dir: &Path, root: &Path, kind: SessionKind) -> String {
        let dir_slug = slugify(&dir.strip_prefix(root).unwrap_or(dir).to_string_lossy());
        let base = format!("{dir_slug}-{}", kind.label_base());
        let mut slug = base.clone();
        let mut n = 2;
        while self.entries.iter().any(|e| e.slug == slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        self.entries.push(Entry {
            dir: dir.to_path_buf(),
            kind,
            slug: slug.clone(),
        });
        slug
    }

    pub fn sync(&mut self, live: &HashSet<String>) {
        self.entries.retain(|e| live.contains(&e.slug));
    }

    /// Pull live sessions that this tool created on a previous run (they carry a
    /// `@rm` dir tag) back into the store. Sessions already tracked, or without a
    /// directory tag (the embedded client, hand-made sessions), are skipped.
    /// Returns the slugs newly adopted.
    pub fn adopt(&mut self, sessions: &[(String, PathBuf, SessionKind)]) -> Vec<String> {
        let mut adopted = Vec::new();
        for (slug, dir, kind) in sessions {
            if self.entries.iter().any(|e| &e.slug == slug) {
                continue;
            }
            self.entries.push(Entry { dir: dir.clone(), kind: *kind, slug: slug.clone() });
            adopted.push(slug.clone());
        }
        adopted
    }

    pub fn by_dir(&self) -> HashMap<PathBuf, Vec<SessionRow>> {
        let mut map: HashMap<PathBuf, Vec<SessionRow>> = HashMap::new();
        let mut counts: HashMap<(PathBuf, SessionKind), usize> = HashMap::new();
        for e in &self.entries {
            let c = counts.entry((e.dir.clone(), e.kind)).or_insert(0);
            *c += 1;
            let label = if *c == 1 {
                e.kind.label_base().to_string()
            } else {
                format!("{} {}", e.kind.label_base(), c)
            };
            map.entry(e.dir.clone()).or_default().push(SessionRow {
                slug: e.slug.clone(),
                kind: e.kind,
                label,
            });
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn slugify_basic_and_separators() {
        assert_eq!(slugify("src"), "src");
        assert_eq!(slugify("src/proto"), "src-proto");
        assert_eq!(slugify("my dir"), "my_dir");
        assert_eq!(slugify(".config"), "_config");
        assert_eq!(slugify("a.b"), "a_b");
        assert_eq!(slugify(""), "root");
        assert_eq!(slugify("."), "root");
    }

    #[test]
    fn slugify_keeps_unicode_letters() {
        assert_eq!(slugify("café"), "café");
    }

    #[test]
    fn store_creates_unique_slugs_per_kind() {
        let mut s = SessionStore::new();
        let root = Path::new("/p");
        let a = s.create(Path::new("/p/src"), root, SessionKind::Shell);
        let b = s.create(Path::new("/p/src"), root, SessionKind::Shell);
        let c = s.create(Path::new("/p/src"), root, SessionKind::Claude);
        assert_eq!(a, "src-shell");
        assert_eq!(b, "src-shell-2");
        assert_eq!(c, "src-claude");
    }

    #[test]
    fn store_labels_index_duplicates_by_dir_and_kind() {
        let mut s = SessionStore::new();
        let root = Path::new("/p");
        s.create(Path::new("/p/src"), root, SessionKind::Shell);
        s.create(Path::new("/p/src"), root, SessionKind::Shell);
        s.create(Path::new("/p/src"), root, SessionKind::Claude);
        let by = s.by_dir();
        let rows = &by[&PathBuf::from("/p/src")];
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["shell", "shell 2", "claude"]);
    }

    #[test]
    fn adopt_adds_untracked_sessions_and_skips_known() {
        let mut s = SessionStore::new();
        let root = Path::new("/p");
        let existing = s.create(Path::new("/p/src"), root, SessionKind::Shell);
        // a prior-run session (would come back from tmux on restart) plus the
        // already-tracked one; only the new one should be adopted.
        let adopted = s.adopt(&[
            (existing.clone(), PathBuf::from("/p/src"), SessionKind::Shell),
            ("p-claude".into(), PathBuf::from("/p"), SessionKind::Claude),
        ]);
        assert_eq!(adopted, vec!["p-claude".to_string()]);
        let by = s.by_dir();
        assert_eq!(by[&PathBuf::from("/p/src")].len(), 1);
        let root_rows = &by[&PathBuf::from("/p")];
        assert_eq!(root_rows.len(), 1);
        assert_eq!(root_rows[0].slug, "p-claude");
        assert_eq!(root_rows[0].kind, SessionKind::Claude);
        // a re-adopt of the same set is a no-op (slugs already present)
        assert!(s.adopt(&[("p-claude".into(), PathBuf::from("/p"), SessionKind::Claude)]).is_empty());
    }

    #[test]
    fn kind_from_tag_defaults_to_shell() {
        assert_eq!(SessionKind::from_tag("claude"), SessionKind::Claude);
        assert_eq!(SessionKind::from_tag("shell"), SessionKind::Shell);
        assert_eq!(SessionKind::from_tag(""), SessionKind::Shell);
    }

    #[test]
    fn store_sync_prunes_dead_sessions() {
        let mut s = SessionStore::new();
        let root = Path::new("/p");
        let a = s.create(Path::new("/p/src"), root, SessionKind::Shell);
        let _b = s.create(Path::new("/p/src"), root, SessionKind::Claude);
        let live: std::collections::HashSet<String> = [a.clone()].into_iter().collect();
        s.sync(&live);
        let by = s.by_dir();
        let rows = &by[&PathBuf::from("/p/src")];
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, a);
    }
}
