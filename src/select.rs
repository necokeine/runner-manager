//! Mouse text selection over the embedded terminal pane.
//!
//! The right pane renders the embedded tmux client's vt100 screen. With the
//! host terminal in mouse-capture mode the OS/terminal can't do its own
//! drag-to-select over that pane, so we implement selection ourselves:
//! `run.rs` tracks an anchor/cursor pair of pane-relative cells as the user
//! drags (or expands a double-click into the word under it via [`word_at`]),
//! `ui.rs` paints the covered cells reversed, and on release the selected text
//! is pulled straight out of the vt100 screen (`vt100::Screen::
//! contents_between`, the canonical clipboard helper — it already trims
//! trailing blanks and stitches wrapped rows) and handed to the clipboard.

use tui_term::vt100;

/// A linear text selection over the terminal pane, in pane-relative cell
/// coordinates `(col, row)` — 0-based from the pane's top-left. `anchor` is
/// where the selection began, `cursor` where it currently ends; either may be
/// the upper-left, so callers go through [`Selection::ends`]. Both ends are
/// **inclusive**: a selection always covers at least one cell (a bare click
/// that should select nothing is represented by no `Selection` at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
}

impl Selection {
    /// A one-cell selection anchored at `(col, row)`.
    pub fn new(col: u16, row: u16) -> Self {
        Selection {
            anchor: (col, row),
            cursor: (col, row),
        }
    }

    /// `(start, end)` ordered in reading order (top-to-bottom, then
    /// left-to-right) so the rest of the code is agnostic to drag direction.
    fn ends(&self) -> ((u16, u16), (u16, u16)) {
        // Order by (row, col): row is the primary key for reading order.
        let key = |p: (u16, u16)| (p.1, p.0);
        if key(self.anchor) <= key(self.cursor) {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// Whether the pane cell at `(col, row)` falls inside the selection — used
    /// by the renderer to paint it reversed. The selection is linear: full rows
    /// between the start and end row, and bounded by the start column on the
    /// first row and the end column (inclusive) on the last.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        let (start, end) = self.ends();
        if row < start.1 || row > end.1 {
            return false;
        }
        let left_ok = row > start.1 || col >= start.0;
        let right_ok = row < end.1 || col <= end.0;
        left_ok && right_ok
    }
}

/// The text covered by `sel` on `screen`, ready for the clipboard. The end
/// cell is included (the user expects the cell under the pointer at release to
/// be copied), so the exclusive end column handed to vt100 is one past it,
/// clamped to the screen width.
pub fn selected_text(screen: &vt100::Screen, sel: &Selection) -> String {
    let (start, end) = sel.ends();
    let (_, cols) = screen.size();
    let end_col_excl = end.0.saturating_add(1).min(cols);
    screen.contents_between(start.1, start.0, end.1, end_col_excl)
}

/// Whether a cell's contents read as part of a word for double-click
/// selection. Besides alphanumerics, the set covers the punctuation that
/// terminal "words" are made of — paths, flags, URLs, slugs — mirroring the
/// word characters terminals like iTerm2 use for their own double-click.
fn is_word_cell(contents: &str) -> bool {
    let mut chars = contents.chars();
    match chars.next() {
        None => false,
        Some(c) => c.is_alphanumeric() || "_-./~@+:=".contains(c),
    }
}

/// The word under pane cell `(col, row)` on `screen`, as a single-row
/// [`Selection`], for double-click word selection. Expands left and right from
/// the clicked cell while cells keep reading as word characters (see
/// [`is_word_cell`]). `None` when the clicked cell is blank, whitespace, a
/// delimiter, or out of bounds.
pub fn word_at(screen: &vt100::Screen, col: u16, row: u16) -> Option<Selection> {
    let cell_is_word = |c: u16| {
        screen
            .cell(row, c)
            .is_some_and(|cell| is_word_cell(&cell.contents()))
    };
    if !cell_is_word(col) {
        return None;
    }
    let mut left = col;
    while left > 0 && cell_is_word(left - 1) {
        left -= 1;
    }
    let (_, cols) = screen.size();
    let mut right = col;
    while right + 1 < cols && cell_is_word(right + 1) {
        right += 1;
    }
    Some(Selection {
        anchor: (left, row),
        cursor: (right, row),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;
    use tui_term::vt100::Parser;

    /// A parser pre-loaded with a couple of lines, mirroring how `pty.rs`
    /// builds one. The lock keeps the borrow checker happy across `screen()`.
    fn screen_with(lines: &[&str]) -> RwLock<Parser> {
        let parser = RwLock::new(Parser::new(24, 80, 0));
        {
            let mut p = parser.write().unwrap();
            for line in lines {
                p.process(line.as_bytes());
                p.process(b"\r\n");
            }
        }
        parser
    }

    fn sel(a: (u16, u16), c: (u16, u16)) -> Selection {
        Selection {
            anchor: a,
            cursor: c,
        }
    }

    #[test]
    fn one_cell_selection_covers_and_copies_that_cell() {
        let lock = screen_with(&["hello world"]);
        let p = lock.read().unwrap();
        let s = Selection::new(3, 0);
        assert!(s.contains(3, 0));
        assert!(!s.contains(2, 0));
        assert!(!s.contains(4, 0));
        assert_eq!(selected_text(p.screen(), &s), "l");
    }

    #[test]
    fn single_row_selection_includes_the_cursor_cell() {
        let lock = screen_with(&["hello world"]);
        let p = lock.read().unwrap();
        // h(0) e(1) l(2) l(3) o(4): dragging 0..=4 yields "hello".
        let s = sel((0, 0), (4, 0));
        assert_eq!(selected_text(p.screen(), &s), "hello");
    }

    #[test]
    fn selection_is_direction_agnostic() {
        let lock = screen_with(&["hello world"]);
        let p = lock.read().unwrap();
        let forward = sel((0, 0), (4, 0));
        let backward = sel((4, 0), (0, 0));
        assert_eq!(
            selected_text(p.screen(), &forward),
            selected_text(p.screen(), &backward)
        );
    }

    #[test]
    fn multi_row_selection_spans_rows_and_trims_trailing_blanks() {
        let lock = screen_with(&["first line", "second line"]);
        let p = lock.read().unwrap();
        // From col 6 of row 0 ("line") through col 5 of row 1 ("second").
        let s = sel((6, 0), (5, 1));
        assert_eq!(selected_text(p.screen(), &s), "line\nsecond");
    }

    #[test]
    fn contains_marks_a_linear_block() {
        // Selection from (6,0) to (5,1): the tail of row 0 and the head of row 1.
        let s = sel((6, 0), (5, 1));
        assert!(s.contains(6, 0)); // first selected cell on row 0
        assert!(s.contains(20, 0)); // rest of row 0 is selected
        assert!(!s.contains(5, 0)); // before the start column on the first row
        assert!(s.contains(0, 1)); // start of the last row
        assert!(s.contains(5, 1)); // end column is inclusive
        assert!(!s.contains(6, 1)); // past the end column on the last row
        assert!(!s.contains(0, 2)); // below the selection entirely
    }

    #[test]
    fn word_at_expands_to_word_boundaries() {
        let lock = screen_with(&["run cargo-test now"]);
        let p = lock.read().unwrap();
        // Clicking anywhere inside "cargo-test" (cols 4..=13) selects all of it.
        let w = word_at(p.screen(), 7, 0).unwrap();
        assert_eq!(w, sel((4, 0), (13, 0)));
        assert_eq!(selected_text(p.screen(), &w), "cargo-test");
    }

    #[test]
    fn word_at_selects_path_like_tokens_whole() {
        let lock = screen_with(&["see src/select.rs:42 for it"]);
        let p = lock.read().unwrap();
        let w = word_at(p.screen(), 10, 0).unwrap();
        assert_eq!(selected_text(p.screen(), &w), "src/select.rs:42");
    }

    #[test]
    fn word_at_on_blank_or_delimiter_is_none() {
        let lock = screen_with(&["a b"]);
        let p = lock.read().unwrap();
        assert_eq!(word_at(p.screen(), 1, 0), None); // the space between
        assert_eq!(word_at(p.screen(), 40, 0), None); // blank cell past the text
        assert_eq!(word_at(p.screen(), 0, 5), None); // empty row below
    }

    #[test]
    fn word_at_stops_at_screen_edges() {
        let lock = screen_with(&["edge"]);
        let p = lock.read().unwrap();
        // A word starting at col 0 must not underflow while scanning left.
        let w = word_at(p.screen(), 0, 0).unwrap();
        assert_eq!(w, sel((0, 0), (3, 0)));
    }
}
