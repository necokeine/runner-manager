use std::io::Read;
use std::path::{Path, PathBuf};

/// Most lines the viewer keeps from one file.
const MAX_LINES: usize = 5000;
/// Most bytes read from one file. `MAX_LINES` alone would not bound memory —
/// a multi-GB single-line file must not be slurped just to show its head.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// A read-only snapshot of a file for the right pane: the first
/// [`MAX_LINES`] lines plus a scroll position.
pub struct FileView {
    /// The file shown (used for the pane title).
    pub path: PathBuf,
    /// The capped lines, or a one-line placeholder for binary/unreadable files.
    pub lines: Vec<String>,
    /// Index of the first visible line.
    pub scroll: usize,
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

impl FileView {
    /// Read `path`, keeping at most [`MAX_LINES`] lines from its first
    /// [`MAX_BYTES`] bytes; a byte-capped file gets a trailing marker line so
    /// the cut-off is never mistaken for the end of the file. Binary
    /// (non-UTF-8) and unreadable files collapse to a one-line placeholder
    /// instead of erroring.
    pub fn load(path: &Path) -> FileView {
        let lines = match read_capped(path) {
            Ok((bytes, truncated)) => match decode(bytes, truncated) {
                Some(text) => {
                    let mut lines: Vec<String> = text
                        .lines()
                        .take(MAX_LINES)
                        .map(|l| l.to_string())
                        .collect();
                    if truncated {
                        lines.push(format!(
                            "<truncated: showing the first {} MB>",
                            MAX_BYTES / (1024 * 1024)
                        ));
                    }
                    lines
                }
                None => vec![format!("<binary file: {}>", name(path))],
            },
            Err(_) => vec![format!("<unable to read: {}>", name(path))],
        };
        FileView {
            path: path.to_path_buf(),
            lines,
            scroll: 0,
        }
    }

    /// Scroll down `n` lines, clamped to the last line.
    pub fn scroll_down(&mut self, n: usize) {
        let max = self.lines.len().saturating_sub(1);
        self.scroll = (self.scroll + n).min(max);
    }

    /// Scroll up `n` lines, clamped to the top.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }
}

/// Read at most [`MAX_BYTES`] bytes of `path`; the flag reports whether the
/// file was cut off there.
fn read_capped(path: &Path) -> std::io::Result<(Vec<u8>, bool)> {
    let file = std::fs::File::open(path)?;
    // Read one byte past the cap so we can tell "exactly MAX_BYTES long" from
    // "truncated"; the extra byte is dropped by the truncate below.
    let want = file.metadata()?.len().min(MAX_BYTES + 1) as usize;
    let mut buf = Vec::with_capacity(want);
    file.take(MAX_BYTES + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() as u64 > MAX_BYTES;
    buf.truncate(MAX_BYTES as usize);
    Ok((buf, truncated))
}

/// UTF-8-decode the (possibly byte-capped) file contents. `None` means the
/// file is genuinely binary; a multibyte character split by the byte cap is
/// not binary and is simply dropped.
fn decode(bytes: Vec<u8>, truncated: bool) -> Option<String> {
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(e) if truncated && e.utf8_error().error_len().is_none() => {
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            // Everything before the split character was already validated, so
            // this cannot fail; `ok()` avoids a panic path all the same.
            String::from_utf8(bytes).ok()
        }
        Err(_) => None,
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
    fn load_caps_bytes_and_survives_a_split_multibyte_char() {
        // A file larger than the byte cap whose cut point lands inside a
        // multibyte character must still load as text, not "<binary>".
        let mut f = NamedTempFile::new().unwrap();
        let cap = MAX_BYTES as usize;
        f.write_all(&vec![b'a'; cap - 1]).unwrap();
        f.write_all("é".as_bytes()).unwrap(); // straddles the cap boundary
        f.write_all(&[b'b'; 64]).unwrap();
        let v = FileView::load(f.path());
        assert_eq!(v.lines.len(), 2);
        assert!(v.lines[0].bytes().all(|b| b == b'a'));
        assert_eq!(v.lines[0].len(), cap - 1);
        // The cut-off is announced rather than silently presented as EOF.
        assert!(v.lines[1].starts_with("<truncated"));
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
