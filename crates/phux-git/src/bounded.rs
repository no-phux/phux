//! Run a child with a deadline and an output cap.
//!
//! The Changes panel reads a local checkout with no bound; a remote client
//! cannot be allowed to hold a server worker or memory that way. A child that
//! outlives its deadline is killed, and one that writes past the cap is
//! killed once the cap is reached, so neither a huge history nor a hung
//! credential helper can pin the caller.

use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(5);
const MAX_STDERR_BYTES: usize = 64 * 1024;
/// How long a finished child's pipes may stay open, held by a descendant it
/// left behind, before the readers are abandoned.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// What a bounded run produced.
#[derive(Debug)]
pub(super) struct Bounded {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    /// `stdout` stopped at the cap; the child was killed.
    pub(super) truncated: bool,
    pub(super) stderr: String,
}

/// Why a bounded run produced nothing.
#[derive(Debug)]
pub(super) enum RunError {
    /// The program could not start; `NotFound` means it is not installed.
    Spawn(std::io::Error),
    /// The deadline passed and the child was killed.
    Timeout,
    /// Waiting on the child failed.
    Wait(String),
}

/// One program invocation with its bounds.
pub(super) struct Run<'a> {
    pub(super) program: &'a str,
    pub(super) dir: &'a Path,
    pub(super) args: &'a [&'a str],
    pub(super) env: &'a [(&'a str, &'a str)],
    pub(super) timeout: Duration,
    pub(super) max_stdout: usize,
    /// Bytes written to the child's stdin, which is otherwise closed.
    pub(super) stdin: Option<Vec<u8>>,
}

/// Inherited variables that would point Git at another repository or index.
const REDIRECTING_ENV: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
];

impl Run<'_> {
    pub(super) fn exec(&self) -> Result<Bounded, RunError> {
        let mut command = Command::new(self.program);
        command
            .current_dir(self.dir)
            .args(self.args)
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group, so a kill reaches hooks and helpers it spawned.
            .process_group(0);
        for key in REDIRECTING_ENV {
            command.env_remove(key);
        }
        for (key, value) in self.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(RunError::Spawn)?;
        if let (Some(bytes), Some(mut pipe)) = (self.stdin.clone(), child.stdin.take()) {
            // A writer thread, so a child that stops reading cannot block
            // the deadline; it ends when the child exits or is killed.
            std::thread::spawn(move || {
                use std::io::Write as _;
                let _ = pipe.write_all(&bytes);
            });
        }
        let stdout = Capture::start(child.stdout.take(), self.max_stdout);
        let stderr = Capture::start(child.stderr.take(), MAX_STDERR_BYTES);
        let status = wait(&mut child, &stdout, Instant::now() + self.timeout)?;
        let grace = Instant::now() + DRAIN_GRACE;
        let (stdout, truncated) = stdout.finish(grace);
        let (stderr, _) = stderr.finish(grace);
        Ok(Bounded {
            status,
            stdout,
            truncated,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

/// One output pipe read on its own thread into a shared, capped buffer, so
/// the bytes read so far survive a reader that never sees end of stream.
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    capped: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Capture {
    fn start(pipe: Option<impl Read + Send + 'static>, cap: usize) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let capped = Arc::new(AtomicBool::new(false));
        let reader = pipe.map(|mut pipe| {
            let (bytes, capped) = (Arc::clone(&bytes), Arc::clone(&capped));
            std::thread::spawn(move || {
                let mut chunk = vec![0_u8; 64 * 1024];
                loop {
                    let read = match pipe.read(&mut chunk) {
                        Ok(0) => return,
                        Ok(read) => read,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => return,
                    };
                    let room = {
                        let mut held = bytes.lock().unwrap_or_else(PoisonError::into_inner);
                        let room = cap.saturating_sub(held.len());
                        held.extend_from_slice(&chunk[..read.min(room)]);
                        room
                    };
                    if read > room {
                        capped.store(true, Ordering::Release);
                        return;
                    }
                }
            })
        });
        Self {
            bytes,
            capped,
            reader,
        }
    }

    /// Whether more than the cap was written.
    fn capped(&self) -> bool {
        self.capped.load(Ordering::Acquire)
    }

    /// The bytes read, once the reader ends or `grace` passes. A descendant
    /// still holding the pipe past `grace` leaves the reader detached; what
    /// it read so far is kept.
    fn finish(self, grace: Instant) -> (Vec<u8>, bool) {
        if let Some(reader) = self.reader {
            while !reader.is_finished() && Instant::now() < grace {
                std::thread::sleep(POLL);
            }
        }
        let bytes = std::mem::take(&mut *self.bytes.lock().unwrap_or_else(PoisonError::into_inner));
        (bytes, self.capped.load(Ordering::Acquire))
    }
}

/// Wait for exit, killing the child at the deadline or once stdout passed
/// its cap (nothing more would be read, and a blocked writer never exits).
fn wait(child: &mut Child, stdout: &Capture, deadline: Instant) -> Result<ExitStatus, RunError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                kill(child);
                return Err(RunError::Wait(error.to_string()));
            }
        }
        if stdout.capped() {
            kill(child);
            return child
                .wait()
                .map_err(|error| RunError::Wait(error.to_string()));
        }
        if Instant::now() >= deadline {
            kill(child);
            return Err(RunError::Timeout);
        }
        std::thread::sleep(POLL);
    }
}

fn kill(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: `kill(2)` on the child's own process group, which
        // `process_group(0)` made equal to its pid; no memory is involved.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration, max_stdout: usize) -> Result<Bounded, RunError> {
        Run {
            program: "sh",
            dir: Path::new("/"),
            args: &["-c", script],
            env: &[],
            timeout,
            max_stdout,
            stdin: None,
        }
        .exec()
    }

    #[test]
    fn output_within_the_cap_is_whole() {
        let out = sh("printf hello; printf oops >&2", Duration::from_secs(5), 16).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello");
        assert!(!out.truncated);
        assert_eq!(out.stderr, "oops");
    }

    #[test]
    fn output_past_the_cap_is_cut_and_the_writer_stopped() {
        let started = Instant::now();
        let out = sh("yes", Duration::from_secs(10), 1024).unwrap();
        assert_eq!(out.stdout.len(), 1024);
        assert!(out.truncated);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn output_exactly_at_the_cap_is_whole() {
        let out = sh("printf 1234", Duration::from_secs(5), 4).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"1234");
        assert!(!out.truncated);
    }

    #[test]
    fn a_descendant_holding_the_pipe_does_not_lose_the_output() {
        let started = Instant::now();
        let out = sh("printf kept; sleep 5 &", Duration::from_secs(5), 64).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"kept");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_hung_child_is_killed_at_the_deadline() {
        let started = Instant::now();
        let out = sh("sleep 30", Duration::from_millis(100), 16);
        assert!(matches!(out, Err(RunError::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let out = Run {
            program: "phux-definitely-not-installed",
            dir: Path::new("/"),
            args: &[],
            env: &[],
            timeout: Duration::from_secs(1),
            max_stdout: 1,
            stdin: None,
        }
        .exec();
        assert!(
            matches!(out, Err(RunError::Spawn(ref error)) if error.kind() == std::io::ErrorKind::NotFound)
        );
    }
}
