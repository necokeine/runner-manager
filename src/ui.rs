use ratatui::layout::{Alignment, Constraint, Direction, Layout as RtLayout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use ratatui::Frame;
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, ChooserRow, Focus};
use crate::rows::{Row, RowKind};
use crate::session::{ClaudePerm, SessionKind};
use crate::tmux::CommandRunner;

pub struct ListLayout {
    pub origin_y: u16,
    /// Left column of the tree content (inside the border). The `[+]` button sits
    /// right after each directory's name, so its column span is per-row, derived
    /// from the label width relative to this origin.
    pub content_x: u16,
    pub row_count: usize,
    /// First row index shown (scroll offset) and the visible row count. Needed
    /// to map a screen row back to its absolute row index when the tree scrolls.
    pub offset: usize,
    pub view_h: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Row(usize),
    Button(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneHit {
    Tree(Option<Hit>),
    Right,
}

pub struct Layout {
    pub tree: ListLayout,
    pub split_col: u16,
    pub term_area: Rect,
}

pub fn resolve_click(col: u16, row: u16, layout: &ListLayout, rows: &[Row]) -> Option<Hit> {
    if row < layout.origin_y {
        return None;
    }
    let visible = (row - layout.origin_y) as usize;
    if visible >= layout.view_h as usize {
        return None;
    }
    let idx = layout.offset + visible;
    if idx >= layout.row_count {
        return None;
    }
    // The `[+]` button is drawn right after a directory's name (see `row_line`),
    // so its hit region is per-row: `content_x + <left width> + 1 ..= + 3`.
    if let Some(r) = rows.get(idx) {
        if matches!(r.kind, RowKind::Dir { .. }) {
            let left = dir_left(r).chars().count() as u16;
            let bstart = layout.content_x + left + 1;
            if col >= bstart && col <= bstart + 2 {
                return Some(Hit::Button(idx));
            }
        }
    }
    Some(Hit::Row(idx))
}

pub fn resolve_pane_click(col: u16, row: u16, split_col: u16, tree_layout: &ListLayout, rows: &[Row]) -> PaneHit {
    if col >= split_col {
        PaneHit::Right
    } else {
        PaneHit::Tree(resolve_click(col, row, tree_layout, rows))
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// The text of a directory row up to (but not including) the `[+]` button:
/// indent + expand icon + name. Shared by the renderer and the click hit-test
/// so the button's column always lines up with what's drawn.
fn dir_left(row: &Row) -> String {
    let indent = "  ".repeat(row.depth);
    let icon = match &row.kind {
        RowKind::Dir { expanded: true } => "▾ ",
        RowKind::Dir { expanded: false } => "▸ ",
        _ => "",
    };
    format!("{indent}{icon}{}", row.label)
}

fn row_line(row: &Row) -> String {
    let indent = "  ".repeat(row.depth);
    match &row.kind {
        // The button sits right after the directory name rather than far right.
        RowKind::Dir { .. } => format!("{} [+]", dir_left(row)),
        // A distinct prefix mark per kind so sessions don't read like files.
        RowKind::Session { kind, .. } => {
            let mark = match kind {
                SessionKind::Shell => "$ ",
                SessionKind::Claude => "✦ ",
            };
            format!("{indent}{mark}{}", row.label)
        }
        RowKind::File => format!("{indent}  {}", row.label),
    }
}

/// The tool's display name shown in the tree-pane banner. Placeholder until the
/// project is named; `big_name` spells it out in 3-row block glyphs.
const APP_NAME: &str = "PJ MA";
/// Rows reserved at the top of the tree pane for the banner: 3 rows of block
/// glyphs plus one hint line.
const BANNER_HEIGHT: u16 = 4;

/// 3-row block-letter glyphs (half-block style) for the characters used by
/// `APP_NAME`. Each entry is the three rows (top→bottom) of one character;
/// unknown chars render blank. Widths differ per letter and are joined with a
/// one-column gap by `big_name`.
fn glyph(c: char) -> [&'static str; 3] {
    match c.to_ascii_uppercase() {
        'P' => ["█▀▀█", "█▀▀▀", "█   "],
        'J' => ["▀▀█▀", "  █ ", "█▄█ "],
        'M' => ["██▄▄██", "█ ▀▀ █", "█    █"],
        'A' => ["▄▀▀▄", "█▄▄█", "█  █"],
        ' ' => ["   ", "   ", "   "],
        _ => ["   ", "   ", "   "],
    }
}

/// Composes `name` into three rows of block glyphs joined by single-space gaps.
fn big_name(name: &str) -> [String; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, c) in name.chars().enumerate() {
        if i > 0 {
            for r in rows.iter_mut() {
                r.push(' ');
            }
        }
        for (r, part) in rows.iter_mut().zip(glyph(c).iter()) {
            r.push_str(part);
        }
    }
    rows
}

/// Renders the tree-pane banner: the tool name in large block letters when the
/// pane is wide enough, otherwise a plain bold name, plus a "press h for help"
/// hint. Centered horizontally; falls back gracefully on narrow/short panes.
fn render_banner(f: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let name_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let hint_style = Style::default().fg(Color::DarkGray);

    let big = big_name(APP_NAME);
    let big_w = big[0].chars().count() as u16;
    let mut lines: Vec<Line> = Vec::new();
    if big_w <= area.width && area.height >= BANNER_HEIGHT {
        for row in big {
            lines.push(Line::styled(row, name_style));
        }
    } else {
        lines.push(Line::styled(APP_NAME, name_style));
    }
    lines.push(Line::styled("press h for help", hint_style));

    let para = Paragraph::new(lines).alignment(Alignment::Center);
    f.render_widget(para, area);
}

pub fn render<R: CommandRunner>(
    f: &mut Frame,
    area: Rect,
    app: &mut App<R>,
    screen: Option<&vt100::Screen>,
) -> Layout {
    let chunks = RtLayout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(app.split_pct),
            Constraint::Percentage(100 - app.split_pct),
        ])
        .split(area);
    let left_area = chunks[0];
    let right_area = chunks[1];

    // ---- left: banner on top, tree below ----
    // Reserve the top rows of the left column for the banner; the tree block
    // (and all its derived geometry) sits in whatever remains, so click and
    // scroll hit-testing follow `inner.y` automatically.
    let left_chunks = RtLayout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(BANNER_HEIGHT), Constraint::Min(0)])
        .split(left_area);
    render_banner(f, left_chunks[0]);
    let tree_area = left_chunks[1];

    let tree_block = Block::default()
        .title("tree")
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Tree));
    let inner = tree_block.inner(tree_area);
    f.render_widget(tree_block, tree_area);

    let view_h = inner.height as usize;
    let total = app.rows.len();
    let items: Vec<ListItem> = app.rows.iter().map(|r| ListItem::new(row_line(r))).collect();
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(app.selected.min(total - 1)));
    }
    // Start from the tracked scroll offset (clamped); the List may nudge it to
    // keep the selection visible, so read the final value back afterwards.
    let max_off = total.saturating_sub(view_h);
    *state.offset_mut() = app.tree_offset.min(max_off);
    f.render_stateful_widget(list, inner, &mut state);
    app.tree_offset = state.offset();

    // Scrollbar on the right border, only when the content overflows.
    if total > view_h {
        // ratatui places the thumb at the bottom of the track when
        // `position == content_length - 1`. Our scroll range is `[0, max_off]`
        // with `max_off = total - view_h`, so content_length must be the number
        // of distinct scroll positions (`max_off + 1`) for the thumb to reach
        // the end. `viewport_content_length` still sizes the thumb as view_h/total.
        let mut sb_state = ScrollbarState::new(max_off + 1)
            .viewport_content_length(view_h)
            .position(app.tree_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        f.render_stateful_widget(
            scrollbar,
            tree_area.inner(Margin { vertical: 1, horizontal: 0 }),
            &mut sb_state,
        );
    }

    let tree_layout = ListLayout {
        origin_y: inner.y,
        content_x: inner.x,
        row_count: total,
        offset: app.tree_offset,
        view_h: inner.height,
    };

    // ---- right: terminal or viewer ----
    // The embedded PTY's vt100 parser is owned by run.rs, so the screen is
    // passed in: Some when the terminal is shown, None when the viewer is.
    let right_focused = app.focus == Focus::Right;
    let right_inner = Block::default().borders(Borders::ALL).inner(right_area);
    if let Some(view) = &app.viewer {
        let title = view
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style(right_focused));
        let body = view.lines.join("\n");
        let para = Paragraph::new(body).block(block).scroll((view.scroll as u16, 0));
        f.render_widget(para, right_area);
    } else {
        let block = Block::default()
            .title("terminal")
            .borders(Borders::ALL)
            .border_style(border_style(right_focused));
        f.render_widget(block, right_area);
        match screen {
            // The embedded client is live: render its vt100 screen.
            Some(screen) => f.render_widget(PseudoTerminal::new(screen), right_inner),
            // No embedded session yet (fresh start, nothing to recover). Show a
            // hint instead of a live terminal until the user starts one.
            None => {
                let hint = Paragraph::new("no active session\n\nselect a directory and press 'a' to start one")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(hint, right_inner);
            }
        }
    }

    Layout {
        tree: tree_layout,
        split_col: right_area.x,
        term_area: right_inner,
    }
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let v = RtLayout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let h = RtLayout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1]);
    h[1]
}

pub fn render_help(f: &mut Frame, area: Rect) {
    let lines = [
        "j / ↓      move down",
        "k / ↑      move up",
        "Enter      expand dir / switch session / view file",
        "a / [+]    new session (shell or claude) on a dir",
        "wheel      scroll the tree (scrollbar shows when needed)",
        "h / ?      this help",
        "Ctrl-q     toggle focus (tree / right pane)",
        "q          quit",
        "",
        "right pane focused: type into the shell, or",
        "j/k/PgUp/PgDn to scroll a file view",
        "",
        "— press any key to close —",
    ];
    let popup = centered_rect(64, 70, area);
    let block = Block::default().title("Keys").borders(Borders::ALL);
    let para = Paragraph::new(lines.join("\n")).block(block);
    f.render_widget(Clear, popup);
    f.render_widget(para, popup);
}

pub fn render_chooser(
    f: &mut Frame,
    area: Rect,
    kind: SessionKind,
    perm: ClaudePerm,
    focus_row: ChooserRow,
) -> Vec<(u16, ChooserRow)> {
    let popup = centered_rect(50, 60, area);
    f.render_widget(Clear, popup);
    let block = Block::default().title("New session").borders(Borders::ALL);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    let mut row_ys: Vec<(u16, ChooserRow)> = Vec::new();
    let mut y = inner.y;

    let radio = |selected: bool| if selected { "(•)" } else { "( )" };
    let arrow = |row: ChooserRow| if row == focus_row { "> " } else { "  " };

    // Kind:
    lines.push(Line::from("Kind:".to_string()));
    y += 1;
    lines.push(Line::from(format!("{}{} shell", arrow(ChooserRow::KindShell), radio(kind == SessionKind::Shell))));
    row_ys.push((y, ChooserRow::KindShell));
    y += 1;
    lines.push(Line::from(format!("{}{} claude", arrow(ChooserRow::KindClaude), radio(kind == SessionKind::Claude))));
    row_ys.push((y, ChooserRow::KindClaude));
    y += 1;

    if kind == SessionKind::Claude {
        lines.push(Line::from("Permission:".to_string()));
        y += 1;
        lines.push(Line::from(format!("{}{} normal", arrow(ChooserRow::PermNormal), radio(perm == ClaudePerm::Normal))));
        row_ys.push((y, ChooserRow::PermNormal));
        y += 1;
        lines.push(Line::from(format!("{}{} skip (--dangerously-skip-permissions)", arrow(ChooserRow::PermSkip), radio(perm == ClaudePerm::Skip))));
        row_ys.push((y, ChooserRow::PermSkip));
        y += 1;
    }

    lines.push(Line::from(String::new()));
    y += 1;
    lines.push(Line::from(format!("{}[ Cancel ]", arrow(ChooserRow::Cancel))));
    row_ys.push((y, ChooserRow::Cancel));
    y += 1;
    lines.push(Line::from(format!("{}[ Create ]", arrow(ChooserRow::Create))));
    row_ys.push((y, ChooserRow::Create));

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
    row_ys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_row(label: &str, depth: usize) -> Row {
        Row { path: std::path::PathBuf::from("/"), label: label.to_string(), depth, kind: RowKind::Dir { expanded: true } }
    }
    fn file_row(label: &str) -> Row {
        Row { path: std::path::PathBuf::from("/"), label: label.to_string(), depth: 0, kind: RowKind::File }
    }

    #[test]
    fn row_line_places_button_after_name_and_marks_sessions() {
        let dir = dir_row("src", 0);
        assert_eq!(row_line(&dir), "▾ src [+]");
        let shell = Row { path: std::path::PathBuf::from("/"), label: "shell".into(), depth: 1, kind: RowKind::Session { slug: "s".into(), kind: SessionKind::Shell } };
        assert_eq!(row_line(&shell), "  $ shell");
        let claude = Row { path: std::path::PathBuf::from("/"), label: "claude".into(), depth: 1, kind: RowKind::Session { slug: "c".into(), kind: SessionKind::Claude } };
        assert_eq!(row_line(&claude), "  ✦ claude");
        assert_eq!(row_line(&file_row("a.rs")), "  a.rs");
    }

    #[test]
    fn resolve_click_distinguishes_row_and_button() {
        // content_x=0; row 1 is depth-0 dir "src": left = "▾ src" (5 chars), so
        // "[+]" occupies columns 6,7,8 (left+1 ..= left+3).
        let rows = vec![file_row("a"), dir_row("src", 0), file_row("c")];
        let layout = ListLayout { origin_y: 1, content_x: 0, row_count: 3, offset: 0, view_h: 10 };
        assert_eq!(resolve_click(2, 1, &layout, &rows), Some(Hit::Row(0)));
        assert_eq!(resolve_click(7, 2, &layout, &rows), Some(Hit::Button(1)));
        // clicking the dir name (not the button) toggles, not opens the chooser
        assert_eq!(resolve_click(2, 2, &layout, &rows), Some(Hit::Row(1)));
        // a file row has no button anywhere
        assert_eq!(resolve_click(7, 3, &layout, &rows), Some(Hit::Row(2)));
        assert_eq!(resolve_click(5, 0, &layout, &rows), None);
        assert_eq!(resolve_click(5, 11, &layout, &rows), None);
    }

    #[test]
    fn resolve_click_accounts_for_scroll_offset() {
        // Scrolled down by 5 rows: the top visible screen row maps to row index 5.
        let mut rows: Vec<Row> = (0..30).map(|i| file_row(&format!("f{i}"))).collect();
        rows[5] = dir_row("src", 0); // "▾ src" -> button at cols 6,7,8
        let layout = ListLayout { origin_y: 1, content_x: 0, row_count: 30, offset: 5, view_h: 8 };
        assert_eq!(resolve_click(2, 1, &layout, &rows), Some(Hit::Row(5)));
        assert_eq!(resolve_click(7, 1, &layout, &rows), Some(Hit::Button(5)));
        // a click past the visible window height is ignored
        assert_eq!(resolve_click(5, 1 + 8, &layout, &rows), None);
    }

    #[test]
    fn resolve_pane_click_splits_on_column() {
        let rows = vec![file_row("a"), file_row("b"), file_row("c")];
        let layout = ListLayout { origin_y: 1, content_x: 0, row_count: 3, offset: 0, view_h: 10 };
        assert_eq!(resolve_pane_click(5, 2, 50, &layout, &rows), PaneHit::Tree(Some(Hit::Row(1))));
        assert_eq!(resolve_pane_click(50, 2, 50, &layout, &rows), PaneHit::Right);
    }

    #[test]
    fn centered_rect_is_centered() {
        let area = Rect { x: 0, y: 0, width: 100, height: 100 };
        assert_eq!(centered_rect(50, 50, area), Rect { x: 25, y: 25, width: 50, height: 50 });
    }

    #[test]
    fn scrollbar_appears_only_when_tree_overflows() {
        use crate::app::App;
        use crate::rows::{Row, RowKind};
        use crate::tmux::{MockRunner, Tmux};
        use crate::viewer::FileView;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::path::{Path, PathBuf};

        let make_rows = |n: usize| -> Vec<Row> {
            (0..n)
                .map(|i| Row {
                    path: PathBuf::from("/"),
                    label: format!("f{i}"),
                    depth: 0,
                    kind: RowKind::File,
                })
                .collect()
        };
        // viewer Some lets render skip the embedded terminal screen.
        let has_thumb = |rows: Vec<Row>| -> bool {
            let mut app = App::new(PathBuf::from("/"), Tmux::new("runner", MockRunner::new()));
            app.rows = rows;
            app.viewer = Some(FileView::load(Path::new("/nonexistent")));
            let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
            terminal
                .draw(|f| {
                    render(f, f.area(), &mut app, None);
                })
                .unwrap();
            let content: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            content.contains('█')
        };
        // 40 rows in an 8-row viewport -> scrollbar thumb visible.
        assert!(has_thumb(make_rows(40)));
        // 3 rows fit -> no scrollbar.
        assert!(!has_thumb(make_rows(3)));
    }

    #[test]
    fn scrollbar_thumb_reaches_ends() {
        use crate::app::App;
        use crate::rows::{Row, RowKind};
        use crate::tmux::{MockRunner, Tmux};
        use crate::viewer::FileView;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::path::{Path, PathBuf};

        let rows: Vec<Row> = (0..40)
            .map(|i| Row {
                path: PathBuf::from("/"),
                label: format!("f{i}"),
                depth: 0,
                kind: RowKind::File,
            })
            .collect();

        // Render at a given scroll offset and return the inclusive (min_y, max_y)
        // of the scrollbar thumb cells.
        let thumb_span = |offset: usize, selected: usize| -> (u16, u16) {
            let mut app = App::new(PathBuf::from("/"), Tmux::new("runner", MockRunner::new()));
            app.rows = rows.clone();
            app.viewer = Some(FileView::load(Path::new("/nonexistent")));
            app.tree_offset = offset;
            app.selected = selected;
            let mut terminal = Terminal::new(TestBackend::new(40, 10 + BANNER_HEIGHT)).unwrap();
            terminal
                .draw(|f| {
                    render(f, f.area(), &mut app, None);
                })
                .unwrap();
            let buf = terminal.backend().buffer().clone();
            let mut min_y = u16::MAX;
            let mut max_y = 0u16;
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    if buf[(x, y)].symbol() == "█" {
                        min_y = min_y.min(y);
                        max_y = max_y.max(y);
                    }
                }
            }
            (min_y, max_y)
        };

        // The tree pane sits below the banner (BANNER_HEIGHT rows). Its track
        // lives inside a 1-cell vertical margin, so it spans rows
        // BANNER_HEIGHT+1 ..= BANNER_HEIGHT+8.
        let top = BANNER_HEIGHT + 1;
        let bottom = BANNER_HEIGHT + 8;
        let (top_min, _) = thumb_span(0, 0);
        assert_eq!(top_min, top, "at the top the thumb should touch the first track cell");

        // Scrolled to the bottom (max_off = 40 - 8 = 32), the thumb must reach
        // the last track cell.
        let (_, bot_max) = thumb_span(32, 39);
        assert_eq!(bot_max, bottom, "at the bottom the thumb should touch the last track cell");
    }

    #[test]
    fn keyboard_nav_past_first_page_does_not_pin_cursor_to_bottom() {
        // Regression for the reported bug: once the selection moved past the
        // first page, the cursor stayed glued to the bottom row. That happened
        // because the offset was recomputed from zero every frame; with a
        // tracked tree_offset, scrolling down then stepping back up moves the
        // cursor within the viewport instead of re-pinning it to the bottom.
        use crate::app::App;
        use crate::rows::{Row, RowKind};
        use crate::tmux::{MockRunner, Tmux};
        use crate::viewer::FileView;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::path::{Path, PathBuf};

        let mut app = App::new(PathBuf::from("/"), Tmux::new("runner", MockRunner::new()));
        app.rows = (0..30)
            .map(|i| Row {
                path: PathBuf::from("/"),
                label: format!("f{i}"),
                depth: 0,
                kind: RowKind::File,
            })
            .collect();
        app.viewer = Some(FileView::load(Path::new("/nonexistent")));

        // 40 wide; height is BANNER_HEIGHT taller than the tree so that after the
        // banner + border the tree inner height is still 8 rows.
        let mut terminal = Terminal::new(TestBackend::new(40, 10 + BANNER_HEIGHT)).unwrap();
        let view_h = 8usize;
        let draw = |app: &mut App<MockRunner>, t: &mut Terminal<TestBackend>| {
            t.draw(|f| {
                render(f, f.area(), app, None);
            })
            .unwrap();
        };

        // Jump well past the first page and render: the cursor lands on the
        // bottom visible row (offset nudged so the selection stays in view).
        app.selected = 25;
        draw(&mut app, &mut terminal);
        assert_eq!(app.tree_offset, 25 + 1 - view_h);
        assert_eq!(app.selected - app.tree_offset, view_h - 1);

        // Step the cursor up; the tracked offset must stay put so the cursor
        // moves up *within* the viewport rather than snapping back to the bottom.
        for _ in 0..4 {
            app.up();
        }
        draw(&mut app, &mut terminal);
        assert_eq!(app.tree_offset, 25 + 1 - view_h, "offset should not jump");
        assert_eq!(app.selected, 21);
        assert!(
            app.selected - app.tree_offset < view_h - 1,
            "cursor should be in the viewport interior, not pinned to the bottom"
        );
    }

    #[test]
    fn big_name_has_three_aligned_rows() {
        let rows = big_name("PJ MA");
        // Three rows of identical display width so the banner stays rectangular.
        let w0 = rows[0].chars().count();
        assert_eq!(w0, rows[1].chars().count());
        assert_eq!(w0, rows[2].chars().count());
        assert!(w0 > 0);
        // Unknown characters render as blanks rather than panicking.
        let blank = big_name("?");
        assert!(blank.iter().all(|r| r.trim().is_empty()));
    }

    #[test]
    fn render_banner_shows_name_and_help_hint() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        // Wide enough for the block-letter name plus the hint line.
        let mut terminal = Terminal::new(TestBackend::new(30, BANNER_HEIGHT)).unwrap();
        terminal
            .draw(|f| {
                render_banner(f, f.area());
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("press h for help"));
        // The large name is drawn with block glyphs.
        assert!(content.contains('█'));
    }

    #[test]
    fn render_banner_falls_back_to_plain_name_when_narrow() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        // Too narrow for the block letters -> the plain name is shown instead,
        // but still wide enough for the hint line.
        let mut terminal = Terminal::new(TestBackend::new(18, BANNER_HEIGHT)).unwrap();
        terminal
            .draw(|f| {
                render_banner(f, f.area());
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("PJ MA"));
        assert!(content.contains("press h for help"));
        assert!(!content.contains('█'));
    }

    #[test]
    fn render_chooser_draws_radios_and_buttons() {
        use crate::app::ChooserRow;
        use crate::session::{ClaudePerm, SessionKind};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let rows = render_chooser(
                    f,
                    f.area(),
                    SessionKind::Claude,
                    ClaudePerm::Skip,
                    ChooserRow::Create,
                );
                assert!(rows.iter().any(|(_, r)| *r == ChooserRow::Create));
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("shell"));
        assert!(content.contains("claude"));
        assert!(content.contains("skip"));
        assert!(content.contains("Cancel"));
        assert!(content.contains("Create"));
    }
}
