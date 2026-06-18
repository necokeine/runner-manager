use ratatui::layout::{Constraint, Direction, Layout as RtLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Focus, CHOOSER_KINDS};
use crate::rows::{Row, RowKind};
use crate::tmux::CommandRunner;

pub struct ListLayout {
    pub origin_y: u16,
    pub button_col_start: u16,
    pub button_col_end: u16,
    pub row_count: usize,
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
    let idx = (row - layout.origin_y) as usize;
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
    app: &App<R>,
    screen: Option<&vt100::Screen>,
) -> Layout {
    let chunks = RtLayout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
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
    let items: Vec<ListItem> = app.rows.iter().map(|r| ListItem::new(row_line(r, width))).collect();
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !app.rows.is_empty() {
        state.select(Some(app.selected.min(app.rows.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);

    let tree_layout = ListLayout {
        origin_y: inner.y,
        button_col_start: inner.x + inner.width.saturating_sub(3),
        button_col_end: inner.x + inner.width.saturating_sub(1),
        row_count: app.rows.len(),
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

pub fn render_chooser(f: &mut Frame, area: Rect, selected: usize) -> Rect {
    let popup = centered_rect(40, 30, area);
    let block = Block::default().title("New session").borders(Borders::ALL);
    let items: Vec<ListItem> = CHOOSER_KINDS
        .iter()
        .map(|k| ListItem::new(format!("  {}", k.label_base())))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(selected.min(CHOOSER_KINDS.len() - 1)));
    f.render_widget(Clear, popup);
    f.render_stateful_widget(list, popup, &mut state);
    popup
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_click_distinguishes_row_and_button() {
        let layout = ListLayout { origin_y: 1, button_col_start: 20, button_col_end: 22, row_count: 3 };
        assert_eq!(resolve_click(5, 1, &layout), Some(Hit::Row(0)));
        assert_eq!(resolve_click(21, 2, &layout), Some(Hit::Button(1)));
        assert_eq!(resolve_click(5, 0, &layout), None);
        assert_eq!(resolve_click(5, 10, &layout), None);
    }

    #[test]
    fn resolve_pane_click_splits_on_column() {
        let layout = ListLayout { origin_y: 1, button_col_start: 38, button_col_end: 40, row_count: 3 };
        assert_eq!(resolve_pane_click(5, 2, 50, &layout), PaneHit::Tree(Some(Hit::Row(1))));
        assert_eq!(resolve_pane_click(50, 2, 50, &layout), PaneHit::Right);
    }

    #[test]
    fn centered_rect_is_centered() {
        let area = Rect { x: 0, y: 0, width: 100, height: 100 };
        assert_eq!(centered_rect(50, 50, area), Rect { x: 25, y: 25, width: 50, height: 50 });
    }
}
