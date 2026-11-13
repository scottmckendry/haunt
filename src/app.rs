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
    banner, base64,
    config::{AppSize, Config},
    graphics::Graphics,
    input,
    notification::{self, Notification, NotificationKind},
    paint::Area,
    pane::{Pane, PaneDims, Progress, SelectKind},
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
    /// Fraction of the half-split width given to the main pane.
    split_ratio: f64,
    /// Whether the pointer is dragging the separator.
    resizing_split: bool,
    tx: SyncSender<Msg>,
    next_id: u64,
    encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    frame_dirty: bool,
    running: bool,
    exit_code: i32,
    notifications: Vec<Notification>,
    last_notification_tick: Instant,
    outer_focus: bool,
    selecting: Option<u64>,
    mouse_capture: Option<u64>,
    click: Option<ClickState>,
    last_autoscroll: Instant,
    pending_writes: Vec<u8>,
    frame_buf: Vec<u8>,
    last_title: Option<String>,
    last_cwd: Option<PathBuf>,
    last_progress: Option<Progress>,
    hostname: String,
    /// Whether the launch banner still has to be written to the main pane.
    banner_pending: bool,
    /// Whether the main pane holds the banner's ghost.
    ghost: bool,
}

impl App {
    pub fn new(cfg: Config, size: Size, theme: Option<Theme>, tx: SyncSender<Msg>) -> Result<Self> {
        let graphics = Graphics::new()?;
        let (main, reader) =
            Pane::spawn(0, &cfg.shell, None, dims_of(size, full_area(size)), theme)?;
        spawn_reader(0, reader, tx.clone());
        let aux = (0..cfg.apps.len()).map(|_| None).collect();
        // Only the main process writes a banner: app panes never get a ghost, and
        // reloading the config does not write another one.
        let banner_pending = cfg.banner;

        Ok(Self {
            cfg,
            theme,
            graphics,
            main,
            aux,
            visible: None,
            focus: Focus::Main,
            size,
            split_ratio: 0.5,
            resizing_split: false,
            tx,
            next_id: 1,
            encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            frame_dirty: true,
            running: true,
            exit_code: 0,
            notifications: Vec::new(),
            last_notification_tick: Instant::now(),
            outer_focus: true,
            selecting: None,
            mouse_capture: None,
            click: None,
            last_autoscroll: Instant::now(),
            pending_writes: Vec::new(),
            frame_buf: Vec::with_capacity(32 * 1024),
            last_title: None,
            last_cwd: None,
            last_progress: None,
            hostname: crate::machine::hostname(),
            banner_pending,
            ghost: false,
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
                if id == self.main.id {
                    self.write_banner();
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
        self.relayout()?;
        self.refresh_ghost();
        Ok(())
    }

    /// Put the banner's ghost back after a resize.
    ///
    /// See [`Graphics::remove_pane`]: a resized surface has no image data left,
    /// so the next frame has to re-send the ghost and place it again on the row
    /// where the banner is, including when the banner has already scrolled away,
    /// in which case the ghost goes with it.
    fn refresh_ghost(&mut self) {
        if self.ghost {
            self.graphics.remove_pane(self.main.id);
        }
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
        let (main_area, aux_area) = layout(self.size, visible_size, self.split_ratio);
        let full_aux = visible_size == Some(AppSize::Full);

        // Reuse the frame buffer: a full repaint can be tens of kilobytes, and
        // at 60 Hz a fresh allocation per frame adds avoidable churn.
        let mut frame = std::mem::take(&mut self.frame_buf);
        frame.clear();

        let mut visible_ids = [self.main.id; 2];
        let visible: &[u64] = match (self.visible, full_aux) {
            (Some(v), true) => match self.aux[v].as_ref() {
                Some(pane) => {
                    visible_ids[0] = pane.id;
                    &visible_ids[..1]
                }
                None => &visible_ids[..1],
            },
            (Some(v), false) => {
                if let Some(pane) = self.aux[v].as_ref() {
                    visible_ids[1] = pane.id;
                    &visible_ids
                } else {
                    &visible_ids[..1]
                }
            }
            (None, _) => &visible_ids[..1],
        };
        self.graphics.begin_frame(visible, &mut frame);

        if visible_size == Some(AppSize::Half) && self.size.cols >= 3 {
            paint_separator(&mut frame, main_area.w, self.size.rows, self.cfg.separator);
        }

        let main_scroll = full_width(main_area, self.size.cols);
        match (self.visible, self.focus, full_aux) {
            (Some(v), _, true) => {
                if let Some(pane) = self.aux[v].as_mut() {
                    let area = aux_area.expect("full aux has an area");
                    pane.paint(
                        &mut frame,
                        area,
                        true,
                        full_width(area, self.size.cols),
                        &mut self.graphics,
                    )?;
                } else {
                    self.main.paint(
                        &mut frame,
                        main_area,
                        true,
                        main_scroll,
                        &mut self.graphics,
                    )?;
                }
            }
            (Some(v), Focus::Main, false) => {
                if let Some(pane) = self.aux[v].as_mut() {
                    let area = aux_area.expect("visible aux has an area");
                    pane.paint(
                        &mut frame,
                        area,
                        false,
                        full_width(area, self.size.cols),
                        &mut self.graphics,
                    )?;
                }
                self.main
                    .paint(&mut frame, main_area, true, main_scroll, &mut self.graphics)?;
            }
            (Some(v), Focus::Aux, false) => {
                self.main.paint(
                    &mut frame,
                    main_area,
                    false,
                    main_scroll,
                    &mut self.graphics,
                )?;
                if let Some(pane) = self.aux[v].as_mut() {
                    let area = aux_area.expect("visible aux has an area");
                    pane.paint(
                        &mut frame,
                        area,
                        true,
                        full_width(area, self.size.cols),
                        &mut self.graphics,
                    )?;
                }
            }
            (None, _, _) => {
                self.main
                    .paint(&mut frame, main_area, true, main_scroll, &mut self.graphics)?
            }
        }

        notification::paint_notifications(
            &mut frame,
            full_area(self.size),
            &self.notifications,
            &self.cfg,
            Instant::now(),
        );

        // Synchronized output keeps a large repaint from being shown half
        // drawn. Small diffs are safe to write directly, and avoiding the
        // atomic commit keeps Ghostty from rendering each sync block at the
        // display refresh rate.
        if frame.len() > SYNC_THRESHOLD {
            out.write_all(b"\x1b[?2026h")?;
            out.write_all(&frame)?;
            out.write_all(b"\x1b[?2026l")?;
        } else {
            out.write_all(&frame)?;
        }
        out.flush()?;
        self.frame_dirty = false;
        // Keep the buffer for reuse, but do not pin memory after an unusually
        // large frame such as a base64-encoded image transfer.
        self.frame_buf = if frame.capacity() <= 256 * 1024 {
            frame
        } else {
            Vec::with_capacity(32 * 1024)
        };
        Ok(())
    }

    /// Write the launch banner into the main pane, once, as its first output.
    ///
    /// The pane has just shown its prompt; the banner scrolls the screen down and
    /// takes the rows above it, so it reads as output of the shell and scrolls
    /// away with the rest of the history.
    fn write_banner(&mut self) {
        if !self.banner_pending {
            return;
        }
        self.banner_pending = false;
        let cursor = self.main.cursor();
        let banner = banner::draw(
            &self.cfg,
            self.size,
            cursor,
            crate::theme::under_ghostty(),
            self.theme,
        );
        self.ghost = banner.ghost;
        // Failing here only means no banner; the shell itself is unaffected.
        let _ = self.main.feed(&banner.bytes);
    }

    pub fn shutdown(&mut self) {
        self.main.shutdown();
        for pane in self.aux.iter_mut().flatten() {
            pane.shutdown();
        }
    }

    fn on_key(&mut self, ev: KeyEvent) -> Result<()> {
        if self.cfg.help_bind.is_some_and(|bind| bind.matches(&ev)) {
            self.push_notification(NotificationKind::Help);
            return Ok(());
        }
        if ev.kind != KeyEventKind::Release
            && !self.notifications.is_empty()
            && ev.code == crossterm::event::KeyCode::Esc
            && ev.modifiers.is_empty()
        {
            self.clear_notifications();
            return Ok(());
        }

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
        self.relayout()?;
        self.push_notification(NotificationKind::Success("Configuration reloaded".into()));
        Ok(())
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
        if self.mouse_capture == Some(id) {
            self.mouse_capture = None;
        }

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
            let dims = dims_of(
                self.size,
                hidden_area(self.size, self.cfg.apps[idx].size, self.split_ratio),
            );
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
        // The separator owns the pointer for the whole drag, even past the
        // outer edges of the terminal.
        if self.resizing_split {
            match ev.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.resize_split_to(ev.column)?;
                    return Ok(());
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.resizing_split = false;
                    return Ok(());
                }
                _ => {}
            }
        }

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

        // Application mouse reporting also captures the gesture: a drag or
        // release outside the pane still belongs to the pane that received
        // the press, so the app sees the release and cannot get stuck.
        if let Some(id) = self.mouse_capture
            && matches!(ev.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        {
            let released = matches!(ev.kind, MouseEventKind::Up(_));
            let sent = self
                .pane_input_area(id)
                .and_then(|area| self.pane_mut(id).map(|pane| pane.send_mouse(ev, area)));
            if released || sent.is_none() {
                self.mouse_capture = None;
            }
            if let Some(result) = sent {
                result?;
            }
            return Ok(());
        }

        // Any new press starts a fresh gesture.
        if matches!(ev.kind, MouseEventKind::Down(_)) {
            self.mouse_capture = None;
        }

        if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
            && self.separator_column() == Some(ev.column)
        {
            self.clear_selections()?;
            self.resizing_split = true;
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
            let captured = matches!(ev.kind, MouseEventKind::Down(_));
            let result = pane.send_mouse(ev, area);
            if captured && result.is_ok() {
                self.mouse_capture = Some(id);
            }
            return result;
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

    /// Timed work for notifications and selection autoscroll.
    pub fn tick(&mut self) -> Result<()> {
        let now = Instant::now();
        let was_animating = self
            .notifications
            .iter()
            .any(|n| n.animating(self.last_notification_tick));
        self.last_notification_tick = now;
        let old_len = self.notifications.len();
        self.notifications.retain(|n| !n.expired(now));
        if old_len != self.notifications.len()
            || was_animating
            || self.notifications.iter().any(|n| n.animating(now))
        {
            self.repaint_overlay_area();
        }
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
        let (main_area, aux_area) = layout(self.size, visible_size, self.split_ratio);
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

    fn pane_input_area(&self, id: u64) -> Option<Area> {
        if self.main.id == id {
            let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
            return Some(layout(self.size, visible_size, self.split_ratio).0);
        }

        let idx = self
            .aux
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|pane| pane.id == id))?;
        if self.visible == Some(idx) {
            return self.pane_area(id);
        }
        // Keep delivering a captured drag or release even if the pane was
        // hidden mid-gesture, so the app can clear its button state.
        Some(hidden_area(
            self.size,
            self.cfg.apps[idx].size,
            self.split_ratio,
        ))
    }

    /// The column of the separator between a visible half-size app and the
    /// main pane, if the split is wide enough to have one.
    fn separator_column(&self) -> Option<u16> {
        let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
        if visible_size != Some(AppSize::Half) || self.size.cols < 3 {
            return None;
        }
        Some(layout(self.size, visible_size, self.split_ratio).0.w)
    }

    fn resize_split_to(&mut self, column: u16) -> Result<()> {
        let available = split_available(self.size);
        if available < 2 {
            return Ok(());
        }
        // Keep at least one column on each side of the separator.
        let main_w = column.clamp(1, available - 1);
        let ratio = f64::from(main_w) / f64::from(available);
        if ratio == self.split_ratio {
            return Ok(());
        }
        self.split_ratio = ratio;
        self.relayout()
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<(Area, u64)> {
        let visible_size = self.visible.map(|idx| self.cfg.apps[idx].size);
        let (main_area, aux_area) = layout(self.size, visible_size, self.split_ratio);

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
        let (main_area, aux_area) = layout(self.size, visible_size, self.split_ratio);

        self.main.resize(dims_of(self.size, main_area))?;
        self.main.force_repaint();

        for idx in 0..self.aux.len() {
            let area = if Some(idx) == self.visible {
                aux_area.expect("visible aux has an area")
            } else {
                hidden_area(self.size, self.cfg.apps[idx].size, self.split_ratio)
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

    fn aggregate_progress(&self) -> Option<Progress> {
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

    pub(crate) fn set_error(&mut self, error: String) {
        self.push_notification(NotificationKind::Error(error));
    }

    fn push_notification(&mut self, kind: NotificationKind) {
        const MAX_NOTIFICATIONS: usize = 8;
        if self.notifications.len() == MAX_NOTIFICATIONS {
            self.notifications.remove(0);
        }
        self.notifications
            .push(Notification::new(kind, Instant::now()));
        self.repaint_overlay_area();
    }

    fn clear_notifications(&mut self) {
        if !self.notifications.is_empty() {
            self.notifications.clear();
            self.repaint_overlay_area();
        }
    }

    fn repaint_overlay_area(&mut self) {
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

fn split_gap(cols: u16) -> u16 {
    u16::from(cols >= 3)
}

fn split_available(size: Size) -> u16 {
    size.cols.saturating_sub(split_gap(size.cols))
}

/// The main pane's width at `ratio`, clamped so both panes stay at least one
/// column wide when the split has room for it.
fn split_main_width(size: Size, ratio: f64) -> u16 {
    let available = split_available(size);
    if available < 2 {
        return available.saturating_sub(1);
    }
    // The epsilon keeps an exact ratio (for example 39/79 from a drag) from
    // flooring to one column less because of floating-point error.
    let width = (f64::from(available) * ratio.clamp(0.0, 1.0) + 1e-6).floor() as u16;
    width.clamp(1, available - 1)
}

fn layout(size: Size, visible: Option<AppSize>, ratio: f64) -> (Area, Option<Area>) {
    let full = full_area(size);
    match visible {
        Some(AppSize::Half) => {
            let gap = split_gap(size.cols);
            let main_w = split_main_width(size, ratio);
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

/// Whether a pane's rectangle covers every column of the outer terminal.
///
/// Only then can a region scroll be used: a terminal scroll region moves whole
/// rows, so a partial-width pane would disturb the pane beside it.
fn full_width(area: Area, cols: u16) -> bool {
    area.x == 0 && area.w == cols
}

fn hidden_area(size: Size, app_size: AppSize, ratio: f64) -> Area {
    match app_size {
        AppSize::Half => layout(size, Some(AppSize::Half), ratio)
            .1
            .expect("half area"),
        AppSize::Full => full_area(size),
    }
}

fn paint_separator(out: &mut Vec<u8>, x: u16, rows: u16, separator: char) {
    out.extend_from_slice(b"\x1b[0;90m\x1b]8;;\x1b\\");
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
fn aggregate_progress(progress: impl Iterator<Item = Progress>) -> Option<Progress> {
    let mut best: Option<Progress> = None;
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
///
/// This only paces an idle loop. Rendering is not capped: the outer terminal
/// already coalesces damage to its own refresh rate, and a fixed frame cap
/// would hobble high-refresh displays (or waste work on low-refresh ones).
pub const TICK: Duration = Duration::from_millis(16);

/// Frames larger than this are wrapped in synchronized output. A small diff
/// does not need the atomic commit; large repaints still get it.
const SYNC_THRESHOLD: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_maps_by_name_and_rejects_removing_running_apps() {
        let old =
            Config::parse("[apps.a]\nbind='ctrl+a'\ncmd=['a']\n[apps.b]\nbind='ctrl+b'\ncmd=['b']")
                .unwrap();
        let new = Config::parse(
            "[apps.b]\nbind='ctrl+c'\ncmd=['new']\n[apps.a]\nbind='ctrl+a'\ncmd=['a']\n\
             [apps.c]\nbind='ctrl+d'\ncmd=['c']",
        )
        .unwrap();
        assert_eq!(
            reload_mapping(&old, &new, &[true, true]).unwrap(),
            vec![Some(1), Some(0), None]
        );
        let removed = Config::parse("[apps.b]\nbind='ctrl+b'\ncmd=['b']").unwrap();
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
        let (main, aux) = layout(size(80, 24), None, 0.5);
        assert_eq!(main, full_area(size(80, 24)));
        assert_eq!(aux, None);
    }

    #[test]
    fn half_aux_splits_the_tab_vertically() {
        let (main, aux) = layout(size(80, 24), Some(AppSize::Half), 0.5);
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
        let (main, aux) = layout(size(80, 24), Some(AppSize::Full), 0.5);
        assert_eq!(main, full_area(size(80, 24)));
        assert_eq!(aux, Some(full_area(size(80, 24))));
    }

    #[test]
    fn hidden_panes_keep_their_visible_size() {
        assert_eq!(
            hidden_area(size(80, 24), AppSize::Half, 0.5),
            Area {
                x: 40,
                y: 0,
                w: 40,
                h: 24
            }
        );
        assert_eq!(
            hidden_area(size(80, 24), AppSize::Full, 0.5),
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
    fn mouse_drag_and_release_stay_with_the_pressed_pane() {
        let cfg = Config::parse("shell=['/bin/sh']").unwrap();
        let (tx, _rx) = channel();
        let mut app = App::new(cfg, size(40, 10), None, tx).unwrap();
        app.main.feed(b"\x1b[?1000h\x1b[?1006h").unwrap();

        let mouse = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };

        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 1))
            .unwrap();
        assert_eq!(app.mouse_capture, Some(app.main.id));
        assert!(app.main.mouse_down());

        // Both events land outside the pane and even outside the terminal.
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 100, 100))
            .unwrap();
        assert_eq!(app.mouse_capture, Some(app.main.id));

        app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 100, 100))
            .unwrap();
        assert_eq!(app.mouse_capture, None);
        assert!(!app.main.mouse_down());

        app.shutdown();
    }

    #[test]
    fn paste_wraps_only_when_bracketed() {
        assert_eq!(paste_bytes("hi", false), b"hi");
        assert_eq!(paste_bytes("hi", true), b"\x1b[200~hi\x1b[201~");
    }

    #[test]
    fn split_accounts_for_separator_and_narrow_terminals() {
        let (main, aux) = layout(size(81, 24), Some(AppSize::Half), 0.5);
        assert_eq!(main.w, 40);
        assert_eq!(aux.unwrap().w, 40);
        let (main, aux) = layout(size(2, 24), Some(AppSize::Half), 0.5);
        assert_eq!(main.w, 1);
        assert_eq!(aux.unwrap().x, 1);
    }

    #[test]
    fn split_ratio_clamps_to_one_column_per_pane() {
        let (main, aux) = layout(size(80, 24), Some(AppSize::Half), 0.0);
        assert_eq!(main.w, 1);
        assert_eq!(aux.unwrap().w, 78);
        let (main, aux) = layout(size(80, 24), Some(AppSize::Half), 1.0);
        assert_eq!(main.w, 78);
        assert_eq!(aux.unwrap().w, 1);

        // An exact dragged ratio must not floor to a column less.
        let ratio = 39.0 / 79.0;
        let (main, aux) = layout(size(80, 24), Some(AppSize::Half), ratio);
        assert_eq!(main.w, 39);
        assert_eq!(aux.unwrap().x, 40);
    }

    #[test]
    fn dragging_the_separator_resizes_the_split() {
        let cfg = Config::parse(
            "shell=['/bin/sh']\n[apps.a]\nbind='ctrl+a'\ncmd=['/bin/sh']\nsize='half'",
        )
        .unwrap();
        let (tx, _rx) = channel();
        let mut app = App::new(cfg, size(80, 10), None, tx).unwrap();
        app.show(0).unwrap();

        let mouse = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        let separator = app.separator_column().unwrap();

        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), separator, 0))
            .unwrap();
        assert!(app.resizing_split);

        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 20, 0))
            .unwrap();
        let (main, aux) = layout(app.size, Some(AppSize::Half), app.split_ratio);
        assert_eq!(main.w, 20);
        assert_eq!(aux.unwrap().x, 21);
        assert_eq!(app.main.term().cols().unwrap(), 20);

        // The drag stays with the separator even past either edge.
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0))
            .unwrap();
        assert_eq!(
            layout(app.size, Some(AppSize::Half), app.split_ratio).0.w,
            1
        );
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 255, 0))
            .unwrap();
        assert_eq!(
            layout(app.size, Some(AppSize::Half), app.split_ratio).0.w,
            78
        );

        app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 255, 0))
            .unwrap();
        assert!(!app.resizing_split);

        // The ratio survives a terminal resize.
        app.resize(size(100, 10)).unwrap();
        assert_eq!(
            layout(app.size, Some(AppSize::Half), app.split_ratio).0.w,
            97
        );

        app.shutdown();
    }

    #[test]
    fn separator_uses_bright_black_and_each_row() {
        let mut frame = Vec::new();
        paint_separator(&mut frame, 39, 2, '│');
        assert_eq!(
            frame,
            "\x1b[0;90m\x1b]8;;\x1b\\\x1b[1;40H│\x1b[2;40H│".as_bytes()
        );
    }

    #[test]
    fn progress_aggregation_prefers_significance_then_percent() {
        use crate::pane::ProgressState;

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
