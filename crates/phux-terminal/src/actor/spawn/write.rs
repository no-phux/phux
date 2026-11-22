//! The PTY writer: resilient writes, the canonical-mode line guard, and
//! servicing one queued input request.

use crate::actor::{EncodedInputRequest, WriteCompletion};
use nix::sys::termios::{InputFlags, LocalFlags};
use nix::unistd::{PathconfVar, fpathconf};
use portable_pty::MasterPty;
use std::io::Write;
use std::sync::Mutex;
use tracing::{debug, error};

/// `EIO`: the slave side is gone. `libc` is macOS-only here, so spell it.
pub(in crate::actor) const EIO: i32 = 5;

/// Why the writer gave up on a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::actor) enum WriteFailure {
    /// The child is gone (`EIO` / `EPIPE` / a zero-length write). Expected
    /// during teardown; the reader thread reports EOF on its own path.
    PaneGone,
    /// Anything else. The pane's input path is dead and the user must be
    /// told, because every other signal keeps looking healthy.
    Fatal,
}

/// A failed PTY write and how many bytes the child already received.
#[derive(Debug)]
pub(in crate::actor) struct WriteError {
    pub(in crate::actor) failure: WriteFailure,
    source: std::io::Error,
    /// Bytes written before the failure; non-zero means a truncated prefix.
    pub(super) written: usize,
}

/// Classify a PTY write/flush error: child gone (routine) versus a real
/// fault that kills the pane's input path.
pub(super) fn classify_write_error(err: &std::io::Error) -> WriteFailure {
    if err.raw_os_error() == Some(EIO) || err.kind() == std::io::ErrorKind::BrokenPipe {
        WriteFailure::PaneGone
    } else {
        WriteFailure::Fatal
    }
}

/// Write every byte, resuming partial writes and retrying `Interrupted`,
/// and report how much landed on failure (which [`Write::write_all`] can't).
#[cfg(test)]
pub(in crate::actor) fn write_all_resilient(
    writer: &mut (dyn Write + Send),
    bytes: &[u8],
) -> Result<(), WriteError> {
    write_all_retry(writer, bytes, None)
}

/// [`write_all_resilient`] for the live master, whose reader shares its
/// `O_NONBLOCK` status flag with the writer.
/// `EAGAIN` waits for output space instead of killing the pane's input.
pub(in crate::actor) fn write_all_pty(
    writer: &mut (dyn Write + Send),
    master: &Mutex<Box<dyn MasterPty + Send>>,
    bytes: &[u8],
) -> Result<(), WriteError> {
    write_all_retry(writer, bytes, Some(master))
}

fn write_all_retry(
    writer: &mut (dyn Write + Send),
    bytes: &[u8],
    master: Option<&Mutex<Box<dyn MasterPty + Send>>>,
) -> Result<(), WriteError> {
    let mut written = 0_usize;
    while written < bytes.len() {
        match writer.write(&bytes[written..]) {
            // No progress possible: treat as the child having gone away.
            Ok(0) => {
                return Err(WriteError {
                    failure: WriteFailure::PaneGone,
                    source: std::io::Error::from(std::io::ErrorKind::WriteZero),
                    written,
                });
            }
            Ok(n) => written += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                let Some(master) = master else {
                    return Err(WriteError {
                        failure: classify_write_error(&err),
                        source: err,
                        written,
                    });
                };
                wait_pty_writable(master);
            }
            Err(err) => {
                return Err(WriteError {
                    failure: classify_write_error(&err),
                    source: err,
                    written,
                });
            }
        }
    }
    Ok(())
}

/// Flush, retrying `Interrupted` and classifying the rest like a write.
#[cfg(test)]
pub(in crate::actor) fn flush_resilient(writer: &mut (dyn Write + Send)) -> Result<(), WriteError> {
    flush_retry(writer, None)
}

/// [`flush_resilient`] that waits out `EAGAIN` on the nonblocking master.
pub(in crate::actor) fn flush_pty(
    writer: &mut (dyn Write + Send),
    master: &Mutex<Box<dyn MasterPty + Send>>,
) -> Result<(), WriteError> {
    flush_retry(writer, Some(master))
}

fn flush_retry(
    writer: &mut (dyn Write + Send),
    master: Option<&Mutex<Box<dyn MasterPty + Send>>>,
) -> Result<(), WriteError> {
    loop {
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                let Some(master) = master else {
                    return Err(WriteError {
                        failure: classify_write_error(&err),
                        source: err,
                        written: 0,
                    });
                };
                wait_pty_writable(master);
            }
            Err(err) => {
                return Err(WriteError {
                    failure: classify_write_error(&err),
                    source: err,
                    written: 0,
                });
            }
        }
    }
}

/// Block until the nonblocking master can accept a write, or a short slice
/// elapses. The slice bounds a master that never becomes writable; the next
/// `write(2)` then reports the real error.
fn wait_pty_writable(master: &Mutex<Box<dyn MasterPty + Send>>) {
    use std::os::fd::{BorrowedFd, RawFd};
    let raw: Option<RawFd> = master
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_raw_fd();
    let Some(raw) = raw else {
        std::thread::sleep(std::time::Duration::from_millis(1));
        return;
    };
    // SAFETY: `raw` is the pane's live PTY master. The mutex guard ended
    // above; the descriptor stays open for the pane's life, and this borrow
    // lasts only for the `poll` call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    let mut fds = [nix::poll::PollFd::new(
        borrowed,
        nix::poll::PollFlags::POLLOUT,
    )];
    let _ = nix::poll::poll(&mut fds, nix::poll::PollTimeout::from(50u16));
}

/// `_POSIX_MAX_CANON`: the minimum canonical line length POSIX guarantees;
/// the fallback when `fpathconf(_PC_MAX_CANON)` is unavailable. Writes up to
/// this size cannot overflow, so they skip the termios query entirely.
const POSIX_MAX_CANON_FLOOR: usize = 255;

/// Why [`canonical_refusal`] refused to hand a payload to the PTY writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::actor) struct CanonicalOverflow {
    /// The pane's canonical-line byte limit at refusal time.
    pub(in crate::actor) limit: usize,
}

/// Does `bytes` contain a line longer than `limit` for a canonical-mode
/// (`ICANON`) line discipline? The queue resets per line, so only the
/// longest run between terminators matters. An overlong line also loses its
/// own terminator, wedging the pane (phux-mjmc).
///
/// `\n` always terminates; `\r` only when `ICRNL` translates it. `VEOL` and
/// `VEOF` are not modeled.
pub(super) fn exceeds_canonical_limit(bytes: &[u8], limit: usize, cr_terminates: bool) -> bool {
    let mut run = 0_usize;
    for &b in bytes {
        let terminates = b == b'\n' || (cr_terminates && b == b'\r');
        if terminates {
            run = 0;
        } else {
            run += 1;
            if run > limit {
                return true;
            }
        }
    }
    false
}

/// If the pane is in canonical mode and `bytes` would overflow it, say why;
/// `None` means write normally, including when termios cannot be read.
///
/// Calls `tcgetattr` on the raw fd rather than `MasterPty::get_termios`:
/// portable-pty's `nix` version differs from ours, and adopted masters
/// return `None` from `get_termios`.
pub(in crate::actor) fn canonical_refusal(
    master: &Mutex<Box<dyn MasterPty + Send>>,
    bytes: &[u8],
) -> Option<CanonicalOverflow> {
    if bytes.len() <= POSIX_MAX_CANON_FLOOR {
        return None;
    }
    let raw_fd = master.lock().ok()?.as_raw_fd()?;
    // SAFETY: `raw_fd` is the pane's live PTY master fd, owned by the
    // `PtyOwned::master` this function is always called through and open
    // for the pane's whole lifetime. The borrow does not outlive this
    // synchronous call and is never used to close or duplicate the fd.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
    let termios = nix::sys::termios::tcgetattr(borrowed).ok()?;
    if !termios.local_flags.contains(LocalFlags::ICANON) {
        return None;
    }
    let limit = canonical_limit(borrowed);
    let cr_terminates = termios.input_flags.contains(InputFlags::ICRNL)
        && !termios.input_flags.contains(InputFlags::IGNCR);
    exceeds_canonical_limit(bytes, limit, cr_terminates).then_some(CanonicalOverflow { limit })
}

/// The pane's canonical-line byte limit: `fpathconf(_PC_MAX_CANON)`, but no
/// lower than the platform's known queue size. Linux reports the 255-byte
/// POSIX floor while `N_TTY` buffers 4096; refusing pastes the kernel would
/// accept is worse than missing a narrow overflow band.
pub(super) fn canonical_limit(fd: std::os::fd::BorrowedFd<'_>) -> usize {
    /// Linux's `N_TTY_BUF_SIZE`, the real canonical queue capacity its
    /// `fpathconf` under-reports as the POSIX floor.
    const LINUX_N_TTY_BUF_SIZE: usize = 4096;

    let reported = fpathconf(fd, PathconfVar::MAX_CANON)
        .ok()
        .flatten()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|&value| value > 0)
        .unwrap_or(POSIX_MAX_CANON_FLOOR);
    let known_floor = if cfg!(target_os = "linux") {
        LINUX_N_TTY_BUF_SIZE
    } else {
        POSIX_MAX_CANON_FLOOR
    };
    reported.max(known_floor)
}

/// What the writer thread does after one request.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::actor) enum WriterLoopControl {
    /// Keep looping (success, or a refusal that leaves input alive).
    Continue,
    /// The child is gone or input hit a fatal error.
    Stop,
}

/// Handle one [`EncodedInputRequest`] on the writer thread: refuse it if it
/// would overflow the canonical line discipline, else write and flush it,
/// reporting the outcome on `request.completion`.
pub(in crate::actor) fn service_write_request(
    writer: &mut (dyn Write + Send),
    master: &Mutex<Box<dyn MasterPty + Send>>,
    request: EncodedInputRequest,
) -> WriterLoopControl {
    if let Some(queued_at) = request.writer_queued_at {
        crate::perf::INPUT_WRITER_QUEUE_WAIT.record_elapsed(queued_at);
    }
    let len = request.bytes.len();
    if let Some(overflow) = canonical_refusal(master, &request.bytes) {
        crate::perf::INPUT_CANONICAL_REFUSED.incr();
        // Refuse before writing: zero bytes beat a truncated, uncompletable
        // line that wedges the pane. Input stays alive.
        error!(
            len,
            limit = overflow.limit,
            "pty writer: refusing write; pane is in canonical mode and this \
             payload has no line terminator within its canonical-line limit, \
             so writing it would silently truncate rather than deliver it — \
             send it as newline-terminated lines, or switch the pane to raw \
             mode first",
        );
        if let Some(completion) = request.completion {
            completion.complete(WriteCompletion::CanonicalLimitExceeded {
                limit: overflow.limit,
            });
        }
        return WriterLoopControl::Continue;
    }
    let write_started = std::time::Instant::now();
    let outcome =
        write_all_pty(writer, master, &request.bytes).and_then(|()| flush_pty(writer, master));
    crate::perf::INPUT_PTY_WRITE.record_elapsed(write_started);
    match outcome {
        Ok(()) => {
            phux_perf::echo_hop::record("pty_write");
            debug!(len, "pty write flushed");
            if let Some(completion) = request.completion {
                completion.complete(WriteCompletion::Delivered);
            }
            WriterLoopControl::Continue
        }
        Err(WriteError {
            failure: WriteFailure::PaneGone,
            source,
            written,
        }) => {
            // Routine teardown; the reader reports EOF on its own path.
            debug!(
                ?source,
                written, len, "pty writer: child gone; input path closing"
            );
            if let Some(completion) = request.completion {
                completion.complete(WriteCompletion::Failed);
            }
            WriterLoopControl::Stop
        }
        Err(WriteError {
            failure: WriteFailure::Fatal,
            source,
            written,
        }) => {
            // Input is dead while output keeps working, so say so loudly.
            error!(
                ?source,
                written, len, "pty writer: write failed; pane input is now dead"
            );
            if let Some(completion) = request.completion {
                completion.complete(WriteCompletion::Failed);
            }
            WriterLoopControl::Stop
        }
    }
}
