use std::{
    env,
    io::{self, Read, Write},
    path::Path,
    sync::{OnceLock, mpsc},
};

use anyhow::Result;
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

/// Owns the child from the moment it is spawned, including during setup.
pub struct ChildGuard(Option<Box<dyn Child + Send + Sync>>);

impl ChildGuard {
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        match self.0.as_mut() {
            Some(child) => {
                let status = child.wait()?;
                self.0 = None;
                Ok(status)
            }
            // The shutdown reaper already reaped the child while an exit
            // notification raced it. Report a generic failure; the real
            // status is unrecoverable but the caller cannot block here.
            None => Ok(ExitStatus::with_exit_code(1)),
        }
    }

    /// The child's process id, if it is still owned.
    pub fn pid(&self) -> Option<u32> {
        self.0.as_ref().and_then(|child| child.process_id())
    }

    /// Queue the child for shutdown without blocking the caller.
    ///
    /// The grace period, final signal, and reaping all happen on the reaper
    /// thread so tearing down a pane never stalls rendering or input.
    pub fn shutdown(&mut self) {
        let Some(child) = self.0.take() else {
            return;
        };
        let sid = child.process_id().map(|pid| pid as libc::pid_t);
        let job = ReapJob::Terminate { child, sid };
        if let Err(mpsc::SendError(ReapJob::Terminate { mut child, sid })) = reaper().send(job) {
            // The reaper is gone; finish synchronously so the child cannot
            // outlive us.
            terminate(child.as_mut(), sid);
        }
    }
}

enum ReapJob {
    Terminate {
        child: Box<dyn Child + Send + Sync>,
        sid: Option<libc::pid_t>,
    },
    /// Wait for every job queued before this one to finish.
    Flush(mpsc::Sender<()>),
}

fn reaper() -> &'static mpsc::Sender<ReapJob> {
    static REAPER: OnceLock<mpsc::Sender<ReapJob>> = OnceLock::new();
    REAPER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<ReapJob>();
        std::thread::Builder::new()
            .name("haunt-pty-reaper".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    match job {
                        ReapJob::Terminate { mut child, sid } => {
                            terminate(child.as_mut(), sid);
                        }
                        ReapJob::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .expect("failed to start pty reaper thread");
        tx
    })
}

/// Block until all previously queued child shutdowns have completed.
///
/// Call this before process exit so children still receive their final signal
/// and are reaped even though normal shutdown is asynchronous.
pub fn flush_shutdowns() {
    let (done, wait) = mpsc::channel();
    if reaper().send(ReapJob::Flush(done)).is_ok() {
        let _ = wait.recv();
    }
}

fn terminate(child: &mut dyn Child, sid: Option<libc::pid_t>) {
    // portable-pty makes the child a session leader, so its pid is also the
    // session id. Interactive shells put each job in its own process group,
    // so signalling only `-pid` would leave those jobs running. Signal every
    // process still in the session instead.
    if let Some(sid) = sid {
        signal_session(sid, libc::SIGHUP);
        // Give shells and well-behaved jobs a moment to exit cleanly before
        // killing anything that ignored the hangup. Reap only after the final
        // signal so the session id cannot be reused.
        std::thread::sleep(std::time::Duration::from_millis(100));
        signal_session(sid, libc::SIGKILL);
    } else {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// Signal the child's process group and every other process in its session.
///
/// Processes that call `setsid` themselves leave the session and are expected
/// to survive; that is how a daemon detaches from its terminal.
fn signal_session(sid: libc::pid_t, signal: libc::c_int) {
    unsafe {
        libc::kill(-sid, signal);
    }
    for pid in session_pids(sid) {
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

#[cfg(target_os = "linux")]
fn session_pids(sid: libc::pid_t) -> Vec<libc::pid_t> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter(|&pid| pid > 0 && unsafe { libc::getsid(pid) } == sid)
        .collect()
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_listallpids(buffer: *mut std::ffi::c_void, buffersize: libc::c_int) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn session_pids(sid: libc::pid_t) -> Vec<libc::pid_t> {
    let bytes = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Vec::new();
    }

    let mut pids = vec![0; bytes as usize / std::mem::size_of::<libc::pid_t>() + 32];
    let bytes = unsafe {
        proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if bytes <= 0 {
        return Vec::new();
    }

    pids.truncate((bytes as usize / std::mem::size_of::<libc::pid_t>()).min(pids.len()));
    pids.retain(|&pid| pid > 0 && unsafe { libc::getsid(pid) } == sid);
    pids
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn session_pids(_sid: libc::pid_t) -> Vec<libc::pid_t> {
    Vec::new()
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
        flush_shutdowns();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn shutdown_kills_descendants_that_ignore_hangup() {
        let path = std::env::temp_dir().join(format!("haunt-pty-test-{}", std::process::id()));
        let script = format!(
            // Job control gives the background job its own process group, so
            // signalling only the shell's group leaves it running.
            "set -m; trap '' HUP; sleep 30 & echo $! > '{}'; wait",
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
        flush_shutdowns();
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
