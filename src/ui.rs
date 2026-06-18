use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::layout::{Constraint, Direction, Layout as RtLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;
use tui_term::vt100;
use tui_term::widget::PseudoTerminal;

use crate::app::Focus;
use crate::tree::Row;

pub struct ListLayout {
    pub origin_y: u16,
    pub button_col_start: u16,
    pub button_col_end: u16,
    pub row_count: usize,
}

pub struct Layout {
    pub tree: ListLayout,
    pub split_col: u16,
    pub term_area: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Row(usize),
    Button(usize),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneHit {
    Tree(Option<Hit>),
    Terminal,
}

pub fn resolve_pane_click(
    col: u16,
    row: u16,
    split_col: u16,
    tree_layout: &ListLayout,
) -> PaneHit {
    if col >= split_col {
        PaneHit::Terminal
    } else {
        PaneHit::Tree(resolve_click(col, row, tree_layout))
    }
}

fn pane_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

pub fn render(
    f: &mut Frame,
    area: Rect,
    rows: &[Row],
    selected: usize,
    active: &HashSet<PathBuf>,
    focus: Focus,
    screen: &vt100::Screen,
) -> Layout {
    let chunks = RtLayout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);
    let tree_area = chunks[0];
    let right_area = chunks[1];

    // ----- left: tree -----
    let tree_block = Block::default()
        .title("tree")
        .borders(Borders::ALL)
        .border_style(pane_border_style(focus == Focus::Tree));
    let inner = tree_block.inner(tree_area);
    f.render_widget(tree_block, tree_area);

    let width = inner.width as usize;
    let mut items: Vec<ListItem> = Vec::new();
    for row in rows {
        let indent = "  ".repeat(row.depth);
        let icon = if row.is_dir {
            if row.expanded { "▾ " } else { "▸ " }
        } else {
            "  "
        };
        let badge = if active.contains(&row.path) { "● " } else { "" };
        let left = format!("{indent}{icon}{badge}{}", row.name);
        let line = if row.is_dir {
            let btn = "[+]";
            let pad = width.saturating_sub(left.chars().count() + btn.len());
            format!("{left}{}{btn}", " ".repeat(pad))
        } else {
            left
        };
        items.push(ListItem::new(line));
    }
    let list =
        List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(selected.min(rows.len() - 1)));
    }
    f.render_stateful_widget(list, inner, &mut state);

    let tree_layout = ListLayout {
        origin_y: inner.y,
        button_col_start: inner.x + inner.width.saturating_sub(3),
        button_col_end: inner.x + inner.width.saturating_sub(1),
        row_count: rows.len(),
    };

    // ----- right: embedded terminal -----
    let term_block = Block::default()
        .title("terminal")
        .borders(Borders::ALL)
        .border_style(pane_border_style(focus == Focus::Terminal));
    let term_inner = term_block.inner(right_area);
    f.render_widget(term_block, right_area);
    let pseudo_term = PseudoTerminal::new(screen);
    f.render_widget(pseudo_term, term_inner);

    Layout {
        tree: tree_layout,
        split_col: right_area.x,
        term_area: term_inner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Row;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[test]
    fn resolve_click_distinguishes_row_and_button() {
        let layout = ListLayout {
            origin_y: 1,
            button_col_start: 20,
            button_col_end: 22,
            row_count: 3,
        };
        assert_eq!(resolve_click(5, 1, &layout), Some(Hit::Row(0)));
        assert_eq!(resolve_click(21, 2, &layout), Some(Hit::Button(1)));
        assert_eq!(resolve_click(5, 0, &layout), None); // above list
        assert_eq!(resolve_click(5, 10, &layout), None); // below rows
    }

    #[test]
    fn render_draws_names_and_button() {
        use crate::app::Focus;
        let rows = vec![
            Row { path: PathBuf::from("/p"), name: "p".into(), is_dir: true, depth: 0, expanded: true },
            Row { path: PathBuf::from("/p/src"), name: "src".into(), is_dir: true, depth: 1, expanded: false },
            Row { path: PathBuf::from("/p/r.md"), name: "r.md".into(), is_dir: false, depth: 1, expanded: false },
        ];
        let active: HashSet<PathBuf> = HashSet::new();
        let parser = tui_term::vt100::Parser::new(24, 80, 0);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let _ = render(f, f.area(), &rows, 0, &active, Focus::Tree, parser.screen());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let content: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(content.contains("src"));
        assert!(content.contains("[+]"));
    }

    #[test]
    fn pane_click_left_of_split_resolves_tree() {
        let layout = ListLayout {
            origin_y: 1,
            button_col_start: 38,
            button_col_end: 40,
            row_count: 3,
        };
        // split at col 50; a click at col 5 row 2 is in the tree on row 1
        assert_eq!(
            resolve_pane_click(5, 2, 50, &layout),
            PaneHit::Tree(Some(Hit::Row(1)))
        );
        // a click on the [+] button column within the tree
        assert_eq!(
            resolve_pane_click(39, 1, 50, &layout),
            PaneHit::Tree(Some(Hit::Button(0)))
        );
        // a tree-region click below the rows resolves to Tree(None)
        assert_eq!(resolve_pane_click(5, 20, 50, &layout), PaneHit::Tree(None));
    }

    #[test]
    fn pane_click_at_or_after_split_is_terminal() {
        let layout = ListLayout {
            origin_y: 1,
            button_col_start: 38,
            button_col_end: 40,
            row_count: 3,
        };
        assert_eq!(resolve_pane_click(50, 2, 50, &layout), PaneHit::Terminal);
        assert_eq!(resolve_pane_click(70, 4, 50, &layout), PaneHit::Terminal);
    }
}
