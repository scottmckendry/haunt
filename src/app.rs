//! Workspace state: main pane, one visible auxiliary pane, focus, layout, and
//! direct chord routing.

use std::{
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{SyncSender, sync_channel},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{Event, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    terminal,
};
use libghostty_vt::{key, terminal::Mode};

use crate::{
    base64,
    config::{AppSize, Config},
    graphics::Graphics,
    input, osc,
    paint::Area,
    pane::{Pane, PaneDims, SelectKind},
    theme::Theme,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
}

/// Match new entries to old panes by name. Never discard a running process.
fn reload_mapping(old: &Config, new: &Config, running: &[bool]) -> Result<Vec<Option<usize>>> {
    for (idx, app) in old.apps.iter().enumerate() {
        if running[idx] && !new.apps.iter().any(|next| next.name == app.name) {
            anyhow::bail!(
                "app {:?} is still running; exit it before removing or renaming it",
                app.name
            );
        }
    }
    Ok(new
        .apps
        .iter()
        .map(|app| old.apps.iter().position(|old| old.name == app.name))
        .collect())
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

struct ClickState {
    time: Instant,
    x: u16,
    y: u16,
    count: u8,
}

pub struct App {
    cfg: Config,
    theme: Option<Theme>,
    graphics: Graphics,
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
    outer_focus: bool,
    selecting: Option<u64>,
    click: Option<ClickState>,
    last_autoscroll: Instant,
    pending_writes: Vec<u8>,
    last_title: Option<String>,
    last_cwd: Option<PathBuf>,
    last_progress: Option<osc::Progress>,
    hostname: String,
}

impl App {
    pub fn new(cfg: Config, size: Size, theme: Option<Theme>, tx: SyncSender<Msg>) -> Result<Self> {
        // Install the PNG decoder before the first pane can receive graphics.
        let graphics = Graphics::new()?;
        let (main, reader) =
            Pane::spawn(0, &cfg.shell, None, dims_of(size, full_area(size)), theme)?;
        spawn_reader(0, reader, tx.clone());
        let aux = (0..cfg.apps.len()).map(|_| None).collect();

        Ok(Self {
            cfg,
            theme,
            graphics,
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
            outer_focus: true,
            selecting: None,
            click: None,
            last_autoscroll: Instant::now(),
            pending_writes: Vec::new(),
            last_title: None,
            last_cwd: None,
            last_progress: None,
            hostname: hostname(),
        })
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub fn handle(&mut self, msg: Msg) -> Result<()> {
        let mut progress_changed = false;
        match msg {
            Msg::Output(id, bytes) => {
                let focused = self.is_focused(id);
                let outer_focus = self.outer_focus;
                let painted = self.is_painted(id);
                let mut effects = Vec::new();
                if let Some(pane) = self.pane_mut(id) {
                    pane.feed(&bytes)?;
                    progress_changed = pane.take_progress_changed();
                    effects = pane.take_effects();
                    if pane.take_focus_events_pending() && focused && outer_focus {
                        pane.send(b"\x1b[I")?;
                    }
                    if painted {
                        self.frame_dirty = true;
                    }
                }
                self.pending_writes.extend_from_slice(&effects);
            }
            Msg::PaneEof(id) => self.on_eof(id)?,
            Msg::Input(Event::Key(ev)) => self.on_key(ev)?,
            Msg::Input(Event::Mouse(ev)) => self.on_mouse(ev)?,
            Msg::Input(Event::Paste(text)) => self.on_paste(text)?,
            Msg::Input(Event::FocusGained) => self.on_outer_focus(true)?,
            Msg::Input(Event::FocusLost) => self.on_outer_focus(false)?,
            Msg::Input(Event::Resize(_, _)) => {
                let size = query_size()?;
                self.resize(size)?;
            }
            Msg::Signal(signal) => {
                self.exit_code = 128 + signal;
                self.running = false;
            }
        }
        self.sync_progress(progress_changed)?;
        self.sync_title();
        self.sync_cwd();
        Ok(())
    }

    pub fn resize(&mut self, size: Size) -> Result<()> {
        self.size = size;
        self.relayout()
    }

    pub fn render(&mut self, out: &mut impl Write) -> Result<()> {
        if !self.pending_writes.is_empty() {
            out.write_all(&self.pending_writes)?;
            out.flush()?;
            self.pending_writes.clear();
        }
        if !self.needs_frame() {
            return Ok(());
        }

        let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
        let (main_area, aux_area) = layout(self.size, visible_size);
        let full_aux = visible_size == Some(AppSize::Full);

        let mut frame = Vec::with_capacity(32 * 1024);
        frame.extend_from_slice(b"\x1b[?2026h");

        let visible: Vec<u64> = match (self.visible, full_aux) {
            (Some(v), true) => match self.aux[v].as_ref() {
                Some(pane) => vec![pane.id],
                None => vec![self.main.id],
            },
            (Some(v), false) => {
                let mut ids = vec![self.main.id];
                if let Some(pane) = self.aux[v].as_ref() {
                    ids.push(pane.id);
                }
                ids
            }
            (None, _) => vec![self.main.id],
        };
        self.graphics.begin_frame(&visible, &mut frame);

        if visible_size == Some(AppSize::Half) && self.size.cols >= 3 {
            paint_separator(&mut frame, main_area.w, self.size.rows, self.cfg.separator);
        }

        match (self.visible, self.focus, full_aux) {
            (Some(v), _, true) => {
                if let Some(pane) = self.aux[v].as_mut() {
                    pane.paint(
                        &mut frame,
                        aux_area.expect("full aux has an area"),
                        true,
                        &mut self.graphics,
                    )?;
                } else {
                    self.main
                        .paint(&mut frame, main_area, true, &mut self.graphics)?;
                }
            }
            (Some(v), Focus::Main, false) => {
                if let Some(pane) = self.aux[v].as_mut() {
                    pane.paint(
                        &mut frame,
                        aux_area.expect("visible aux has an area"),
                        false,
                        &mut self.graphics,
                    )?;
                }
                self.main
                    .paint(&mut frame, main_area, true, &mut self.graphics)?;
            }
            (Some(v), Focus::Aux, false) => {
                self.main
                    .paint(&mut frame, main_area, false, &mut self.graphics)?;
                if let Some(pane) = self.aux[v].as_mut() {
                    pane.paint(
                        &mut frame,
                        aux_area.expect("visible aux has an area"),
                        true,
                        &mut self.graphics,
                    )?;
                }
            }
            (None, _, _) => self
                .main
                .paint(&mut frame, main_area, true, &mut self.graphics)?,
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

        // Typing clears a local selection, as in Ghostty.
        if ev.kind != KeyEventKind::Release {
            self.clear_selections()?;
        }

        if ev.kind != KeyEventKind::Release {
            self.focused_mut().scroll_to_live();
        }

        if self.cfg.reload_bind.matches(&ev) {
            return self.reload_config();
        }

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

    fn reload_config(&mut self) -> Result<()> {
        let cfg = match Config::reload() {
            Ok(cfg) => cfg,
            Err(err) => {
                self.set_error(format!("reload failed: {err:#}"));
                return Ok(());
            }
        };
        let running: Vec<bool> = self.aux.iter().map(Option::is_some).collect();
        let mapping = match reload_mapping(&self.cfg, &cfg, &running) {
            Ok(mapping) => mapping,
            Err(err) => {
                self.set_error(format!("reload failed: {err:#}"));
                return Ok(());
            }
        };
        let visible = self
            .visible
            .and_then(|old| mapping.iter().position(|&i| i == Some(old)));
        let mut old_aux = std::mem::take(&mut self.aux);
        self.aux = mapping
            .iter()
            .map(|idx| idx.and_then(|i| old_aux[i].take()))
            .collect();
        self.visible = visible;
        self.cfg = cfg;
        self.main.force_repaint();
        for pane in self.aux.iter_mut().flatten() {
            pane.force_repaint();
        }
        self.relayout()
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

        let old_id = self.focused_id();
        if let Some(mut pane) = self.aux[idx].take() {
            let _ = pane.wait();
        }

        self.graphics.remove_pane(id);
        if self.visible == Some(idx) {
            self.visible = None;
            self.focus = Focus::Main;
            self.focus_changed(old_id)?;
            self.relayout()?;
        } else {
            self.frame_dirty = true;
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
            let old_id = self.focused_id();
            self.visible = None;
            self.focus = Focus::Main;
            self.focus_changed(old_id)?;
            self.relayout()
        } else {
            self.show(idx)
        }
    }

    fn show(&mut self, idx: usize) -> Result<()> {
        let old_id = self.focused_id();
        if self.aux[idx].is_none() {
            let dims = dims_of(self.size, hidden_area(self.size, self.cfg.apps[idx].size));
            let cmd = self.cfg.apps[idx].cmd.clone();
            let name = self.cfg.apps[idx].name.clone();
            // Aux apps start where the main pane's foreground process is, not
            // where haunt was launched.
            let cwd = self.main.cwd();
            let id = self.next_id;

            match Pane::spawn(id, &cmd, cwd.as_deref(), dims, self.theme) {
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
        self.focus_changed(old_id)?;
        self.relayout()
    }

    fn focused_id(&self) -> u64 {
        if self.focus == Focus::Aux
            && let Some(pane) = self.visible.and_then(|idx| self.aux[idx].as_ref())
        {
            return pane.id;
        }
        self.main.id
    }

    fn is_focused(&self, id: u64) -> bool {
        self.focused_id() == id
    }

    fn focused(&self) -> &Pane {
        if self.focus == Focus::Aux
            && let Some(pane) = self.visible.and_then(|idx| self.aux[idx].as_ref())
        {
            return pane;
        }
        &self.main
    }

    fn focus_changed(&mut self, old_id: u64) -> Result<()> {
        self.frame_dirty = true;
        let new_id = self.focused_id();
        if old_id != new_id && self.outer_focus {
            self.send_focus_event(old_id, false)?;
            self.send_focus_event(new_id, true)?;
        }
        Ok(())
    }

    fn on_outer_focus(&mut self, focused: bool) -> Result<()> {
        if self.outer_focus == focused {
            return Ok(());
        }
        self.outer_focus = focused;
        let id = self.focused_id();
        self.send_focus_event(id, focused)
    }

    fn send_focus_event(&mut self, id: u64, focused: bool) -> Result<()> {
        if let Some(pane) = self.pane_mut(id)
            && pane.mode(Mode::FOCUS_EVENT)
        {
            pane.send(if focused { b"\x1b[I" } else { b"\x1b[O" })?;
        }
        Ok(())
    }

    fn on_mouse(&mut self, ev: MouseEvent) -> Result<()> {
        // A drag or release belongs to the pane where the selection started,
        // even when the pointer has left that pane.
        if matches!(
            ev.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        ) && let Some(id) = self.selecting
        {
            let Some(area) = self.pane_area(id) else {
                self.selecting = None;
                return Ok(());
            };
            let (x, y) = local_cell(area, ev.column, ev.row);
            let finished = if let Some(pane) = self.pane_mut(id) {
                match ev.kind {
                    MouseEventKind::Drag(MouseButton::Left) => {
                        pane.selection_drag(x, y)?;
                        None
                    }
                    MouseEventKind::Up(MouseButton::Left) => pane.selection_finish()?,
                    _ => None,
                }
            } else {
                None
            };
            if matches!(ev.kind, MouseEventKind::Up(MouseButton::Left)) {
                self.selecting = None;
                if let Some(text) = finished {
                    self.copy_selection(&text);
                }
            }
            return Ok(());
        }

        let Some((area, id)) = self.pane_at(ev.column, ev.row) else {
            return Ok(());
        };

        // A click focuses the pane under the pointer, whichever pane it is.
        if matches!(ev.kind, MouseEventKind::Down(_)) {
            self.focus_pane(id)?;
            self.clear_selections()?;
        }

        // Shift overrides app mouse capture, as in Ghostty.
        let tracks = self.pane_mut(id).is_some_and(|pane| pane.tracks_mouse());
        let local = ev.modifiers.contains(KeyModifiers::SHIFT) || !tracks;

        if local && let MouseEventKind::Down(MouseButton::Left) = ev.kind {
            let count = self.next_click_count(ev.column, ev.row);
            let kind = match count {
                1 => SelectKind::Cell,
                2 => SelectKind::Word,
                _ => SelectKind::Line,
            };
            let (x, y) = local_cell(area, ev.column, ev.row);
            if let Some(pane) = self.pane_mut(id) {
                pane.selection_start(x, y, kind)?;
            }
            self.selecting = Some(id);
            return Ok(());
        }

        let Some(pane) = self.pane_mut(id) else {
            return Ok(());
        };

        if !local && pane.tracks_mouse() {
            return pane.send_mouse(ev, area);
        }

        match ev.kind {
            MouseEventKind::ScrollUp => pane.scroll(-3),
            MouseEventKind::ScrollDown => pane.scroll(3),
            _ => Ok(()),
        }
    }

    fn next_click_count(&mut self, x: u16, y: u16) -> u8 {
        let now = Instant::now();
        let count = match &self.click {
            Some(click)
                if now.duration_since(click.time) <= Duration::from_millis(400)
                    && click.x.abs_diff(x) <= 2
                    && click.y.abs_diff(y) <= 2 =>
            {
                (click.count % 3) + 1
            }
            _ => 1,
        };
        self.click = Some(ClickState {
            time: now,
            x,
            y,
            count,
        });
        count
    }

    fn clear_selections(&mut self) -> Result<()> {
        self.selecting = None;
        self.main.clear_selection()?;
        for pane in self.aux.iter_mut().flatten() {
            pane.clear_selection()?;
        }
        Ok(())
    }

    fn copy_selection(&mut self, text: &[u8]) {
        self.pending_writes.extend_from_slice(b"\x1b]52;c;");
        base64::encode_into(text, &mut self.pending_writes);
        self.pending_writes.push(0x07);
    }

    /// Timed work for selection autoscroll; called once per event-loop tick.
    pub fn tick(&mut self) -> Result<()> {
        let Some(id) = self.selecting else {
            return Ok(());
        };
        let now = Instant::now();
        if now.duration_since(self.last_autoscroll) < Duration::from_millis(40) {
            return Ok(());
        }
        let moved = self
            .pane_mut(id)
            .map(Pane::selection_autoscroll)
            .transpose()?
            .unwrap_or(false);
        if moved {
            self.last_autoscroll = now;
        }
        Ok(())
    }

    fn focus_pane(&mut self, id: u64) -> Result<()> {
        let target = if id == self.main.id {
            Focus::Main
        } else if self
            .visible
            .is_some_and(|idx| self.aux[idx].as_ref().is_some_and(|pane| pane.id == id))
        {
            Focus::Aux
        } else {
            return Ok(());
        };

        if self.focus == target {
            return Ok(());
        }
        let old_id = self.focused_id();
        self.focus = target;
        self.focus_changed(old_id)
    }

    fn pane_area(&self, id: u64) -> Option<Area> {
        let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
        let (main_area, aux_area) = layout(self.size, visible_size);
        if self.main.id == id {
            return Some(main_area);
        }
        let visible = self.visible?;
        self.aux[visible]
            .as_ref()
            .is_some_and(|pane| pane.id == id)
            .then_some(aux_area)
            .flatten()
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<(Area, u64)> {
        let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
        let (main_area, aux_area) = layout(self.size, visible_size);

        if let (Some(idx), Some(area)) = (self.visible, aux_area)
            && contains(area, x, y)
        {
            return self.aux[idx].as_ref().map(|pane| (area, pane.id));
        }

        contains(main_area, x, y).then_some((main_area, self.main.id))
    }

    fn focused_mut(&mut self) -> &mut Pane {
        if self.focus == Focus::Aux
            && let Some(pane) = self.visible.and_then(|idx| self.aux[idx].as_mut())
        {
            return pane;
        }
        &mut self.main
    }

    fn on_paste(&mut self, text: String) -> Result<()> {
        self.clear_selections()?;
        let bracketed = self.focused().mode(Mode::BRACKETED_PASTE);
        let bytes = paste_bytes(&text, bracketed);
        self.send_to_focus(&bytes)
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

    fn aggregate_progress(&self) -> Option<osc::Progress> {
        aggregate_progress(
            std::iter::once(self.main.progress())
                .chain(self.aux.iter().flatten().map(Pane::progress))
                .flatten(),
        )
    }

    /// Forward the most significant progress among all panes, including hidden
    /// ones. `force` re-sends an unchanged aggregate for keepalive reports.
    fn sync_progress(&mut self, force: bool) -> Result<()> {
        let next = self.aggregate_progress();
        if !force && next == self.last_progress {
            return Ok(());
        }
        let previous = self.last_progress;
        self.last_progress = next;

        match next {
            None => {
                if previous.is_some() {
                    self.pending_writes.extend_from_slice(b"\x1b]9;4;0\x07");
                }
            }
            Some(progress) => {
                self.pending_writes.extend_from_slice(b"\x1b]9;4;");
                self.pending_writes.push(b'0' + progress.state.code());
                if let Some(percent) = progress.percent {
                    self.pending_writes.push(b';');
                    self.pending_writes
                        .extend_from_slice(percent.to_string().as_bytes());
                }
                self.pending_writes.push(0x07);
            }
        }
        Ok(())
    }

    /// Forward the focused pane's title to the outer terminal.
    ///
    /// An unset or cleared pane title emits an empty OSC 2 so a previous
    /// title is not left stuck on the tab.
    fn sync_title(&mut self) {
        let title = match self.focus {
            Focus::Main => self.main.title(),
            Focus::Aux => self
                .visible
                .and_then(|idx| self.aux[idx].as_ref())
                .and_then(Pane::title)
                .or_else(|| self.main.title()),
        };

        if title == self.last_title {
            return;
        }

        self.pending_writes.extend_from_slice(b"\x1b]2;");
        if let Some(title) = &title {
            self.pending_writes.extend_from_slice(title.as_bytes());
        }
        self.pending_writes.push(0x07);
        self.last_title = title;
    }

    /// Report the focused pane's working directory to the outer terminal.
    /// Ghostty tracks this from OSC 7 and uses it to give new tabs and splits
    /// the same cwd as the surface they were opened from.
    fn sync_cwd(&mut self) {
        let cwd = self.focused().cwd();
        if cwd == self.last_cwd {
            return;
        }
        self.last_cwd = cwd.clone();
        let Some(cwd) = cwd else { return };
        self.pending_writes
            .extend_from_slice(&osc7_sequence(&self.hostname, &cwd));
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

/// Build the OSC 7 sequence Ghostty accepts for cwd inheritance. The
/// `kitty-shell-cwd` scheme carries a raw, unencoded path so spaces and `%`
/// survive intact; control bytes are dropped so a path cannot terminate or
/// corrupt the sequence.
fn osc7_sequence(hostname: &str, cwd: &Path) -> Vec<u8> {
    let path: String = cwd
        .to_string_lossy()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut out = Vec::with_capacity(hostname.len() + path.len() + 21);
    out.extend_from_slice(b"\x1b]7;kitty-shell-cwd://");
    out.extend_from_slice(hostname.as_bytes());
    out.extend_from_slice(path.as_bytes());
    out.push(0x07);
    out
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "localhost".into())
}

fn contains(area: Area, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.w)
        && y >= area.y
        && y < area.y.saturating_add(area.h)
}

fn local_cell(area: Area, x: u16, y: u16) -> (u16, u16) {
    let clamp = |value: u16, origin: u16, len: u16| {
        (value as i32 - origin as i32).clamp(0, len.saturating_sub(1) as i32) as u16
    };
    (clamp(x, area.x, area.w), clamp(y, area.y, area.h))
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
            let gap = u16::from(size.cols >= 3);
            let main_w = (size.cols - gap) / 2;
            let aux_w = size.cols - gap - main_w;
            (
                Area {
                    x: 0,
                    y: 0,
                    w: main_w,
                    h: size.rows,
                },
                Some(Area {
                    x: main_w + gap,
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
        AppSize::Half => layout(size, Some(AppSize::Half)).1.expect("half area"),
        AppSize::Full => full_area(size),
    }
}

fn paint_separator(out: &mut Vec<u8>, x: u16, rows: u16, separator: char) {
    out.extend_from_slice(b"\x1b[0m\x1b]8;;\x1b\\");
    for y in 0..rows {
        write!(
            out,
            "\x1b[{};{}H{}",
            u32::from(y) + 1,
            u32::from(x) + 1,
            separator
        )
        .expect("writing to a Vec cannot fail");
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

/// Pick the most significant progress across panes; ties use the higher
/// percentage.
fn aggregate_progress(progress: impl Iterator<Item = osc::Progress>) -> Option<osc::Progress> {
    let mut best: Option<osc::Progress> = None;
    for progress in progress {
        let better = match best {
            None => true,
            Some(current) => {
                let rank = progress.state.priority();
                let current_rank = current.state.priority();
                rank > current_rank
                    || (rank == current_rank
                        && progress.percent.unwrap_or(0) > current.percent.unwrap_or(0))
            }
        };
        if better {
            best = Some(progress);
        }
    }
    best
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.as_bytes().to_vec()
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

    #[test]
    fn reload_maps_by_name_and_rejects_removing_running_apps() {
        let old = Config::parse("[[app]]\nname='a'\nbind='ctrl+a'\ncmd=['a']\n[[app]]\nname='b'\nbind='ctrl+b'\ncmd=['b']").unwrap();
        let new = Config::parse("[[app]]\nname='b'\nbind='ctrl+c'\ncmd=['new']\n[[app]]\nname='a'\nbind='ctrl+a'\ncmd=['a']\n[[app]]\nname='c'\nbind='ctrl+d'\ncmd=['c']").unwrap();
        assert_eq!(
            reload_mapping(&old, &new, &[true, true]).unwrap(),
            vec![Some(1), Some(0), None]
        );
        let removed = Config::parse("[[app]]\nname='b'\nbind='ctrl+b'\ncmd=['b']").unwrap();
        assert!(reload_mapping(&old, &removed, &[true, false]).is_err());
        assert_eq!(
            reload_mapping(&old, &removed, &[false, true]).unwrap(),
            vec![Some(1)]
        );
    }

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
                w: 39,
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
    fn area_contains_checks_bounds() {
        let area = full_area(size(80, 24));
        assert!(contains(area, 0, 0));
        assert!(contains(area, 79, 23));
        assert!(!contains(area, 80, 23));
        assert!(!contains(area, 79, 24));
    }

    #[test]
    fn osc7_carries_a_raw_path() {
        assert_eq!(
            osc7_sequence("host", Path::new("/tmp/a b/100%")),
            b"\x1b]7;kitty-shell-cwd://host/tmp/a b/100%\x07"
        );
        assert_eq!(
            osc7_sequence("host", Path::new("/tmp/a\nb")),
            b"\x1b]7;kitty-shell-cwd://host/tmp/ab\x07"
        );
    }

    #[test]
    fn paste_wraps_only_when_bracketed() {
        assert_eq!(paste_bytes("hi", false), b"hi");
        assert_eq!(paste_bytes("hi", true), b"\x1b[200~hi\x1b[201~");
    }

    #[test]
    fn split_accounts_for_separator_and_narrow_terminals() {
        let (main, aux) = layout(size(81, 24), Some(AppSize::Half));
        assert_eq!(main.w, 40);
        assert_eq!(aux.unwrap().w, 40);
        let (main, aux) = layout(size(2, 24), Some(AppSize::Half));
        assert_eq!(main.w, 1);
        assert_eq!(aux.unwrap().x, 1);
    }

    #[test]
    fn separator_uses_default_style_and_each_row() {
        let mut frame = Vec::new();
        paint_separator(&mut frame, 39, 2, '│');
        assert_eq!(
            frame,
            "\x1b[0m\x1b]8;;\x1b\\\x1b[1;40H│\x1b[2;40H│".as_bytes()
        );
    }

    #[test]
    fn progress_aggregation_prefers_significance_then_percent() {
        use osc::{Progress, ProgressState};

        let set = |percent| Progress {
            state: ProgressState::Set,
            percent,
        };
        let indeterminate = Progress {
            state: ProgressState::Indeterminate,
            percent: Some(90),
        };
        let error = Progress {
            state: ProgressState::Error,
            percent: None,
        };

        assert_eq!(aggregate_progress(std::iter::empty()), None);
        assert_eq!(
            aggregate_progress([set(Some(10)), set(Some(75)), set(Some(50))].into_iter()),
            Some(set(Some(75)))
        );
        assert_eq!(
            aggregate_progress([set(Some(99)), indeterminate, error].into_iter()),
            Some(error)
        );
        assert_eq!(
            aggregate_progress([set(Some(99)), indeterminate].into_iter()),
            Some(indeterminate)
        );
    }
}
