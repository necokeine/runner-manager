//! Mouse text selection over the embedded terminal pane.
//!
//! The right pane renders the embedded tmux client's vt100 screen. With the
//! host terminal in mouse-capture mode the OS can't do its own drag-to-select,
//! so we implement selection ourselves: `run.rs` tracks an anchor/cursor pair
//! of pane-relative cells as the user drags, `ui.rs` paints the covered cells
//! reversed, and on release the selected text is pulled straight out of the
//! vt100 screen (`vt100::Screen::contents_between`, the canonical clipboard
//! helper — it already trims trailing blanks and stitches wrapped rows) and
//! handed to the clipboard.

use tui_term::vt100;

/// A linear text selection over the terminal pane, in pane-relative cell
/// coordinates `(col, row)` — 0-based from the pane's top-left. `anchor` is
/// where the drag began, `cursor` where it currently is; either may be the
/// upper-left, so callers go through [`Selection::ends`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
}

impl Selection {
    /// A zero-width selection anchored at one cell (a bare click before any
    /// drag). Reports [`is_empty`](Self::is_empty) until the cursor moves.
    pub fn new(col: u16, row: u16) -> Self {
        Selection { anchor: (col, row), cursor: (col, row) }
    }

    /// True when anchor and cursor are the same cell — a plain click that
    /// covers nothing and must not copy.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.cursor
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
        if self.is_empty() {
            return false;
        }
        let (start, end) = self.ends();
        if row < start.1 || row > end.1 {
            return false;
        }
        let left_ok = row > start.1 || col >= start.0;
        let right_ok = row < end.1 || col <= end.0;
        left_ok && right_ok
    }
}

/// The text covered by `sel` on `screen`, ready for the clipboard. Empty for an
/// empty selection. The end cell is included (the user expects the cell under
/// the pointer at release to be copied), so the exclusive end column handed to
/// vt100 is one past it, clamped to the screen width.
pub fn selected_text(screen: &vt100::Screen, sel: &Selection) -> String {
    if sel.is_empty() {
        return String::new();
    }
    let (start, end) = sel.ends();
    let (_, cols) = screen.size();
    let end_col_excl = end.0.saturating_add(1).min(cols);
    screen.contents_between(start.1, start.0, end.1, end_col_excl)
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
        Selection { anchor: a, cursor: c }
    }

    #[test]
    fn empty_selection_covers_nothing_and_copies_nothing() {
        let lock = screen_with(&["hello world"]);
        let p = lock.read().unwrap();
        let s = Selection::new(3, 0);
        assert!(s.is_empty());
        assert!(!s.contains(3, 0));
        assert_eq!(selected_text(p.screen(), &s), "");
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
}
