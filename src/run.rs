use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, Focus, Popup};
use crate::git::GitStatuses;
use crate::keys::{encode_key, encode_wheel};
use crate::pty::{ParserHandle, Pty};
use crate::tmux::{SystemRunner, Tmux};
use crate::ui::{self, Hit, PaneHit};

/// Minimum idle gap between git-status scans, measured from when the previous
/// scan *finished*. The scan runs on a background thread, so this only bounds
/// how eagerly we re-scan — it never blocks input.
const GIT_RESCAN_INTERVAL: Duration = Duration::from_millis(1000);

/// Compute the git-status snapshot for `root` on a background thread and send
/// it back over `tx`. Kept off the UI thread because a full `git status` of a
/// large tree (a parent of many repos) can take seconds; running it inline —
/// as startup and the per-second sync once did — froze the UI.
fn spawn_git_scan(root: PathBuf, tx: Sender<GitStatuses>) {
    thread::spawn(move || {
        let _ = tx.send(GitStatuses::load(&root));
    });
}

pub fn run(root: PathBuf, socket: String) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tmux = Tmux::new(socket.clone(), SystemRunner);
    let mut app = App::new(root, tmux);

    // Create the project-local config dir (`<root>/.pjma`) up front: the tmux
    // socket lives inside it, so it must exist before any tmux command runs.
    let _ = app.config.ensure_dir();

    // Recovery: attach the embedded client to the session the user was last
    // active in rather than a throwaway scratch session. If no sessions exist
    // (fresh start, nothing to recover), spawn nothing — the right pane stays
    // empty until the first session is created, at which point the run loop
    // attaches to it via `pending_respawn`.
    let latest = app.tmux.latest_session().ok().flatten();
    // Label the terminal pane with the recovered session from the first frame;
    // `sync` later reconciles this against the client's real session.
    app.current_session = latest.clone();
    let mut pty: Option<Pty> = None;
    let mut parser: Option<ParserHandle> = None;
    if let Some(name) = &latest {
        let args = ["tmux", "-S", socket.as_str(), "new-session", "-A", "-s", name.as_str()];
        match Pty::spawn(&args, 24, 80) {
            Ok(p) => {
                parser = Some(p.parser());
                pty = Some(p);
            }
            Err(e) => {
                let _ = disable_raw_mode();
                let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
                return Err(e);
            }
        }
    }

    // Only wait for / configure a tmux server when we actually attached a
    // client. With no sessions there is no server to talk to yet; the first
    // `create_session` starts one and the respawn path re-applies the options.
    if pty.is_some() {
        for _ in 0..20 {
            if app.host_client_ready() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // Keep every session alive across our own lifetime: never detach a
        // client just because its session was destroyed, and never destroy a
        // session just because it has no attached client (which is exactly what
        // happens to the session we were viewing when we quit). Only `exit`
        // inside a session ends it. `destroy-unattached` defaults to off but a
        // user's tmux config can turn it on, so we force it off to be safe.
        let _ = app.tmux.set_global_option("detach-on-destroy", "off");
        let _ = app.tmux.set_global_option("destroy-unattached", "off");
        // Let the embedded client scroll its own scrollback: with mouse on,
        // a forwarded wheel event puts the pane into copy-mode (showing the
        // old logs) unless a full-screen app has grabbed the mouse itself.
        let _ = app.tmux.set_global_option("mouse", "on");
    }
    let _ = app.sync();
    // Re-expand the directories the user had open last session.
    app.restore_expanded();

    // Git colouring is computed off the UI thread: kick off the first scan now
    // and apply each result as it arrives (see the loop top). A single
    // in-flight scan at a time (`git_inflight`) prevents a slow scan from
    // piling up behind itself. The feature is off by default (see
    // `Config::git_status_enabled`); when disabled we spawn no scans at all and
    // the tree renders in its default colours until the user toggles it on (`g`).
    let (git_tx, git_rx) = mpsc::channel::<GitStatuses>();
    let mut git_inflight = false;
    if app.git_enabled {
        spawn_git_scan(app.root.clone(), git_tx.clone());
        git_inflight = true;
    }
    let mut last_git = Instant::now();

    let mut last_term_size: (u16, u16) = (0, 0);
    let mut last_sync = Instant::now();
    let mut area_width: u16 = 0;
    let mut dragging_split = false;
    // Clickable chooser regions: (row y, x start, x end inclusive, the row).
    let mut chooser_row_ys: Vec<(u16, u16, u16, crate::app::ChooserRow)> = Vec::new();
    let mut confirm_row_ys: Vec<(u16, bool)> = Vec::new();

    // The render is by far the most expensive thing this loop does (it copies
    // the whole embedded-terminal grid and rebuilds every widget), so we only
    // draw when something actually changed. `dirty` starts `true` for the first
    // frame; thereafter it is re-armed by terminal output, input events, resize,
    // the periodic sync, and background git results. The last computed layout is
    // kept so mouse hit-testing still works on frames we skip drawing.
    let mut dirty = true;
    let mut layout: Option<ui::Layout> = None;

    let result = loop {
        // Apply a finished background git scan, if one landed since last frame.
        // `try_recv` never blocks, so a still-running scan just leaves the
        // current colours in place.
        match git_rx.try_recv() {
            Ok(git) => {
                // Drop a scan that finished after the user toggled the feature
                // off; `toggle_git_status` already cleared the colours.
                if app.git_enabled {
                    app.apply_git(git);
                }
                git_inflight = false;
                last_git = Instant::now();
                dirty = true;
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {}
        }
        // Re-scan once the idle gap has elapsed since the last scan finished,
        // but only when the feature is on and none is in flight — a slow scan
        // must not stack up.
        if app.git_enabled && !git_inflight && last_git.elapsed() >= GIT_RESCAN_INTERVAL {
            spawn_git_scan(app.root.clone(), git_tx.clone());
            git_inflight = true;
        }

        // Fresh output from the embedded terminal is a reason to redraw.
        if let Some(p) = &pty {
            if p.take_dirty() {
                dirty = true;
            }
        }

        // Reconcile sessions once a second. Sync can change the row list,
        // briefs, or the shown session (hence the terminal title), so a redraw
        // has to follow — but at most once per second, not every poll tick.
        if last_sync.elapsed() >= Duration::from_millis(1000) {
            let _ = app.sync();
            last_sync = Instant::now();
            dirty = true;
        }

        // Only pay for a render when something changed since the last frame.
        if dirty {
            let mut captured: Option<ui::Layout> = None;
            let draw_res = terminal.draw(|f| {
                area_width = f.area().width;
                let screen_guard = if app.viewer.is_none() {
                    parser.as_ref().map(crate::pty::read_screen)
                } else {
                    None
                };
                let screen = screen_guard.as_ref().map(|g| g.screen());
                captured = Some(ui::render(f, f.area(), &mut app, screen));
                drop(screen_guard);
                match &app.popup {
                    Popup::Help => ui::render_help(f, f.area()),
                    Popup::Chooser { kind, perm, resume, .. } => {
                        let focus_row = app.chooser_focus_row();
                        chooser_row_ys = ui::render_chooser(
                            f,
                            f.area(),
                            *kind,
                            *perm,
                            &app.chooser_resumes,
                            *resume,
                            focus_row,
                        );
                    }
                    Popup::ConfirmClose { slug } => {
                        confirm_row_ys = ui::render_confirm_close(f, f.area(), slug);
                    }
                    Popup::None => {}
                }
            });
            if let Err(e) = draw_res {
                break Err(e);
            }
            let new_layout = captured.expect("render returns a Layout");

            if app.viewer.is_none() {
                if let Some(p) = &mut pty {
                    let term_size = (new_layout.term_area.height, new_layout.term_area.width);
                    if term_size != last_term_size && term_size.0 > 0 && term_size.1 > 0 {
                        let _ = p.resize(term_size.0, term_size.1);
                        last_term_size = term_size;
                    }
                }
            }
            layout = Some(new_layout);
            dirty = false;
        }

        if !event::poll(Duration::from_millis(33)).unwrap_or(false) {
            continue;
        }

        // We have drawn at least once (the first pass starts `dirty`), so a
        // layout is always available for hit-testing here.
        let layout = layout.as_ref().expect("a frame is drawn before any event");

        let event = event::read();
        // Any input we act on — a keypress, a mouse action, or a terminal
        // resize — changes the next frame, so arm a redraw before dispatching.
        match &event {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => dirty = true,
            Ok(Event::Mouse(_)) | Ok(Event::Resize(..)) => dirty = true,
            _ => {}
        }

        match event {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                match app.popup.clone() {
                    Popup::Help => {
                        app.popup = Popup::None;
                    }
                    Popup::Chooser { .. } => match key.code {
                        KeyCode::Esc => app.chooser_cancel(),
                        // Enter commits the form from any group (Cancel still
                        // cancels); Space acts on the focused button.
                        KeyCode::Enter => {
                            let _ = app.chooser_commit();
                        }
                        KeyCode::Char(' ') => {
                            let _ = app.chooser_activate();
                        }
                        // Up/Down move between selection groups; Left/Right
                        // change the option within the focused group.
                        KeyCode::Down | KeyCode::Char('j') => app.chooser_group_move(1),
                        KeyCode::Up | KeyCode::Char('k') => app.chooser_group_move(-1),
                        KeyCode::Right | KeyCode::Char('l') => app.chooser_option_move(1),
                        KeyCode::Left | KeyCode::Char('h') => app.chooser_option_move(-1),
                        KeyCode::Tab => app.chooser_group_cycle(1),
                        KeyCode::BackTab => app.chooser_group_cycle(-1),
                        _ => {}
                    },
                    Popup::ConfirmClose { .. } => match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            let _ = app.confirm_close();
                        }
                        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                            app.cancel_close()
                        }
                        _ => {}
                    },
                    Popup::None => {
                        if key.code == KeyCode::Char('q')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            app.toggle_focus();
                        } else {
                            match app.focus {
                                Focus::Tree => match key.code {
                                    KeyCode::Char('q') => break Ok(()),
                                    KeyCode::Char('h') | KeyCode::Char('?') => {
                                        app.popup = Popup::Help;
                                    }
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
                                    // Toggle git-status colouring (off by
                                    // default). Turning it on kicks an
                                    // immediate scan so colours appear promptly.
                                    KeyCode::Char('g') => {
                                        let now_on = app.toggle_git_status();
                                        if now_on && !git_inflight {
                                            spawn_git_scan(app.root.clone(), git_tx.clone());
                                            git_inflight = true;
                                        }
                                    }
                                    _ => {}
                                },
                                Focus::Right => {
                                    if app.viewer.is_some() {
                                        match key.code {
                                            KeyCode::Char('j') | KeyCode::Down => {
                                                app.viewer_scroll(1, false)
                                            }
                                            KeyCode::Char('k') | KeyCode::Up => {
                                                app.viewer_scroll(-1, false)
                                            }
                                            KeyCode::PageDown => app.viewer_scroll(1, true),
                                            KeyCode::PageUp => app.viewer_scroll(-1, true),
                                            _ => {}
                                        }
                                    } else if let Some(p) = &mut pty {
                                        // Typing changes what is on screen, so any
                                        // selection painted over it is now stale.
                                        app.clear_selection();
                                        let _ = p.write_input(&encode_key(key));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Mouse(m)) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => match app.popup.clone() {
                    Popup::Help => app.popup = Popup::None,
                    Popup::Chooser { .. } => {
                        match chooser_row_ys
                            .iter()
                            .find(|(y, x0, x1, _)| *y == m.row && m.column >= *x0 && m.column <= *x1)
                        {
                            Some((_, _, _, row)) => {
                                let _ = app.chooser_click(*row);
                            }
                            None => app.chooser_cancel(),
                        }
                    }
                    Popup::ConfirmClose { .. } => {
                        match confirm_row_ys.iter().find(|(y, _)| *y == m.row) {
                            Some((_, true)) => {
                                let _ = app.confirm_close();
                            }
                            // The No button or anywhere else dismisses.
                            _ => app.cancel_close(),
                        }
                    }
                    Popup::None => {
                        let border = layout.split_col;
                        let on_border =
                            m.column + 1 >= border && m.column <= border.saturating_add(1);
                        if let Some(tab) = ui::resolve_tab_click(m.column, m.row, &layout.tabs) {
                            app.focus = Focus::Tree;
                            app.set_tab(tab);
                        } else if on_border {
                            dragging_split = true;
                        } else {
                            match ui::resolve_pane_click(m.column, m.row, layout.split_col, &layout.tree, &app.rows) {
                                PaneHit::Right => {
                                    app.focus = Focus::Right;
                                    // Start a text selection when the press lands
                                    // inside the live terminal (not the viewer,
                                    // not the border). A bare click stays empty
                                    // and copies nothing.
                                    app.clear_selection();
                                    if app.viewer.is_none() {
                                        if let Some((c, r)) = cell_in_pane(m.column, m.row, layout.term_area) {
                                            app.begin_selection(c, r);
                                        }
                                    }
                                }
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
                    }
                },
                MouseEventKind::Drag(MouseButton::Left) => {
                    if dragging_split {
                        app.split_pct = crate::app::col_to_split_pct(m.column, area_width);
                    } else if app.selection.is_some() {
                        // Extend the selection, clamping the pointer into the
                        // pane so a drag past its edge selects to the boundary.
                        if let Some((c, r)) = clamp_cell(m.column, m.row, layout.term_area) {
                            app.update_selection(c, r);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    // Save the dragged width once the drag ends, not on every
                    // intermediate move.
                    if dragging_split {
                        app.persist_split();
                    } else if let Some(sel) = app.selection {
                        if sel.is_empty() {
                            // A bare click selected nothing; drop the empty marker.
                            app.clear_selection();
                        } else if let Some(p) = &parser {
                            // Pull the covered text from the current screen and
                            // copy it; leave the highlight up as confirmation
                            // until the next interaction clears it.
                            let guard = crate::pty::read_screen(p);
                            let text = crate::select::selected_text(guard.screen(), &sel);
                            drop(guard);
                            if !text.is_empty() {
                                crate::clipboard::copy(&text);
                                app.status = format!("copied {} chars to clipboard", text.chars().count());
                            }
                        }
                    }
                    dragging_split = false;
                }
                MouseEventKind::ScrollDown => {
                    if matches!(app.popup, Popup::None) {
                        if m.column < layout.split_col {
                            app.scroll_tree(3, layout.tree.view_h as usize);
                        } else if let Some(v) = &mut app.viewer {
                            v.scroll_down(3);
                        } else {
                            app.clear_selection();
                            forward_wheel(&mut pty, false, m.column, m.row, layout.term_area);
                        }
                    }
                }
                MouseEventKind::ScrollUp => {
                    if matches!(app.popup, Popup::None) {
                        if m.column < layout.split_col {
                            app.scroll_tree(-3, layout.tree.view_h as usize);
                        } else if let Some(v) = &mut app.viewer {
                            v.scroll_up(3);
                        } else {
                            app.clear_selection();
                            forward_wheel(&mut pty, true, m.column, m.row, layout.term_area);
                        }
                    }
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e),
        }

        // The embedded terminal PTY dies when the last tmux session exits. If a
        // new session was just created with no client to switch into, respawn
        // the PTY attached to it so the right pane fills with the new session.
        if let Some(slug) = app.pending_respawn.take() {
            let needs_spawn = pty.as_ref().is_none_or(|p| !p.is_alive());
            if needs_spawn {
                let args = ["tmux", "-S", socket.as_str(), "new-session", "-A", "-s", slug.as_str()];
                if let Ok(p) = Pty::spawn(&args, 24, 80) {
                    parser = Some(p.parser());
                    pty = Some(p);
                    last_term_size = (0, 0); // force a resize so the session fills the pane
                    // A brand-new tmux server lost the global options; re-apply.
                    let _ = app.tmux.set_global_option("detach-on-destroy", "off");
                    let _ = app.tmux.set_global_option("destroy-unattached", "off");
                    let _ = app.tmux.set_global_option("mouse", "on");
                    app.status = format!("reopened terminal for {slug}");
                }
            }
        }
    };

    // Detach the embedded client cleanly instead of letting the PTY close yank
    // it away. The tmux server and all of its sessions keep running; reopening
    // re-attaches and re-lists them.
    if let Ok(Some(tty)) = app.tmux.host_tty() {
        let _ = app.tmux.detach_client(&tty);
    }

    let restore_raw = disable_raw_mode();
    let restore_screen = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
    result.and(restore_raw).and(restore_screen)
}

/// Map an absolute screen cell to a pane-relative `(col, row)`, but only when
/// it lies strictly inside `term`. Used to decide whether a left-press begins a
/// terminal selection.
fn cell_in_pane(col: u16, row: u16, term: ratatui::layout::Rect) -> Option<(u16, u16)> {
    if term.width == 0 || term.height == 0 {
        return None;
    }
    if col < term.x || row < term.y || col >= term.x + term.width || row >= term.y + term.height {
        return None;
    }
    Some((col - term.x, row - term.y))
}

/// Like [`cell_in_pane`] but clamps the pointer into the pane instead of
/// rejecting out-of-bounds positions, so a drag past the pane edge extends the
/// selection to the boundary. Returns `None` only for a zero-area pane.
fn clamp_cell(col: u16, row: u16, term: ratatui::layout::Rect) -> Option<(u16, u16)> {
    if term.width == 0 || term.height == 0 {
        return None;
    }
    let c = col.saturating_sub(term.x).min(term.width - 1);
    let r = row.saturating_sub(term.y).min(term.height - 1);
    Some((c, r))
}

/// Forward a mouse-wheel tick to the embedded terminal, but only when a live
/// client exists and the pointer is over its pane. Screen coordinates are
/// translated to the tmux client's 1-based, pane-local origin so the wheel
/// report lands on the right pane; tmux (`mouse on`) turns it into scrollback
/// scrolling, revealing the old logs.
fn forward_wheel(pty: &mut Option<Pty>, up: bool, col: u16, row: u16, term: ratatui::layout::Rect) {
    let Some(p) = pty else { return };
    if !p.is_alive() {
        return;
    }
    let in_pane = col >= term.x
        && row >= term.y
        && col < term.x + term.width
        && row < term.y + term.height;
    if !in_pane {
        return;
    }
    let _ = p.write_input(&encode_wheel(up, col - term.x + 1, row - term.y + 1));
}
