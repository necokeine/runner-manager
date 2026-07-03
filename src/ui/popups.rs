//! Popup rendering: the help overlay, the new-session chooser form, and the
//! close-session confirmation. Each renderer returns the clickable geometry of
//! its options so `run.rs` can resolve mouse clicks against what was drawn.

use ratatui::layout::{Constraint, Direction, Layout as RtLayout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::ChooserRow;
use crate::claude::ResumeSession;
use crate::session::{ClaudePerm, SessionKind};

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
        "g          toggle git-status colouring (off by default)",
        "wheel      scroll the tree (scrollbar shows when needed)",
        "h / ?      this help",
        "Ctrl-q     toggle focus (tree / right pane)",
        "q          quit",
        "",
        "right pane focused: type into the shell, or",
        "j/k/PgUp/PgDn to scroll a file view",
        "wheel over the terminal scrolls its history (old logs)",
        "drag in the terminal to select text → copies to clipboard",
        "double-click a word in the terminal to select + copy it",
        "",
        "— press any key to close —",
    ];
    let popup = centered_rect(64, 70, area);
    let block = Block::default().title("Keys").borders(Borders::ALL);
    let para = Paragraph::new(lines.join("\n")).block(block);
    f.render_widget(Clear, popup);
    f.render_widget(para, popup);
}

/// The radio marker for a (un)selected option, including its trailing space.
fn radio_marker(selected: bool) -> &'static str {
    if selected {
        "(•) "
    } else {
        "( ) "
    }
}

/// Build one horizontal line of options indented under a group label. Each
/// `cell` is the already-formatted option text (radio marker + label, or a
/// `[ button ]`). The focused option is drawn reversed, and its column span is
/// recorded in `hits` so a mouse click maps back to the row.
fn options_line(
    x0: u16,
    y: u16,
    cells: &[(String, ChooserRow)],
    focus_row: ChooserRow,
    hits: &mut Vec<(u16, u16, u16, ChooserRow)>,
) -> Line<'static> {
    const INDENT: u16 = 2;
    const GAP: u16 = 3;
    let mut spans: Vec<Span> = vec![Span::raw(" ".repeat(INDENT as usize))];
    let mut col = x0 + INDENT;
    for (i, (cell, row)) in cells.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" ".repeat(GAP as usize)));
            col += GAP;
        }
        let w = cell.chars().count() as u16;
        let style = if *row == focus_row {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(cell.clone(), style));
        hits.push((y, col, col + w.saturating_sub(1), *row));
        col += w;
    }
    Line::from(spans)
}

/// Draw the new-session form. Options are grouped (Kind / Permission / Resume /
/// actions); the layout puts each group's label on its own line with its
/// options laid out horizontally below it, mirroring the navigation model
/// (Up/Down between groups, Left/Right between options). Returns the clickable
/// column span `(y, x_start, x_end, row)` of every option for mouse hit-testing.
pub fn render_chooser(
    f: &mut Frame,
    area: Rect,
    kind: SessionKind,
    perm: ClaudePerm,
    resumes: &[ResumeSession],
    resume_sel: Option<usize>,
    focus_row: ChooserRow,
) -> Vec<(u16, u16, u16, ChooserRow)> {
    let popup = centered_rect(50, 70, area);
    f.render_widget(Clear, popup);
    let block = Block::default().title("New session").borders(Borders::ALL);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    let mut hits: Vec<(u16, u16, u16, ChooserRow)> = Vec::new();
    let mut y = inner.y;

    // Kind group.
    lines.push(Line::from("Kind:".to_string()));
    y += 1;
    let kind_cells = vec![
        (
            format!("{}shell", radio_marker(kind == SessionKind::Shell)),
            ChooserRow::KindShell,
        ),
        (
            format!("{}claude", radio_marker(kind == SessionKind::Claude)),
            ChooserRow::KindClaude,
        ),
        (
            format!("{}codex", radio_marker(kind == SessionKind::Codex)),
            ChooserRow::KindCodex,
        ),
    ];
    lines.push(options_line(inner.x, y, &kind_cells, focus_row, &mut hits));
    y += 1;

    if kind == SessionKind::Claude {
        lines.push(Line::from(String::new()));
        y += 1;
        lines.push(Line::from("Permission:".to_string()));
        y += 1;
        let perm_cells = vec![
            (
                format!("{}normal", radio_marker(perm == ClaudePerm::Normal)),
                ChooserRow::PermNormal,
            ),
            (
                format!("{}skip", radio_marker(perm == ClaudePerm::Skip)),
                ChooserRow::PermSkip,
            ),
        ];
        lines.push(options_line(inner.x, y, &perm_cells, focus_row, &mut hits));
        y += 1;

        // Resume picker: only shown when this directory has Claude history.
        // Entries stay vertical (full-width labels) but are still one group:
        // Left/Right walks the entries, the line spans the inner width for clicks.
        if !resumes.is_empty() {
            lines.push(Line::from(String::new()));
            y += 1;
            lines.push(Line::from("Resume:".to_string()));
            y += 1;
            let x_end = inner.x + inner.width.saturating_sub(1);
            let new_focused = focus_row == ChooserRow::ResumeNew;
            let new_style = if new_focused {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("  {}new session", radio_marker(resume_sel.is_none())),
                new_style,
            )));
            hits.push((y, inner.x, x_end, ChooserRow::ResumeNew));
            y += 1;
            // Keep the label inside the popup; reserve room for the "  (•) " prefix.
            let label_width = (inner.width as usize).saturating_sub(6);
            for (i, s) in resumes.iter().enumerate() {
                let row = ChooserRow::Resume(i);
                let style = if focus_row == row {
                    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}{}",
                        radio_marker(resume_sel == Some(i)),
                        resume_label(s, label_width)
                    ),
                    style,
                )));
                hits.push((y, inner.x, x_end, row));
                y += 1;
            }
        }
    }

    // Action buttons (no group label).
    lines.push(Line::from(String::new()));
    y += 1;
    let action_cells = vec![
        ("[ Cancel ]".to_string(), ChooserRow::Cancel),
        ("[ Create ]".to_string(), ChooserRow::Create),
    ];
    lines.push(options_line(
        inner.x,
        y,
        &action_cells,
        focus_row,
        &mut hits,
    ));

    lines.push(Line::from(String::new()));
    lines.push(Line::from(Span::styled(
        "↑↓ group · ←→ option · ↵ start · esc",
        Style::default().fg(Color::DarkGray),
    )));

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
    hits
}

/// Draw the "really close this session?" confirmation. Returns the screen `y` of
/// each clickable button paired with its choice (`true` = Yes), so `run.rs` can
/// resolve a mouse click the same way it does the chooser's rows.
pub fn render_confirm_close(f: &mut Frame, area: Rect, slug: &str) -> Vec<(u16, bool)> {
    let popup = centered_rect(50, 30, area);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .title("Close session")
        .borders(Borders::ALL);
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn centered_rect_is_centered() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        assert_eq!(
            centered_rect(50, 50, area),
            Rect {
                x: 25,
                y: 25,
                width: 50,
                height: 50
            }
        );
    }

    #[test]
    fn render_chooser_draws_radios_and_buttons() {
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
                assert!(rows.iter().any(|(_, _, _, r)| *r == ChooserRow::Create));
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
                assert!(rows.iter().any(|(_, _, _, r)| *r == ChooserRow::Resume(0)));
                assert!(rows.iter().any(|(_, _, _, r)| *r == ChooserRow::ResumeNew));
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

    #[test]
    fn render_chooser_lays_kind_options_horizontally() {
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|f| {
                hits = render_chooser(
                    f,
                    f.area(),
                    SessionKind::Shell,
                    ClaudePerm::Normal,
                    &[],
                    None,
                    ChooserRow::KindShell,
                );
            })
            .unwrap();
        let shell = hits
            .iter()
            .find(|(_, _, _, r)| *r == ChooserRow::KindShell)
            .unwrap();
        let claude = hits
            .iter()
            .find(|(_, _, _, r)| *r == ChooserRow::KindClaude)
            .unwrap();
        // Same row, side by side, with shell's span ending before claude's begins.
        assert_eq!(shell.0, claude.0);
        assert!(
            shell.2 < claude.1,
            "shell {shell:?} should sit left of claude {claude:?}"
        );
        // The Cancel/Create buttons sit on a later row (a different group).
        let create = hits
            .iter()
            .find(|(_, _, _, r)| *r == ChooserRow::Create)
            .unwrap();
        assert!(create.0 > shell.0);
    }
}
