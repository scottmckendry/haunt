use std::{
    env,
    io::{self, Read, Write},
    path::Path,
};

use anyhow::Result;
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

/// Owns the child from the moment it is spawned, including during setup.
pub struct ChildGuard(Option<Box<dyn Child + Send + Sync>>);

impl ChildGuard {
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.0.as_mut().expect("child already reaped").wait()?;
        self.0 = None;
        Ok(status)
    }

    pub fn shutdown(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };

        // portable-pty makes the child a session leader, so its pid is also
        // the process-group id. Signal the group rather than only the leader:
        // otherwise foreground and background descendants survive haunt.
        if let Some(pid) = child.process_id() {
            let pgid = pid as libc::pid_t;
            unsafe {
                libc::kill(-pgid, libc::SIGHUP);
            }
            // Give shells and well-behaved jobs a moment to exit cleanly
            // before killing anything that ignored the hangup. Reap only
            // after the final group signal so the pgid cannot be reused.
            std::thread::sleep(std::time::Duration::from_millis(100));
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        } else {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A spawned child process attached to a pseudo-terminal.
pub struct Pty {
    pub master: Box<dyn MasterPty + Send>,
    pub child: ChildGuard,
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

    let child = ChildGuard(Some(pair.slave.spawn_command(cmd)?));
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    Ok(Pty {
        master: pair.master,
        child,
        reader,
        writer,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn child_is_killed_and_reaped_when_setup_unwinds() {
        let pty = spawn(
            "/bin/sh",
            &["-c".into(), "exec sleep 30".into()],
            None,
            24,
            80,
            0,
            0,
        )
        .unwrap();
        let pid = pty.child.0.as_ref().unwrap().process_id().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pty = pty;
            panic!("simulate setup failure");
        }));
        assert!(result.is_err());
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn shutdown_kills_descendants_that_ignore_hangup() {
        let path = std::env::temp_dir().join(format!("haunt-pty-test-{}", std::process::id()));
        let script = format!(
            "trap '' HUP; sleep 30 & echo $! > '{}'; wait",
            path.display()
        );
        std::fs::remove_file(&path).ok();
        let mut pty = spawn("/bin/sh", &["-c".into(), script], None, 24, 80, 0, 0).unwrap();

        let descendant = (0..100)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
            })
            .expect("descendant pid was not written");
        assert!(Path::new(&format!("/proc/{descendant}")).exists());

        pty.child.shutdown();
        std::fs::remove_file(&path).ok();
        // SIGKILL makes the descendant a zombie until its new parent reaps
        // it, so allow /proc to settle before asserting it is gone.
        let gone = (0..100).any(|_| {
            if Path::new(&format!("/proc/{descendant}")).exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
                false
            } else {
                true
            }
        });
        assert!(gone, "descendant {descendant} survived shutdown");
    }

    #[test]
    fn wait_preserves_child_exit_status() {
        let mut pty = spawn(
            "/bin/sh",
            &["-c".into(), "exit 42".into()],
            None,
            24,
            80,
            0,
            0,
        )
        .unwrap();
        assert_eq!(pty.child.wait().unwrap().exit_code(), 42);
        pty.child.shutdown();
    }
}
