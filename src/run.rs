use std::io;
use std::path::PathBuf;
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
use crate::keys::encode_key;
use crate::pty::Pty;
use crate::tmux::{SystemRunner, Tmux};
use crate::ui::{self, Hit, PaneHit};

pub fn run(root: PathBuf, socket: String, _editor: String) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let pty_args = ["tmux", "-L", socket.as_str(), "new-session", "-A", "-s", "scratch"];
    let mut pty = match Pty::spawn(&pty_args, 24, 80) {
        Ok(p) => p,
        Err(e) => {
            let _ = disable_raw_mode();
            let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
            return Err(e);
        }
    };
    let parser = pty.parser();

    let tmux = Tmux::new(socket, SystemRunner);
    let mut app = App::new(root, tmux);

    for _ in 0..20 {
        if app.host_client_ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = app.tmux.set_global_option("detach-on-destroy", "off");
    let _ = app.sync();

    let mut last_term_size: (u16, u16) = (0, 0);
    let mut last_sync = Instant::now();

    let result = loop {
        let mut captured: Option<ui::Layout> = None;
        let draw_res = terminal.draw(|f| {
            let screen_guard = if app.viewer.is_none() {
                Some(parser.read().unwrap())
            } else {
                None
            };
            let screen = screen_guard.as_ref().map(|g| g.screen());
            captured = Some(ui::render(f, f.area(), &app, screen));
            drop(screen_guard);
            match &app.popup {
                Popup::Help => ui::render_help(f, f.area()),
                Popup::Chooser { selected, .. } => {
                    let _ = ui::render_chooser(f, f.area(), *selected);
                }
                Popup::None => {}
            }
        });
        if let Err(e) = draw_res {
            break Err(e);
        }
        let layout = captured.expect("render returns a Layout");

        if app.viewer.is_none() {
            let term_size = (layout.term_area.height, layout.term_area.width);
            if term_size != last_term_size && term_size.0 > 0 && term_size.1 > 0 {
                let _ = pty.resize(term_size.0, term_size.1);
                last_term_size = term_size;
            }
        }

        if last_sync.elapsed() >= Duration::from_millis(1000) {
            let _ = app.sync();
            last_sync = Instant::now();
        }

        if !event::poll(Duration::from_millis(33)).unwrap_or(false) {
            continue;
        }

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                match app.popup.clone() {
                    Popup::Help => {
                        app.popup = Popup::None;
                    }
                    Popup::Chooser { .. } => match key.code {
                        KeyCode::Esc => app.chooser_cancel(),
                        KeyCode::Enter => {
                            let _ = app.chooser_confirm();
                        }
                        KeyCode::Down | KeyCode::Char('j') => app.chooser_move(1),
                        KeyCode::Up | KeyCode::Char('k') => app.chooser_move(-1),
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
                                    KeyCode::Char('a') => app.open_chooser(),
                                    KeyCode::Char('j') | KeyCode::Down => app.down(),
                                    KeyCode::Char('k') | KeyCode::Up => app.up(),
                                    KeyCode::Enter => {
                                        let _ = app.activate();
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
                                    } else {
                                        let _ = pty.write_input(&encode_key(key));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Mouse(m)) => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    match app.popup.clone() {
                        Popup::Help => app.popup = Popup::None,
                        Popup::Chooser { .. } => app.chooser_cancel(),
                        Popup::None => {
                            match ui::resolve_pane_click(m.column, m.row, layout.split_col, &layout.tree) {
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
                                        None => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };

    let restore_raw = disable_raw_mode();
    let restore_screen = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
    result.and(restore_raw).and(restore_screen)
}
