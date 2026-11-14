//! haunt — a small terminal multiplexer for Ghostty.
//!
//! One process per Ghostty tab. The main shell pane is parsed with
//! libghostty-vt and re-emitted to the outer terminal, preserving Ghostty's
//! native features.

mod app;
mod banner;
mod base64;
mod config;
mod graphics;
mod input;
mod machine;
mod notification;
mod paint;
mod pane;
mod pty;
mod shell;
mod theme;

use std::{
    io::{BufWriter, Write},
    process::ExitCode,
    sync::mpsc::RecvTimeoutError,
    thread,
};

use anyhow::Result;
use crossterm::{
    cursor, event, execute, style,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGUSR1, SIGUSR2};

use app::{App, Msg};

fn main() -> ExitCode {
    let result = run();
    // Also covers an early error return, where the app (and its panes) may
    // have been dropped before `run` reached its own shutdown path.
    pty::flush_shutdowns();
    match result {
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
    // SAFETY: this runs once, before any threads or libghostty calls.
    unsafe {
        libghostty_vt::kitty::graphics::set_png_decoder(Some(Box::new(
            libghostty_vt::kitty::graphics::RustPngDecoder::new(),
        )))?
    };
    let (cfg, config_error) = startup_config(config::Config::load());
    let size = app::query_size()?;
    let theme = theme::Theme::load();

    terminal::enable_raw_mode()?;
    let _guard = Guard;

    // Render frames through a large buffer: the default stdout handle is a
    // 1024-byte LineWriter, which splits a full frame into a write syscall per
    // KiB. A single buffered write keeps the render path to one syscall.
    let mut stdout = BufWriter::with_capacity(64 * 1024, std::io::stdout().lock());
    execute!(
        stdout,
        EnterAlternateScreen,
        event::EnableBracketedPaste,
        event::EnableFocusChange,
        event::EnableMouseCapture,
        cursor::Hide,
        style::ResetColor
    )?;
    execute!(
        stdout,
        event::PushKeyboardEnhancementFlags(
            event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    )?;

    let (tx, rx) = app::channel();
    let mut app = App::new(cfg, size, theme, tx.clone())?;
    if let Some(error) = config_error {
        app.set_error(error);
    }

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
            let Ok(mut signals) =
                signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM, SIGUSR1, SIGUSR2])
            else {
                return;
            };
            for signal in signals.forever() {
                let msg = match signal {
                    SIGUSR1 | SIGUSR2 => Msg::RefreshTheme,
                    _ => Msg::Signal(signal),
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
    }

    // Pace timed work (notification/autoscroll animation) from a fixed
    // deadline rather than sleeping TICK after each iteration, so per-frame
    // handling and render time doesn't stretch the animation interval.
    // Rendering itself stays uncapped: while messages keep arriving,
    // recv_timeout returns immediately and the loop never throttles.
    let mut next_tick = std::time::Instant::now() + app::TICK;
    while app.running() {
        let now = std::time::Instant::now();
        if next_tick <= now {
            // Don't accumulate catch-up sleeps after a busy burst.
            next_tick = now + app::TICK;
        }
        match rx.recv_timeout(next_tick - now) {
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

        app.tick()?;
        app.render(&mut stdout)?;
    }

    app.shutdown();
    Ok(app.exit_code())
}

fn startup_config(result: Result<config::Config>) -> (config::Config, Option<String>) {
    match result {
        Ok(cfg) => (cfg, None),
        Err(err) => (
            config::Config::parse("").expect("default configuration is valid"),
            Some(format!("config load failed: {err:#}")),
        ),
    }
}

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        // Leave synchronized-output mode and restore the default cursor shape
        // even if a frame was interrupted.
        let _ = out.write_all(b"\x1b[?2026l\x1b[0 q");
        let _ = execute!(
            out,
            event::PopKeyboardEnhancementFlags,
            event::DisableFocusChange,
            event::DisableBracketedPaste,
            event::DisableMouseCapture,
            cursor::Show,
            style::ResetColor,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_config_uses_defaults_and_reports_errors() {
        for text in ["shell = [", "reload_bind = 'bad+key'"] {
            let (cfg, error) = startup_config(config::Config::parse(text));
            assert_eq!(cfg.reload_bind.to_string(), "ctrl+shift+r");
            assert_eq!(cfg.separator, ' ');
            assert!(cfg.apps.is_empty());
            assert!(error.unwrap().contains("config load failed:"));
        }
        let (cfg, error) = startup_config(config::Config::parse("separator = '|'"));
        assert_eq!(cfg.separator, '|');
        assert!(error.is_none());
    }
}
