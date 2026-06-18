use std::io;
use std::path::PathBuf;
use std::time::Duration;

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

use crate::app::{App, Focus};
use crate::input::{map_key, Action};
use crate::keys::encode_key;
use crate::pty::Pty;
use crate::tmux::{SystemRunner, Tmux};
use crate::ui::{self, Hit, PaneHit};

pub fn run(root: PathBuf, socket: String, editor: String) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Spawn the embedded terminal: a tmux client attached to (or creating) the
    // scratch session on the runner socket. Initial size is a placeholder; the
    // first resize after the initial draw corrects it.
    let pty_args = ["tmux", "-L", socket.as_str(), "new-session", "-A", "-s", "scratch"];
    let pty = Pty::spawn(&pty_args, 24, 80);
    let mut pty = match pty {
        Ok(p) => p,
        Err(e) => {
            let _ = disable_raw_mode();
            let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture);
            return Err(e);
        }
    };
    let parser = pty.parser();

    let tmux = Tmux::new(socket, SystemRunner);
    let mut app = App::new(root, tmux, editor);
    let _ = app.sync_active();

    let mut last_term_size: (u16, u16) = (0, 0);

    let result = loop {
        let mut captured: Option<ui::Layout> = None;
        let draw_res = terminal.draw(|f| {
            let guard = parser.read().unwrap();
            captured = Some(ui::render(
                f,
                f.area(),
                &app.rows,
                app.selected,
                &app.active,
                app.focus,
                guard.screen(),
            ));
        });
        if let Err(e) = draw_res {
            break Err(e);
        }
        let layout = captured.expect("render returns a Layout");

        // Resize the PTY to match the terminal pane's inner area.
        let term_size = (layout.term_area.height, layout.term_area.width);
        if term_size != last_term_size && term_size.0 > 0 && term_size.1 > 0 {
            let _ = pty.resize(term_size.0, term_size.1);
            last_term_size = term_size;
        }

        if !event::poll(Duration::from_millis(33)).unwrap_or(false) {
            continue; // tick: redraw to reflect new PTY output
        }

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                // Ctrl-q toggles focus regardless of current focus.
                if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.toggle_focus();
                } else {
                    match app.focus {
                        Focus::Tree => match map_key(key) {
                            Action::Quit => break Ok(()),
                            Action::Up => app.up(),
                            Action::Down => app.down(),
                            Action::Activate => {
                                let _ = app.activate();
                            }
                            Action::OpenSession => {
                                let _ = app.open_session();
                            }
                            Action::Kill => {
                                let _ = app.kill_selected();
                            }
                            Action::Noop => {}
                        },
                        Focus::Terminal => {
                            let _ = pty.write_input(&encode_key(key));
                        }
                    }
                }
            }
            Ok(Event::Mouse(m)) => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    match ui::resolve_pane_click(m.column, m.row, layout.split_col, &layout.tree) {
                        PaneHit::Terminal => app.focus = Focus::Terminal,
                        PaneHit::Tree(hit) => {
                            app.focus = Focus::Tree;
                            match hit {
                                Some(Hit::Row(idx)) => {
                                    app.selected = idx;
                                    let _ = app.activate();
                                }
                                Some(Hit::Button(idx)) => {
                                    app.selected = idx;
                                    let _ = app.open_session();
                                }
                                None => {}
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
    let restore_screen = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    result.and(restore_raw).and(restore_screen)
}
