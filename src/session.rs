use std::collections::HashMap;
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

#[derive(Default)]
pub struct SessionRegistry {
    by_slug: HashMap<String, PathBuf>,
    by_path: HashMap<PathBuf, String>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn slug_for(&mut self, path: &Path, root: &Path) -> String {
        if let Some(existing) = self.by_path.get(path) {
            return existing.clone();
        }
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
        let base = slugify(&rel);
        let mut slug = base.clone();
        let mut n = 2;
        while self.by_slug.contains_key(&slug) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        self.by_slug.insert(slug.clone(), path.to_path_buf());
        self.by_path.insert(path.to_path_buf(), slug.clone());
        slug
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
    fn registry_is_stable_per_path() {
        let mut reg = SessionRegistry::new();
        let root = Path::new("/p");
        let a = reg.slug_for(Path::new("/p/src"), root);
        let b = reg.slug_for(Path::new("/p/src"), root);
        assert_eq!(a, b);
        assert_eq!(a, "src");
    }

    #[test]
    fn registry_disambiguates_collisions() {
        let mut reg = SessionRegistry::new();
        let root = Path::new("/p");
        let a = reg.slug_for(Path::new("/p/a.b"), root); // -> a_b
        let b = reg.slug_for(Path::new("/p/a:b"), root); // also -> a_b, must differ
        assert_eq!(a, "a_b");
        assert_eq!(b, "a_b-2");
    }
}
