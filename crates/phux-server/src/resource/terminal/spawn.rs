//! PTY spawn/adopt, the reader and writer bridge threads, and pane command
//! construction.

use super::{EncodedInputRequest, TerminalActorError, WriteCompletion};
use nix::sys::termios::{InputFlags, LocalFlags};
use nix::unistd::{PathconfVar, fpathconf};
use phux_core::process::ExitOutcome;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

/// PTY read chunk ceiling. macOS returns at most 1024 bytes per read anyway;
/// this is sized for platforms that can fill it while keeping
/// `PTY_READ_CHUNK * PTY_CHANNEL_DEPTH` a sane per-pane memory bound.
pub(super) const PTY_READ_CHUNK: usize = 16 * 1024;

/// Depth of the reader-thread -> actor PTY channel.
///
/// Bounded so a runaway producer gets backpressure (the reader blocks, the
/// child stalls in the line discipline) instead of unbounded memory. The
/// actor never waits on the reader, so blocking it cannot deadlock. The
/// depth must exceed `MAX_PTY_COALESCE` or it throttles the batcher.
pub(super) const PTY_CHANNEL_DEPTH: usize = 128;

/// `EIO`: the slave side is gone. `libc` is macOS-only here, so spell it.
pub(super) const EIO: i32 = 5;

/// Why the writer gave up on a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteFailure {
    /// The child is gone (`EIO` / `EPIPE` / a zero-length write). Expected
    /// during teardown; the reader thread reports EOF on its own path.
    PaneGone,
    /// Anything else. The pane's input path is dead and the user must be
    /// told, because every other signal keeps looking healthy.
    Fatal,
}

/// A failed PTY write and how many bytes the child already received.
#[derive(Debug)]
struct WriteError {
    failure: WriteFailure,
    source: std::io::Error,
    /// Bytes written before the failure; non-zero means a truncated prefix.
    written: usize,
}

/// Classify a PTY write/flush error: child gone (routine) versus a real
/// fault that kills the pane's input path.
fn classify_write_error(err: &std::io::Error) -> WriteFailure {
    if err.raw_os_error() == Some(EIO) || err.kind() == std::io::ErrorKind::BrokenPipe {
        WriteFailure::PaneGone
    } else {
        WriteFailure::Fatal
    }
}

/// Write every byte, resuming partial writes and retrying `Interrupted`,
/// and report how much landed on failure (which [`Write::write_all`] can't).
fn write_all_resilient(writer: &mut (dyn Write + Send), bytes: &[u8]) -> Result<(), WriteError> {
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
fn flush_resilient(writer: &mut (dyn Write + Send)) -> Result<(), WriteError> {
    loop {
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
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

/// `_POSIX_MAX_CANON`: the minimum canonical line length POSIX guarantees;
/// the fallback when `fpathconf(_PC_MAX_CANON)` is unavailable. Writes up to
/// this size cannot overflow, so they skip the termios query entirely.
const POSIX_MAX_CANON_FLOOR: usize = 255;

/// Why [`canonical_refusal`] refused to hand a payload to the PTY writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CanonicalOverflow {
    /// The pane's canonical-line byte limit at refusal time.
    limit: usize,
}

/// Does `bytes` contain a line longer than `limit` for a canonical-mode
/// (`ICANON`) line discipline? The queue resets per line, so only the
/// longest run between terminators matters. An overlong line also loses its
/// own terminator, wedging the pane (phux-mjmc).
///
/// `\n` always terminates; `\r` only when `ICRNL` translates it. `VEOL` and
/// `VEOF` are not modeled.
fn exceeds_canonical_limit(bytes: &[u8], limit: usize, cr_terminates: bool) -> bool {
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
fn canonical_refusal(
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
fn canonical_limit(fd: std::os::fd::BorrowedFd<'_>) -> usize {
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
enum WriterLoopControl {
    /// Keep looping (success, or a refusal that leaves input alive).
    Continue,
    /// The child is gone or input hit a fatal error.
    Stop,
}

/// Handle one [`EncodedInputRequest`] on the writer thread: refuse it if it
/// would overflow the canonical line discipline, else write and flush it,
/// reporting the outcome on `request.completion`.
fn service_write_request(
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
        write_all_resilient(writer, &request.bytes).and_then(|()| flush_resilient(writer));
    crate::perf::INPUT_PTY_WRITE.record_elapsed(write_started);
    match outcome {
        Ok(()) => {
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

/// PTY-side resources of a [`TerminalActor`](crate::terminal_actor::TerminalActor).
///
/// Drop order is the reverse of declaration: writer thread (VEOF), reader
/// dup, child, then the master. Dropping the reader hangs up a master the
/// poller was still holding.
pub(crate) struct PtyOwned {
    /// Master handle, kept for resize ioctls and the writer's termios checks.
    pub(crate) master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child on the slave side, reaped in `TerminalActor::shutdown_pty`.
    pub(crate) child: Box<dyn Child + Send + Sync>,
    /// Quiet-poller registration, or a dedicated reader when the pane is hot.
    pub(super) reader: super::park::PtyReader,
    /// Writer thread; exits when its receiver closes.
    pub(crate) writer_thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PtyOwned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyOwned")
            .field("child", &self.child)
            .finish_non_exhaustive()
    }
}

/// Events flowing from the PTY reader thread into the actor.
#[derive(Debug)]
pub(crate) enum PtyEvent {
    /// A chunk read from the master, refcounted for broadcast. `read_at`
    /// feeds the `pty.queue_wait` metric.
    Bytes {
        /// The bytes read.
        chunk: bytes::Bytes,
        /// When the read completed.
        read_at: std::time::Instant,
    },
    /// The PTY hit EOF or errored. Either way: the child is going away.
    Eof,
}

/// Map a `portable_pty::ExitStatus` to an [`ExitOutcome`].
///
/// Exit code, signal number, or neither when unknown. portable-pty reports signals by name
/// (`strsignal(3)` text, or `"signal N"` for adopted children), which
/// [`signal_number`] maps back.
pub(crate) fn exit_outcome(status: &portable_pty::ExitStatus) -> ExitOutcome {
    status.signal().map_or_else(
        || ExitOutcome::exited(i32::try_from(status.exit_code()).unwrap_or(i32::MAX)),
        |name| ExitOutcome {
            status: None,
            signal: signal_number(name),
        },
    )
}

/// The signal number behind a `portable_pty` signal name; `None` for a
/// non-signal name, never a guess.
fn signal_number(name: &str) -> Option<i32> {
    adopted_signal_number(name).or_else(|| {
        strsignal_names()
            .iter()
            .find(|(described, _)| described == name)
            .map(|(_, signal)| *signal)
    })
}

/// `portable-pty-adopt` renders a signal death as `"signal N"`.
fn adopted_signal_number(name: &str) -> Option<i32> {
    name.strip_prefix("signal ")?.parse().ok()
}

/// `portable_pty`'s name for each signal number, built by running its own
/// `ExitStatus` conversion over synthetic signal deaths, so names match this
/// platform exactly without a hand-kept table.
fn strsignal_names() -> &'static [(String, i32)] {
    use std::os::unix::process::ExitStatusExt;
    /// Covers the classic signals and Linux's real-time range.
    const HIGHEST_SIGNAL: i32 = 64;
    static NAMES: std::sync::OnceLock<Vec<(String, i32)>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        (1..=HIGHEST_SIGNAL)
            .filter_map(|signal| {
                // Low seven bits = the terminating signal (WIFSIGNALED).
                let status = std::process::ExitStatus::from_raw(signal);
                let converted = portable_pty::ExitStatus::from(status);
                converted.signal().map(|name| (name.to_owned(), signal))
            })
            .collect()
    })
}

#[cfg(test)]
mod exit_outcome_tests {
    use std::os::unix::process::ExitStatusExt;

    use super::{ExitOutcome, exit_outcome};

    fn std_death_by(signal: i32) -> portable_pty::ExitStatus {
        portable_pty::ExitStatus::from(std::process::ExitStatus::from_raw(signal))
    }

    /// A signal death reports the signal, not "no status".
    #[test]
    fn signal_death_is_reported_as_signal_not_none() {
        assert_eq!(exit_outcome(&std_death_by(9)), ExitOutcome::signaled(9));
        assert_eq!(exit_outcome(&std_death_by(15)), ExitOutcome::signaled(15));
        // The adopted-child rendering maps back too.
        assert_eq!(
            exit_outcome(&portable_pty::ExitStatus::with_signal("signal 2")),
            ExitOutcome::signaled(2)
        );
    }

    #[test]
    fn exit_codes_stay_codes() {
        assert_eq!(
            exit_outcome(&portable_pty::ExitStatus::with_exit_code(0)),
            ExitOutcome::exited(0)
        );
        assert_eq!(
            exit_outcome(&portable_pty::ExitStatus::with_exit_code(42)),
            ExitOutcome::exited(42)
        );
    }

    /// A name that is not a signal (the adopted child's `ECHILD` sentinel)
    /// is unknown, never a fabricated code or signal.
    #[test]
    fn a_non_signal_name_is_unknown() {
        assert_eq!(
            exit_outcome(&portable_pty::ExitStatus::with_signal(
                portable_pty_adopt::ECHILD_EXIT_SIGNAL_NAME
            )),
            ExitOutcome::UNKNOWN
        );
    }
}

/// The shell for server-spawned panes: `configured` (`defaults.shell`) when
/// non-blank, else `$SHELL`, else `/bin/sh`.
#[must_use]
pub fn resolve_shell(configured: Option<&str>) -> String {
    resolve_shell_from(configured, std::env::var("SHELL").ok())
}

/// Env-independent core of [`resolve_shell`].
fn resolve_shell_from(configured: Option<&str>, env_shell: Option<String>) -> String {
    configured
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or(env_shell)
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// The argv flag that puts `shell` into login mode.
///
/// Matched on basename: `-l` for bash/zsh/sh, `--login` for fish, `None` for anything else.
/// Unknown shells get no flag because an unrecognized flag can fail the exec
/// (ADR-0073, `docs/operations.md`).
#[must_use]
pub fn login_flag_for_shell(shell: &str) -> Option<&'static str> {
    let name = std::path::Path::new(shell)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(shell);
    match name {
        "bash" | "zsh" | "sh" => Some("-l"),
        "fish" => Some("--login"),
        _ => None,
    }
}

/// Add `shell`'s login flag to `cmd` when `login` is set and the shell is known.
fn apply_login_mode(cmd: &mut CommandBuilder, shell: &str, login: bool) {
    if !login {
        return;
    }
    if let Some(flag) = login_flag_for_shell(shell) {
        cmd.arg(flag);
    }
}

/// A [`CommandBuilder`] for a plain interactive `shell`.
///
/// `login` requests login mode (for service-managed servers, whose panes
/// otherwise lack profile-provided `PATH`); a terminal-launched server
/// passes `false` because re-sourcing profiles is not idempotent.
///
/// `TERM` is [`DEFAULT_TERM`]: ghostty's terminfo advertises kitty keyboard
/// support, which breaks ncurses apps such as htop. `defaults.term` and
/// `SPAWN_RESOURCE.term` opt back in.
#[must_use]
pub fn default_shell_command(shell: &str, login: bool) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    apply_login_mode(&mut cmd, shell, login);
    cmd.env("TERM", DEFAULT_TERM);
    cmd
}

/// Baseline `TERM` for [`default_shell_command`] and [`shell_command`];
/// matches the `defaults.term` schema default, which [`apply_term`]
/// overrides at runtime.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// Override `TERM` on `cmd` with the server's configured `defaults.term`.
pub fn apply_term(cmd: &mut CommandBuilder, term: &str) {
    cmd.env("TERM", term);
}

/// Inject `PHUX_TERMINAL_ID`, the pane's local wire id, so in-pane processes
/// can name their pane. Only `Local` ids yield a value. Paired with
/// [`apply_server_socket`], which names the server.
pub fn apply_terminal_id(
    cmd: &mut CommandBuilder,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
) {
    if let Some(id) = wire_terminal_id.local_id() {
        cmd.env("PHUX_TERMINAL_ID", id.to_string());
    }
}

/// Inject `PHUX_SOCKET` so in-pane `phux` verbs reach this server rather
/// than the default socket. `None` leaves the inherited value alone.
pub fn apply_server_socket(cmd: &mut CommandBuilder, socket_path: Option<&std::path::Path>) {
    if let Some(path) = socket_path {
        cmd.env("PHUX_SOCKET", path.as_os_str());
    }
}

/// Strip the server-private `PHUX_UPGRADE_*` handoff from a pane child's
/// environment, so a `phux server` inside the pane cannot re-exec into the
/// outer server's binary on upgrade.
pub(crate) fn clear_upgrade_handoff_env(cmd: &mut CommandBuilder) {
    for key in crate::upgrade::HANDOFF_ENV_VARS {
        cmd.env_remove(key);
    }
}

/// Claude Code's nested-session markers. A `claude` that inherits
/// `CLAUDE_CODE_CHILD_SESSION` silently keeps no transcript.
const CLAUDE_CODE_ENV_PREFIX: &str = "CLAUDE_CODE_";
const CLAUDE_CODE_ENV_BARE: &str = "CLAUDECODE";

/// Strip Claude Code's `CLAUDE_CODE_*` / `CLAUDECODE` markers from a pane
/// child's environment. A server started from inside a Claude Code session
/// carries them forever, and every later `claude` in a pane would then run
/// as a nested session with no transcript. Matched by prefix because Claude
/// Code owns that vocabulary.
pub(crate) fn clear_agent_host_env(cmd: &mut CommandBuilder) {
    let leaked: Vec<String> = cmd
        .iter_full_env_as_str()
        .filter(|&(key, _)| key.starts_with(CLAUDE_CODE_ENV_PREFIX) || key == CLAUDE_CODE_ENV_BARE)
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in leaked {
        cmd.env_remove(key);
    }
}

/// Apply a wire-supplied cwd to `cmd`.
///
/// A cwd already on `cmd` (from a server-wide override command) wins. A path that is not an enterable
/// directory is dropped with a warning, so a stale client path degrades to
/// the default directory instead of failing the spawn. `session` is for
/// the log only.
pub fn apply_spawn_cwd(builder: &mut CommandBuilder, cwd: Option<&str>, session: &str) {
    let Some(path) = cwd else {
        return;
    };
    if builder.get_cwd().is_some() {
        return;
    }
    if dir_is_enterable(std::path::Path::new(path)) {
        builder.cwd(path);
    } else {
        warn!(
            session = %session,
            cwd = %path,
            "wire cwd is not an enterable directory; \
             falling back to the default spawn directory",
        );
    }
}

/// `path` is a directory the child can `chdir` into (search permission, not
/// just `is_dir`). Best effort: TOCTOU can still fail the spawn.
fn dir_is_enterable(path: &std::path::Path) -> bool {
    path.is_dir() && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

/// A [`CommandBuilder`] that runs `command` via `<shell> [-l] -c`, so quoting
/// behaves as at a prompt and the pane closes when the command exits.
#[must_use]
pub fn shell_command(shell: &str, command: &str, login: bool) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    apply_login_mode(&mut cmd, shell, login);
    cmd.arg("-c");
    cmd.arg(command);
    cmd.env("TERM", DEFAULT_TERM);
    cmd
}
type SpawnedPty = (
    mpsc::Receiver<PtyEvent>,
    mpsc::Sender<EncodedInputRequest>,
    PtyOwned,
);

/// Receive from `rx` when `Some`; otherwise pend forever (a select! arm for
/// PTY-less actors).
pub(crate) async fn recv_or_pending(rx: Option<&mut mpsc::Receiver<PtyEvent>>) -> Option<PtyEvent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Open a PTY, spawn `cmd` on the slave, and start the bridge threads.
pub(crate) fn spawn_pty(
    mut cmd: CommandBuilder,
    cols: u16,
    rows: u16,
) -> Result<SpawnedPty, TerminalActorError> {
    let pty_system = native_pty_system();
    // Nonzero initial pixel size for children that query `TIOCGWINSZ` before
    // the first client resize.
    let (cell_w, cell_h) = super::DEFAULT_CELL_PX;
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(cell_w),
            pixel_height: rows.saturating_mul(cell_h),
        })
        .map_err(|e| TerminalActorError::OpenPty(e.to_string()))?;

    clear_upgrade_handoff_env(&mut cmd);
    clear_agent_host_env(&mut cmd);
    let child = super::fd_shrink::spawn_pane_child(&*pair.slave, &*pair.master, cmd)
        .map_err(|reason| TerminalActorError::Spawn(spawn_failure_reason(&reason)))?;
    // Our slave copy would prevent EOF on the master after the child exits.
    drop(pair.slave);

    start_pty_bridge(pair.master, child)
}

/// A PTY spawn failure as one line a user can act on. portable-pty's
/// command-not-found text spans lines and quotes the whole `PATH` it
/// searched, which the refusal would carry verbatim to every client.
fn spawn_failure_reason(err: &dyn std::fmt::Display) -> String {
    const NOT_FOUND: &str = "No viable candidates found in PATH";
    let text = err.to_string();
    let text = match text.find(NOT_FOUND) {
        Some(at) => format!("{}not found in PATH", &text[..at]),
        None => text,
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Adopt an inherited PTY master fd and child pid after a graceful-upgrade
/// exec (ADR-0032) and start fresh bridge threads around them.
pub(crate) fn adopt_pty(
    master_fd: std::os::fd::RawFd,
    child_pid: i32,
) -> Result<SpawnedPty, TerminalActorError> {
    // SAFETY: `master_fd` is the inherited PTY master (FD_CLOEXEC cleared
    // before the exec), owned solely by this process now.
    let master: Box<dyn MasterPty + Send> =
        Box::new(unsafe { portable_pty_adopt::AdoptedMaster::from_raw_fd(master_fd) });
    let child: Box<dyn Child + Send + Sync> =
        Box::new(portable_pty_adopt::AdoptedChild::new(child_pid));
    start_pty_bridge(master, child)
}

/// Budget for the reader's post-actor drain: the hangup grace plus the reap
/// budget, i.e. the whole window in which the child may still be writing.
/// It only bounds a child that outlives the actor and writes forever.
const ORPHAN_DRAIN_BUDGET: std::time::Duration = {
    // Tests stretch the hangup ceiling (up to a 30 s trap-marker hold plus
    // 2.5 s); the reader cannot see that gate, so include it. EOF still ends
    // the drain.
    const GRACE: std::time::Duration = if cfg!(test) {
        std::time::Duration::from_millis(30_000 + 2_500)
    } else {
        super::PANE_KILL_GRACE
    };
    GRACE.saturating_add(super::PANE_KILL_REAP_BUDGET)
};

/// Keep draining the master after the actor is gone, until EOF or
/// [`ORPHAN_DRAIN_BUDGET`]. An unread master blocks the child's `write(2)`
/// after ~1 KiB, which would wedge a job flushing in its `SIGHUP` handler.
///
/// The budget is checked only between reads; a read blocked by an outside
/// holder of the slave is why `shutdown_pty` bounds the join itself.
pub(super) fn drain_master_to_eof<R: Read>(reader: &mut R, buf: &mut [u8]) {
    drain_master_with_budget(reader, buf, ORPHAN_DRAIN_BUDGET);
}

/// [`drain_master_to_eof`] with an explicit budget (for tests).
fn drain_master_with_budget<R: Read>(reader: &mut R, buf: &mut [u8], budget: std::time::Duration) {
    let deadline = std::time::Instant::now() + budget;
    loop {
        match reader.read(buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(err) => {
                debug!(?err, "pty reader thread: read error while draining orphan");
                return;
            }
        }
        if std::time::Instant::now() >= deadline {
            debug!("pty reader thread: orphan drain budget expired; leaving the master unread");
            return;
        }
    }
}

/// Hand one PTY chunk to the actor: `try_send` first (cheap), then
/// `blocking_send` only when the queue is really full, which is the
/// backpressure that stalls a runaway child. `Break` means the actor is gone.
pub(super) fn send_pty_chunk(
    tx: &mpsc::Sender<PtyEvent>,
    chunk: bytes::Bytes,
    read_at: std::time::Instant,
) -> std::ops::ControlFlow<()> {
    use tokio::sync::mpsc::error::TrySendError;
    match tx.try_send(PtyEvent::Bytes { chunk, read_at }) {
        Ok(()) => std::ops::ControlFlow::Continue(()),
        Err(TrySendError::Full(event)) => {
            // The actor is behind the child.
            crate::perf::PTY_READER_BLOCKED.incr();
            if tx.blocking_send(event).is_err() {
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
        Err(TrySendError::Closed(_)) => std::ops::ControlFlow::Break(()),
    }
}

/// Duplicate the master for the reader. `F_DUPFD_CLOEXEC` keeps the dup out of
/// pane children; the flag is per-descriptor, not a status flag, so it does
/// not make the writer's dup non-blocking.
fn dup_master_reader(master: &dyn MasterPty) -> Result<std::fs::File, TerminalActorError> {
    use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};
    let raw = master
        .as_raw_fd()
        .ok_or_else(|| TerminalActorError::PtyIo("pty master has no file descriptor".to_owned()))?;
    // SAFETY: `raw` belongs to `master`, which the caller holds across this
    // dup. The borrow ends before `master` is used again.
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    let fd = nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(0))
        .map_err(|err| TerminalActorError::PtyIo(err.to_string()))?;
    // SAFETY: `F_DUPFD_CLOEXEC` just created `fd` and this process owns it.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(std::fs::File::from(owned))
}

/// Shared tail of [`spawn_pty`] / [`adopt_pty`]: start the bridge threads and
/// assemble the [`PtyOwned`] bundle and channel endpoints.
fn start_pty_bridge(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
) -> Result<SpawnedPty, TerminalActorError> {
    let reader = dup_master_reader(&*master)?;
    let writer = master
        .take_writer()
        .map_err(|e| TerminalActorError::PtyIo(e.to_string()))?;
    let master = Arc::new(Mutex::new(master));
    // The writer inspects termios before writes; `PtyOwned::master` keeps the
    // other clone for resize ioctls.
    let master_for_writer = Arc::clone(&master);

    let (pty_tx_to_actor, pty_rx_for_actor) = mpsc::channel::<PtyEvent>(PTY_CHANNEL_DEPTH);
    let (input_tx_to_writer, mut input_rx_for_writer) =
        mpsc::channel::<EncodedInputRequest>(super::PTY_WRITER_QUEUE);

    let reader = super::park::attach(reader, pty_tx_to_actor);

    let writer_thread = std::thread::Builder::new()
        .name("phux-pty-writer".to_owned())
        .stack_size(super::park::READER_STACK)
        .spawn(move || {
            crate::perf::promote_helper_thread("phux-pty-writer");
            // portable-pty's writer `Drop` writes `\n` + VEOF. After a failed
            // write that newline would commit a truncated line to the shell,
            // so only the clean path runs the destructor; the failure paths
            // leak one fd of an already-dead input path.
            let mut writer = std::mem::ManuallyDrop::new(writer);
            loop {
                let Some(request) = input_rx_for_writer.blocking_recv() else {
                    // `shutdown_pty` dropped the sender; give the child EOF.
                    std::mem::ManuallyDrop::into_inner(writer);
                    return;
                };
                match service_write_request(&mut **writer, &master_for_writer, request) {
                    WriterLoopControl::Continue => {}
                    WriterLoopControl::Stop => return,
                }
            }
        })
        .map_err(|e| TerminalActorError::PtyIo(e.to_string()))?;

    Ok((
        pty_rx_for_actor,
        input_tx_to_writer,
        PtyOwned {
            master,
            child,
            reader,
            writer_thread: Some(writer_thread),
        },
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod writer_tests {
    use super::*;
    use std::io::ErrorKind;

    /// A `Write` whose every call is scripted.
    struct ScriptedWriter {
        /// Popped front-to-back, one per `write` call.
        script: Vec<Result<usize, std::io::Error>>,
        /// Bytes the "child" actually received.
        received: Vec<u8>,
        flush_script: Vec<Result<(), std::io::Error>>,
    }

    impl ScriptedWriter {
        fn new(script: Vec<Result<usize, std::io::Error>>) -> Self {
            Self {
                script,
                received: Vec::new(),
                flush_script: Vec::new(),
            }
        }
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            match self.script.remove(0) {
                Ok(n) => {
                    let n = n.min(buf.len());
                    self.received.extend_from_slice(&buf[..n]);
                    Ok(n)
                }
                Err(e) => Err(e),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.flush_script.is_empty() {
                return Ok(());
            }
            self.flush_script.remove(0)
        }
    }

    /// `EIO`/`EPIPE` are routine child exit, not a fatal input fault.
    #[test]
    fn child_exit_errnos_classify_as_pane_gone() {
        assert_eq!(
            classify_write_error(&std::io::Error::from_raw_os_error(EIO)),
            WriteFailure::PaneGone,
        );
        assert_eq!(
            classify_write_error(&std::io::Error::from(ErrorKind::BrokenPipe)),
            WriteFailure::PaneGone,
        );
        assert_eq!(
            classify_write_error(&std::io::Error::from(ErrorKind::PermissionDenied)),
            WriteFailure::Fatal,
        );
    }

    /// Short writes are resumed.
    #[test]
    fn partial_writes_are_resumed_until_the_payload_lands() {
        let mut w = ScriptedWriter::new(vec![Ok(3), Ok(3), Ok(3)]);
        write_all_resilient(&mut w, b"abcdefghi").expect("should complete");
        assert_eq!(w.received, b"abcdefghi");
    }

    /// `EINTR` is a signal artifact, never a delivery failure.
    #[test]
    fn interrupted_is_retried() {
        let mut w = ScriptedWriter::new(vec![
            Err(std::io::Error::from(ErrorKind::Interrupted)),
            Ok(5),
        ]);
        write_all_resilient(&mut w, b"hello").expect("should complete");
        assert_eq!(w.received, b"hello");
    }

    /// A failure reports how much the child already ingested.
    #[test]
    fn failure_reports_the_partial_write_count() {
        let mut w = ScriptedWriter::new(vec![Ok(4), Err(std::io::Error::from_raw_os_error(EIO))]);
        let err = write_all_resilient(&mut w, b"abcdefgh").expect_err("should fail");
        assert_eq!(err.failure, WriteFailure::PaneGone);
        assert_eq!(err.written, 4, "must report the truncated prefix length");
    }

    /// A zero-length write ends the loop instead of spinning.
    #[test]
    fn zero_length_write_terminates_instead_of_spinning() {
        let mut w = ScriptedWriter::new(vec![Ok(0)]);
        let err = write_all_resilient(&mut w, b"abc").expect_err("should fail");
        assert_eq!(err.failure, WriteFailure::PaneGone);
        assert_eq!(err.written, 0);
    }

    /// A child exiting between write and flush is teardown, not a fault.
    #[test]
    fn flush_classifies_child_exit_as_pane_gone() {
        let mut w = ScriptedWriter::new(vec![]);
        w.flush_script = vec![Err(std::io::Error::from_raw_os_error(EIO))];
        let err = flush_resilient(&mut w).expect_err("should fail");
        assert_eq!(err.failure, WriteFailure::PaneGone);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `phux spawn nosuch` printed "spawn failed: spawn failed: Unable to
    /// spawn nosuch because:" and then the server's whole `PATH`, several
    /// kilobytes long. The refusal is one line and names no `PATH` value.
    #[test]
    fn a_missing_command_is_one_line_without_the_path() {
        let mut cmd = CommandBuilder::new("phux-no-such-command-for-this-test");
        cmd.env("PATH", "/nonexistent/a:/nonexistent/b");
        let pair = native_pty_system()
            .openpty(PtySize::default())
            .expect("openpty");
        let err = pair.slave.spawn_command(cmd).expect_err("no such command");
        let reason = spawn_failure_reason(&err);
        assert_eq!(
            reason,
            "Unable to spawn phux-no-such-command-for-this-test because: not found in PATH"
        );
        assert_eq!(
            TerminalActorError::Spawn(reason.clone()).to_string(),
            reason
        );
    }

    #[test]
    fn resolve_shell_precedence() {
        let zsh = || Some("/bin/zsh".to_owned());
        assert_eq!(
            resolve_shell_from(Some("/opt/fancy/fish"), zsh()),
            "/opt/fancy/fish"
        );
        assert_eq!(resolve_shell_from(None, zsh()), "/bin/zsh");
        assert_eq!(resolve_shell_from(Some("  "), zsh()), "/bin/zsh");
        assert_eq!(resolve_shell_from(None, None), "/bin/sh");
    }

    fn argv_strings(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn login_flags_by_shell() {
        let cases: &[(&str, bool, &[&str])] = &[
            ("/bin/zsh", false, &["/bin/zsh"]),
            ("/bin/bash", true, &["/bin/bash", "-l"]),
            ("/bin/zsh", true, &["/bin/zsh", "-l"]),
            ("sh", true, &["sh", "-l"]),
            (
                "/opt/homebrew/bin/fish",
                true,
                &["/opt/homebrew/bin/fish", "--login"],
            ),
            ("/opt/exotic/rc", true, &["/opt/exotic/rc"]),
        ];
        for (shell, login, argv) in cases {
            assert_eq!(
                argv_strings(&default_shell_command(shell, *login)),
                *argv,
                "{shell}"
            );
        }
    }

    #[test]
    fn shell_command_applies_login_flag_before_dash_c() {
        let cmd = shell_command("/bin/zsh", "htop", true);
        assert_eq!(argv_strings(&cmd), ["/bin/zsh", "-l", "-c", "htop"]);
        let cmd = shell_command("/opt/fancy/fish", "btop --utf-force", false);
        assert_eq!(
            argv_strings(&cmd),
            ["/opt/fancy/fish", "-c", "btop --utf-force"]
        );
    }
}

/// Canonical-mode write guard (phux-mjmc): the pure predicate, then real-PTY
/// tests through the production writer thread.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod canonical_guard_tests {
    use super::*;
    use crate::terminal_actor::WriteCompletionSink;
    use portable_pty::CommandBuilder;
    use std::time::Duration;

    /// Short on purpose: an absence check only gets easier under load.
    const NOTHING_ARRIVES_WINDOW: Duration = Duration::from_millis(300);
    const DELIVERY_DEADLINE: Duration = Duration::from_secs(10);

    #[test]
    fn canonical_limit_counts_the_longest_line() {
        let mut many_short = Vec::new();
        for _ in 0..50 {
            many_short.extend(std::iter::repeat_n(b'x', 100));
            many_short.push(b'\n');
        }
        let mut overlong_then_cr = vec![b'a'; 1800];
        overlong_then_cr.push(b'\r');
        let mut cr_at_limit = vec![b'a'; 1024];
        cr_at_limit.push(b'\r');
        cr_at_limit.extend(std::iter::repeat_n(b'b', 10));
        let cases: &[(&[u8], bool, bool)] = &[
            (&[b'a'; 1024], true, false),
            (&[b'a'; 1025], true, true),
            (&many_short, true, false),
            // The terminating CR arrives past the overflow point.
            (&overlong_then_cr, true, true),
            // `\r` ends the line only with ICRNL.
            (&cr_at_limit, true, false),
            (&cr_at_limit, false, true),
        ];
        for (i, (bytes, cr_terminates, overflows)) in cases.iter().enumerate() {
            assert_eq!(
                exceeds_canonical_limit(bytes, 1024, *cr_terminates),
                *overflows,
                "case {i}"
            );
        }
    }

    /// The OS-default PTY (canonical mode) with the slave held open and
    /// unread, so there is no foreground echo-back.
    fn open_default_pty() -> (
        Box<dyn portable_pty::MasterPty + Send>,
        Box<dyn portable_pty::SlavePty + Send>,
    ) {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        (pair.master, pair.slave)
    }

    /// The limit the guard resolves on this platform (1024 darwin, 4096
    /// Linux); over-limit payloads are sized from it.
    fn platform_canonical_limit() -> usize {
        let (master, _slave) = open_default_pty();
        let raw_fd = master.as_raw_fd().expect("real pty has a raw fd");
        // SAFETY: `raw_fd` names `master`, kept alive by the binding above
        // for the whole call; the borrow does not outlive this function and
        // is never used to close or duplicate the fd.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        canonical_limit(borrowed)
    }

    /// Mechanism proof: `write_all_resilient` alone lets the kernel truncate
    /// an overlong canonical line, which is why the guard sits in front of it.
    ///
    /// It measures what the pane's reader receives on the slave side, not
    /// the master's echo: Linux echoes every byte of an overflowing line
    /// (each one overwrites the last queue slot), so how much echo one read
    /// returned depended on scheduling, and hosted CI once read 4120 bytes.
    #[test]
    fn write_all_resilient_alone_truncates_a_canonical_mode_line() {
        // Linux truncates the line and delivers it at the newline; macOS
        // drops the overflow and the newline with it, so nothing arrives.
        const LINE_WAIT: Duration = if cfg!(target_os = "linux") {
            DELIVERY_DEADLINE
        } else {
            NOTHING_ARRIVES_WINDOW
        };
        let (master, _slave) = open_default_pty();
        let slave_path = master.tty_name().expect("real pty names its slave");
        let slave_reader = std::fs::File::from(
            nix::fcntl::open(
                &slave_path,
                nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_NOCTTY,
                nix::sys::stat::Mode::empty(),
            )
            .expect("open the slave for reading"),
        );
        let mut writer = master.take_writer().expect("take writer");
        let raw_fd = master.as_raw_fd().expect("real pty has a raw fd");
        // SAFETY: `raw_fd` names `master`, which this test keeps alive via
        // the `master` binding for the whole function; the borrow does not
        // outlive this synchronous call.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        let limit = canonical_limit(borrowed);

        // The pane's side: read until the line completes or the pty closes.
        let (line_tx, line_rx) = std::sync::mpsc::channel();
        let slave_thread = std::thread::spawn(move || {
            let mut slave_reader = slave_reader;
            let mut line = Vec::new();
            let mut buf = [0u8; 8192];
            while !line.contains(&b'\n') {
                match slave_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => line.extend_from_slice(&buf[..n]),
                }
            }
            let _ = line_tx.send(line);
        });

        // Oversized line plus a terminator so a truncated line is readable.
        let payload = vec![b'a'; limit + 37];
        write_all_resilient(&mut *writer, &payload)
            .expect("the kernel write(2) itself succeeds regardless");
        write_all_resilient(&mut *writer, b"\n").expect("terminator write succeeds");
        flush_resilient(&mut *writer).expect("flush");

        let line = line_rx.recv_timeout(LINE_WAIT).unwrap_or_default();
        // Hanging up the master ends a slave read still waiting on a line.
        drop(writer);
        drop(master);
        slave_thread.join().expect("slave reader thread");
        assert!(
            line.len() <= limit,
            "expected at most {limit} bytes (the canonical limit) to reach \
             the reader, got {}: the canonical queue did not overflow, so \
             this payload was too small to reproduce phux-mjmc on this \
             platform",
            line.len()
        );
    }

    /// Spawn `cmd` under a real PTY and collect output up to the first `]`.
    async fn spawn_and_capture(cmd: CommandBuilder) -> String {
        let (mut pty_rx, _input_tx, mut pty) =
            spawn_pty(cmd, 80, 24).expect("spawn sh under a real pty");
        let mut received = Vec::new();
        let deadline = tokio::time::Instant::now() + DELIVERY_DEADLINE;
        while !received.contains(&b']') {
            match tokio::time::timeout_at(deadline, pty_rx.recv()).await {
                Ok(Some(PtyEvent::Bytes { chunk, .. })) => received.extend_from_slice(&chunk),
                Ok(Some(PtyEvent::Eof) | None) | Err(_) => break,
            }
        }
        let _ = pty.child.kill();
        String::from_utf8_lossy(&received).into_owned()
    }

    /// A pane child never inherits the server's `PHUX_UPGRADE_*` handoff.
    #[tokio::test(flavor = "current_thread")]
    async fn pane_children_never_inherit_the_upgrade_handoff_env() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(
            "printf '[%s|%s|%s]' \"${PHUX_UPGRADE_SOURCE_EXE-unset}\" \
             \"${PHUX_UPGRADE_SNAPSHOT_DIR-unset}\" \"${PHUX_SOCKET-unset}\"",
        );
        for key in crate::upgrade::HANDOFF_ENV_VARS {
            cmd.env(key, "/leaked/by/an/upgraded/server");
        }
        cmd.env("PHUX_SOCKET", "/tmp/kept.sock");
        let out = spawn_and_capture(cmd).await;
        assert!(
            out.contains("[unset|unset|/tmp/kept.sock]"),
            "a pane child must see PHUX_SOCKET but no PHUX_UPGRADE_* variable; got {out:?}"
        );
    }

    /// A pane child never inherits Claude Code's nested-session markers from
    /// the server's environment (a `claude` there would keep no transcript).
    #[tokio::test(flavor = "current_thread")]
    async fn pane_children_never_inherit_claude_code_nested_session_markers() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(
            "printf '[%s|%s|%s|%s]' \"${CLAUDE_CODE_CHILD_SESSION-unset}\" \
             \"${CLAUDE_CODE_SESSION_ID-unset}\" \"${CLAUDECODE-unset}\" \
             \"${PHUX_SOCKET-unset}\"",
        );
        cmd.env("CLAUDE_CODE_CHILD_SESSION", "1");
        cmd.env("CLAUDE_CODE_SESSION_ID", "leaked-by-the-server-env");
        cmd.env("CLAUDECODE", "1");
        cmd.env("PHUX_SOCKET", "/tmp/kept.sock");
        let out = spawn_and_capture(cmd).await;
        assert!(
            out.contains("[unset|unset|unset|/tmp/kept.sock]"),
            "a pane child must see PHUX_SOCKET but no Claude Code marker; got {out:?}"
        );
    }

    /// Send `bytes` to the writer thread and wait for its completion.
    async fn write_and_wait(
        input_tx: &tokio::sync::mpsc::Sender<EncodedInputRequest>,
        bytes: Vec<u8>,
    ) -> WriteCompletion {
        let (completion_tx, completed) = WriteCompletionSink::channel();
        input_tx
            .try_send(EncodedInputRequest::acknowledged(bytes, completion_tx))
            .expect("writer mailbox has room");
        tokio::task::spawn_blocking(move || completed.recv_timeout(DELIVERY_DEADLINE))
            .await
            .expect("blocking task")
            .expect("writer thread must reply")
    }

    /// A newline-free overlong write is refused before any byte is sent.
    #[tokio::test(flavor = "current_thread")]
    async fn newline_free_write_over_the_limit_is_refused_before_any_byte_is_sent() {
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
        match write_and_wait(&input_tx, vec![b'a'; 4097]).await {
            WriteCompletion::CanonicalLimitExceeded { limit } => assert!(limit > 0),
            other => panic!("expected CanonicalLimitExceeded, got {other:?}"),
        }
        let extra = tokio::time::timeout(NOTHING_ARRIVES_WINDOW, pty_rx.recv()).await;
        assert!(
            extra.is_err(),
            "a refused payload must not put any bytes on the wire"
        );
        let _ = pty.child.kill();
    }

    /// An overlong run plus terminating CR is refused whole instead of
    /// wedging the pane.
    #[tokio::test(flavor = "current_thread")]
    async fn terminating_cr_past_the_limit_is_refused_not_wedged() {
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
        let mut payload = vec![b'a'; platform_canonical_limit() + 776];
        payload.push(b'\r');
        assert!(matches!(
            write_and_wait(&input_tx, payload).await,
            WriteCompletion::CanonicalLimitExceeded { .. }
        ));
        let extra = tokio::time::timeout(NOTHING_ARRIVES_WINDOW, pty_rx.recv()).await;
        assert!(
            extra.is_err(),
            "a refused payload must not put any bytes on the wire"
        );
        let _ = pty.child.kill();
    }

    /// A refusal leaves the input path alive for later writes.
    #[tokio::test(flavor = "current_thread")]
    async fn refusal_does_not_wedge_the_pane_for_later_writes() {
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
        assert!(matches!(
            write_and_wait(&input_tx, vec![b'a'; 5000]).await,
            WriteCompletion::CanonicalLimitExceeded { .. }
        ));
        assert_eq!(
            write_and_wait(&input_tx, b"hello\n".to_vec()).await,
            WriteCompletion::Delivered
        );
        let got = tokio::time::timeout(DELIVERY_DEADLINE, pty_rx.recv())
            .await
            .expect("must not hang")
            .expect("pty output channel open");
        match got {
            PtyEvent::Bytes { chunk: bytes, .. } => {
                assert!(bytes.windows(5).any(|w| w == b"hello"));
            }
            PtyEvent::Eof => panic!("pane closed unexpectedly"),
        }
        let _ = pty.child.kill();
    }

    /// Raw mode stays on the unchecked path: a large newline-free payload is
    /// delivered intact.
    #[tokio::test(flavor = "current_thread")]
    async fn raw_mode_delivers_a_large_newline_free_payload_intact() {
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
        // Raw mode (and no echo) on the shared master, as a TUI would set it.
        {
            let raw_fd = pty
                .master
                .lock()
                .expect("master lock")
                .as_raw_fd()
                .expect("raw fd");
            // SAFETY: `raw_fd` names `pty.master`, kept alive by `pty` for
            // the rest of this test; the borrow does not outlive this call.
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
            let mut termios = nix::sys::termios::tcgetattr(borrowed).expect("tcgetattr");
            termios
                .local_flags
                .remove(LocalFlags::ICANON | LocalFlags::ECHO);
            nix::sys::termios::tcsetattr(borrowed, nix::sys::termios::SetArg::TCSANOW, &termios)
                .expect("tcsetattr");
        }

        let payload = vec![b'x'; 8192];
        assert_eq!(
            write_and_wait(&input_tx, payload.clone()).await,
            WriteCompletion::Delivered
        );

        let mut received = Vec::new();
        while received.len() < payload.len() {
            let chunk = tokio::time::timeout(DELIVERY_DEADLINE, pty_rx.recv())
                .await
                .expect("must not hang")
                .expect("pty output channel open");
            match chunk {
                PtyEvent::Bytes { chunk: bytes, .. } => received.extend_from_slice(&bytes),
                PtyEvent::Eof => panic!("pty closed before full delivery"),
            }
        }
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
        let _ = pty.child.kill();
    }

    fn collect_for(rx: &mut mpsc::Receiver<PtyEvent>, timeout: Duration) -> Vec<u8> {
        let deadline = std::time::Instant::now() + timeout;
        let mut got = Vec::new();
        let mut last_byte: Option<std::time::Instant> = None;
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(PtyEvent::Bytes { chunk, .. }) => {
                    got.extend_from_slice(&chunk);
                    last_byte = Some(std::time::Instant::now());
                }
                Ok(PtyEvent::Eof) | Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {
                    // A pane that already printed a line and then went quiet is
                    // done. Waiting out `timeout` would sit on the promote test's
                    // `sleep`.
                    let quiet =
                        last_byte.is_some_and(|at| at.elapsed() >= Duration::from_millis(50));
                    if quiet && got.contains(&b'\n') {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        got
    }

    /// Idle panes sit on the shared poller. They must not each grow a reader thread.
    #[test]
    fn idle_panes_share_one_poller() {
        let parked = super::super::park::parked_count();
        let hot = super::super::park::hot_count();
        let mut live = Vec::new();
        for _ in 0..8 {
            let mut cmd = CommandBuilder::new("sleep");
            cmd.arg("60");
            live.push(spawn_pty(cmd, 80, 24).expect("spawn sleep"));
        }
        assert_eq!(super::super::park::parked_count(), parked + 8);
        assert_eq!(super::super::park::hot_count(), hot);
        for (_, _, mut pty) in live {
            let _ = pty.child.kill();
        }
    }

    /// The first burst promotes the pane onto a dedicated reader.
    #[test]
    fn output_promotes_a_pane_off_the_poller() {
        let before = super::super::park::promote_count();
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("printf 'hello-from-pty\\n'; sleep 30");
        let (mut rx, _, mut pty) = spawn_pty(cmd, 80, 24).expect("spawn a pane that prints once");
        let got = collect_for(&mut rx, DELIVERY_DEADLINE);
        assert!(
            got.windows(14).any(|window| window == b"hello-from-pty"),
            "missing prompt bytes in {got:?}"
        );
        // The byte is queued before the poller records the promotion.
        let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
        while super::super::park::promote_count() <= before && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            super::super::park::promote_count() > before,
            "a pane that produced output should leave the shared poller"
        );
        let _ = pty.child.kill();
    }

    /// A high fd held open in the server must not be inherited, and on Linux
    /// the child's allocated fd table must collapse back down.
    #[test]
    fn pane_child_does_not_inherit_the_servers_fd_table() {
        use std::os::fd::FromRawFd;
        let devnull = std::fs::File::open("/dev/null").expect("devnull");
        let high = nix::fcntl::fcntl(&devnull, nix::fcntl::FcntlArg::F_DUPFD(4_000))
            .expect("dup a high fd");
        let probe = format!(
            "result=CLEAN; \
             if [ -e /dev/fd/{high} ]; then result=LEAK; fi; \
             open=0; i=0; \
             while [ \"$i\" -lt 64 ]; do \
               if [ -e \"/dev/fd/$i\" ]; then open=$((open + 1)); fi; \
               i=$((i + 1)); \
             done; \
             fdsize=na; \
             if [ -r /proc/self/status ]; then \
               fdsize=$(awk '/^FDSize:/ {{ print $2 }}' /proc/self/status); \
             fi; \
             printf '%s COUNT:%s FDSize:%s\\n' \"$result\" \"$open\" \"$fdsize\""
        );
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(probe);
        let (mut rx, _, mut pty) = spawn_pty(cmd, 80, 24).expect("spawn probe");
        let got = collect_for(&mut rx, DELIVERY_DEADLINE);
        let text = String::from_utf8_lossy(&got);
        assert!(text.contains("CLEAN"), "child inherited fd {high}: {text}");
        let count = text.split_whitespace().find_map(|field| {
            field
                .strip_prefix("COUNT:")
                .and_then(|value| value.parse::<u64>().ok())
        });
        let count = count.expect("COUNT line");
        assert!(
            count < 16,
            "child kept {count} fds below 64; the spawn should not copy the server table: {text}"
        );
        #[cfg(target_os = "linux")]
        {
            let fdsize = text.split_whitespace().find_map(|field| {
                field
                    .strip_prefix("FDSize:")
                    .and_then(|value| value.parse::<u64>().ok())
            });
            let fdsize = fdsize.expect("FDSize line");
            assert!(
                fdsize < 512,
                "fd table stayed fat (FDSize {fdsize}) after the trampoline"
            );
        }
        let _ = pty.child.kill();
        // SAFETY: `fcntl` just created `high` and this test owns it.
        drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(high) });
    }
}

#[cfg(test)]
mod drain_tests {
    use super::{ORPHAN_DRAIN_BUDGET, drain_master_with_budget};
    use std::io::Read;

    /// A reader that never runs out of bytes — the runaway child.
    struct Endless;
    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
    }

    /// A reader that yields `n` full buffers and then reports EOF.
    struct Finite(usize);
    impl Read for Finite {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Ok(0);
            }
            self.0 -= 1;
            Ok(buf.len())
        }
    }

    /// A reader whose descriptor has gone away.
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from_raw_os_error(5)) // EIO
        }
    }

    #[test]
    fn a_runaway_child_cannot_pin_the_reader_past_its_budget() {
        let mut buf = [0_u8; 64];
        let budget = std::time::Duration::from_millis(50);
        let started = std::time::Instant::now();
        drain_master_with_budget(&mut Endless, &mut buf, budget);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= budget,
            "the drain must actually serve its budget before giving up; gave up after {elapsed:?}",
        );
        assert!(
            elapsed < budget * 20,
            "the drain must give up NEAR its budget, not eventually; took {elapsed:?}",
        );
    }

    #[test]
    fn eof_ends_the_drain_immediately() {
        let mut buf = [0_u8; 64];
        let started = std::time::Instant::now();
        drain_master_with_budget(&mut Finite(3), &mut buf, ORPHAN_DRAIN_BUDGET);
        assert!(
            started.elapsed() < ORPHAN_DRAIN_BUDGET,
            "a child that finishes must not hold the reader for the whole budget",
        );
    }

    #[test]
    fn a_read_error_ends_the_drain_immediately() {
        let mut buf = [0_u8; 64];
        let started = std::time::Instant::now();
        drain_master_with_budget(&mut Broken, &mut buf, ORPHAN_DRAIN_BUDGET);
        assert!(
            started.elapsed() < ORPHAN_DRAIN_BUDGET,
            "EIO means the descriptor is gone; there is nothing left to drain",
        );
    }
}
