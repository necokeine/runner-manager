use std::io::{self, Read, Write};
use std::sync::{Arc, RwLock};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

// tui_term re-exports vt100, so Screen types unify with the renderer (Task 5).
use tui_term::vt100;

pub struct Pty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    parser: Arc<RwLock<vt100::Parser>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pty {
    pub fn spawn(args: &[&str], rows: u16, cols: u16) -> io::Result<Self> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(size)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let mut cmd = CommandBuilder::new(args[0]);
        cmd.args(&args[1..]);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::other(e.to_string()))?;

        let parser = Arc::new(RwLock::new(vt100::Parser::new(rows, cols, 0)));
        let reader_parser = Arc::clone(&parser);
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = reader_parser.write() {
                            p.process(&buf[..n]);
                        }
                    }
                }
            }
        });

        Ok(Pty {
            master: pair.master,
            writer,
            parser,
            _child: child,
        })
    }

    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
        if let Ok(mut p) = self.parser.write() {
            p.set_size(rows, cols);
        }
        Ok(())
    }

    pub fn parser(&self) -> Arc<RwLock<vt100::Parser>> {
        Arc::clone(&self.parser)
    }
}
