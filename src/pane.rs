//! One PTY-backed pane: terminal state, PTY, child process, render state.

use std::{
    env,
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Result;
use libghostty_vt::{
    RenderState, Terminal, TerminalOptions,
    render::{CellIterator, RowIterator},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode,
        PrimaryDeviceAttributes, SecondaryDeviceAttributes, SizeReportSize,
    },
};
use portable_pty::{Child, MasterPty, PtySize};

use crate::{
    paint::{self, Area},
    pty,
};

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
    child: Box<dyn Child + Send + Sync>,
    render_state: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    responses: Arc<Mutex<Vec<u8>>>,
    dims: Arc<Mutex<PaneDims>>,
    exited: bool,
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
        let dims_shared = Arc::new(Mutex::new(dims));

        let mut term: Terminal<'static, 'static> = Terminal::new(TerminalOptions {
            cols: dims.cols,
            rows: dims.rows,
            max_scrollback: 10_000,
        })?;
        term.resize(dims.cols, dims.rows, dims.cell_w as u32, dims.cell_h as u32)?;

        {
            let responses = Arc::clone(&responses);
            term.on_pty_write(move |_term, data| {
                responses.lock().unwrap().extend_from_slice(data);
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
                responses,
                dims: dims_shared,
                exited: false,
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
        *self.dims.lock().unwrap() = dims;
        self.dirty = true;
        Ok(())
    }

    pub fn paint(&mut self, out: &mut Vec<u8>, area: Area, focused: bool) -> Result<()> {
        let snapshot = self.render_state.update(&self.term)?;
        paint::draw(
            &snapshot,
            &mut self.rows_it,
            &mut self.cells_it,
            out,
            area,
            self.force_full,
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

    /// The working directory of the pane's current foreground process.
    pub fn cwd(&self) -> Option<PathBuf> {
        let pgid = self.master.process_group_leader()?;
        let path = std::fs::read_link(format!("/proc/{pgid}/cwd")).ok()?;
        path.is_dir().then_some(path)
    }

    /// Wait for the child to exit and return its status.
    pub fn wait(&mut self) -> Result<i32> {
        let status = self.child.wait()?;
        self.exited = true;
        Ok(status.exit_code() as i32)
    }

    pub fn shutdown(&mut self) {
        if self.exited {
            return;
        }
        self.exited = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
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

impl Drop for Pane {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

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
}
