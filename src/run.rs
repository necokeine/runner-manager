use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::Once;
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

use crate::app::{col_to_split_pct, App, ChooserRow, Focus, Popup};
use crate::project::git::GitStatuses;
use crate::term::keys::{encode_key, encode_wheel};
use crate::term::pty::{read_screen, ParserHandle, Pty};
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

/// Spawn the embedded terminal PTY attached to `session` on `socket`
/// (`new-session -A` attaches if the session exists, creates it otherwise).
fn spawn_attached_pty(socket: &str, session: &str) -> io::Result<Pty> {
    Pty::spawn(
        &["tmux", "-S", socket, "new-session", "-A", "-s", session],
        24,
        80,
    )
}

/// Global options every tmux server we talk to must carry. Applied after a
/// client is attached at startup and re-applied after a respawn (a brand-new
/// server starts from the user's config, losing them):
/// - `detach-on-destroy off` / `destroy-unattached off`: keep every session
///   alive across our own lifetime — never detach a client because its session
///   was destroyed, and never destroy a session because it has no attached
///   client (which is exactly what happens to the session we were viewing when
///   we quit). Only `exit` inside a session ends it. `destroy-unattached`
///   defaults to off but a user's tmux config can turn it on, so force it.
/// - `mouse on`: let the embedded client scroll its own scrollback — a
///   forwarded wheel event puts the pane into copy-mode (showing the old logs)
///   unless a full-screen app has grabbed the mouse itself.
fn apply_tmux_options(tmux: &Tmux<SystemRunner>) {
    let _ = tmux.set_global_option("detach-on-destroy", "off");
    let _ = tmux.set_global_option("destroy-unattached", "off");
    let _ = tmux.set_global_option("mouse", "on");
}

/// Put the user's terminal back into a usable state — cooked mode, main
/// screen, mouse reporting off — reporting any failure. This is the single
/// definition of "restore"; every exit path funnels through it.
fn restore_terminal_checked() -> io::Result<()> {
    let raw = disable_raw_mode();
    let screen = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    raw.and(screen)
}

/// Best-effort [`restore_terminal_checked`] for paths that can't report
/// (the panic hook, `Drop`). Idempotent.
fn restore_terminal() {
    let _ = restore_terminal_checked();
}

/// Restores the terminal on drop unless defused. Guards every exit from `run`
/// after raw mode is enabled — including `?`s and early returns added later —
/// so no path can strand the user's shell on the alternate screen. The normal
/// teardown defuses it and calls the error-reporting restore itself.
struct RestoreGuard {
    defused: bool,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if !self.defused {
            restore_terminal();
        }
    }
}

/// Chain a panic hook that restores the terminal before the previous hook
/// prints the panic message. Without this, a panic on the UI thread unwinds
/// past the normal teardown and leaves the shell in raw mode on the alternate
/// screen — with the message drawn there and immediately lost. Only main-thread
/// panics restore (a background-thread panic doesn't end the process, and
/// yanking the alternate screen away from a still-running UI would be worse).
/// Must be installed *before* `pty::install_panic_filter` so the filter's
/// silence check runs first and skips this hook for the vt100 panics that are
/// deliberately caught while the UI keeps running.
fn install_restore_on_panic() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if thread::current().name() == Some("main") {
                restore_terminal();
            }
            prev(info);
        }));
    });
}

/// Set up the terminal and drive the whole TUI until the user quits: draws via
/// `ui::render`, routes key/mouse events by popup and focus, forwards
/// keystrokes to the embedded PTY, and runs the periodic tmux sync and
/// background git scans. Returns when `q` is pressed or on a terminal I/O
/// error, restoring the terminal either way.
pub fn run(root: PathBuf, socket: String) -> io::Result<()> {
    install_restore_on_panic();
    // Silence the panic message for the vt100 parser panics the reader thread
    // deliberately catches (see `pty::install_panic_filter`); otherwise, during
    // a rapid splitter-drag resize, that message bleeds onto the TUI.
    crate::term::pty::install_panic_filter();

    enable_raw_mode()?;
    // From here on, every exit — `?`, early return, or unwind — must restore
    // the terminal; the guard makes that structural instead of per-call-site.
    let mut guard = RestoreGuard { defused: false };
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
        match spawn_attached_pty(&socket, name) {
            Ok(p) => {
                parser = Some(p.parser());
                pty = Some(p);
            }
            Err(e) => return Err(e), // the guard restores the terminal
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
        apply_tmux_options(&app.tmux);
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
    // Clickable chooser regions: (row y, x start, x end inclusive, the row),
    // plus the popup rect so a click inside it that misses every control is a
    // harmless no-op rather than a cancel.
    let mut chooser_rect = ratatui::layout::Rect::default();
    let mut chooser_row_ys: Vec<(u16, u16, u16, ChooserRow)> = Vec::new();
    // Confirm-close buttons: (row y, x start, x end inclusive, true = Yes).
    let mut confirm_buttons: Vec<(u16, u16, u16, bool)> = Vec::new();

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
                    parser.as_ref().map(read_screen)
                } else {
                    None
                };
                let screen = screen_guard.as_ref().map(|g| g.screen());
                captured = Some(ui::render(f, f.area(), &mut app, screen));
                drop(screen_guard);
                match &app.popup {
                    Popup::Help => ui::render_help(f, f.area()),
                    Popup::Chooser {
                        kind, perm, resume, ..
                    } => {
                        let focus_row = app.chooser_focus_row();
                        (chooser_rect, chooser_row_ys) = ui::render_chooser(
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
                        confirm_buttons = ui::render_confirm_close(f, f.area(), slug);
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

        match event::poll(Duration::from_millis(33)) {
            Ok(true) => {}
            Ok(false) => continue,
            // A persistently failing poll would otherwise spin this loop at
            // 100% CPU with no way to quit; treat it like a read error.
            Err(e) => break Err(e),
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
                        match ui::resolve_span(m.column, m.row, &chooser_row_ys) {
                            Some(row) => {
                                let _ = app.chooser_click(*row);
                            }
                            // A miss inside the popup (a group label, a blank
                            // line) keeps the form; only a click outside
                            // cancels it.
                            None if !chooser_rect.contains((m.column, m.row).into()) => {
                                app.chooser_cancel()
                            }
                            None => {}
                        }
                    }
                    Popup::ConfirmClose { .. } => {
                        // Only a click on the "[ Yes ]" text itself confirms
                        // the kill; the No button or anywhere else dismisses.
                        match ui::resolve_span(m.column, m.row, &confirm_buttons) {
                            Some(true) => {
                                let _ = app.confirm_close();
                            }
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
                            match ui::resolve_pane_click(
                                m.column,
                                m.row,
                                layout.split_col,
                                &layout.tree,
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
                    }
                },
                MouseEventKind::Drag(MouseButton::Left) => {
                    if dragging_split {
                        app.split_pct = col_to_split_pct(m.column, area_width);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    // Save the dragged width once the drag ends, not on every
                    // intermediate move.
                    if dragging_split {
                        app.persist_split();
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
                if let Ok(p) = spawn_attached_pty(&socket, &slug) {
                    parser = Some(p.parser());
                    pty = Some(p);
                    last_term_size = (0, 0); // force a resize so the session fills the pane
                                             // A brand-new tmux server lost the global options; re-apply.
                    apply_tmux_options(&app.tmux);
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

    // Normal teardown reports restore failures instead of swallowing them, so
    // defuse the guard and run the checked restore directly.
    guard.defused = true;
    result.and(restore_terminal_checked())
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
    let in_pane =
        col >= term.x && row >= term.y && col < term.x + term.width && row < term.y + term.height;
    if !in_pane {
        return;
    }
    let _ = p.write_input(&encode_wheel(up, col - term.x + 1, row - term.y + 1));
}
