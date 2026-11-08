//! Workspace state: main pane, one visible auxiliary pane, focus, layout, and
//! direct chord routing.

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
    config::{AppSize, Config},
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
    Output(u64, Vec<u8>),
    PaneEof(u64),
    Input(Event),
    Signal(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Main,
    Aux,
}

pub struct App {
    cfg: Config,
    main: Pane,
    aux: Vec<Option<Pane>>,
    visible: Option<usize>,
    focus: Focus,
    size: Size,
    tx: SyncSender<Msg>,
    next_id: u64,
    encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    frame_dirty: bool,
    running: bool,
    exit_code: i32,
    error: Option<String>,
}

impl App {
    pub fn new(cfg: Config, size: Size, tx: SyncSender<Msg>) -> Result<Self> {
        let (main, reader) = Pane::spawn(0, &cfg.shell, None, dims_of(size, full_area(size)))?;
        spawn_reader(0, reader, tx.clone());
        let aux = (0..cfg.apps.len()).map(|_| None).collect();

        Ok(Self {
            cfg,
            main,
            aux,
            visible: None,
            focus: Focus::Main,
            size,
            tx,
            next_id: 1,
            encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            frame_dirty: true,
            running: true,
            exit_code: 0,
            error: None,
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
            Msg::Output(id, bytes) => {
                if let Some(pane) = self.pane_mut(id) {
                    pane.feed(&bytes)?;
                    if self.is_painted(id) {
                        self.frame_dirty = true;
                    }
                }
            }
            Msg::PaneEof(id) => self.on_eof(id)?,
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
        self.relayout()
    }

    pub fn render(&mut self, out: &mut impl Write) -> Result<()> {
        if !self.needs_frame() {
            return Ok(());
        }

        let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
        let (main_area, aux_area) = layout(self.size, visible_size);
        let full_aux = visible_size == Some(AppSize::Full);

        let mut frame = Vec::with_capacity(32 * 1024);
        frame.extend_from_slice(b"\x1b[?2026h");

        match (self.visible, self.focus, full_aux) {
            (Some(v), _, true) => {
                if let Some(pane) = self.aux[v].as_mut() {
                    pane.paint(&mut frame, aux_area.expect("full aux has an area"), true)?;
                } else {
                    self.main.paint(&mut frame, main_area, true)?;
                }
            }
            (Some(v), Focus::Main, false) => {
                if let Some(pane) = self.aux[v].as_mut() {
                    pane.paint(
                        &mut frame,
                        aux_area.expect("visible aux has an area"),
                        false,
                    )?;
                }
                self.main.paint(&mut frame, main_area, true)?;
            }
            (Some(v), Focus::Aux, false) => {
                self.main.paint(&mut frame, main_area, false)?;
                if let Some(pane) = self.aux[v].as_mut() {
                    pane.paint(&mut frame, aux_area.expect("visible aux has an area"), true)?;
                }
            }
            (None, _, _) => self.main.paint(&mut frame, main_area, true)?,
        }

        if let Some(error) = &self.error {
            paint_error(&mut frame, full_area(self.size), error);
        }

        frame.extend_from_slice(b"\x1b[?2026l");
        out.write_all(&frame)?;
        out.flush()?;
        self.frame_dirty = false;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        self.main.shutdown();
        for pane in self.aux.iter_mut().flatten() {
            pane.shutdown();
        }
    }

    fn on_key(&mut self, ev: KeyEvent) -> Result<()> {
        self.clear_error();

        if let Some(idx) = self.cfg.apps.iter().position(|app| app.chord.matches(&ev)) {
            return self.toggle(idx);
        }

        let mut buf = Vec::with_capacity(64);
        if self.focus == Focus::Aux
            && let Some(v) = self.visible
            && let Some(pane) = self.aux[v].as_ref()
        {
            input::encode(
                pane.term(),
                ev,
                &mut self.encoder,
                &mut self.key_event,
                &mut buf,
            )?;
        } else {
            input::encode(
                self.main.term(),
                ev,
                &mut self.encoder,
                &mut self.key_event,
                &mut buf,
            )?;
        }

        self.send_to_focus(&buf)
    }

    fn send_to_focus(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.focus == Focus::Aux
            && let Some(v) = self.visible
            && let Some(pane) = self.aux[v].as_mut()
        {
            return pane.send(bytes);
        }
        self.main.send(bytes)
    }

    fn on_eof(&mut self, id: u64) -> Result<()> {
        if self.main.id == id {
            self.exit_code = self.main.wait()?;
            self.running = false;
            return Ok(());
        }

        let Some(idx) = self
            .aux
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|pane| pane.id == id))
        else {
            return Ok(());
        };

        if let Some(mut pane) = self.aux[idx].take() {
            let _ = pane.wait();
        }

        if self.visible == Some(idx) {
            self.visible = None;
            self.focus = Focus::Main;
            self.relayout()?;
        }
        Ok(())
    }

    fn is_painted(&self, id: u64) -> bool {
        id == self.main.id
            || self
                .visible
                .and_then(|idx| self.aux[idx].as_ref())
                .is_some_and(|pane| pane.id == id)
    }

    fn pane_mut(&mut self, id: u64) -> Option<&mut Pane> {
        if self.main.id == id {
            return Some(&mut self.main);
        }
        self.aux.iter_mut().flatten().find(|pane| pane.id == id)
    }

    fn toggle(&mut self, idx: usize) -> Result<()> {
        if self.visible == Some(idx) {
            self.visible = None;
            self.focus = Focus::Main;
            self.relayout()
        } else {
            self.show(idx)
        }
    }

    fn show(&mut self, idx: usize) -> Result<()> {
        if self.aux[idx].is_none() {
            let dims = dims_of(self.size, hidden_area(self.size, self.cfg.apps[idx].size));
            let cmd = self.cfg.apps[idx].cmd.clone();
            let name = self.cfg.apps[idx].name.clone();
            // Aux apps start where the main pane's foreground process is, not
            // where haunt was launched.
            let cwd = self.main.cwd();
            let id = self.next_id;

            match Pane::spawn(id, &cmd, cwd.as_deref(), dims) {
                Ok((pane, reader)) => {
                    self.next_id += 1;
                    self.aux[idx] = Some(pane);
                    spawn_reader(id, reader, self.tx.clone());
                }
                Err(err) => {
                    self.set_error(format!("failed to start {name}: {err:#}"));
                    return Ok(());
                }
            }
        }

        self.visible = Some(idx);
        self.focus = Focus::Aux;
        self.error = None;
        self.relayout()
    }

    fn relayout(&mut self) -> Result<()> {
        let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
        let (main_area, aux_area) = layout(self.size, visible_size);

        self.main.resize(dims_of(self.size, main_area))?;
        self.main.force_repaint();

        for idx in 0..self.aux.len() {
            let area = if Some(idx) == self.visible {
                aux_area.expect("visible aux has an area")
            } else {
                hidden_area(self.size, self.cfg.apps[idx].size)
            };
            if let Some(pane) = self.aux[idx].as_mut() {
                pane.resize(dims_of(self.size, area))?;
                pane.force_repaint();
            }
        }

        self.frame_dirty = true;
        Ok(())
    }

    fn needs_frame(&self) -> bool {
        self.frame_dirty
            || self.main.dirty
            || self
                .visible
                .and_then(|idx| self.aux[idx].as_ref())
                .is_some_and(|pane| pane.dirty)
    }

    fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.frame_dirty = true;
    }

    fn clear_error(&mut self) {
        if self.error.take().is_none() {
            return;
        }
        self.main.force_repaint();
        if let Some(idx) = self.visible
            && let Some(pane) = self.aux[idx].as_mut()
        {
            pane.force_repaint();
        }
        self.frame_dirty = true;
    }
}

fn spawn_reader(id: u64, mut reader: Box<dyn Read + Send>, tx: SyncSender<Msg>) {
    thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Msg::Output(id, buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(Msg::PaneEof(id));
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

fn layout(size: Size, visible: Option<AppSize>) -> (Area, Option<Area>) {
    let full = full_area(size);
    match visible {
        Some(AppSize::Half) => {
            let main_w = size.cols / 2;
            let aux_w = size.cols - main_w;
            (
                Area {
                    x: 0,
                    y: 0,
                    w: main_w,
                    h: size.rows,
                },
                Some(Area {
                    x: main_w,
                    y: 0,
                    w: aux_w,
                    h: size.rows,
                }),
            )
        }
        Some(AppSize::Full) => (full, Some(full)),
        None => (full, None),
    }
}

fn hidden_area(size: Size, app_size: AppSize) -> Area {
    match app_size {
        AppSize::Half => {
            let main_w = size.cols / 2;
            Area {
                x: main_w,
                y: 0,
                w: size.cols - main_w,
                h: size.rows,
            }
        }
        AppSize::Full => full_area(size),
    }
}

fn dims_of(size: Size, area: Area) -> PaneDims {
    PaneDims {
        cols: area.w,
        rows: area.h,
        cell_w: size.cell_w,
        cell_h: size.cell_h,
    }
}

fn paint_error(out: &mut Vec<u8>, area: Area, message: &str) {
    if area.w == 0 || area.h == 0 {
        return;
    }

    let width = area.w as usize;
    let mut text: String = message.chars().take(width).collect();
    let len = text.chars().count();
    text.extend(std::iter::repeat_n(' ', width.saturating_sub(len)));

    out.extend_from_slice(b"\x1b[?25l");
    out.extend_from_slice(
        format!("\x1b[{};{}H\x1b[0m\x1b[7m", area.y + area.h, area.x + 1).as_bytes(),
    );
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[0m");
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
/// backpressure to the PTY readers rather than growing an unbounded queue.
pub fn channel() -> (SyncSender<Msg>, std::sync::mpsc::Receiver<Msg>) {
    sync_channel(256)
}

/// Sleep for one event-loop tick.
pub const TICK: Duration = Duration::from_millis(16);

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> Size {
        Size {
            cols,
            rows,
            cell_w: 8,
            cell_h: 16,
        }
    }

    #[test]
    fn hidden_main_uses_the_full_tab() {
        let (main, aux) = layout(size(80, 24), None);
        assert_eq!(main, full_area(size(80, 24)));
        assert_eq!(aux, None);
    }

    #[test]
    fn half_aux_splits_the_tab_vertically() {
        let (main, aux) = layout(size(80, 24), Some(AppSize::Half));
        assert_eq!(
            main,
            Area {
                x: 0,
                y: 0,
                w: 40,
                h: 24
            }
        );
        assert_eq!(
            aux,
            Some(Area {
                x: 40,
                y: 0,
                w: 40,
                h: 24
            })
        );
    }

    #[test]
    fn full_aux_covers_the_main_pane() {
        let (main, aux) = layout(size(80, 24), Some(AppSize::Full));
        assert_eq!(main, full_area(size(80, 24)));
        assert_eq!(aux, Some(full_area(size(80, 24))));
    }

    #[test]
    fn hidden_panes_keep_their_visible_size() {
        assert_eq!(
            hidden_area(size(80, 24), AppSize::Half),
            Area {
                x: 40,
                y: 0,
                w: 40,
                h: 24
            }
        );
        assert_eq!(
            hidden_area(size(80, 24), AppSize::Full),
            full_area(size(80, 24))
        );
    }

    #[test]
    fn odd_widths_give_the_extra_column_to_the_aux() {
        let (main, aux) = layout(size(81, 24), Some(AppSize::Half));
        assert_eq!(main.w, 40);
        assert_eq!(aux.unwrap().w, 41);
    }
}
