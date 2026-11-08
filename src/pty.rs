use std::{
    env,
    io::{Read, Write},
    path::Path,
};

use anyhow::Result;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// A spawned child process attached to a pseudo-terminal.
pub struct Pty {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

pub fn spawn(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    rows: u16,
    cols: u16,
    pixel_width: u16,
    pixel_height: u16,
) -> Result<Pty> {
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width,
        pixel_height,
    })?;

    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    // portable-pty falls back to $HOME when no cwd is set. `None` should mean
    // "inherit haunt's cwd", which Ghostty set from the launching surface.
    let cwd = cwd
        .map(Path::to_path_buf)
        .or_else(|| env::current_dir().ok());
    if let Some(cwd) = &cwd {
        cmd.cwd(cwd);
    }

    // The inner application is talking to libghostty-vt, so Ghostty's native
    // protocols are supported by construction.
    if env::var_os("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }
    if env::var_os("COLORTERM").is_none() {
        cmd.env("COLORTERM", "truecolor");
    }

    let child = pair.slave.spawn_command(cmd)?;
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    Ok(Pty {
        master: pair.master,
        child,
        reader,
        writer,
    })
}
