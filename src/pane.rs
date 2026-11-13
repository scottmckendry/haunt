//! One PTY-backed pane: terminal state, PTY, child process, render state.

use std::{
    env,
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Result;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use libghostty_vt::{
    RenderState, Terminal,
    fmt::Formatter,
    mouse,
    render::{CellIterator, RowIterator},
    screen::{Screen, TrackedGridRef},
    selection::{SelectLineOptions, SelectWordBetweenOptions, Selection},
    terminal::{
        ClipboardLocation, ClipboardWrite, ColorScheme, ConformanceLevel, DeviceAttributeFeature,
        DeviceAttributes, DeviceType, Mode, Point, PointCoordinate, PrimaryDeviceAttributes,
        ProgressState as VtProgressState, ScrollViewport, SecondaryDeviceAttributes,
        SizeReportSize,
    },
};
use portable_pty::{MasterPty, PtySize};

use crate::{
    base64,
    graphics::Graphics,
    input,
    paint::{self, Area, PaintState},
    pty,
    theme::Theme,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    Set,
    Error,
    Indeterminate,
    Pause,
}

impl ProgressState {
    /// Higher wins when several panes report progress.
    pub fn priority(self) -> u8 {
        match self {
            Self::Error => 3,
            Self::Indeterminate => 2,
            Self::Set | Self::Pause => 1,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Set => 1,
            Self::Error => 2,
            Self::Indeterminate => 3,
            Self::Pause => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub state: ProgressState,
    pub percent: Option<u8>,
}

/// Per-pane cap on libghostty's Kitty image storage. This bounds the memory
/// used by images a pane can retain without silently dropping smaller images.
const KITTY_IMAGE_STORAGE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneDims {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
}

impl PaneDims {
    fn pixel_width(self) -> u16 {
        (self.cols as u32 * self.cell_w as u32).min(u16::MAX as u32) as u16
    }

    fn pixel_height(self) -> u16 {
        (self.rows as u32 * self.cell_h as u32).min(u16::MAX as u32) as u16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    Cell,
    Word,
    Line,
}

#[derive(Default)]
struct ProgressTracking {
    current: Option<Progress>,
    changed: bool,
}

struct SelectionState {
    anchor: TrackedGridRef,
    focus: TrackedGridRef,
    kind: SelectKind,
    dragging: bool,
    pointer: (u16, u16),
}

/// XTVERSION reply for inner programs.
///
/// Report the outer terminal's identity, not `haunt`'s, so apps enable the
/// capabilities the outer terminal actually supports.
fn outer_terminal_identity() -> &'static str {
    static IDENTITY: OnceLock<String> = OnceLock::new();
    IDENTITY
        .get_or_init(|| {
            let program = env::var("TERM_PROGRAM")
                .ok()
                .filter(|program| !program.trim().is_empty())
                .or_else(|| {
                    env::var("TERM")
                        .ok()
                        .filter(|term| term.contains("ghostty"))
                        .map(|_| "ghostty".to_string())
                })
                .or_else(|| env::var("TERM").ok())
                .unwrap_or_else(|| "xterm".to_string());
            let program = program.trim();

            let version = env::var("TERM_PROGRAM_VERSION")
                .ok()
                .filter(|version| !version.trim().is_empty())
                .or_else(|| {
                    let dir = env::var("GHOSTTY_RESOURCES_DIR").ok()?;
                    dir.split('/')
                        .find_map(|part| part.strip_prefix("ghostty-"))
                        .map(str::to_string)
                });

            match version {
                Some(version) => format!("{program} {}", version.trim()),
                None => program.to_string(),
            }
        })
        .as_str()
}

pub struct Pane {
    pub id: u64,
    term: Terminal<'static, 'static>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: pty::ChildGuard,
    render_state: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    paint_state: PaintState,
    responses: Arc<Mutex<Vec<u8>>>,
    effects: Arc<Mutex<Vec<u8>>>,
    title: Arc<Mutex<Option<String>>>,
    dims: Arc<Mutex<PaneDims>>,
    progress: Arc<Mutex<ProgressTracking>>,
    scheme: Arc<Mutex<Option<ColorScheme>>>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    mouse_down: bool,
    selection: Option<SelectionState>,
    force_full: bool,
    focus_events: bool,
    focus_events_pending: bool,
    pub dirty: bool,
}

impl Pane {
    pub fn spawn(
        id: u64,
        cmd: &[String],
        cwd: Option<&std::path::Path>,
        dims: PaneDims,
        theme: Option<Theme>,
    ) -> Result<(Self, Box<dyn Read + Send>)> {
        let (program, args) = cmd
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("empty command"))?;

        let pty::Pty {
            master,
            child,
            reader,
            writer,
        } = pty::spawn(
            program,
            args,
            cwd,
            dims.rows,
            dims.cols,
            dims.pixel_width(),
            dims.pixel_height(),
        )?;

        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));
        let responses = Arc::new(Mutex::new(Vec::<u8>::new()));
        let effects = Arc::new(Mutex::new(Vec::<u8>::new()));
        let title = Arc::new(Mutex::new(None::<String>));
        let dims_shared = Arc::new(Mutex::new(dims));
        let progress = Arc::new(Mutex::new(ProgressTracking::default()));

        let mut term: Terminal<'static, 'static> = Terminal::new(dims.cols, dims.rows)?;
        term.set_scrollback_max_bytes(Some(usize::MAX))?;
        term.set_scrollback_max_lines(Some(10_000))?;
        if let Some(theme) = theme {
            theme.apply(&mut term)?;
        }
        term.resize(dims.cols, dims.rows, dims.cell_w as u32, dims.cell_h as u32)?;
        // Snacks and similar clients transmit images by local file path
        // (`t=f`) rather than embedding the payload, so allow every medium
        // the library supports in addition to direct data.
        term.set_kitty_image_storage_limit(KITTY_IMAGE_STORAGE_LIMIT)?
            .set_kitty_image_from_file_allowed(true)?
            .set_kitty_image_temp_file_dir(Some(&env::temp_dir()))?
            .set_kitty_image_from_shared_mem_allowed(true)?;

        {
            let responses = Arc::clone(&responses);
            term.on_pty_write(move |_term, data| {
                responses.lock().unwrap().extend_from_slice(data);
            })?;
        }
        {
            let effects = Arc::clone(&effects);
            term.on_bell(move |_term| effects.lock().unwrap().push(0x07))?;
        }
        {
            let effects = Arc::clone(&effects);
            term.on_clipboard_write(move |_term, write| {
                forward_clipboard(&effects, &write);
                write.reply(Ok(()), false);
            })?;
        }
        {
            let effects = Arc::clone(&effects);
            term.on_desktop_notification(move |_term, notification| {
                let mut out = effects.lock().unwrap();
                let title = notification.title();
                // OSC 9;4 is progress, not a notification. An OSC 777
                // notification with an empty title can still have that body.
                if title.is_empty() && !notification.body().starts_with(b"4;") {
                    out.extend_from_slice(b"\x1b]9;");
                } else {
                    out.extend_from_slice(b"\x1b]777;notify;");
                    // A semicolon separates the title from the body in OSC 777.
                    out.extend(
                        title
                            .iter()
                            .copied()
                            .filter(|b| *b >= 0x20 && *b != 0x7f)
                            .map(|b| if b == b';' { b' ' } else { b }),
                    );
                    out.push(b';');
                }
                out.extend(
                    notification
                        .body()
                        .iter()
                        .copied()
                        .filter(|b| *b >= 0x20 && *b != 0x7f),
                );
                out.push(0x07);
            })?;
        }
        {
            let progress = Arc::clone(&progress);
            term.on_progress_report(move |_term, report| {
                let Ok(state) = report.state() else { return };
                let state = match state {
                    VtProgressState::Remove => None,
                    VtProgressState::Set => Some(ProgressState::Set),
                    VtProgressState::Error => Some(ProgressState::Error),
                    VtProgressState::Indeterminate => Some(ProgressState::Indeterminate),
                    VtProgressState::Pause => Some(ProgressState::Pause),
                    _ => return,
                };
                let mut progress = progress.lock().unwrap();
                progress.current = state.map(|state| Progress {
                    state,
                    percent: report.progress(),
                });
                progress.changed = true;
            })?;
        }
        {
            let current = Arc::clone(&title);
            term.on_title_changed(move |term| {
                if let Ok(title) = term.title() {
                    *current.lock().unwrap() = Some(String::from_utf8_lossy(title).into_owned());
                }
            })?;
        }
        {
            let dims = Arc::clone(&dims_shared);
            term.on_size(move |_term| {
                let dims = *dims.lock().unwrap();
                Some(SizeReportSize {
                    rows: dims.rows,
                    columns: dims.cols,
                    cell_width: dims.cell_w as u32,
                    cell_height: dims.cell_h as u32,
                })
            })?;
        }
        term.on_device_attributes(|_term| {
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(
                    ConformanceLevel::VT220,
                    &[
                        DeviceAttributeFeature::COLUMNS_132,
                        DeviceAttributeFeature::SELECTIVE_ERASE,
                        DeviceAttributeFeature::ANSI_COLOR,
                        DeviceAttributeFeature::CLIPBOARD,
                    ],
                ),
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 1,
                    rom_cartridge: 0,
                },
                tertiary: Default::default(),
            })
        })?;
        term.on_xtversion(|_term| Some(outer_terminal_identity()))?;
        let scheme = Arc::new(Mutex::new(theme.and_then(Theme::scheme)));
        {
            let scheme = Arc::clone(&scheme);
            term.on_color_scheme(move |_term| *scheme.lock().unwrap())?;
        }

        let mut paint_state = PaintState::default();
        if let Some(theme) = theme {
            paint_state.selection_fg = theme.selection_fg;
            paint_state.selection_bg = theme.selection_bg;
        }

        Ok((
            Self {
                id,
                term,
                writer,
                master,
                child,
                render_state: RenderState::new()?,
                rows_it: RowIterator::new()?,
                cells_it: CellIterator::new()?,
                paint_state,
                responses,
                effects,
                title,
                dims: dims_shared,
                progress,
                scheme,
                mouse_encoder: mouse::Encoder::new()?,
                mouse_event: mouse::Event::new()?,
                mouse_down: false,
                selection: None,
                force_full: true,
                focus_events: false,
                focus_events_pending: false,
                dirty: true,
            },
            reader,
        ))
    }

    /// Feed bytes read from the PTY into the terminal and answer any queries.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        self.term.vt_write(bytes);
        self.dirty = true;
        self.flush_responses()?;

        // A pane that enables focus reporting after it was already focused
        // misses the transition. Flag it so App can synthesize a focus-in.
        let focus_events = self.mode(Mode::FOCUS_EVENT);
        if focus_events && !self.focus_events {
            self.focus_events_pending = true;
        }
        self.focus_events = focus_events;
        Ok(())
    }

    /// Whether the pane just enabled focus reporting; clears the flag.
    pub fn take_focus_events_pending(&mut self) -> bool {
        std::mem::take(&mut self.focus_events_pending)
    }

    /// The pane's cursor, in viewport cells.
    pub fn cursor(&self) -> (u16, u16) {
        (
            self.term.cursor_x().unwrap_or(0),
            self.term.cursor_y().unwrap_or(0),
        )
    }

    /// Whether a terminal mode is enabled by the inner program.
    pub fn mode(&self, mode: Mode) -> bool {
        self.term.mode(mode).unwrap_or(false)
    }

    pub fn send(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.writer.lock().unwrap().write_all(bytes)?;
        Ok(())
    }

    /// The title set by the inner program, or `None` if it never set one.
    pub fn title(&self) -> Option<String> {
        self.title.lock().unwrap().clone()
    }

    /// Take effects (bell, notifications) generated since the last call.
    pub fn take_effects(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.effects.lock().unwrap())
    }

    /// Current OSC 9;4 progress reported by the child, if any.
    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().unwrap().current
    }

    /// Whether the child reported progress since the last call.
    ///
    /// Keepalives matter: Ghostty expires progress that is not refreshed, and
    /// apps may re-send the same indeterminate report every second.
    pub fn take_progress_changed(&mut self) -> bool {
        std::mem::take(&mut self.progress.lock().unwrap().changed)
    }

    pub fn resize(&mut self, dims: PaneDims) -> Result<()> {
        if *self.dims.lock().unwrap() == dims {
            return Ok(());
        }
        self.term
            .resize(dims.cols, dims.rows, dims.cell_w as u32, dims.cell_h as u32)?;
        self.master.resize(PtySize {
            rows: dims.rows,
            cols: dims.cols,
            pixel_width: dims.pixel_width(),
            pixel_height: dims.pixel_height(),
        })?;
        // Programs that enable mode 2048 (in-band resize reports) stop
        // listening for SIGWINCH. libghostty queues the report during
        // `term.resize`, so deliver it now instead of waiting for the child's
        // next output to flush it.
        self.flush_responses()?;
        *self.dims.lock().unwrap() = dims;
        self.dirty = true;
        Ok(())
    }

    pub fn paint(
        &mut self,
        out: &mut Vec<u8>,
        area: Area,
        focused: bool,
        scrollable: bool,
        graphics: &mut Graphics,
    ) -> Result<()> {
        self.sync_selection()?;
        let snapshot = self.render_state.update(&self.term)?;

        // Forward image data and placements first: the painter needs the
        // image-ID mapping to rewrite Unicode placeholder cells.
        let graphics_repaint = graphics.draw_pane(self.id, &self.term, out, area)?;

        paint::draw(
            &snapshot,
            &self.term,
            graphics,
            self.id,
            &mut self.rows_it,
            &mut self.cells_it,
            &mut self.paint_state,
            out,
            area,
            scrollable,
            self.force_full || graphics_repaint,
        )?;
        if focused {
            paint::draw_cursor(&snapshot, out, area)?;
        }
        self.force_full = false;
        self.dirty = false;
        Ok(())
    }

    /// Repaint every row on the next paint, ignoring dirty tracking.
    pub fn force_repaint(&mut self) {
        self.force_full = true;
        self.dirty = true;
    }

    pub fn term(&self) -> &Terminal<'static, 'static> {
        &self.term
    }

    /// Install a refreshed theme and repaint the pane with it.
    pub fn apply_theme(&mut self, theme: Option<Theme>) -> Result<()> {
        if let Some(theme) = theme {
            theme.apply(&mut self.term)?;
            self.paint_state.selection_fg = theme.selection_fg;
            self.paint_state.selection_bg = theme.selection_bg;
        } else {
            Theme::clear(&mut self.term)?;
            self.paint_state.selection_fg = None;
            self.paint_state.selection_bg = None;
        }
        *self.scheme.lock().unwrap() = theme.and_then(Theme::scheme);
        self.force_repaint();
        Ok(())
    }
    /// Whether the inner program asked for mouse reporting.
    pub fn tracks_mouse(&self) -> bool {
        self.term.is_mouse_tracking().unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn mouse_down(&self) -> bool {
        self.mouse_down
    }

    /// Encode a mouse event for this pane and write it to the PTY.
    ///
    /// `ev` is in outer-terminal cells; `area` is the pane's rectangle, used
    /// to translate into pane-local coordinates and supply encoder geometry.
    pub fn send_mouse(&mut self, ev: MouseEvent, area: Area) -> Result<()> {
        let (action, button) = match ev.kind {
            MouseEventKind::Down(b) => (mouse::Action::Press, Some(mouse_button(b))),
            MouseEventKind::Up(b) => (mouse::Action::Release, Some(mouse_button(b))),
            MouseEventKind::Drag(b) => (mouse::Action::Motion, Some(mouse_button(b))),
            MouseEventKind::Moved => (mouse::Action::Motion, None),
            MouseEventKind::ScrollUp => (mouse::Action::Press, Some(mouse::Button::Four)),
            MouseEventKind::ScrollDown => (mouse::Action::Press, Some(mouse::Button::Five)),
            MouseEventKind::ScrollLeft => (mouse::Action::Press, Some(mouse::Button::Six)),
            MouseEventKind::ScrollRight => (mouse::Action::Press, Some(mouse::Button::Seven)),
        };

        if action == mouse::Action::Press {
            self.mouse_down = true;
        } else if action == mouse::Action::Release {
            self.mouse_down = false;
        }

        let dims = *self.dims.lock().unwrap();
        let cell_w = u32::from(dims.cell_w.max(1));
        let cell_h = u32::from(dims.cell_h.max(1));
        let x = f32::from(ev.column.saturating_sub(area.x));
        let y = f32::from(ev.row.saturating_sub(area.y));

        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(input::mods_from(ev.modifiers))
            .set_position(mouse::Position {
                x: (x + 0.5) * cell_w as f32,
                y: (y + 0.5) * cell_h as f32,
            });

        self.mouse_encoder
            .set_options_from_terminal(&self.term)
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(area.w) * cell_w,
                screen_height: u32::from(area.h) * cell_h,
                cell_width: cell_w,
                cell_height: cell_h,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })?
            .set_any_button_pressed(self.mouse_down)
            .set_track_last_cell(true);

        let mut buf = Vec::with_capacity(32);
        self.mouse_encoder
            .encode_to_vec(&self.mouse_event, &mut buf)?;
        self.send(&buf)
    }

    /// Handle a wheel notch, scrolling scrollback or emulating cursor keys.
    ///
    /// Alternate-screen programs that do not request mouse reporting (such as
    /// `less`, `man`, and `vim`) expect the wheel to arrive as Up/Down keys,
    /// because the alternate screen has no scrollback to move.
    pub fn wheel_scroll(&mut self, delta: isize) -> Result<()> {
        if self
            .term
            .active_screen()
            .is_ok_and(|screen| screen == Screen::Alternate)
        {
            let up = delta < 0;
            let key: &[u8] = match (up, self.mode(Mode::DECCKM)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let mut buf = Vec::with_capacity(delta.unsigned_abs() * key.len());
            for _ in 0..delta.unsigned_abs() {
                buf.extend_from_slice(key);
            }
            return self.send(&buf);
        }
        self.scroll(delta)
    }

    /// Scroll the viewport by `delta` rows (negative is up).
    ///
    /// Does nothing when the pane has no scrollback, including on the
    /// alternate screen.
    pub fn scroll(&mut self, delta: isize) -> Result<()> {
        let before = self.term.scrollbar()?;
        if before.total <= before.len {
            return Ok(());
        }
        self.term.scroll_viewport(ScrollViewport::Delta(delta));
        if self.term.scrollbar()?.offset != before.offset {
            self.dirty = true;
        }
        Ok(())
    }

    /// Return the viewport to the live screen.
    pub fn scroll_to_live(&mut self) {
        let Ok(before) = self.term.scrollbar() else {
            return;
        };
        self.term.scroll_viewport(ScrollViewport::Bottom);
        if self
            .term
            .scrollbar()
            .is_ok_and(|after| after.offset != before.offset)
        {
            self.dirty = true;
        }
    }

    /// Start a local selection at viewport cell `(x, y)`.
    pub fn selection_start(&mut self, x: u16, y: u16, kind: SelectKind) -> Result<()> {
        let point = Point::Viewport(PointCoordinate { x, y: y as u32 });
        let anchor = self.term.track_grid_ref(point)?;
        let focus = self.term.track_grid_ref(point)?;
        self.selection = Some(SelectionState {
            anchor,
            focus,
            kind,
            dragging: false,
            pointer: (x, y),
        });
        self.sync_selection()?;
        self.dirty = true;
        Ok(())
    }

    /// Extend the active local selection to viewport cell `(x, y)`.
    pub fn selection_drag(&mut self, x: u16, y: u16) -> Result<()> {
        if self.selection.is_none() {
            return Ok(());
        }
        if let Some(selection) = self.selection.as_mut() {
            selection.pointer = (x, y);
            selection.dragging = true;
            let point = Point::Viewport(PointCoordinate { x, y: y as u32 });
            selection.focus.set(&mut self.term, point)?;
        }
        self.sync_selection()?;
        self.dirty = true;
        Ok(())
    }

    /// Finish a selection and return its text when it should be copied.
    ///
    /// A plain click (no drag) clears the selection and returns `None`.
    pub fn selection_finish(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(selection) = self.selection.as_ref() else {
            return Ok(None);
        };
        if selection.kind == SelectKind::Cell && !selection.dragging {
            self.clear_selection()?;
            return Ok(None);
        }

        self.sync_selection()?;
        let Some(mut formatter) = Formatter::new_selection(&self.term)? else {
            return Ok(None);
        };
        formatter.set_unwrap(true)?.set_trim(true)?;
        let bytes = formatter.format_alloc(None)?.to_vec();
        Ok((!bytes.is_empty()).then_some(bytes))
    }

    /// Drop any local selection.
    pub fn clear_selection(&mut self) -> Result<()> {
        if self.selection.take().is_some() {
            self.term.set_selection(None)?;
            self.dirty = true;
        }
        Ok(())
    }

    /// Keep dragging a selection alive past the pane edge.
    ///
    /// Returns `true` when the viewport moved.
    pub fn selection_autoscroll(&mut self) -> Result<bool> {
        let Some((x, y, dragging)) = self
            .selection
            .as_ref()
            .map(|selection| (selection.pointer.0, selection.pointer.1, selection.dragging))
        else {
            return Ok(false);
        };
        if !dragging {
            return Ok(false);
        }

        let rows = self.dims.lock().unwrap().rows;
        let (delta, edge) = if y == 0 {
            (-1isize, 0u16)
        } else if y >= rows.saturating_sub(1) {
            (1isize, rows.saturating_sub(1))
        } else {
            return Ok(false);
        };

        let before = self.term.scrollbar()?.offset;
        self.term.scroll_viewport(ScrollViewport::Delta(delta));
        if self.term.scrollbar()?.offset == before {
            return Ok(false);
        }

        if let Some(selection) = self.selection.as_mut() {
            let point = Point::Viewport(PointCoordinate { x, y: edge as u32 });
            selection.focus.set(&mut self.term, point)?;
        }
        self.sync_selection()?;
        self.dirty = true;
        Ok(true)
    }

    fn sync_selection(&self) -> Result<()> {
        let Some(selection) = self.selection.as_ref() else {
            self.term.set_selection(None)?;
            return Ok(());
        };
        let selection = self.build_selection(selection)?;
        self.term.set_selection(selection.as_ref())?;
        Ok(())
    }

    fn build_selection<'a>(&'a self, state: &SelectionState) -> Result<Option<Selection<'a>>> {
        let Some(anchor) = state.anchor.snapshot(&self.term)? else {
            return Ok(None);
        };
        let Some(focus) = state.focus.snapshot(&self.term)? else {
            return Ok(None);
        };

        let selection = match state.kind {
            SelectKind::Cell => Selection::new(anchor, focus, false),
            SelectKind::Word => {
                let start = self
                    .term
                    .select_word_between(SelectWordBetweenOptions::new(
                        anchor.clone(),
                        focus.clone(),
                    ))?;
                let end = self
                    .term
                    .select_word_between(SelectWordBetweenOptions::new(focus, anchor))?;
                match (start, end) {
                    (Some(start), Some(end)) => Selection::new(start.start(), end.end(), false),
                    _ => return Ok(None),
                }
            }
            SelectKind::Line => {
                let start = self.term.select_line(SelectLineOptions::new(anchor))?;
                let end = self.term.select_line(SelectLineOptions::new(focus))?;
                match (start, end) {
                    (Some(start), Some(end)) => Selection::new(start.start(), end.end(), false),
                    _ => return Ok(None),
                }
            }
        };

        Ok(Some(selection))
    }

    /// The working directory of the pane's current foreground process.
    pub fn cwd(&self) -> Option<PathBuf> {
        let pgid = self.master.process_group_leader()?;
        process_cwd(pgid)
    }

    /// Wait for the child to exit and return its status.
    pub fn wait(&mut self) -> Result<i32> {
        Ok(self.child.wait()?.exit_code() as i32)
    }

    pub fn shutdown(&mut self) {
        self.child.shutdown();
    }

    fn flush_responses(&mut self) -> Result<()> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Ok(());
        }
        self.writer.lock().unwrap().write_all(&responses)?;
        responses.clear();
        Ok(())
    }
}

fn forward_clipboard(effects: &Arc<Mutex<Vec<u8>>>, write: &ClipboardWrite<'_>) {
    let selector = match write.location() {
        ClipboardLocation::Standard => b'c',
        ClipboardLocation::Selection => b's',
        ClipboardLocation::Primary => b'p',
    };

    // Prefer a plain-text representation; fall back to the first one.
    let mut chosen = None;
    for content in write.contents() {
        if content.mime == b"text/plain" {
            chosen = Some(content);
            break;
        }
        chosen.get_or_insert(content);
    }

    let mut out = effects.lock().unwrap();
    out.extend_from_slice(b"\x1b]52;");
    out.push(selector);
    out.push(b';');
    if let Some(content) = chosen {
        base64::encode_into(content.data, &mut out);
    }
    out.push(0x07);
}

#[cfg(target_os = "linux")]
fn process_cwd(pid: libc::pid_t) -> Option<PathBuf> {
    let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    path.is_dir().then_some(path)
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidinfo(
        pid: libc::c_int,
        flavor: libc::c_int,
        arg: u64,
        buffer: *mut std::ffi::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn process_cwd(pid: libc::pid_t) -> Option<PathBuf> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
    // SAFETY: `info` points to `size` writable bytes and `pid` is a live
    // process group id returned by the pty. The kernel writes at most `size`
    // bytes.
    let written = unsafe {
        proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size as libc::c_int,
        )
    };
    if written != size as libc::c_int {
        return None;
    }
    // SAFETY: the kernel reported writing the whole structure.
    let info = unsafe { info.assume_init() };
    let path = &info.pvi_cdir.vip_path;
    // SAFETY: `vip_path` is a plain byte array that the kernel initialised.
    let bytes = unsafe {
        std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), std::mem::size_of_val(path))
    };
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let cwd = PathBuf::from(OsStr::from_bytes(&bytes[..end]));
    cwd.is_dir().then_some(cwd)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_cwd(_pid: libc::pid_t) -> Option<PathBuf> {
    None
}

fn mouse_button(button: MouseButton) -> mouse::Button {
    match button {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Middle => mouse::Button::Middle,
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn enables_kitty_image_file_mediums() {
        let (pane, _reader) = Pane::spawn(
            5,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 4,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();
        let term = pane.term();
        assert_eq!(term.is_kitty_image_from_file_allowed().unwrap(), Some(true));
        assert_eq!(
            term.kitty_image_temp_file_dir().unwrap(),
            Some(env::temp_dir().as_path())
        );
        assert_eq!(
            term.is_kitty_image_from_shared_mem_allowed().unwrap(),
            Some(true)
        );
    }

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn resize_flushes_in_band_size_reports() {
        let (mut pane, _reader) = Pane::spawn(
            10,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        pane.writer = Arc::new(Mutex::new(Box::new(SharedWriter(Arc::clone(&written)))));

        // Programs such as Neovim enable in-band resize reports and then rely
        // on them instead of SIGWINCH.
        pane.feed(b"\x1b[?2048h").unwrap();
        written.lock().unwrap().clear();

        pane.resize(PaneDims {
            cols: 100,
            rows: 30,
            cell_w: 10,
            cell_h: 20,
        })
        .unwrap();
        assert_eq!(
            written.lock().unwrap().as_slice(),
            b"\x1b[48;30;100;600;1000t"
        );

        pane.shutdown();
    }

    #[test]
    fn cell_word_and_line_selections_format() {
        let (mut pane, _reader) = Pane::spawn(
            4,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 25,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();
        pane.feed(b"hello world\r\nsecond line\r\n").unwrap();

        // A click without a drag clears instead of copying one cell.
        pane.selection_start(0, 0, SelectKind::Cell).unwrap();
        assert_eq!(pane.selection_finish().unwrap(), None);

        pane.selection_start(0, 0, SelectKind::Cell).unwrap();
        pane.selection_drag(4, 0).unwrap();
        assert_eq!(
            pane.selection_finish().unwrap().as_deref(),
            Some(&b"hello"[..])
        );

        // Drags are directional.
        pane.selection_start(4, 0, SelectKind::Cell).unwrap();
        pane.selection_drag(0, 0).unwrap();
        assert_eq!(
            pane.selection_finish().unwrap().as_deref(),
            Some(&b"hello"[..])
        );

        // Double-click a word.
        pane.selection_start(1, 0, SelectKind::Word).unwrap();
        assert_eq!(
            pane.selection_finish().unwrap().as_deref(),
            Some(&b"hello"[..])
        );

        // Double-click then drag selects to the nearest word.
        pane.selection_start(1, 0, SelectKind::Word).unwrap();
        pane.selection_drag(8, 0).unwrap();
        assert_eq!(
            pane.selection_finish().unwrap().as_deref(),
            Some(&b"hello world"[..])
        );

        // Triple-click selects the line, trimmed.
        pane.selection_start(1, 0, SelectKind::Line).unwrap();
        assert_eq!(
            pane.selection_finish().unwrap().as_deref(),
            Some(&b"hello world"[..])
        );

        pane.shutdown();
    }

    #[test]
    fn clipboard_writes_are_forwarded() {
        let (mut pane, _reader) = Pane::spawn(
            9,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        pane.feed(b"\x1b]52;c;Zm9v\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x1b]52;c;Zm9v\x07");

        pane.feed(b"\x1b]52;s;YmFy\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x1b]52;s;YmFy\x07");

        // An empty payload clears the destination.
        pane.feed(b"\x1b]52;p;\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x1b]52;p;\x07");

        pane.shutdown();
    }

    #[test]
    fn bell_and_notifications_become_outer_effects() {
        let (mut pane, _reader) = Pane::spawn(
            7,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        pane.feed(b"\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x07");
        assert!(pane.take_effects().is_empty());

        pane.feed(b"\x1b]9;build done\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x1b]9;build done\x07");

        pane.feed(b"\x1b]777;notify;title;").unwrap();
        assert!(pane.take_effects().is_empty());
        pane.feed(b"body\x1b\\").unwrap();
        assert_eq!(pane.take_effects(), b"\x1b]777;notify;title;body\x07");

        pane.feed(b"\x1b]777;notify;;4;1;50\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x1b]777;notify;;4;1;50\x07");

        // Effects remain in their original order, including across OSCs.
        pane.feed(b"\x07\x1b]9;done\x07\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x07\x1b]9;done\x07\x07");

        // Cancelled OSCs must not become notifications.
        pane.feed(b"\x1b]9;cancelled\x18\x07").unwrap();
        assert_eq!(pane.take_effects(), b"\x07");

        pane.shutdown();
    }

    #[test]
    fn progress_is_tracked_and_reports_keepalives() {
        let (mut pane, _reader) = Pane::spawn(
            8,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        assert_eq!(pane.progress(), None);
        assert!(!pane.take_progress_changed());

        pane.feed(b"\x1b]9;4;1;").unwrap();
        assert!(!pane.take_progress_changed());
        pane.feed(b"40\x07").unwrap();
        assert_eq!(
            pane.progress(),
            Some(Progress {
                state: ProgressState::Set,
                percent: Some(40),
            })
        );
        assert!(pane.take_progress_changed());

        pane.feed(b"\x1b]9;4;1;40\x07").unwrap();
        assert!(pane.take_progress_changed());

        for (sequence, state, percent) in [
            (&b"\x1b]9;4;2;25\x07"[..], ProgressState::Error, Some(25)),
            (&b"\x1b]9;4;3\x1b\\"[..], ProgressState::Indeterminate, None),
            (&b"\x1b]9;4;4;75\x07"[..], ProgressState::Pause, Some(75)),
        ] {
            pane.feed(sequence).unwrap();
            assert_eq!(pane.progress(), Some(Progress { state, percent }));
            assert!(pane.take_progress_changed());
        }

        pane.feed(b"\x1b]9;4;0\x07").unwrap();
        assert_eq!(pane.progress(), None);
        assert!(pane.take_progress_changed());

        pane.feed(b"\x1b]9;4;1;40\x18\x07").unwrap();
        assert_eq!(pane.progress(), None);
        assert!(!pane.take_progress_changed());

        pane.shutdown();
    }

    #[test]
    fn title_changes_are_tracked_including_clearing() {
        let (mut pane, _reader) = Pane::spawn(
            6,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        assert_eq!(pane.title(), None);

        pane.feed(b"\x1b]2;hello\x07").unwrap();
        assert_eq!(pane.title().as_deref(), Some("hello"));

        // A cleared title is distinct from one that was never set, so App can
        // forward the empty title and clear the tab title.
        pane.feed(b"\x1b]2;\x07").unwrap();
        assert_eq!(pane.title().as_deref(), Some(""));

        pane.shutdown();
    }

    #[test]
    fn enabling_focus_events_is_signalled_once() {
        let (mut pane, _reader) = Pane::spawn(
            1,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        assert!(!pane.take_focus_events_pending());

        pane.feed(b"\x1b[?1004h").unwrap();
        assert!(pane.take_focus_events_pending());
        assert!(!pane.take_focus_events_pending());

        pane.feed(b"\x1b[?1004l").unwrap();
        pane.feed(b"\x1b[?1004h").unwrap();
        assert!(pane.take_focus_events_pending());

        pane.shutdown();
    }

    #[test]
    fn scroll_moves_the_viewport_and_returns_to_live() {
        let (mut pane, _reader) = Pane::spawn(
            2,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        for i in 0..50 {
            pane.feed(format!("line {i}\r\n").as_bytes()).unwrap();
        }

        let bottom = pane.term().scrollbar().unwrap();
        assert!(bottom.total > bottom.len, "expected scrollback");
        assert_eq!(bottom.offset, bottom.total - bottom.len);

        pane.scroll(-3).unwrap();
        assert_eq!(pane.term().scrollbar().unwrap().offset, bottom.offset - 3);

        pane.scroll_to_live();
        assert_eq!(pane.term().scrollbar().unwrap().offset, bottom.offset);

        pane.shutdown();
    }

    #[test]
    fn applying_a_theme_updates_defaults_and_selection_colours() {
        use libghostty_vt::style::RgbColor;

        let (mut pane, _reader) = Pane::spawn(
            2,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        pane.writer = Arc::new(Mutex::new(Box::new(SharedWriter(Arc::clone(&written)))));

        let theme = Theme {
            bg: Some(RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            }),
            selection_fg: Some(RgbColor {
                r: 0xff,
                g: 0xff,
                b: 0xff,
            }),
            selection_bg: Some(RgbColor {
                r: 0x65,
                g: 0x43,
                b: 0x21,
            }),
            ..Default::default()
        };
        pane.apply_theme(Some(theme)).unwrap();
        assert_eq!(pane.paint_state.selection_bg, theme.selection_bg);
        assert!(pane.dirty);
        pane.feed(b"\x1b]11;?\x1b\\").unwrap();
        let got = String::from_utf8_lossy(&written.lock().unwrap()).to_string();
        assert!(got.contains("]11;rgb:1212/3434/5656"), "got {got:?}");

        written.lock().unwrap().clear();
        pane.apply_theme(None).unwrap();
        assert_eq!(pane.paint_state.selection_fg, None);
        assert_eq!(pane.paint_state.selection_bg, None);
        pane.feed(b"\x1b]11;?\x1b\\").unwrap();
        let got = String::from_utf8_lossy(&written.lock().unwrap()).to_string();
        assert!(!got.contains("]11;rgb:1212/3434/5656"), "got {got:?}");

        pane.shutdown();
    }

    #[test]
    fn alternate_screen_wheel_emulates_cursor_keys() {
        let (mut pane, _reader) = Pane::spawn(
            2,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();
        let written = Arc::new(Mutex::new(Vec::new()));
        pane.writer = Arc::new(Mutex::new(Box::new(SharedWriter(Arc::clone(&written)))));

        for i in 0..50 {
            pane.feed(format!("line {i}\r\n").as_bytes()).unwrap();
        }
        let bottom = pane.term().scrollbar().unwrap();
        written.lock().unwrap().clear();
        pane.wheel_scroll(-3).unwrap();
        assert_eq!(pane.term().scrollbar().unwrap().offset, bottom.offset - 3);
        assert!(written.lock().unwrap().is_empty());

        // Alternate-screen apps without mouse reporting get cursor keys.
        pane.feed(b"\x1b[?1049h").unwrap();
        written.lock().unwrap().clear();
        pane.wheel_scroll(-3).unwrap();
        assert_eq!(written.lock().unwrap().as_slice(), b"\x1b[A\x1b[A\x1b[A");

        // DECCKM switches to the application cursor-key encoding.
        pane.feed(b"\x1b[?1h").unwrap();
        written.lock().unwrap().clear();
        pane.wheel_scroll(2).unwrap();
        assert_eq!(written.lock().unwrap().as_slice(), b"\x1bOB\x1bOB");

        pane.shutdown();
    }

    #[test]
    fn selection_autoscroll_moves_the_viewport() {
        let (mut pane, _reader) = Pane::spawn(
            5,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        for i in 0..50 {
            pane.feed(format!("line {i}\r\n").as_bytes()).unwrap();
        }
        pane.scroll(-10).unwrap();
        let before = pane.term().scrollbar().unwrap().offset;

        pane.selection_start(0, 0, SelectKind::Cell).unwrap();
        pane.selection_drag(0, 4).unwrap();
        assert!(pane.selection_autoscroll().unwrap());
        assert_eq!(pane.term().scrollbar().unwrap().offset, before + 1);

        pane.shutdown();
    }

    #[test]
    fn detects_mouse_tracking() {
        let (mut pane, _reader) = Pane::spawn(
            3,
            &["/bin/sh".to_string()],
            None,
            PaneDims {
                cols: 20,
                rows: 5,
                cell_w: 8,
                cell_h: 16,
            },
            None,
        )
        .unwrap();

        assert!(!pane.tracks_mouse());
        pane.feed(b"\x1b[?1000h\x1b[?1006h").unwrap();
        assert!(pane.tracks_mouse());

        pane.shutdown();
    }
}
