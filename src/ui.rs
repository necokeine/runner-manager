use ratatui::layout::{Constraint, Direction, Layout as RtLayout, Margin, Rect};
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
    pub button_col_start: u16,
    pub button_col_end: u16,
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

pub fn resolve_click(col: u16, row: u16, layout: &ListLayout) -> Option<Hit> {
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
    if col >= layout.button_col_start && col <= layout.button_col_end {
        Some(Hit::Button(idx))
    } else {
        Some(Hit::Row(idx))
    }
}

pub fn resolve_pane_click(col: u16, row: u16, split_col: u16, tree_layout: &ListLayout) -> PaneHit {
    if col >= split_col {
        PaneHit::Right
    } else {
        PaneHit::Tree(resolve_click(col, row, tree_layout))
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn row_line(row: &Row, width: usize) -> String {
    let indent = "  ".repeat(row.depth);
    match &row.kind {
        RowKind::Dir { expanded } => {
            let icon = if *expanded { "▾ " } else { "▸ " };
            let left = format!("{indent}{icon}{}", row.label);
            let btn = "[+]";
            let pad = width.saturating_sub(left.chars().count() + btn.len());
            format!("{left}{}{btn}", " ".repeat(pad))
        }
        RowKind::Session { .. } => format!("{indent}• {}", row.label),
        RowKind::File => format!("{indent}  {}", row.label),
    }
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
    let tree_area = chunks[0];
    let right_area = chunks[1];

    // ---- left: tree ----
    let tree_block = Block::default()
        .title("tree")
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Tree));
    let inner = tree_block.inner(tree_area);
    f.render_widget(tree_block, tree_area);

    let width = inner.width as usize;
    let view_h = inner.height as usize;
    let total = app.rows.len();
    let items: Vec<ListItem> = app.rows.iter().map(|r| ListItem::new(row_line(r, width))).collect();
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
        button_col_start: inner.x + inner.width.saturating_sub(3),
        button_col_end: inner.x + inner.width.saturating_sub(1),
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
        let screen = screen.expect("terminal screen present when viewer is None");
        f.render_widget(PseudoTerminal::new(screen), right_inner);
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

    #[test]
    fn resolve_click_distinguishes_row_and_button() {
        let layout = ListLayout { origin_y: 1, button_col_start: 20, button_col_end: 22, row_count: 3, offset: 0, view_h: 10 };
        assert_eq!(resolve_click(5, 1, &layout), Some(Hit::Row(0)));
        assert_eq!(resolve_click(21, 2, &layout), Some(Hit::Button(1)));
        assert_eq!(resolve_click(5, 0, &layout), None);
        assert_eq!(resolve_click(5, 10, &layout), None);
    }

    #[test]
    fn resolve_click_accounts_for_scroll_offset() {
        // Scrolled down by 5 rows: the top visible screen row maps to row index 5.
        let layout = ListLayout { origin_y: 1, button_col_start: 20, button_col_end: 22, row_count: 30, offset: 5, view_h: 8 };
        assert_eq!(resolve_click(5, 1, &layout), Some(Hit::Row(5)));
        assert_eq!(resolve_click(21, 1, &layout), Some(Hit::Button(5)));
        // a click past the visible window height is ignored
        assert_eq!(resolve_click(5, 1 + 8, &layout), None);
    }

    #[test]
    fn resolve_pane_click_splits_on_column() {
        let layout = ListLayout { origin_y: 1, button_col_start: 38, button_col_end: 40, row_count: 3, offset: 0, view_h: 10 };
        assert_eq!(resolve_pane_click(5, 2, 50, &layout), PaneHit::Tree(Some(Hit::Row(1))));
        assert_eq!(resolve_pane_click(50, 2, 50, &layout), PaneHit::Right);
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
            let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
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

        // The track lives inside a 1-cell vertical margin of the 10-row pane,
        // so it spans rows 1..=8.
        let (top_min, _) = thumb_span(0, 0);
        assert_eq!(top_min, 1, "at the top the thumb should touch the first track cell");

        // Scrolled to the bottom (max_off = 40 - 8 = 32), the thumb must reach
        // the last track cell (row 8).
        let (_, bot_max) = thumb_span(32, 39);
        assert_eq!(bot_max, 8, "at the bottom the thumb should touch the last track cell");
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

        // 40x10 -> tree inner height is 8 rows after the border.
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
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
