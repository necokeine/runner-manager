use ratatui::layout::{Alignment, Constraint, Direction, Layout as RtLayout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use ratatui::Frame;
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, ChooserRow, Focus, TreeTab};
use crate::claude::ResumeSession;
use crate::git::{GitStatus, GitStatuses};
use crate::rows::{Row, RowKind};
use crate::select::Selection;
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
    Close(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneHit {
    Tree(Option<Hit>),
    Right,
}

/// Clickable geometry of the tab bar: the row it sits on and, per tab, the
/// inclusive column span of its label. `run.rs` resolves a mouse click against
/// these to switch views.
pub struct TabBar {
    pub y: u16,
    pub hits: Vec<(u16, u16, TreeTab)>,
}

pub struct Layout {
    pub tree: ListLayout,
    pub split_col: u16,
    pub term_area: Rect,
    pub tabs: TabBar,
}

/// Which tab (if any) a click at `(col, row)` lands on.
pub fn resolve_tab_click(col: u16, row: u16, tabs: &TabBar) -> Option<TreeTab> {
    if row != tabs.y {
        return None;
    }
    tabs.hits
        .iter()
        .find(|(start, end, _)| col >= *start && col <= *end)
        .map(|(_, _, tab)| *tab)
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
    // The `[+]` (dir) and `[×]` (session) buttons are drawn right after the
    // row's name (see `row_line`), so each hit region is per-row:
    // `content_x + <left width> + 1 ..= + 3`.
    if let Some(r) = rows.get(idx) {
        match &r.kind {
            RowKind::Dir { .. } => {
                let left = dir_left(r).chars().count() as u16;
                let bstart = layout.content_x + left + 1;
                if col >= bstart && col <= bstart + 2 {
                    return Some(Hit::Button(idx));
                }
            }
            RowKind::Session { .. } => {
                let left = session_left(r).chars().count() as u16;
                let bstart = layout.content_x + left + 1;
                if col >= bstart && col <= bstart + 2 {
                    return Some(Hit::Close(idx));
                }
            }
            RowKind::File => {}
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

/// A session row's text up to (but not including) the `[×]` close button:
/// indent + kind mark + name. A distinct prefix mark per kind keeps sessions
/// from reading like files. Shared by the renderer and the click hit-test so
/// the close button's column always lines up with what's drawn.
fn session_left(row: &Row) -> String {
    let indent = "  ".repeat(row.depth);
    let mark = match &row.kind {
        RowKind::Session { kind: SessionKind::Shell, .. } => "$ ",
        RowKind::Session { kind: SessionKind::Claude, .. } => "✦ ",
        RowKind::Session { kind: SessionKind::Codex, .. } => "⌬ ",
        _ => "",
    };
    format!("{indent}{mark}{}", row.label)
}

/// The git-status colour for a row, or `None` when it should render plain.
/// Sessions are never coloured (they aren't tracked paths); directories and
/// files take the colour of their `git status` entry, following git's own
/// policy — staged ("Changes to be committed") green, dirty/untracked red,
/// ignored grey.
fn row_style(row: &Row, git: &GitStatuses) -> Option<Style> {
    if matches!(row.kind, RowKind::Session { .. }) {
        return None;
    }
    let color = match git.get(&row.path)? {
        GitStatus::Staged => Color::Green,
        GitStatus::Modified | GitStatus::Untracked => Color::Red,
        GitStatus::Ignored => Color::DarkGray,
    };
    Some(Style::default().fg(color))
}

fn row_line(row: &Row) -> String {
    let indent = "  ".repeat(row.depth);
    match &row.kind {
        // The button sits right after the directory name rather than far right.
        RowKind::Dir { .. } => format!("{} [+]", dir_left(row)),
        // A close button sits right after the session name, mirroring `[+]`.
        RowKind::Session { .. } => format!("{} [×]", session_left(row)),
        RowKind::File => format!("{indent}  {}", row.label),
    }
}

/// The tool's display name shown in the tree-pane banner. Placeholder until the
/// project is named; `big_name` spells it out in 3-row block glyphs.
const APP_NAME: &str = "PJ MA";
/// Rows reserved at the top of the tree pane for the banner: 3 rows of block
/// glyphs plus one hint line.
const BANNER_HEIGHT: u16 = 4;
/// The tab bar (directory / project) is a single row between banner and tree.
const TAB_HEIGHT: u16 = 1;

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

/// Render the directory/project tab bar and report each tab's clickable span.
/// The active tab is drawn reversed; inactive tabs are dimmed.
fn render_tabs(f: &mut Frame, area: Rect, active: TreeTab) -> TabBar {
    let mut spans: Vec<Span> = Vec::new();
    let mut hits: Vec<(u16, u16, TreeTab)> = Vec::new();
    let mut x = area.x;
    for (i, tab) in [TreeTab::Directory, TreeTab::Project].into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            x += 1;
        }
        let text = format!(" {} ", tab.label());
        let w = text.chars().count() as u16;
        let style = if tab == active {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        hits.push((x, x + w.saturating_sub(1), tab));
        spans.push(Span::styled(text, style));
        x += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    TabBar { y: area.y, hits }
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

    // ---- left: banner, tab bar, then the active view ----
    // Reserve the top rows for the banner and the one-row tab bar; the tree
    // block (and all its derived geometry) sits in whatever remains, so click
    // and scroll hit-testing follow `inner.y` automatically.
    let left_chunks = RtLayout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(BANNER_HEIGHT),
            Constraint::Length(TAB_HEIGHT),
            Constraint::Min(0),
        ])
        .split(left_area);
    render_banner(f, left_chunks[0]);
    let tabs = render_tabs(f, left_chunks[1], app.tab);
    let tree_area = left_chunks[2];

    let tree_block = Block::default()
        .title(app.tab.label())
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Tree));
    let inner = tree_block.inner(tree_area);
    f.render_widget(tree_block, tree_area);

    let view_h = inner.height as usize;
    let total = app.rows.len();
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|r| match row_style(r, &app.git) {
            Some(style) => ListItem::new(Line::styled(row_line(r), style)),
            None => ListItem::new(row_line(r)),
        })
        .collect();
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

    // Project view with nothing open: a hint reads better than a blank pane.
    if app.tab == TreeTab::Project && total == 0 {
        let hint = Paragraph::new("no open sessions")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, inner);
    }

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
            .title(app.terminal_title())
            .borders(Borders::ALL)
            .border_style(border_style(right_focused));
        f.render_widget(block, right_area);
        match screen {
            // The embedded client is live: render its vt100 screen, then paint
            // any in-progress mouse selection over it.
            Some(screen) => {
                f.render_widget(PseudoTerminal::new(screen), right_inner);
                if let Some(sel) = app.selection {
                    paint_selection(f, right_inner, sel);
                }
            }
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
        tabs,
    }
}

/// Paint a selection over the already-rendered terminal pane by flipping the
/// `REVERSED` modifier on each covered cell. `Style::default()` leaves the
/// cell's own colours alone and only toggles the reverse attribute, so the
/// highlight reads as inverted text whatever the underlying styling is. `term`
/// is the terminal pane rect; the selection is in pane-relative cells.
fn paint_selection(f: &mut Frame, term: Rect, sel: Selection) {
    if sel.is_empty() {
        return;
    }
    let buf = f.buffer_mut();
    let highlight = Style::default().add_modifier(Modifier::REVERSED);
    for r in 0..term.height {
        for c in 0..term.width {
            if !sel.contains(c, r) {
                continue;
            }
            let x = term.x + c;
            let y = term.y + r;
            if x < buf.area.right() && y < buf.area.bottom() {
                buf[(x, y)].set_style(highlight);
            }
        }
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
        "Tab        switch directory / project view",
        "Enter      expand dir / switch session / view file",
        "a / [+]    new session (shell or claude) on a dir",
        "x / [×]    close the selected session (asks to confirm)",
        "wheel      scroll the tree (scrollbar shows when needed)",
        "h / ?      this help",
        "Ctrl-q     toggle focus (tree / right pane)",
        "q          quit",
        "",
        "right pane focused: type into the shell, or",
        "j/k/PgUp/PgDn to scroll a file view",
        "wheel over the terminal scrolls its history (old logs)",
        "drag in the terminal to select text → copies to clipboard",
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
    resumes: &[ResumeSession],
    resume_sel: Option<usize>,
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
    lines.push(Line::from(format!("{}{} codex", arrow(ChooserRow::KindCodex), radio(kind == SessionKind::Codex))));
    row_ys.push((y, ChooserRow::KindCodex));
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

        // Resume picker: only shown when this directory has Claude history.
        if !resumes.is_empty() {
            lines.push(Line::from("Resume:".to_string()));
            y += 1;
            lines.push(Line::from(format!(
                "{}{} new session",
                arrow(ChooserRow::ResumeNew),
                radio(resume_sel.is_none())
            )));
            row_ys.push((y, ChooserRow::ResumeNew));
            y += 1;
            // Keep the label inside the popup; reserve room for the "> (•) " prefix.
            let label_width = (inner.width as usize).saturating_sub(7);
            for (i, s) in resumes.iter().enumerate() {
                let row = ChooserRow::Resume(i);
                lines.push(Line::from(format!(
                    "{}{} {}",
                    arrow(row),
                    radio(resume_sel == Some(i)),
                    resume_label(s, label_width)
                )));
                row_ys.push((y, row));
                y += 1;
            }
        }
    }

    lines.push(Line::from(String::new()));
    y += 1;
    lines.push(Line::from(format!("{}[ Cancel ]", arrow(ChooserRow::Cancel))));
    row_ys.push((y, ChooserRow::Cancel));
    y += 1;
    lines.push(Line::from(format!("{}[ Create ]", arrow(ChooserRow::Create))));
    row_ys.push((y, ChooserRow::Create));

    lines.push(Line::from(String::new()));
    lines.push(Line::from(Span::styled(
        "↑/↓/Tab move · Enter create · Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
    row_ys
}

/// Draw the "really close this session?" confirmation. Returns the screen `y` of
/// each clickable button paired with its choice (`true` = Yes), so `run.rs` can
/// resolve a mouse click the same way it does the chooser's rows.
pub fn render_confirm_close(f: &mut Frame, area: Rect, slug: &str) -> Vec<(u16, bool)> {
    let popup = centered_rect(50, 30, area);
    f.render_widget(Clear, popup);
    let block = Block::default().title("Close session").borders(Borders::ALL);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    let mut row_ys: Vec<(u16, bool)> = Vec::new();
    let mut y = inner.y;

    lines.push(Line::from(format!("Close session \"{slug}\"?")));
    y += 1;
    lines.push(Line::from("This kills its tmux session.".to_string()));
    y += 1;
    lines.push(Line::from(String::new()));
    y += 1;
    lines.push(Line::from("[ Yes ]   y / Enter".to_string()));
    row_ys.push((y, true));
    y += 1;
    lines.push(Line::from("[ No ]    n / Esc".to_string()));
    row_ys.push((y, false));

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
    row_ys
}

/// One-line label for a resumable session: its last prompt, or a short id when
/// the transcript has no plain prompt yet, truncated to `max` columns.
fn resume_label(s: &ResumeSession, max: usize) -> String {
    let text = if s.last_command.is_empty() {
        let short: String = s.id.chars().take(8).collect();
        format!("({short}…)")
    } else {
        s.last_command.clone()
    };
    truncate(&text, max)
}

/// Truncate to at most `max` columns, appending '…' when shortened.
fn truncate(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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
    fn row_style_colours_files_and_dirs_by_git_status() {
        use crate::git::{GitStatus, GitStatuses};
        use std::path::PathBuf;

        let staged = PathBuf::from("/repo/staged.rs");
        let dirty = PathBuf::from("/repo/dirty");
        let ignored = PathBuf::from("/repo/ignored.log");
        let git = GitStatuses::from_entries([
            (staged.clone(), GitStatus::Staged),
            (dirty.clone(), GitStatus::Untracked),
            (ignored.clone(), GitStatus::Ignored),
        ]);

        let path_row = |path: PathBuf, kind: RowKind| Row { path, label: "x".into(), depth: 0, kind };

        // Staged path renders green; untracked (dirty) renders red; ignored grey.
        let r = path_row(staged.clone(), RowKind::File);
        assert_eq!(row_style(&r, &git), Some(Style::default().fg(Color::Green)));
        let r = path_row(dirty.clone(), RowKind::Dir { expanded: false });
        assert_eq!(row_style(&r, &git), Some(Style::default().fg(Color::Red)));
        let r = path_row(ignored.clone(), RowKind::File);
        assert_eq!(row_style(&r, &git), Some(Style::default().fg(Color::DarkGray)));

        // A path git didn't report stays uncoloured.
        let r = path_row(PathBuf::from("/repo/clean.rs"), RowKind::File);
        assert_eq!(row_style(&r, &git), None);

        // Sessions are never coloured, even when their path has a status.
        let r = path_row(staged, RowKind::Session { slug: "s".into(), kind: SessionKind::Shell });
        assert_eq!(row_style(&r, &git), None);
    }

    #[test]
    fn row_line_places_button_after_name_and_marks_sessions() {
        let dir = dir_row("src", 0);
        assert_eq!(row_line(&dir), "▾ src [+]");
        // Sessions carry a per-kind mark and a `[×]` close button after the name.
        let shell = Row { path: std::path::PathBuf::from("/"), label: "shell".into(), depth: 1, kind: RowKind::Session { slug: "s".into(), kind: SessionKind::Shell } };
        assert_eq!(row_line(&shell), "  $ shell [×]");
        let claude = Row { path: std::path::PathBuf::from("/"), label: "claude".into(), depth: 1, kind: RowKind::Session { slug: "c".into(), kind: SessionKind::Claude } };
        assert_eq!(row_line(&claude), "  ✦ claude [×]");
        assert_eq!(row_line(&file_row("a.rs")), "  a.rs");
    }

    #[test]
    fn resolve_click_hits_session_close_button() {
        // content_x=0; row 1 is a depth-1 shell session "shell": session_left is
        // "  $ shell" (9 chars), so "[×]" occupies columns 10,11,12.
        let sess = Row {
            path: std::path::PathBuf::from("/"),
            label: "shell".into(),
            depth: 1,
            kind: RowKind::Session { slug: "src-shell".into(), kind: SessionKind::Shell },
        };
        let rows = vec![file_row("a"), sess];
        let layout = ListLayout { origin_y: 1, content_x: 0, row_count: 2, offset: 0, view_h: 10 };
        // clicking the name (not the cross) selects the row
        assert_eq!(resolve_click(3, 2, &layout, &rows), Some(Hit::Row(1)));
        // clicking the cross closes the session
        assert_eq!(resolve_click(11, 2, &layout, &rows), Some(Hit::Close(1)));
        // just past the cross is still the row, not the close button
        assert_eq!(resolve_click(13, 2, &layout, &rows), Some(Hit::Row(1)));
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
        // viewer Some lets render skip the embedded terminal screen. Root at an
        // empty tempdir (not `/`) so App::new's git scan finds nothing instantly.
        let has_thumb = |rows: Vec<Row>| -> bool {
            let dir = tempfile::tempdir().unwrap();
            let mut app = App::new(dir.path().to_path_buf(), Tmux::new("runner", MockRunner::new()));
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
            let dir = tempfile::tempdir().unwrap();
            let mut app = App::new(dir.path().to_path_buf(), Tmux::new("runner", MockRunner::new()));
            app.rows = rows.clone();
            app.viewer = Some(FileView::load(Path::new("/nonexistent")));
            app.tree_offset = offset;
            app.selected = selected;
            let mut terminal = Terminal::new(TestBackend::new(40, 10 + BANNER_HEIGHT + TAB_HEIGHT)).unwrap();
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

        // The tree pane sits below the banner and the tab bar. Its track lives
        // inside a 1-cell vertical margin, so it spans rows
        // BANNER_HEIGHT+TAB_HEIGHT+1 ..= BANNER_HEIGHT+TAB_HEIGHT+8.
        let top = BANNER_HEIGHT + TAB_HEIGHT + 1;
        let bottom = BANNER_HEIGHT + TAB_HEIGHT + 8;
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

        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().to_path_buf(), Tmux::new("runner", MockRunner::new()));
        app.rows = (0..30)
            .map(|i| Row {
                path: PathBuf::from("/"),
                label: format!("f{i}"),
                depth: 0,
                kind: RowKind::File,
            })
            .collect();
        app.viewer = Some(FileView::load(Path::new("/nonexistent")));

        // 40 wide; height is BANNER_HEIGHT + TAB_HEIGHT taller than the tree so
        // that after the banner, tab bar, and border the tree inner height is
        // still 8 rows.
        let mut terminal = Terminal::new(TestBackend::new(40, 10 + BANNER_HEIGHT + TAB_HEIGHT)).unwrap();
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
    fn render_tabs_draws_both_labels_and_reports_hits() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut tab_bar = None;
        let mut terminal = Terminal::new(TestBackend::new(30, 1)).unwrap();
        terminal
            .draw(|f| {
                tab_bar = Some(render_tabs(f, f.area(), TreeTab::Project));
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("directory"));
        assert!(content.contains("project"));
        let bar = tab_bar.unwrap();
        // Clicking inside the "directory" label resolves to that tab; clicking
        // inside "project" resolves to the other; a miss returns None.
        let dir_hit = bar.hits.iter().find(|(_, _, t)| *t == TreeTab::Directory).unwrap();
        assert_eq!(resolve_tab_click(dir_hit.0, bar.y, &bar), Some(TreeTab::Directory));
        let proj_hit = bar.hits.iter().find(|(_, _, t)| *t == TreeTab::Project).unwrap();
        assert_eq!(resolve_tab_click(proj_hit.0, bar.y, &bar), Some(TreeTab::Project));
        // A click on a different row never hits a tab.
        assert_eq!(resolve_tab_click(dir_hit.0, bar.y + 1, &bar), None);
    }

    #[test]
    fn render_paints_terminal_selection_reversed() {
        use crate::app::{App, Focus};
        use crate::select::Selection;
        use crate::tmux::{MockRunner, Tmux};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::sync::RwLock;
        use tui_term::vt100::Parser;

        // A live screen with some text in the top-left of the terminal pane.
        let parser = RwLock::new(Parser::new(24, 80, 0));
        parser.write().unwrap().process(b"hello world");

        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().to_path_buf(), Tmux::new("runner", MockRunner::new()));
        app.focus = Focus::Right;
        // Select the first five cells of the first terminal row ("hello").
        app.selection = Some(Selection { anchor: (0, 0), cursor: (4, 0) });

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut term_area = None;
        terminal
            .draw(|f| {
                let p = parser.read().unwrap();
                let layout = render(f, f.area(), &mut app, Some(p.screen()));
                term_area = Some(layout.term_area);
            })
            .unwrap();

        let term = term_area.unwrap();
        let buf = terminal.backend().buffer();
        // The first selected cell is reversed; a cell past the selection is not.
        let selected = &buf[(term.x, term.y)];
        assert!(selected.modifier.contains(Modifier::REVERSED));
        let unselected = &buf[(term.x + 6, term.y)];
        assert!(!unselected.modifier.contains(Modifier::REVERSED));
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
                    &[],
                    None,
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

    #[test]
    fn render_chooser_shows_resume_rows() {
        use crate::app::ChooserRow;
        use crate::claude::ResumeSession;
        use crate::session::{ClaudePerm, SessionKind};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let resumes = vec![ResumeSession {
            id: "deadbeef-0000".into(),
            last_command: "refactor the chooser".into(),
            modified: std::time::SystemTime::UNIX_EPOCH,
        }];
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|f| {
                let rows = render_chooser(
                    f,
                    f.area(),
                    SessionKind::Claude,
                    ClaudePerm::Normal,
                    &resumes,
                    Some(0),
                    ChooserRow::Resume(0),
                );
                assert!(rows.iter().any(|(_, r)| *r == ChooserRow::Resume(0)));
                assert!(rows.iter().any(|(_, r)| *r == ChooserRow::ResumeNew));
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains("Resume:"));
        assert!(content.contains("new session"));
        assert!(content.contains("refactor the chooser"));
    }
}
