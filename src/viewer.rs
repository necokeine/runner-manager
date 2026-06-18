use std::path::{Path, PathBuf};

const MAX_LINES: usize = 5000;

pub struct FileView {
    pub path: PathBuf,
    pub lines: Vec<String>,
    pub scroll: usize,
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

impl FileView {
    pub fn load(path: &Path) -> FileView {
        let lines = match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text.lines().take(MAX_LINES).map(|l| l.to_string()).collect(),
                Err(_) => vec![format!("<binary file: {}>", name(path))],
            },
            Err(_) => vec![format!("<unable to read: {}>", name(path))],
        };
        FileView {
            path: path.to_path_buf(),
            lines,
            scroll: 0,
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max = self.lines.len().saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn load_reads_utf8_lines() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
        let v = FileView::load(f.path());
        assert_eq!(v.lines, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn load_binary_shows_placeholder() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0xff, 0xfe, 0x00, 0x01]).unwrap();
        let v = FileView::load(f.path());
        assert_eq!(v.lines.len(), 1);
        assert!(v.lines[0].starts_with("<binary file:"));
    }

    #[test]
    fn load_caps_line_count() {
        let mut f = NamedTempFile::new().unwrap();
        for _ in 0..6000 {
            writeln!(f, "x").unwrap();
        }
        let v = FileView::load(f.path());
        assert_eq!(v.lines.len(), 5000);
    }

    #[test]
    fn scroll_clamps() {
        let v0 = FileView {
            path: std::path::PathBuf::from("/x"),
            lines: vec!["a".into(), "b".into(), "c".into()],
            scroll: 0,
        };
        let mut v = v0;
        v.scroll_down(10);
        assert_eq!(v.scroll, 2); // clamped to lines.len()-1
        v.scroll_up(10);
        assert_eq!(v.scroll, 0);
    }
}
