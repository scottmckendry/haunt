//! Workspace state for the main pane and the outer-terminal event loop.

use std::{
    io::{ErrorKind, Read, Write},
    sync::mpsc::{SyncSender, sync_channel},
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{Event, KeyEvent},
    terminal,
};
use libghostty_vt::key;

use crate::{
    input,
    paint::Area,
    pane::{Pane, PaneDims},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
}

pub enum Msg {
    Output(Vec<u8>),
    Eof,
    Input(Event),
    Signal(i32),
}

pub struct App {
    pane: Pane,
    size: Size,
    encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    frame_dirty: bool,
    running: bool,
    exit_code: i32,
}

impl App {
    pub fn new(shell: &[String], size: Size, tx: SyncSender<Msg>) -> Result<Self> {
        let (pane, reader) = Pane::spawn(shell, None, dims_of(size))?;
        spawn_reader(reader, tx);

        Ok(Self {
            pane,
            size,
            encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            frame_dirty: true,
            running: true,
            exit_code: 0,
        })
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub fn handle(&mut self, msg: Msg) -> Result<()> {
        match msg {
            Msg::Output(bytes) => {
                self.pane.feed(&bytes)?;
                self.frame_dirty = true;
            }
            Msg::Eof => {
                self.exit_code = self.pane.wait()?;
                self.running = false;
            }
            Msg::Input(Event::Key(ev)) => self.on_key(ev)?,
            Msg::Input(Event::Resize(_, _)) => {
                let size = query_size()?;
                self.resize(size)?;
            }
            Msg::Input(_) => {}
            Msg::Signal(signal) => {
                self.exit_code = 128 + signal;
                self.running = false;
            }
        }
        Ok(())
    }

    pub fn resize(&mut self, size: Size) -> Result<()> {
        self.size = size;
        self.pane.resize(dims_of(size))?;
        self.frame_dirty = true;
        Ok(())
    }

    pub fn render(&mut self, out: &mut impl Write) -> Result<()> {
        if !self.frame_dirty && !self.pane.dirty {
            return Ok(());
        }

        let mut frame = Vec::with_capacity(16 * 1024);
        frame.extend_from_slice(b"\x1b[?2026h");
        self.pane.paint(&mut frame, full_area(self.size))?;
        frame.extend_from_slice(b"\x1b[?2026l");

        out.write_all(&frame)?;
        out.flush()?;
        self.frame_dirty = false;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        self.pane.shutdown();
    }

    fn on_key(&mut self, ev: KeyEvent) -> Result<()> {
        let mut buf = Vec::with_capacity(64);
        input::encode(
            self.pane.term(),
            ev,
            &mut self.encoder,
            &mut self.key_event,
            &mut buf,
        )?;
        self.pane.send(&buf)
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, tx: SyncSender<Msg>) {
    thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Msg::Output(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(Msg::Eof);
    });
}

fn full_area(size: Size) -> Area {
    Area {
        x: 0,
        y: 0,
        w: size.cols,
        h: size.rows,
    }
}

fn dims_of(size: Size) -> PaneDims {
    PaneDims {
        cols: size.cols,
        rows: size.rows,
        cell_w: size.cell_w,
        cell_h: size.cell_h,
    }
}

pub fn query_size() -> Result<Size> {
    if let Ok(ws) = terminal::window_size()
        && ws.columns > 0
        && ws.rows > 0
    {
        let cell_w = (ws.width / ws.columns).max(1);
        let cell_h = (ws.height / ws.rows).max(1);
        return Ok(Size {
            cols: ws.columns,
            rows: ws.rows,
            cell_w,
            cell_h,
        });
    }

    let (cols, rows) = terminal::size()?;
    Ok(Size {
        cols: cols.max(1),
        rows: rows.max(1),
        cell_w: 8,
        cell_h: 16,
    })
}

/// Create the event channel with bounded capacity. A slow main loop applies
/// backpressure to the PTY reader rather than growing an unbounded queue.
pub fn channel() -> (SyncSender<Msg>, std::sync::mpsc::Receiver<Msg>) {
    sync_channel(256)
}

/// Sleep for one event-loop tick.
pub const TICK: Duration = Duration::from_millis(16);
