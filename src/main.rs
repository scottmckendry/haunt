//! haunt — a small terminal multiplexer for Ghostty.
//!
//! One process per Ghostty tab. The main shell pane is parsed with
//! libghostty-vt and re-emitted to the outer terminal, preserving Ghostty's
//! native features.

mod app;
mod input;
mod paint;
mod pane;
mod pty;
mod shell;

use std::{io::Write, process::ExitCode, sync::mpsc::RecvTimeoutError, thread};

use anyhow::Result;
use crossterm::{
    cursor, event, execute, style,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use app::{App, Msg};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from((code & 0xff) as u8),
        Err(err) => {
            // The terminal guard has already restored the outer terminal, so
            // this is visible instead of being lost under the alternate screen.
            eprintln!("haunt: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<i32> {
    let shell = shell::default_shell();
    let size = app::query_size()?;

    terminal::enable_raw_mode()?;
    let _guard = Guard;

    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        cursor::Hide,
        style::ResetColor
    )?;

    let (tx, rx) = app::channel();
    let mut app = App::new(&shell, size, tx.clone())?;

    {
        // Outer terminal input -> the main pane.
        let tx = tx.clone();
        thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(Msg::Input(ev)).is_err() {
                    break;
                }
            }
        });
    }

    {
        // Signals that should still restore the outer terminal.
        let tx = tx.clone();
        thread::spawn(move || {
            let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])
            else {
                return;
            };
            for signal in signals.forever() {
                if tx.send(Msg::Signal(signal)).is_err() {
                    break;
                }
            }
        });
    }

    while app.running() {
        match rx.recv_timeout(app::TICK) {
            Ok(msg) => app.handle(msg)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(msg) = rx.try_recv() {
            app.handle(msg)?;
            if !app.running() {
                break;
            }
        }

        app.render(&mut stdout)?;
    }

    app.shutdown();
    Ok(app.exit_code())
}

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        // Leave synchronized-output mode and restore the default cursor shape
        // even if a frame was interrupted.
        let _ = out.write_all(b"\x1b[?2026l\x1b[0 q");
        let _ = execute!(out, cursor::Show, style::ResetColor, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
