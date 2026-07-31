//! Input routing: translates crossterm key and mouse events into `App`
//! mutations plus the few [`Action`]s only the run loop can perform (it owns
//! the PTY, the git worker channel, and the loop itself). Extracted from the
//! event loop so the whole keymap — popup dispatch, focus dispatch, mouse hit
//! resolution, splitter drags — is unit-testable over an `App<MockRunner>`
//! without a real terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{col_to_split_pct, App, ChooserRow, Focus, Popup};
use crate::term::keys::{encode_key, encode_wheel};
use crate::tmux::CommandRunner;
use crate::ui::{self, Hit, PaneHit};

/// A side effect the router cannot perform itself because the run loop owns
/// the resource it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Leave the event loop: the user quit.
    Quit,
    /// Write these bytes — an encoded keystroke or SGR wheel report — to the
    /// embedded terminal's PTY.
    WriteToPty(Vec<u8>),
    /// Git colouring was just toggled on: start a background scan unless one
    /// is already in flight (only the loop knows).
    SpawnGitScan,
}

/// Geometry of the last drawn frame, captured by the run loop after each
/// render; mouse events are resolved against it. Popup spans are empty on
/// frames where that popup was not drawn, so a stale span can never register
/// a hit.
pub struct Geometry {
    /// Pane, tree, and tab geometry returned by `ui::render`.
    pub layout: ui::Layout,
    /// The chooser popup's rect: a click inside it that hits no control is a
    /// harmless no-op rather than a cancel.
    pub chooser_rect: Rect,
    /// Clickable chooser rows as `(y, x_start, x_end, row)`, x-inclusive.
    pub chooser_hits: Vec<(u16, u16, u16, ChooserRow)>,
    /// Confirm-close buttons as `(y, x_start, x_end, is_yes)`, x-inclusive.
    pub confirm_buttons: Vec<(u16, u16, u16, bool)>,
    /// Full frame width, for translating a splitter drag into a percent.
    pub area_width: u16,
}

/// Routes input events to `App` mutations, carrying the one piece of state
/// that spans events: whether a splitter drag is in progress.
#[derive(Default)]
pub struct Router {
    dragging_split: bool,
}

impl Router {
    /// A router with no drag in progress.
    pub fn new() -> Self {
        Self::default()
    }

    /// Route one key press: popups own all keys while open; otherwise the key
    /// goes to the focused pane. Returns the [`Action`] the loop must perform,
    /// if any.
    pub fn route_key<R: CommandRunner>(
        &mut self,
        app: &mut App<R>,
        key: KeyEvent,
    ) -> Option<Action> {
        match app.popup.clone() {
            Popup::Help => {
                app.popup = Popup::None;
                None
            }
            Popup::Chooser { .. } => {
                chooser_key(app, key);
                None
            }
            Popup::ConfirmClose { .. } => {
                confirm_key(app, key);
                None
            }
            Popup::None => plain_key(app, key),
        }
    }

    /// Route one mouse event against the last drawn frame's `geom`. Returns
    /// the [`Action`] the loop must perform, if any.
    pub fn route_mouse<R: CommandRunner>(
        &mut self,
        app: &mut App<R>,
        m: MouseEvent,
        geom: &Geometry,
    ) -> Option<Action> {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.left_down(app, m.column, m.row, geom),
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.dragging_split {
                    app.split_pct = col_to_split_pct(m.column, geom.area_width);
                }
                None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Save the dragged width once the drag ends, not on every
                // intermediate move.
                if self.dragging_split {
                    app.persist_split();
                }
                self.dragging_split = false;
                None
            }
            MouseEventKind::ScrollUp => wheel(app, true, m.column, m.row, geom),
            MouseEventKind::ScrollDown => wheel(app, false, m.column, m.row, geom),
            _ => None,
        }
    }

    /// A left-button press: popups resolve against their drawn spans; with no
    /// popup open the click goes to the tab bar, the splitter, or a pane.
    fn left_down<R: CommandRunner>(
        &mut self,
        app: &mut App<R>,
        col: u16,
        row: u16,
        geom: &Geometry,
    ) -> Option<Action> {
        match app.popup.clone() {
            Popup::Help => app.popup = Popup::None,
            Popup::Chooser { .. } => match ui::resolve_span(col, row, &geom.chooser_hits) {
                Some(hit) => {
                    let _ = app.chooser_click(*hit);
                }
                // A miss inside the popup (a group label, a blank line) keeps
                // the form; only a click outside cancels it.
                None if !geom.chooser_rect.contains((col, row).into()) => app.chooser_cancel(),
                None => {}
            },
            Popup::ConfirmClose { .. } => {
                // Only a click on the "[ Yes ]" text itself confirms the kill;
                // the No button or anywhere else dismisses.
                match ui::resolve_span(col, row, &geom.confirm_buttons) {
                    Some(true) => {
                        let _ = app.confirm_close();
                    }
                    _ => app.cancel_close(),
                }
            }
            Popup::None => {
                let border = geom.layout.split_col;
                let on_border = col + 1 >= border && col <= border.saturating_add(1);
                if let Some(tab) = ui::resolve_tab_click(col, row, &geom.layout.tabs) {
                    app.focus = Focus::Tree;
                    app.set_tab(tab);
                } else if on_border {
                    self.dragging_split = true;
                } else {
                    pane_click(app, col, row, geom);
                }
            }
        }
        None
    }
}

/// A left click that landed in a pane: focus it, and act on the tree row or
/// button under the cursor.
fn pane_click<R: CommandRunner>(app: &mut App<R>, col: u16, row: u16, geom: &Geometry) {
    match ui::resolve_pane_click(
        col,
        row,
        geom.layout.split_col,
        &geom.layout.tree,
        &app.rows,
    ) {
        PaneHit::Right => app.focus = Focus::Right,
        PaneHit::Tree(hit) => {
            app.focus = Focus::Tree;
            match hit {
                Some(Hit::Row(idx)) => {
                    app.selected = idx;
                    let _ = app.activate();
                }
                Some(Hit::Button(idx)) => {
                    app.selected = idx;
                    app.open_chooser();
                }
                Some(Hit::Close(idx)) => {
                    app.selected = idx;
                    app.request_close(idx);
                }
                None => {}
            }
        }
    }
}

/// Keys while the chooser popup is open. Enter commits the form from any
/// group (Cancel still cancels); Space acts on the focused button; Up/Down
/// move between selection groups, Left/Right change the option within the
/// focused group; Tab/Shift-Tab cycle groups with wrap-around.
fn chooser_key<R: CommandRunner>(app: &mut App<R>, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.chooser_cancel(),
        KeyCode::Enter => {
            let _ = app.chooser_commit();
        }
        KeyCode::Char(' ') => {
            let _ = app.chooser_activate();
        }
        // Pure form navigation goes straight to the form.
        code => {
            let Popup::Chooser(form) = &mut app.popup else {
                return;
            };
            match code {
                KeyCode::Down | KeyCode::Char('j') => form.group_move(1),
                KeyCode::Up | KeyCode::Char('k') => form.group_move(-1),
                KeyCode::Right | KeyCode::Char('l') => form.option_move(1),
                KeyCode::Left | KeyCode::Char('h') => form.option_move(-1),
                KeyCode::Tab => form.group_cycle(1),
                KeyCode::BackTab => form.group_cycle(-1),
                _ => {}
            }
        }
    }
}

/// Keys while the close-session confirmation is open: y/Enter confirm the
/// kill, n/Esc dismiss, everything else is ignored.
fn confirm_key<R: CommandRunner>(app: &mut App<R>, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            let _ = app.confirm_close();
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => app.cancel_close(),
        _ => {}
    }
}

/// Keys with no popup open: Ctrl-q toggles focus between the panes; every
/// other key is dispatched to whichever pane has focus.
fn plain_key<R: CommandRunner>(app: &mut App<R>, key: KeyEvent) -> Option<Action> {
    if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.toggle_focus();
        return None;
    }
    match app.focus {
        Focus::Tree => tree_key(app, key),
        Focus::Right => right_key(app, key),
    }
}

/// Keys while the tree pane has focus (navigation and app-level commands).
fn tree_key<R: CommandRunner>(app: &mut App<R>, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('q') => return Some(Action::Quit),
        KeyCode::Char('h') | KeyCode::Char('?') => app.popup = Popup::Help,
        KeyCode::Tab => app.toggle_tab(),
        KeyCode::Char('a') => app.open_chooser(),
        KeyCode::Char('x') => app.request_close(app.selected),
        KeyCode::Char('j') | KeyCode::Down => app.down(),
        KeyCode::Char('k') | KeyCode::Up => app.up(),
        KeyCode::Enter => {
            let _ = app.activate();
        }
        KeyCode::Char('<') => app.narrow_split(),
        KeyCode::Char('>') => app.widen_split(),
        // Toggle git-status colouring (off by default). Turning it on asks the
        // loop for an immediate scan so colours appear promptly.
        KeyCode::Char('g') => {
            return app.toggle_git_status().then_some(Action::SpawnGitScan);
        }
        _ => {}
    }
    None
}

/// Keys while the right pane has focus: scroll the file viewer when one is
/// open, otherwise forward the encoded keystroke to the embedded PTY.
fn right_key<R: CommandRunner>(app: &mut App<R>, key: KeyEvent) -> Option<Action> {
    if app.viewer.is_some() {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.viewer_scroll(1, false),
            KeyCode::Char('k') | KeyCode::Up => app.viewer_scroll(-1, false),
            KeyCode::PageDown => app.viewer_scroll(1, true),
            KeyCode::PageUp => app.viewer_scroll(-1, true),
            _ => {}
        }
        return None;
    }
    let bytes = encode_key(key);
    // Keys with no PTY encoding (F-keys etc.) forward nothing.
    (!bytes.is_empty()).then_some(Action::WriteToPty(bytes))
}

/// A wheel tick with no popup open: over the tree it scrolls the row list,
/// over an open file viewer it scrolls the file, over the terminal pane it is
/// translated to a pane-local SGR report for the embedded client (tmux turns
/// it into scrollback scrolling). Off-pane ticks forward nothing.
fn wheel<R: CommandRunner>(
    app: &mut App<R>,
    up: bool,
    col: u16,
    row: u16,
    geom: &Geometry,
) -> Option<Action> {
    if !matches!(app.popup, Popup::None) {
        return None;
    }
    if col < geom.layout.split_col {
        app.scroll_tree(if up { -3 } else { 3 }, geom.layout.tree.view_h as usize);
        return None;
    }
    if let Some(v) = &mut app.viewer {
        if up {
            v.scroll_up(3);
        } else {
            v.scroll_down(3);
        }
        return None;
    }
    let term = geom.layout.term_area;
    let in_pane =
        col >= term.x && row >= term.y && col < term.x + term.width && row < term.y + term.height;
    in_pane.then(|| Action::WriteToPty(encode_wheel(up, col - term.x + 1, row - term.y + 1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::rows::RowKind;
    use crate::app::testutil::{
        app_over_tempdir, create_src_shell, open_dir_chooser, push_create_seq,
    };
    use crate::app::TreeTab;
    use crate::tmux::MockRunner;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }
    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn left_down(col: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), col, row)
    }

    /// A synthetic frame geometry over `app`'s current rows: tree list starting
    /// at screen row 1 / column 0, split at column 20, terminal pane inner rect
    /// at (21, 1) sized 20 by 10, tab bar on row 0.
    fn geom_for(app: &App<MockRunner>) -> Geometry {
        Geometry {
            layout: ui::Layout {
                tree: ui::ListLayout {
                    origin_y: 1,
                    content_x: 0,
                    row_count: app.rows.len(),
                    offset: 0,
                    view_h: 10,
                },
                split_col: 20,
                term_area: Rect {
                    x: 21,
                    y: 1,
                    width: 20,
                    height: 10,
                },
                tabs: ui::TabBar {
                    y: 0,
                    hits: vec![(0, 10, TreeTab::Directory), (11, 19, TreeTab::Project)],
                },
            },
            chooser_rect: Rect::default(),
            chooser_hits: Vec::new(),
            confirm_buttons: Vec::new(),
            area_width: 100,
        }
    }

    fn session_row_index(app: &App<MockRunner>) -> usize {
        app.rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Session { .. }))
            .expect("a session row exists")
    }

    #[test]
    fn q_quits_from_tree_but_types_into_the_terminal_from_right() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        assert_eq!(router.route_key(&mut app, ch('q')), Some(Action::Quit));
        app.focus = Focus::Right;
        assert_eq!(
            router.route_key(&mut app, ch('q')),
            Some(Action::WriteToPty(b"q".to_vec()))
        );
    }

    #[test]
    fn ctrl_q_toggles_focus_both_ways() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(router.route_key(&mut app, ctrl_q), None);
        assert_eq!(app.focus, Focus::Right);
        assert_eq!(router.route_key(&mut app, ctrl_q), None);
        assert_eq!(app.focus, Focus::Tree);
    }

    #[test]
    fn help_opens_from_tree_and_any_key_closes_it() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        router.route_key(&mut app, ch('h'));
        assert_eq!(app.popup, Popup::Help);
        // While the popup is open, any key dismisses it — even one that would
        // otherwise quit.
        assert_eq!(router.route_key(&mut app, ch('q')), None);
        assert_eq!(app.popup, Popup::None);
    }

    #[test]
    fn x_opens_confirm_close_and_y_or_n_resolve_it() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        create_src_shell(&mut app);
        app.focus = Focus::Tree;
        app.selected = session_row_index(&app);

        // n dismisses without killing.
        router.route_key(&mut app, ch('x'));
        assert!(matches!(app.popup, Popup::ConfirmClose { ref slug } if slug == "src-shell"));
        let calls_before = app.tmux.runner.call_count();
        router.route_key(&mut app, ch('n'));
        assert_eq!(app.popup, Popup::None);
        assert_eq!(app.tmux.runner.call_count(), calls_before);

        // y kills the session.
        router.route_key(&mut app, ch('x'));
        app.tmux.runner.push(true, ""); // kill-session
        router.route_key(&mut app, ch('y'));
        assert_eq!(app.popup, Popup::None);
        assert!(!app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn a_opens_chooser_esc_cancels_enter_creates() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        let src_idx = app
            .rows
            .iter()
            .position(|r| r.label == "src")
            .expect("src row");
        app.selected = src_idx;

        router.route_key(&mut app, ch('a'));
        assert!(matches!(app.popup, Popup::Chooser { .. }));
        router.route_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.popup, Popup::None);

        router.route_key(&mut app, ch('a'));
        push_create_seq(&mut app);
        assert_eq!(router.route_key(&mut app, key(KeyCode::Enter)), None);
        assert_eq!(app.popup, Popup::None);
        assert!(app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
    }

    #[test]
    fn g_requests_a_scan_only_when_toggling_on() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        assert!(!app.git_enabled);
        assert_eq!(
            router.route_key(&mut app, ch('g')),
            Some(Action::SpawnGitScan)
        );
        assert!(app.git_enabled);
        assert_eq!(router.route_key(&mut app, ch('g')), None);
        assert!(!app.git_enabled);
    }

    #[test]
    fn right_pane_keys_scroll_an_open_viewer_instead_of_typing() {
        let (d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        std::fs::write(d.path().join("notes.txt"), "a\nb\nc\n").unwrap();
        app.open_file(&d.path().join("notes.txt"));
        app.focus = Focus::Right;
        assert_eq!(router.route_key(&mut app, ch('j')), None);
        assert_eq!(app.viewer.as_ref().unwrap().scroll, 1);
        assert_eq!(router.route_key(&mut app, ch('k')), None);
        assert_eq!(app.viewer.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn keys_without_a_pty_encoding_forward_nothing() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        app.focus = Focus::Right;
        assert_eq!(router.route_key(&mut app, key(KeyCode::F(5))), None);
    }

    #[test]
    fn tab_toggles_the_left_pane_view() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        assert_eq!(app.tab, TreeTab::Directory);
        router.route_key(&mut app, key(KeyCode::Tab));
        assert_eq!(app.tab, TreeTab::Project);
    }

    #[test]
    fn click_in_the_right_pane_focuses_it() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        let geom = geom_for(&app);
        assert_eq!(router.route_mouse(&mut app, left_down(30, 5), &geom), None);
        assert_eq!(app.focus, Focus::Right);
    }

    #[test]
    fn click_on_a_dir_row_expands_it_and_its_button_opens_the_chooser() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        let geom = geom_for(&app);
        let src_idx = app
            .rows
            .iter()
            .position(|r| r.label == "src")
            .expect("src row");
        let src_y = geom.layout.tree.origin_y + src_idx as u16;

        // Clicking the name toggles the dir open.
        router.route_mouse(&mut app, left_down(2, src_y), &geom);
        assert!(app.rows.iter().any(|r| r.label == "a.rs"));

        // "src" sits at depth 1: "  " + "▾ " + "src" is 7 columns, so its
        // `[+]` button spans columns 8..=10.
        let geom = geom_for(&app);
        router.route_mouse(&mut app, left_down(8, src_y), &geom);
        assert!(matches!(app.popup, Popup::Chooser { .. }));
    }

    #[test]
    fn border_drag_resizes_the_split_and_persists_on_release() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        let geom = geom_for(&app);
        // Press on the border column (split_col = 20) starts the drag.
        router.route_mouse(&mut app, left_down(20, 5), &geom);
        router.route_mouse(
            &mut app,
            mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5),
            &geom,
        );
        assert_eq!(app.split_pct, 50);
        router.route_mouse(
            &mut app,
            mouse(MouseEventKind::Up(MouseButton::Left), 50, 5),
            &geom,
        );
        assert_eq!(app.config.load_split(), Some(50));
        // A later drag without a border press must not resize.
        router.route_mouse(
            &mut app,
            mouse(MouseEventKind::Drag(MouseButton::Left), 70, 5),
            &geom,
        );
        assert_eq!(app.split_pct, 50);
    }

    #[test]
    fn wheel_over_the_terminal_pane_forwards_a_pane_local_sgr_report() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        let geom = geom_for(&app);
        // (21, 1) is the terminal pane's top-left cell -> pane-local (1, 1).
        assert_eq!(
            router.route_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 21, 1), &geom),
            Some(Action::WriteToPty(b"\x1b[<64;1;1M".to_vec()))
        );
        // Right of the split but below the pane: nothing to forward.
        assert_eq!(
            router.route_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 21, 30), &geom),
            None
        );
    }

    #[test]
    fn wheel_over_an_open_viewer_scrolls_the_file_not_the_pty() {
        let (d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        std::fs::write(d.path().join("notes.txt"), "a\nb\nc\nd\ne\n").unwrap();
        app.open_file(&d.path().join("notes.txt"));
        let geom = geom_for(&app);
        assert_eq!(
            router.route_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 30, 5), &geom),
            None
        );
        assert_eq!(app.viewer.as_ref().unwrap().scroll, 3);
    }

    #[test]
    fn click_on_the_tab_bar_switches_views() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        let geom = geom_for(&app);
        // Column 12 on the bar row falls in the "project" span (11..=19).
        router.route_mouse(&mut app, left_down(12, 0), &geom);
        assert_eq!(app.tab, TreeTab::Project);
        assert_eq!(app.focus, Focus::Tree);
    }

    #[test]
    fn chooser_clicks_resolve_against_drawn_spans_and_outside_cancels() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        open_dir_chooser(&mut app);
        let mut geom = geom_for(&app);
        geom.chooser_rect = Rect {
            x: 10,
            y: 3,
            width: 30,
            height: 10,
        };
        geom.chooser_hits = vec![(5, 12, 20, ChooserRow::KindClaude)];

        // A hit selects the option.
        router.route_mouse(&mut app, left_down(15, 5), &geom);
        match &app.popup {
            Popup::Chooser(form) => {
                assert_eq!(form.kind, crate::tmux::session::SessionKind::Claude)
            }
            other => panic!("expected an open chooser, got {other:?}"),
        }
        // A miss inside the popup keeps the form open.
        router.route_mouse(&mut app, left_down(11, 4), &geom);
        assert!(matches!(app.popup, Popup::Chooser { .. }));
        // A click outside the popup cancels it.
        router.route_mouse(&mut app, left_down(50, 20), &geom);
        assert_eq!(app.popup, Popup::None);
    }

    #[test]
    fn confirm_close_click_kills_only_on_the_yes_button() {
        let (_d, mut app) = app_over_tempdir();
        let mut router = Router::new();
        create_src_shell(&mut app);
        app.request_close(session_row_index(&app));
        let mut geom = geom_for(&app);
        geom.confirm_buttons = vec![(5, 12, 18, true), (6, 12, 17, false)];

        // Anywhere but the Yes span dismisses without killing.
        let calls_before = app.tmux.runner.call_count();
        router.route_mouse(&mut app, left_down(14, 6), &geom);
        assert_eq!(app.popup, Popup::None);
        assert_eq!(app.tmux.runner.call_count(), calls_before);

        // The Yes span confirms the kill.
        app.request_close(session_row_index(&app));
        app.tmux.runner.push(true, ""); // kill-session
        router.route_mouse(&mut app, left_down(14, 5), &geom);
        assert_eq!(app.popup, Popup::None);
        assert!(!app
            .rows
            .iter()
            .any(|r| matches!(r.kind, RowKind::Session { .. })));
    }
}
