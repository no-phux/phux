//! Record a sidecar in its pidfile before it can run.
//!
//! The forked child sends its pid, then blocks before `exec` until the
//! server has written the pidfile. A server killed in between closes the
//! gate's write end, so the child exits instead of running unrecorded where
//! the next start's orphan reap cannot find it.

use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::os::fd::AsRawFd;

pub(super) struct Gate {
    pid_rx: PipeReader,
    pid_tx: PipeWriter,
    go_rx: PipeReader,
    go_tx: PipeWriter,
}

/// The descriptors the forked child uses. Plain numbers, so the `pre_exec`
/// closure allocates nothing.
#[derive(Clone, Copy)]
pub(super) struct ChildEnd {
    pid_tx: i32,
    go_rx: i32,
    go_tx: i32,
}

impl Gate {
    /// Both pipes are close-on-exec, so the sidecar never inherits them.
    pub(super) fn new() -> io::Result<Self> {
        let (pid_rx, pid_tx) = io::pipe()?;
        let (go_rx, go_tx) = io::pipe()?;
        Ok(Self {
            pid_rx,
            pid_tx,
            go_rx,
            go_tx,
        })
    }

    pub(super) fn child_end(&self) -> ChildEnd {
        ChildEnd {
            pid_tx: self.pid_tx.as_raw_fd(),
            go_rx: self.go_rx.as_raw_fd(),
            go_tx: self.go_tx.as_raw_fd(),
        }
    }

    /// Run `spawn` (which forks a child that calls [`ChildEnd::wait`]) and,
    /// on a second thread, `record` the child's pid before releasing it.
    /// `spawn` blocks until the child execs, so the release cannot happen
    /// on this thread.
    pub(super) fn run<T>(self, spawn: impl FnOnce() -> T, record: impl FnOnce(i32) + Send) -> T {
        let Self {
            mut pid_rx,
            pid_tx,
            go_rx,
            mut go_tx,
        } = self;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let mut pid = [0_u8; 4];
                // EOF: the child failed before the gate and reported it to
                // `spawn` instead.
                if pid_rx.read_exact(&mut pid).is_ok() {
                    record(i32::from_ne_bytes(pid));
                    let _ = go_tx.write_all(&[1]);
                }
            });
            let out = spawn();
            // The child has exec'd or died; our write end is the last one,
            // so dropping it ends the recorder's read if it is still waiting.
            drop(pid_tx);
            drop(go_rx);
            out
        })
    }
}

impl ChildEnd {
    /// Report this pid and wait for the server to release us.
    ///
    /// # Safety
    ///
    /// Call only in the forked child before `exec`. Every call is
    /// async-signal-safe and nothing allocates.
    pub(super) unsafe fn wait(self) -> io::Result<()> {
        // SAFETY: the caller is the forked child. Closing our copy of the
        // write end lets the read below see EOF once the server is gone.
        unsafe {
            libc::close(self.go_tx);
            let pid = libc::getpid().to_ne_bytes();
            if libc::write(self.pid_tx, pid.as_ptr().cast(), pid.len()) != 4 {
                return Err(io::Error::last_os_error());
            }
            let mut go = 0_u8;
            loop {
                match libc::read(self.go_rx, (&raw mut go).cast(), 1) {
                    1 => return Ok(()),
                    0 => return Err(io::Error::from_raw_os_error(libc::EPIPE)),
                    _ => {
                        let err = io::Error::last_os_error();
                        if err.kind() != io::ErrorKind::Interrupted {
                            return Err(err);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    use super::Gate;

    #[test]
    fn the_record_lands_before_the_child_runs() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record");
        let ran = dir.path().join("ran");
        let gate = Gate::new().unwrap();
        let end = gate.child_end();
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "test -e '{}' && touch '{}'",
            record.display(),
            ran.display()
        ));
        // SAFETY: `wait` is async-signal-safe.
        unsafe { cmd.pre_exec(move || end.wait()) };
        let mut child = gate
            .run(
                || cmd.spawn(),
                |pid| std::fs::write(&record, pid.to_string()).unwrap(),
            )
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert!(ran.exists(), "the child ran before its record was written");
    }

    #[test]
    fn a_server_gone_before_the_record_means_the_child_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let gate = Gate::new().unwrap();
        let end = gate.child_end();
        let Gate {
            pid_rx: _pid_rx,
            pid_tx: _pid_tx,
            go_rx: _go_rx,
            go_tx,
        } = gate;
        // The server's write end is gone, as after a `SIGKILL`.
        drop(go_tx);
        let mut cmd = Command::new("/usr/bin/touch");
        cmd.arg(&ran);
        // SAFETY: `wait` is async-signal-safe.
        unsafe { cmd.pre_exec(move || end.wait()) };
        assert!(cmd.spawn().is_err(), "an unreleased child must not exec");
        assert!(!ran.exists());
    }
}
