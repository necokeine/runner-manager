use std::io;
use std::path::PathBuf;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;
use crate::input::{map_key, Action};
use crate::tmux::{SystemRunner, Tmux};
use crate::ui::{self, Hit, ListLayout};

pub fn run(root: PathBuf, socket: String, editor: String) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tmux = Tmux::new(socket, SystemRunner);
    let mut app = App::new(root, tmux, editor);
    let _ = app.sync_active();

    let mut layout = ListLayout {
        origin_y: 0,
        button_col_start: 0,
        button_col_end: 0,
        row_count: 0,
    };

    let result = loop {
        if let Err(e) = terminal.draw(|f| {
            layout = ui::render(f, f.area(), &app.rows, app.selected, &app.active);
        }) {
            break Err(e);
        }

        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match map_key(key) {
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
            Ok(Event::Mouse(m)) => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    if let Some(hit) = ui::resolve_click(m.column, m.row, &layout) {
                        match hit {
                            Hit::Row(idx) => {
                                app.selected = idx;
                                let _ = app.activate();
                            }
                            Hit::Button(idx) => {
                                app.selected = idx;
                                let _ = app.open_session();
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
