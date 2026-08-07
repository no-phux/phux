//! Submodule for terminal actor internals.

use super::{EncodedInputRequest, TerminalActorError, WriteCompletion};
use nix::sys::termios::{InputFlags, LocalFlags};
use nix::unistd::{PathconfVar, fpathconf};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

/// Default PTY read chunk size. Mirrors the example. Sized comfortably
/// above the typical libghostty escape-sequence span so a single read
/// rarely splits a sequence boundary.
const PTY_READ_CHUNK: usize = 4096;

/// `EIO` on a PTY master write means the slave side is gone — the child
/// exited or closed its end. Every Unix spells it 5; `std::io::ErrorKind`
/// has no stable variant for it, and `libc` is a macOS-gated dependency in
/// this crate, so the numeric constant is the portable spelling.
const EIO: i32 = 5;

/// Bound on `WouldBlock` retries, and the pause between attempts.
///
/// Nothing in phux sets `O_NONBLOCK` on the master and portable-pty does not
/// either, so `WouldBlock` is unreachable today. It is handled anyway
/// because the cost is a few lines and the failure it prevents is the exact
/// one [`write_all_resilient`] exists to kill: a transient errno permanently
/// severing a live pane's input (phux-oxd7). Should the fd ever become
/// non-blocking, this degrades to a bounded stall instead of a dead pane.
const WOULD_BLOCK_RETRIES: u32 = 50;
const WOULD_BLOCK_BACKOFF: std::time::Duration = std::time::Duration::from_millis(2);

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

/// A failed PTY write, carrying how much of the payload the child already
/// received before the failure.
#[derive(Debug)]
struct WriteError {
    failure: WriteFailure,
    source: std::io::Error,
    /// Bytes successfully written before the failure. Non-zero means the
    /// child ingested a truncated prefix — the fact that makes an
    /// unconditional retry unsafe and a silent failure unacceptable.
    written: usize,
}

/// Classify a PTY write/flush error into "child went away" versus "real
/// fault". The distinction is the point: the previous writer treated both
/// as terminal for the pane's entire input path, so a routine child exit
/// mid-write produced the same permanent input death as a genuine fault.
fn classify_write_error(err: &std::io::Error) -> WriteFailure {
    if err.raw_os_error() == Some(EIO) || err.kind() == std::io::ErrorKind::BrokenPipe {
        WriteFailure::PaneGone
    } else {
        WriteFailure::Fatal
    }
}

/// Write every byte of `bytes`, resuming across partial writes and retrying
/// the transient errno classes.
///
/// [`Write::write_all`] is insufficient on two counts. It retries only
/// `Interrupted`, so a `WouldBlock` propagates as a hard error; and it
/// reports no progress count, so the caller cannot tell how much of the
/// payload the child already received. Both matter because a failure here
/// previously killed the pane's input path for good.
fn write_all_resilient(writer: &mut (dyn Write + Send), bytes: &[u8]) -> Result<(), WriteError> {
    let mut written = 0_usize;
    let mut would_block = 0_u32;
    while written < bytes.len() {
        match writer.write(&bytes[written..]) {
            // A zero-length write with bytes outstanding means no progress
            // is possible; treat it as the child having gone away rather
            // than spinning forever.
            Ok(0) => {
                return Err(WriteError {
                    failure: WriteFailure::PaneGone,
                    source: std::io::Error::from(std::io::ErrorKind::WriteZero),
                    written,
                });
            }
            Ok(n) => {
                written += n;
                would_block = 0;
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                would_block += 1;
                if would_block > WOULD_BLOCK_RETRIES {
                    return Err(WriteError {
                        failure: WriteFailure::Fatal,
                        source: err,
                        written,
                    });
                }
                std::thread::sleep(WOULD_BLOCK_BACKOFF);
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

/// Flush, retrying `Interrupted` and classifying the rest the same way
/// [`write_all_resilient`] does. By flush time the bytes are already in the
/// kernel, so `written` is reported as the full payload length by the
/// caller's accounting rather than tracked here.
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

/// `_POSIX_MAX_CANON` (POSIX.1-2017 `<limits.h>`): the minimum canonical-mode
/// line length every conforming implementation must support. Real limits are
/// almost always larger and platform-specific (empirically 1024 on this
/// darwin build's `MAX_CANON`, per phux-mjmc's repro) — [`canonical_refusal`]
/// asks the kernel for the real number via `fpathconf(_PC_MAX_CANON)` on the
/// pane's own master fd rather than hardcoding a platform table. This floor
/// is used only as the fallback when that query is unavailable.
const POSIX_MAX_CANON_FLOOR: usize = 255;

/// Below this size, no POSIX-conformant canonical-mode line discipline can
/// possibly overflow — [`POSIX_MAX_CANON_FLOOR`] is the guaranteed minimum
/// every implementation supports. Skipping the termios syscall below this
/// size keeps the interactive fast path (individual keystrokes and short
/// escape sequences — the overwhelming majority of writes) free of the extra
/// `tcgetattr` [`canonical_refusal`] would otherwise cost on every write.
const CANONICAL_CHECK_SKIP_THRESHOLD: usize = POSIX_MAX_CANON_FLOOR;

/// Why [`canonical_refusal`] refused to hand a payload to the PTY writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CanonicalOverflow {
    /// The pane's real canonical-line byte limit, as reported by
    /// [`canonical_refusal`] at refusal time.
    limit: usize,
}

/// Pure predicate (phux-mjmc): does `bytes`, delivered to a canonical-mode
/// (`ICANON`) line discipline, contain any *line* — a run between two
/// terminators, or from the last terminator to the end of the payload —
/// longer than `limit` bytes?
///
/// This is the actual overflow condition, not "is the whole payload longer
/// than `limit`": the canonical queue resets on every completed line, so a
/// payload built from many short newline-terminated lines can be arbitrarily
/// large and still fit, while a single overlong line anywhere in the batch
/// overflows regardless of what surrounds it — including the pathological
/// case phux-mjmc reported, where the overflowing line's own terminating CR
/// arrives after the queue is already full and is itself dropped, wedging
/// the pane forever.
///
/// A line terminates on `\n` unconditionally, and on `\r` only when
/// `cr_terminates` is set — that is, when the line discipline's `ICRNL`
/// input flag translates `\r` to `\n` on the way in (the default on a
/// freshly opened PTY, and the mechanism behind phux-mjmc's "terminating
/// CR" repro). Other discipline-configurable terminators (`VEOL`, `VEOF`)
/// are deliberately not modeled: they are rarely reconfigured in practice,
/// and getting `\n` / `\r` right covers every case in the bug report.
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

/// If `master`'s pane is currently in canonical mode (`ICANON`) and `bytes`
/// would overflow it (see [`exceeds_canonical_limit`]), return why —
/// otherwise `None`, meaning the write should proceed on the normal path.
///
/// Queries termios itself (`tcgetattr`) rather than going through
/// [`MasterPty::get_termios`]: `portable-pty` 0.9 depends on `nix` 0.28,
/// while this crate depends on `nix` 0.29 for [`LocalFlags`] /
/// [`InputFlags`] (already pulled in for the process-group / signal
/// surface — see this crate's `Cargo.toml`), and cargo happily resolves
/// both into the same binary as unrelated types. Comparing `Termios`
/// bitflags across that version split does not type-check, so this calls
/// `nix::sys::termios::tcgetattr` directly on the master's raw fd instead,
/// which also means an adopted graceful-upgrade master (whose
/// `get_termios` would return `None`) is checked exactly like a freshly
/// spawned one.
///
/// `None` covers both "not canonical" and "cannot tell" (fd unavailable,
/// `tcgetattr` errors) — a query failure must never newly refuse a write
/// this guard cannot actually evaluate.
fn canonical_refusal(
    master: &Mutex<Box<dyn MasterPty + Send>>,
    bytes: &[u8],
) -> Option<CanonicalOverflow> {
    if bytes.len() <= CANONICAL_CHECK_SKIP_THRESHOLD {
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

/// The pane's real canonical-line byte limit.
///
/// `MAX_CANON` is not a portable compile-time constant, so the kernel is
/// asked via `fpathconf(_PC_MAX_CANON)` on the pane's own fd rather than a
/// hardcoded platform table. But that number is a **floor, not the truth**:
/// POSIX specifies `_PC_MAX_CANON` as the minimum a conforming
/// implementation must support, and Linux answers with the 255-byte POSIX
/// floor while its `N_TTY` line discipline actually buffers 4096. Darwin
/// reports its real 1024.
///
/// Taking the larger of the two matters because the two ways of being wrong
/// are not symmetric. Refusing a payload the kernel would have accepted
/// breaks working pastes — on Linux, every newline-free payload over 255
/// bytes, which is the common case this guard is supposed to leave alone.
/// Accepting one the kernel drops leaves phux-mjmc's wedge in place for a
/// narrow band. The first is a regression phux would ship to every Linux
/// user; the second is the status quo ante. So the guard refuses only what
/// it is confident overflows.
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

/// Whether the writer thread's loop should keep draining requests or shut
/// down, as decided by [`service_write_request`].
enum WriterLoopControl {
    /// Keep looping — either the write succeeded, or it was refused but the
    /// pane's input path is still alive (phux-mjmc: a canonical-limit
    /// refusal is not a writer fault).
    Continue,
    /// The child is gone or the write hit a genuinely fatal error; the
    /// caller returns from the thread.
    Stop,
}

/// Handle one [`EncodedInputRequest`] on the writer thread: refuse it up
/// front if it would overflow the pane's canonical-mode line discipline
/// (phux-mjmc), otherwise write and flush it — reporting the outcome on
/// `request.completion` if the caller is waiting for one either way.
///
/// Split out of [`start_pty_bridge`]'s writer-thread closure purely to keep
/// that closure under the line-count lint; the split has no behavioral
/// significance.
fn service_write_request(
    writer: &mut (dyn Write + Send),
    master: &Mutex<Box<dyn MasterPty + Send>>,
    request: EncodedInputRequest,
) -> WriterLoopControl {
    let len = request.bytes.len();
    if let Some(overflow) = canonical_refusal(master, &request.bytes) {
        // Loud on purpose (phux-mjmc): silently calling `write_all_resilient`
        // here would succeed from the kernel's point of view while the
        // canonical-mode line discipline dropped everything past
        // `overflow.limit` — and, if the payload's own terminator falls past
        // that point, drops the terminator too, wedging the pane's input
        // permanently (the queue never empties because nothing ever
        // completes the line). Refusing before the write means zero bytes
        // reach the pane instead of a truncated, uncompletable prefix. The
        // pane's input path stays alive — this is not a fatal writer error,
        // just one rejected payload — so the loop continues rather than
        // stopping.
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
            let _ = completion.send(WriteCompletion::CanonicalLimitExceeded {
                limit: overflow.limit,
            });
        }
        return WriterLoopControl::Continue;
    }
    let outcome =
        write_all_resilient(writer, &request.bytes).and_then(|()| flush_resilient(writer));
    match outcome {
        Ok(()) => {
            debug!(len, "pty write flushed");
            if let Some(completion) = request.completion {
                let _ = completion.send(WriteCompletion::Delivered);
            }
            WriterLoopControl::Continue
        }
        Err(WriteError {
            failure: WriteFailure::PaneGone,
            source,
            written,
        }) => {
            // The child exited or closed the slave. Routine teardown, not a
            // fault: the reader thread is reporting EOF on its own path and
            // the actor will close the pane. Anything still queued is moot.
            debug!(
                ?source,
                written, len, "pty writer: child gone; input path closing"
            );
            if let Some(completion) = request.completion {
                let _ = completion.send(WriteCompletion::Failed);
            }
            WriterLoopControl::Stop
        }
        Err(WriteError {
            failure: WriteFailure::Fatal,
            source,
            written,
        }) => {
            // Genuinely unexpected. The pane's input path is dead and cannot
            // be revived; say so loudly, with the partial-write count,
            // because output, snapshots, and command acks all keep working
            // while input silently goes nowhere.
            error!(
                ?source,
                written, len, "pty writer: write failed; pane input is now dead"
            );
            if let Some(completion) = request.completion {
                let _ = completion.send(WriteCompletion::Failed);
            }
            WriterLoopControl::Stop
        }
    }
}

/// Bundle of PTY-side resources owned by a
/// [`TerminalActor`](crate::terminal_actor::TerminalActor) with a real PTY.
///
/// Fields are kept in struct-declaration order so drop order matches the
/// teardown contract: writer thread first (so the writer channel closes
/// before the master), then the master (which sends EOF to the slave),
/// then the child, then the reader thread.
pub(crate) struct PtyOwned {
    /// Master handle — owned by the actor so resize ioctls can be
    /// issued. Wrapped in `Arc` so the writer thread can hold a clone
    /// (it doesn't, currently — the writer thread owns its own
    /// `Box<dyn Write + Send>` taken via `MasterPty::take_writer` —
    /// but the field keeps the master alive for resize / drop-on-exit).
    #[allow(dead_code, reason = "kept alive; methods invoked through &self")]
    pub(crate) master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child process spawned on the slave side. Reaped in
    /// [`TerminalActor::shutdown_pty`](crate::terminal_actor::TerminalActor::shutdown_pty).
    pub(crate) child: Box<dyn Child + Send + Sync>,
    /// Reader-thread join handle. Reader exits when the master is
    /// dropped (EOF on the read fd) or when its `mpsc::Sender` closes.
    pub(crate) reader_thread: Option<JoinHandle<()>>,
    /// Writer-thread join handle. Writer exits when its `mpsc::Receiver`
    /// closes (i.e., the actor's `pty_tx` sender is dropped).
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
    /// A chunk of bytes read from the PTY master.
    Bytes(Vec<u8>),
    /// The PTY hit EOF or errored. Either way: the child is going away.
    Eof,
}

/// Map a `portable_pty::ExitStatus` into the `TERMINAL_CLOSED.exit_status`
/// wire shape (phux-4li.11).
///
/// `Some(code)` for `_exit(n)`, `None` for signal-killed or
/// unknown-cause exits. `portable_pty::ExitStatus` keeps its
/// `signal: Option<String>` field private; the only way through the
/// public surface to distinguish a signal-driven death from `_exit(1)`
/// is the `Display` impl, which formats signal kills as
/// `"Terminated by <name>"` and exits as `"Exited with code N"` /
/// `"Success"`. Parsing the prefix is the stable contract; if upstream
/// ever exposes `signal()` we can swap this for a structured probe
/// without touching call sites.
pub(crate) fn exit_status_to_wire(status: &portable_pty::ExitStatus) -> Option<i32> {
    let rendered = status.to_string();
    if rendered.starts_with("Terminated by") {
        return None;
    }
    // Both "Success" (success() == true) and "Exited with code N" hit
    // this branch. `exit_code()` returns u32 — coerce into i32 saturating
    // at i32::MAX, since `TERMINAL_CLOSED.exit_status` is `Option<i32>`
    // on the wire and the practical exit-code range is 0..=255.
    Some(i32::try_from(status.exit_code()).unwrap_or(i32::MAX))
}

/// Resolve the shell server-spawned panes run (phux-i0e8.4.1):
/// `configured` (the server's `defaults.shell`) when set, else `$SHELL`,
/// else `/bin/sh` (POSIX-guaranteed).
///
/// The seam mirrors the `defaults.term` / [`apply_term`] precedent: the
/// binary resolves once from its single config load and threads the
/// result into every server-owned spawn path, so a mid-run environment
/// change cannot make two panes disagree. A configured value that is
/// empty or whitespace-only is treated as unset rather than spawning an
/// empty program name.
#[must_use]
pub fn resolve_shell(configured: Option<&str>) -> String {
    resolve_shell_from(configured, std::env::var("SHELL").ok())
}

/// Env-independent core of [`resolve_shell`], split out so the
/// precedence is testable without mutating the process environment
/// (nextest runs tests in parallel; `set_var` races).
fn resolve_shell_from(configured: Option<&str>, env_shell: Option<String>) -> String {
    configured
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or(env_shell)
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// Which single argv flag puts `shell` into its platform login mode
/// (phux-87rr).
///
/// Spawning with this flag re-runs the profile scripts a login shell
/// reads (macOS `/etc/zprofile` + `~/.zprofile`, `/etc/profile` +
/// `~/.bash_profile`, etc.) — the mechanism [`default_shell_command`]
/// and [`shell_command`] use when `login` is `true`. Matched on the
/// shell's basename so a full path (`/opt/homebrew/bin/fish`) resolves
/// the same as a bare name. Researched per-shell, not assumed:
///
/// | shell        | flag       |
/// |--------------|------------|
/// | `bash`       | `-l`       |
/// | `zsh`        | `-l`       |
/// | `fish`       | `--login`  |
/// | `sh`         | `-l`       |
///
/// `sh` is included because `/bin/sh` is the documented last-resort
/// fallback ([`resolve_shell`]): on macOS it is bash built with its `sh`
/// personality, and on Linux it is almost always `dash` — both accept
/// `-l` to mean "act as a login shell" (dash documents this explicitly;
/// bash-as-sh reads `/etc/profile` then `$ENV` under `-l` same as plain
/// bash).
///
/// Anything else — a custom shell, a wrapper script, a typo in
/// `defaults.shell` — returns `None` and gets NO login flag. This is a
/// deliberate, documented choice over guessing: an unrecognized program
/// has unknown flag semantics, and handing it a flag it does not
/// understand can fail the exec outright (`bash: -l: invalid option` is
/// forgiving; plenty of programs are not). A pane whose profile never
/// ran is a documented limitation; a pane that never starts is a much
/// worse regression. See `docs/operations.md`'s "Service-managed pane
/// environment" section for the user-facing version of this table, and
/// ADR-0073 for the decision record.
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

/// Add `shell`'s login flag (see [`login_flag_for_shell`]) to `cmd` when
/// `login` is `true` and the shell is recognized. A no-op otherwise, so
/// callers can pass `login` unconditionally without an extra branch.
fn apply_login_mode(cmd: &mut CommandBuilder, shell: &str, login: bool) {
    if !login {
        return;
    }
    if let Some(flag) = login_flag_for_shell(shell) {
        cmd.arg(flag);
    }
}

/// Build the [`CommandBuilder`] for a pane that runs a plain interactive
/// shell — `shell` is the already-resolved program (see
/// [`resolve_shell`]).
///
/// `login` puts the shell into its platform login mode (see
/// [`login_flag_for_shell`]) when `true` — the treatment a
/// service-managed server's panes need so profile-provided `PATH`
/// entries (Homebrew, Nix) exist, since launchd/systemd never ran a
/// login shell to source them (phux-87rr). An ordinary terminal-launched
/// server passes `false`: its own environment is already a fully
/// initialized login shell's, so re-sourcing profile scripts a second
/// time is not idempotent for every setup (PATH duplication is the mild
/// failure; nvm/rbenv/direnv guards misfiring is not).
///
/// Sets `TERM=xterm-256color` on the spawned process. This is deliberate
/// (phux-7vx): we previously advertised `TERM=ghostty`, but ghostty's
/// terminfo carries the `fullkbd` extended capability that ncurses
/// applications read as "kitty keyboard protocol available." Several
/// ncurses TUIs (htop is the canonical reproducer) then push the kitty
/// progressive-enhancement flags on startup via `CSI > N u`. libghostty's
/// per-pane `Terminal` honours that push, after which the per-pane key
/// encoder correctly emits CSI-u sequences (e.g. `\x1b[113;1u` for `q`).
/// The trouble is the round-trip on the app's side: htop in particular
/// does NOT actually parse incoming CSI-u for the keys it cares about,
/// so the user's `q` quit no longer reaches htop's key dispatch.
///
/// `xterm-256color` is the universally-recognised safe baseline: 256
/// colours and the standard xterm key vocabulary, no kitty advertisement.
/// Apps that want kitty mode still get it — they have to enable it
/// explicitly with `CSI > N u`, at which point the encoder pivots to
/// CSI-u (validated in `tests/htop_keys.rs`). The encoder's terminal-
/// state awareness is unchanged; only the default advertisement is.
///
/// Trade-off: phux loses ghostty-specific terminfo extensions (sixel,
/// kitty graphics caps as advertised by terminfo, the ghostty-specific
/// SGR colour extensions). Those features are still reachable when the
/// app opts in directly, and both opt-in paths exist today: the
/// server-wide `defaults.term` config knob and the per-spawn
/// `SPAWN_TERMINAL.term` wire field (phux-ign).
///
/// Status of the "revert to ghostty" question (phux-0o8): the
/// round-trip harness in `tests/kip_roundtrip.rs` proves the phux stack
/// itself round-trips the kitty keyboard protocol end-to-end under
/// `TERM=ghostty` (nvim opts in via CSI-u and every key still lands;
/// fzf/less/vim/btop are regression-free) — but htop, the canonical
/// phux-7vx reproducer, was not available to test and is exactly the
/// ncurses-`fullkbd` shape that broke before. The default therefore
/// deliberately stays `xterm-256color`; flip it only with fresh htop
/// evidence (the harness has an `#[ignore]`d htop probe ready).
#[must_use]
pub fn default_shell_command(shell: &str, login: bool) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    apply_login_mode(&mut cmd, shell, login);
    cmd.env("TERM", DEFAULT_TERM);
    cmd
}

/// The baseline `TERM` baked into [`default_shell_command`] and
/// [`shell_command`].
///
/// Matches `phux_config`'s `defaults.term` schema default. The runtime
/// overrides this per-server with the configured `defaults.term` via
/// [`apply_term`]; this constant is the value used when a `CommandBuilder`
/// is built without server config in scope (tests,
/// [`super::TerminalActor::new_with_default_shell`]).
///
/// `xterm-256color` is the universally-recognised safe baseline (phux-7vx
/// / phux-ign): 256 colours and the standard xterm key vocabulary, no
/// kitty-keyboard advertisement — so ncurses TUIs like htop keep working.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// Override the `TERM` env on `cmd` with `term`, the server's configured
/// `defaults.term`.
///
/// `CommandBuilder::env` overwrites, so this cleanly replaces the baseline
/// set by [`default_shell_command`] / [`shell_command`]. Callers in the
/// runtime apply this after building the command from the wire/config so a
/// single server-wide `TERM` default flows to the seed session,
/// attach-time creation, and `SPAWN_TERMINAL`.
pub fn apply_term(cmd: &mut CommandBuilder, term: &str) {
    cmd.env("TERM", term);
}

/// Inject `PHUX_TERMINAL_ID` (phux-w7mj) — the spawned pane's own local
/// wire id — into `cmd`'s environment.
///
/// A process running inside the pane uses it to name itself on the phux
/// wire. The id names WHICH pane but not WHICH server; zero-config
/// self-targeting needs the pair, so every spawn site applies
/// [`apply_server_socket`] alongside this (phux-cufw).
///
/// The agent-record wrapper (`examples/plugins/agent-tools`) reads this
/// var as an `@N` selector to attribute its records to the pane it runs
/// in; because the server now always provides it, the wrapper needs no
/// manual id. The value matches the hook `PHUX_TERMINAL_ID` (the same
/// `local_id().to_string()`), so both surfaces name a pane identically.
///
/// Set only for a `Local` wire id — the sole shape a freshly-spawned pane
/// receives. A `Satellite` id has no server-local `@N` and yields no var.
/// Interning the wire id is idempotent, so callers can intern pre-spawn
/// (to inject here) and re-intern after `spawn_terminal_actor` for the
/// same value.
pub fn apply_terminal_id(
    cmd: &mut CommandBuilder,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
) {
    if let Some(id) = wire_terminal_id.local_id() {
        cmd.env("PHUX_TERMINAL_ID", id.to_string());
    }
}

/// Inject `PHUX_SOCKET` (phux-cufw) — the UDS path the spawning server
/// listens on — so an in-pane `phux` verb resolves the pane's own server.
///
/// Without it, a server bound to a non-default socket spawns panes whose
/// bare `phux` invocations silently resolve the default socket path and
/// talk to a different server: `PHUX_TERMINAL_ID` names WHICH pane, this
/// names WHICH server, and only the pair identifies a pane.
///
/// `None` (state built without the runtime mirror, e.g. state-only
/// tests) leaves the child environment untouched, preserving whatever
/// `PHUX_SOCKET` the daemon itself inherited.
pub fn apply_server_socket(cmd: &mut CommandBuilder, socket_path: Option<&std::path::Path>) {
    if let Some(path) = socket_path {
        cmd.env("PHUX_SOCKET", path.as_os_str());
    }
}

/// Apply a wire-supplied working directory to `cmd` with the uniform
/// validation and fallback the seed-and-attach create path and the
/// `SESSION_CREATE_KEY` create-without-attach path share (phux-0v1l).
///
/// Precedence: a cwd already set on `cmd` is never clobbered. Only a
/// server-wide override command (`attach_create_seed_command`) can carry
/// one, and its configuration wins wholesale — the wire cwd is applied only
/// over an otherwise cwd-less builder. Both create paths call this after
/// building the command, so their precedence is identical.
///
/// Validation: the wire cwd is honored only when it names an existing,
/// *enterable* directory on this host (a directory the process can `chdir`
/// into — existence plus search/`X_OK` permission).
/// `portable_pty`'s spawn fails outright on a cwd it cannot enter, which
/// would turn a stale or foreign client-supplied path into a failed session
/// create/attach; instead an invalid path is dropped with a warn and the
/// builder's cwd stays unset, so the child lands wherever a `cwd: None`
/// spawn would. This never fails the caller — a bad cwd degrades to the
/// default directory, matching the fallback both paths document.
///
/// `session` names the target session for the warn log only.
pub fn apply_spawn_cwd(builder: &mut CommandBuilder, cwd: Option<&str>, session: &str) {
    let Some(path) = cwd else {
        return;
    };
    if builder.get_cwd().is_some() {
        // A server-wide override command pinned the cwd; it wins wholesale.
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

/// Best-effort check that `path` is a directory the spawned child can
/// actually enter (phux-0v1l).
///
/// A plain `is_dir()` gate accepts a directory the server cannot `chdir`
/// into — e.g. a mode-700 directory owned by another user — which then
/// fails the PTY spawn, contradicting the "fall back, never fail" contract.
/// This additionally requires search (execute, `X_OK`) permission, the
/// exact permission `chdir` needs, checked against the process's real
/// uid/gid via `rustix` (libc-free, cross-platform). It is best-effort:
/// a TOCTOU race or an exotic filesystem can still surprise the spawn, in
/// which case the actor build surfaces the error normally.
fn dir_is_enterable(path: &std::path::Path) -> bool {
    path.is_dir() && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

/// Build a [`CommandBuilder`] that runs a user-supplied command line as a
/// seed pane's initial program (e.g. `defaults.spawn-on-attach`,
/// phux-07y).
///
/// The command runs via `<shell> -c <command>` (or `<shell> -l -c
/// <command>` when `login` is `true` — see [`login_flag_for_shell`] and
/// [`default_shell_command`]'s doc for why a service-managed server
/// needs this) — `shell` is the resolved default shell (see
/// [`resolve_shell`]: `defaults.shell`, then `$SHELL`, then `/bin/sh`) —
/// so shell quoting and arguments inside `command` behave the same as
/// they would at an interactive prompt, and the pane closes when the
/// command exits. `TERM` is set to match [`default_shell_command`].
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
    mpsc::UnboundedReceiver<PtyEvent>,
    mpsc::Sender<EncodedInputRequest>,
    PtyOwned,
);

/// Receive from `rx` when `Some`; otherwise park forever. Used as a
/// select! arm so the actor's loop can run with or without a PTY
/// without an `expect()` or branching `if`.
pub(crate) async fn recv_or_pending(
    rx: Option<&mut mpsc::UnboundedReceiver<PtyEvent>>,
) -> Option<PtyEvent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Open a PTY pair, spawn `cmd` on the slave, and start the reader /
/// writer bridge threads. Returns the actor-side channel endpoints and
/// a [`PtyOwned`] bundle to keep the resources alive.
pub(crate) fn spawn_pty(
    cmd: CommandBuilder,
    cols: u16,
    rows: u16,
) -> Result<SpawnedPty, TerminalActorError> {
    let pty_system = native_pty_system();
    // Derive the initial winsize pixel fields from the fallback cell size so a
    // child that reads `TIOCGWINSZ` before any client resize (e.g. `kitten
    // icat` preflighting at shell startup) sees nonzero pixel dimensions. The
    // first client resize replaces these with the display's real cell size.
    let (cell_w, cell_h) = super::DEFAULT_CELL_PX;
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(cell_w),
            pixel_height: rows.saturating_mul(cell_h),
        })
        .map_err(|e| TerminalActorError::OpenPty(e.to_string()))?;

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| TerminalActorError::Spawn(e.to_string()))?;
    // Drop the slave side: the child inherits the fds, and we don't
    // need our copy. Keeping it would prevent EOF on master read after
    // the child exits.
    drop(pair.slave);

    start_pty_bridge(pair.master, child)
}

/// Adopt an inherited PTY master fd + child PID (survivors of a graceful-
/// upgrade `execve`) into a [`PtyOwned`], starting fresh bridge threads on the
/// adopted descriptor. The PTY itself is not re-opened and the child is not
/// re-spawned — they kept running across the exec; this only rebuilds the
/// server-side plumbing around them (ADR-0032).
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

/// Shared tail of [`spawn_pty`] / [`adopt_pty`]: take the master's reader +
/// writer halves, start the reader / writer bridge threads, and assemble the
/// [`PtyOwned`] bundle + actor-side channel endpoints.
fn start_pty_bridge(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
) -> Result<SpawnedPty, TerminalActorError> {
    let mut reader = master
        .try_clone_reader()
        .map_err(|e| TerminalActorError::PtyIo(e.to_string()))?;
    let writer = master
        .take_writer()
        .map_err(|e| TerminalActorError::PtyIo(e.to_string()))?;
    let master = Arc::new(Mutex::new(master));
    // The writer thread needs its own handle on the master to inspect its
    // termios before a write (phux-mjmc); `PtyOwned::master` below keeps the
    // other clone alive for resize ioctls.
    let master_for_writer = Arc::clone(&master);

    let (pty_tx_to_actor, pty_rx_for_actor) = mpsc::unbounded_channel::<PtyEvent>();
    let (input_tx_to_writer, mut input_rx_for_writer) =
        mpsc::channel::<EncodedInputRequest>(super::DEFAULT_INPUT_MAILBOX);

    let reader_thread = std::thread::Builder::new()
        .name("phux-pty-reader".to_owned())
        .spawn(move || {
            let mut buf = [0u8; PTY_READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = pty_tx_to_actor.send(PtyEvent::Eof);
                        break;
                    }
                    Ok(n) => {
                        debug!(n, "pty read");
                        if pty_tx_to_actor
                            .send(PtyEvent::Bytes(buf[..n].to_vec()))
                            .is_err()
                        {
                            // Actor went away.
                            break;
                        }
                    }
                    Err(err) => {
                        debug!(?err, "pty reader thread: read error");
                        let _ = pty_tx_to_actor.send(PtyEvent::Eof);
                        break;
                    }
                }
            }
        })
        .map_err(|e| TerminalActorError::PtyIo(e.to_string()))?;

    let writer_thread = std::thread::Builder::new()
        .name("phux-pty-writer".to_owned())
        .spawn(move || {
            // `take_writer` hands back portable-pty's `UnixMasterWriter`,
            // whose `Drop` writes `\n` followed by the pane's VEOF into the
            // master. On a clean shutdown that is the intended courtesy: the
            // child sees EOF. After a FAILED write it is a hazard — that
            // newline terminates whatever truncated prefix the line
            // discipline is still holding, committing a partial line to a
            // canonical-mode shell exactly as if the user had pressed Enter
            // (phux-oxd7). Holding the writer in `ManuallyDrop` runs the
            // destructor on the clean path only. The failure paths leak one
            // dup'd fd for a pane whose input is already dead, which is the
            // right trade against executing a command nobody typed.
            let mut writer = std::mem::ManuallyDrop::new(writer);
            loop {
                let Some(request) = input_rx_for_writer.blocking_recv() else {
                    // Sender dropped — `shutdown_pty` is tearing the pane
                    // down. Run the destructor so the child still gets EOF.
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
            reader_thread: Some(reader_thread),
            writer_thread: Some(writer_thread),
        },
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod writer_tests {
    use super::*;
    use std::io::ErrorKind;

    /// A `Write` whose every call is scripted, so each errno class can be
    /// exercised without a real PTY.
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

    /// `EIO` and `EPIPE` mean the child went away — routine teardown. The
    /// bug (phux-oxd7) was treating them identically to a real fault and
    /// killing the pane's entire input path forever.
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

    /// A short write must be resumed, not abandoned. The kernel is free to
    /// accept fewer bytes than offered on every call.
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

    /// The core regression. `Write::write_all` propagates `WouldBlock` as a
    /// hard error, and the old writer treated any error as terminal — so a
    /// single transient EAGAIN killed a live pane's input permanently.
    #[test]
    fn would_block_is_retried_rather_than_killing_the_pane() {
        let mut w = ScriptedWriter::new(vec![
            Err(std::io::Error::from(ErrorKind::WouldBlock)),
            Err(std::io::Error::from(ErrorKind::WouldBlock)),
            Ok(4),
        ]);
        write_all_resilient(&mut w, b"data").expect("transient EAGAIN must not be fatal");
        assert_eq!(w.received, b"data");

        // Prove the test is not vacuous: the `write_all` the old writer used
        // fails on this exact script, which is precisely how one transient
        // EAGAIN became permanent pane-input death.
        let mut old = ScriptedWriter::new(vec![
            Err(std::io::Error::from(ErrorKind::WouldBlock)),
            Err(std::io::Error::from(ErrorKind::WouldBlock)),
            Ok(4),
        ]);
        assert_eq!(
            old.write_all(b"data")
                .expect_err("write_all must surface WouldBlock as an error")
                .kind(),
            ErrorKind::WouldBlock,
        );
    }

    /// Retries are bounded: a permanently un-writable fd must not hang the
    /// writer thread forever.
    #[test]
    fn would_block_retries_are_bounded() {
        let script = (0..=WOULD_BLOCK_RETRIES + 1)
            .map(|_| Err(std::io::Error::from(ErrorKind::WouldBlock)))
            .collect();
        let mut w = ScriptedWriter::new(script);
        let err = write_all_resilient(&mut w, b"x").expect_err("must give up eventually");
        assert_eq!(err.failure, WriteFailure::Fatal);
    }

    /// A failure must report how much the child already ingested. Without
    /// the count, a truncated prefix is indistinguishable from a clean
    /// rejection, and neither the log nor a future retry can be correct.
    #[test]
    fn failure_reports_the_partial_write_count() {
        let mut w = ScriptedWriter::new(vec![Ok(4), Err(std::io::Error::from_raw_os_error(EIO))]);
        let err = write_all_resilient(&mut w, b"abcdefgh").expect_err("should fail");
        assert_eq!(err.failure, WriteFailure::PaneGone);
        assert_eq!(err.written, 4, "must report the truncated prefix length");
    }

    /// A zero-length write with bytes outstanding means no progress is
    /// possible; the loop must exit rather than spin forever.
    #[test]
    fn zero_length_write_terminates_instead_of_spinning() {
        let mut w = ScriptedWriter::new(vec![Ok(0)]);
        let err = write_all_resilient(&mut w, b"abc").expect_err("should fail");
        assert_eq!(err.failure, WriteFailure::PaneGone);
        assert_eq!(err.written, 0);
    }

    /// Flush classifies the same way writes do — a child that exits between
    /// the write and the flush is teardown, not a fault.
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

    /// phux-i0e8.4.1: a configured `defaults.shell` wins over `$SHELL`.
    #[test]
    fn resolve_shell_prefers_the_configured_shell() {
        assert_eq!(
            resolve_shell_from(Some("/opt/fancy/fish"), Some("/bin/zsh".to_owned())),
            "/opt/fancy/fish"
        );
    }

    /// phux-i0e8.4.1: with `defaults.shell` unset (or blank — an empty
    /// program name must never be spawned), `$SHELL` is honored.
    #[test]
    fn resolve_shell_falls_back_to_env_shell() {
        assert_eq!(
            resolve_shell_from(None, Some("/bin/zsh".to_owned())),
            "/bin/zsh"
        );
        assert_eq!(
            resolve_shell_from(Some("  "), Some("/bin/zsh".to_owned())),
            "/bin/zsh"
        );
    }

    /// phux-i0e8.4.1: with neither configured nor `$SHELL`, the
    /// POSIX-guaranteed `/bin/sh` is the last resort.
    #[test]
    fn resolve_shell_falls_back_to_bin_sh() {
        assert_eq!(resolve_shell_from(None, None), "/bin/sh");
    }

    /// Spawn path: the resolved shell IS the program the pane runs —
    /// `default_shell_command` builds its `CommandBuilder` around it, so
    /// a configured `defaults.shell` (threaded via `resolve_shell`)
    /// drives the spawned child, not `$SHELL`.
    #[test]
    fn default_shell_command_spawns_the_resolved_shell() {
        let cmd = default_shell_command(
            &resolve_shell_from(Some("/opt/fancy/fish"), Some("/bin/zsh".to_owned())),
            false,
        );
        let argv = cmd.get_argv();
        assert_eq!(argv.len(), 1, "a plain shell takes no arguments");
        assert_eq!(argv[0], "/opt/fancy/fish");
    }

    /// `CommandBuilder::get_argv` returns `Vec<OsString>`; collect it into
    /// plain `String`s so assertions can compare against string literals
    /// without an `OsString` on every expected side.
    fn argv_strings(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// phux-87rr acceptance criterion 6: an ordinary (non-service) server
    /// spawns plain, non-login panes — `login = false` must add no flag
    /// even for a shell that has one.
    #[test]
    fn non_login_spawn_adds_no_flag() {
        let cmd = default_shell_command("/bin/zsh", false);
        assert_eq!(argv_strings(&cmd), vec!["/bin/zsh".to_owned()]);
    }

    /// phux-87rr acceptance criterion 3: bash, zsh, and the `/bin/sh`
    /// fallback all take `-l` for login mode.
    #[test]
    fn login_spawn_passes_dash_l_to_bash_zsh_and_sh() {
        for shell in ["/bin/bash", "/bin/zsh", "/bin/sh", "bash", "zsh", "sh"] {
            let cmd = default_shell_command(shell, true);
            assert_eq!(
                argv_strings(&cmd),
                vec![shell.to_owned(), "-l".to_owned()],
                "shell = {shell}"
            );
        }
    }

    /// phux-87rr acceptance criterion 3: fish uses `--login`, not `-l`.
    #[test]
    fn login_spawn_passes_dash_dash_login_to_fish() {
        let cmd = default_shell_command("/opt/homebrew/bin/fish", true);
        assert_eq!(
            argv_strings(&cmd),
            vec!["/opt/homebrew/bin/fish".to_owned(), "--login".to_owned()]
        );
    }

    /// phux-87rr: an unrecognized `defaults.shell` gets no login flag at
    /// all, even when `login` is requested — an explicit, documented
    /// choice over risking a fatal exec on a flag the shell may not
    /// understand.
    #[test]
    fn login_spawn_adds_no_flag_for_an_unknown_shell() {
        let cmd = default_shell_command("/opt/exotic/rc", true);
        assert_eq!(argv_strings(&cmd), vec!["/opt/exotic/rc".to_owned()]);
        assert_eq!(login_flag_for_shell("/opt/exotic/rc"), None);
    }

    /// phux-87rr: `shell_command` (the `defaults.spawn-on-attach` /
    /// `--seed-command` path) applies the same login flag ahead of
    /// `-c <command>`, so a service-managed server's seeded command also
    /// sees a profile-initialized `PATH`.
    #[test]
    fn shell_command_applies_login_flag_before_dash_c() {
        let cmd = shell_command("/bin/zsh", "htop", true);
        assert_eq!(
            argv_strings(&cmd),
            vec![
                "/bin/zsh".to_owned(),
                "-l".to_owned(),
                "-c".to_owned(),
                "htop".to_owned(),
            ]
        );
    }

    /// Basename matching: a full path to a recognized shell resolves the
    /// same flag as the bare name.
    #[test]
    fn login_flag_matches_on_basename() {
        assert_eq!(login_flag_for_shell("/usr/local/bin/bash"), Some("-l"));
        assert_eq!(
            login_flag_for_shell("/opt/homebrew/bin/fish"),
            Some("--login")
        );
    }
}

/// phux-mjmc: canonical-mode PTY write guard. Two layers — the pure
/// terminator/line-length predicate (fast, no PTY needed) and real-PTY
/// integration tests that exercise the actual writer thread through
/// [`spawn_pty`], since the bug and the fix both live at the boundary
/// between phux's write path and the kernel's line discipline.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod canonical_guard_tests {
    use super::*;
    use portable_pty::CommandBuilder;
    use std::time::Duration;

    /// Ceiling for "and nothing else arrives" checks — the opposite
    /// polarity from a delivery wait, so it must stay short (load can only
    /// make an absence check pass harder, never flakier).
    const NOTHING_ARRIVES_WINDOW: Duration = Duration::from_millis(300);
    /// Generous ceiling for "this must complete" waits against a real PTY
    /// and a real `cat` child.
    const DELIVERY_DEADLINE: Duration = Duration::from_secs(10);

    // -------------------------------------------------------------------
    // Pure predicate: exceeds_canonical_limit
    // -------------------------------------------------------------------

    #[test]
    fn single_line_exactly_at_limit_fits() {
        assert!(!exceeds_canonical_limit(&vec![b'a'; 1024], 1024, true));
    }

    #[test]
    fn single_line_one_byte_over_limit_overflows() {
        assert!(exceeds_canonical_limit(&vec![b'a'; 1025], 1024, true));
    }

    /// The total payload size is irrelevant; only the longest line is. A
    /// payload many times the limit, built entirely from short
    /// newline-terminated lines, must fit.
    #[test]
    fn many_short_terminated_lines_never_overflow_regardless_of_total_size() {
        let mut bytes = Vec::new();
        for _ in 0..50 {
            bytes.extend(std::iter::repeat_n(b'x', 100));
            bytes.push(b'\n');
        }
        assert!(
            bytes.len() > 1024,
            "test is only meaningful if the total exceeds the limit"
        );
        assert!(!exceeds_canonical_limit(&bytes, 1024, true));
    }

    /// phux-mjmc's second repro, exactly: 1800 newline-free bytes then a
    /// terminating CR. The CR arrives 776 bytes past the overflow point and
    /// cannot rescue the line — this is the "permanent wedge" mechanism,
    /// not just data loss.
    #[test]
    fn terminating_cr_past_the_limit_cannot_rescue_the_line() {
        let mut bytes = vec![b'a'; 1800];
        bytes.push(b'\r');
        assert!(exceeds_canonical_limit(&bytes, 1024, true));
    }

    /// `cr_terminates` is what makes a CR meaningful at all: with it clear
    /// (no `ICRNL` translation), a mid-payload CR is just another ordinary
    /// byte and does not end the line.
    #[test]
    fn cr_terminates_only_when_the_flag_says_so() {
        let mut bytes = vec![b'a'; 1024];
        bytes.push(b'\r');
        bytes.extend(std::iter::repeat_n(b'b', 10));
        assert!(
            !exceeds_canonical_limit(&bytes, 1024, true),
            "with ICRNL, the CR at position 1024 ends the first line \
             before it can overflow"
        );
        assert!(
            exceeds_canonical_limit(&bytes, 1024, false),
            "without ICRNL, the CR is an ordinary byte and the whole \
             1035-byte run is one overlong line"
        );
    }

    // -------------------------------------------------------------------
    // Real-PTY mechanism proof: the low-level primitive does not protect
    // against this on its own, which is why the guard has to sit in front
    // of `write_all_resilient` rather than inside it. This is true both
    // before and after phux-mjmc's fix — the fix never touches
    // `write_all_resilient` — so this test documents the mechanism the fix
    // exists to route around, rather than the fix itself.
    // -------------------------------------------------------------------

    /// Open a real PTY pair with the exact call [`spawn_pty`] uses
    /// (`native_pty_system().openpty(..)`, no explicit termios — the OS
    /// default, which is cooked/`ICANON` mode). The slave is kept open
    /// (never read) so the line discipline stays alive without a foreground
    /// process complicating the byte stream with its own echo-back.
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

    /// The canonical-line limit the guard will resolve for any pane on this
    /// platform, read off a scratch pty.
    ///
    /// Tests that need an over-limit payload derive its size from this
    /// rather than hardcoding a number: darwin's queue is 1024 and Linux's
    /// is 4096, so a literal that overflows one sits comfortably inside the
    /// other and silently stops testing anything.
    fn platform_canonical_limit() -> usize {
        let (master, _slave) = open_default_pty();
        let raw_fd = master.as_raw_fd().expect("real pty has a raw fd");
        // SAFETY: `raw_fd` names `master`, kept alive by the binding above
        // for the whole call; the borrow does not outlive this function and
        // is never used to close or duplicate the fd.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        canonical_limit(borrowed)
    }

    #[test]
    fn write_all_resilient_alone_truncates_a_canonical_mode_line() {
        let (master, _slave) = open_default_pty();
        let mut writer = master.take_writer().expect("take writer");
        let mut reader = master.try_clone_reader().expect("clone reader");
        let raw_fd = master.as_raw_fd().expect("real pty has a raw fd");
        // SAFETY: `raw_fd` names `master`, which this test keeps alive via
        // the `master` binding for the whole function; the borrow does not
        // outlive this synchronous call.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        // Resolve the limit exactly as the guard does. Reading `fpathconf`
        // directly here sized the payload at 255+37 on Linux, which does not
        // overflow that platform's real 4096-byte queue, so this
        // demonstration failed on CI while passing on darwin. See
        // `canonical_limit` for why the reported value is only a floor.
        let limit = canonical_limit(borrowed);

        // Oversized, newline-free payload, then a terminator to flush the
        // line — without a terminator nothing would be readable at all
        // (phux-mjmc's own repro methodology).
        let payload = vec![b'a'; limit + 37];
        write_all_resilient(&mut *writer, &payload)
            .expect("the kernel write(2) itself succeeds regardless");
        write_all_resilient(&mut *writer, b"\n").expect("terminator write succeeds");
        flush_resilient(&mut *writer).expect("flush");

        let mut buf = [0u8; 8192];
        let n = reader.read(&mut buf).expect("the echoed, completed line");
        // The line discipline's ECHO reflects exactly what its canonical
        // queue accepted before the line terminated. If the platform's real
        // `MAX_CANON` ever changed, this bounds still hold: `n` must be
        // capped at the queue's real capacity, strictly less than what was
        // written.
        assert!(
            n <= limit + 1,
            "expected at most {} bytes (limit + newline) echoed back, got \
             {n}: the canonical queue did not overflow, so this payload \
             was too small to reproduce phux-mjmc on this platform",
            limit + 1
        );
        assert!(
            n < payload.len() + 1,
            "expected fewer than the {} bytes written to come back; if \
             this fails, this platform's canonical queue held the whole \
             oversized line and the guard's premise does not hold here",
            payload.len() + 1
        );
    }

    // -------------------------------------------------------------------
    // Real-PTY integration through the actual writer thread
    // (`spawn_pty` -> the production `canonical_refusal` guard).
    // -------------------------------------------------------------------

    /// phux-mjmc's first repro: 4097 entirely newline-free bytes. Old code
    /// wrote this straight through (`write_all_resilient` reports success —
    /// the kernel's `write(2)` itself never fails) and lost everything past
    /// the canonical limit with zero feedback. The guard must refuse it
    /// before any byte reaches the kernel.
    #[tokio::test(flavor = "current_thread")]
    async fn newline_free_write_over_the_limit_is_refused_before_any_byte_is_sent() {
        let cmd = CommandBuilder::new("cat");
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(cmd, 80, 24).expect("spawn cat under a real pty");

        let payload = vec![b'a'; 4097];
        let (completion_tx, completed) = std::sync::mpsc::channel();
        input_tx
            .try_send(EncodedInputRequest::acknowledged(payload, completion_tx))
            .expect("writer mailbox has room");

        // The refusal is synchronous (`tcgetattr` + `fpathconf` + a linear
        // scan, no PTY I/O) so it resolves almost immediately.
        let outcome =
            tokio::task::spawn_blocking(move || completed.recv_timeout(DELIVERY_DEADLINE))
                .await
                .expect("blocking task")
                .expect("writer thread must reply");
        match outcome {
            WriteCompletion::CanonicalLimitExceeded { limit } => {
                assert!(limit > 0, "must report a real limit, not a placeholder");
            }
            other => panic!("expected CanonicalLimitExceeded, got {other:?}"),
        }

        // Nothing reached the kernel: no echo, and `cat` never saw a
        // completed line to forward.
        let extra = tokio::time::timeout(NOTHING_ARRIVES_WINDOW, pty_rx.recv()).await;
        assert!(
            extra.is_err(),
            "a refused payload must not put any bytes on the wire"
        );
        let _ = pty.child.kill();
    }

    /// phux-mjmc's second, more dangerous repro: a newline-free run past the
    /// canonical limit followed by a terminating CR. Old code lost the CR
    /// along with the rest of the overflow, so the line never completed and
    /// the pane was permanently wedged. The guard must refuse the whole
    /// batch up front.
    ///
    /// The bead's own repro used a literal 1800 bytes, which overflows
    /// darwin's 1024-byte queue but sits comfortably inside Linux's 4096 --
    /// where the guard then correctly does NOT refuse, and this test failed
    /// asserting a refusal that should not happen. The payload is sized from
    /// the platform's real limit so the test asserts the invariant rather
    /// than one machine's arithmetic.
    #[tokio::test(flavor = "current_thread")]
    async fn terminating_cr_past_the_limit_is_refused_not_wedged() {
        let cmd = CommandBuilder::new("cat");
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(cmd, 80, 24).expect("spawn cat under a real pty");

        let mut payload = vec![b'a'; platform_canonical_limit() + 776];
        payload.push(b'\r');
        let (completion_tx, completed) = std::sync::mpsc::channel();
        input_tx
            .try_send(EncodedInputRequest::acknowledged(payload, completion_tx))
            .expect("writer mailbox has room");

        let outcome =
            tokio::task::spawn_blocking(move || completed.recv_timeout(DELIVERY_DEADLINE))
                .await
                .expect("blocking task")
                .expect("writer thread must reply");
        assert!(matches!(
            outcome,
            WriteCompletion::CanonicalLimitExceeded { .. }
        ));

        let extra = tokio::time::timeout(NOTHING_ARRIVES_WINDOW, pty_rx.recv()).await;
        assert!(
            extra.is_err(),
            "a refused payload must not put any bytes on the wire, \
             including the terminator that old code would have dropped"
        );
        let _ = pty.child.kill();
    }

    /// A refusal must not kill the pane's input path: a normal
    /// newline-terminated write handed to the writer immediately afterward
    /// must still be delivered.
    #[tokio::test(flavor = "current_thread")]
    async fn refusal_does_not_wedge_the_pane_for_later_writes() {
        let cmd = CommandBuilder::new("cat");
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(cmd, 80, 24).expect("spawn cat under a real pty");

        let oversized = vec![b'a'; 5000];
        let (tx1, rx1) = std::sync::mpsc::channel();
        input_tx
            .try_send(EncodedInputRequest::acknowledged(oversized, tx1))
            .expect("writer mailbox has room");
        let outcome1 = tokio::task::spawn_blocking(move || rx1.recv_timeout(DELIVERY_DEADLINE))
            .await
            .expect("blocking task")
            .expect("writer thread must reply");
        assert!(matches!(
            outcome1,
            WriteCompletion::CanonicalLimitExceeded { .. }
        ));

        let (tx2, rx2) = std::sync::mpsc::channel();
        input_tx
            .try_send(EncodedInputRequest::acknowledged(b"hello\n".to_vec(), tx2))
            .expect("writer mailbox has room");
        let outcome2 = tokio::task::spawn_blocking(move || rx2.recv_timeout(DELIVERY_DEADLINE))
            .await
            .expect("blocking task")
            .expect("writer thread must reply");
        assert_eq!(outcome2, WriteCompletion::Delivered);

        let got = tokio::time::timeout(DELIVERY_DEADLINE, pty_rx.recv())
            .await
            .expect("must not hang")
            .expect("pty output channel open");
        match got {
            PtyEvent::Bytes(bytes) => {
                assert!(bytes.windows(5).any(|w| w == b"hello"));
            }
            PtyEvent::Eof => panic!("pane closed unexpectedly"),
        }
        let _ = pty.child.kill();
    }

    /// Raw mode (`ICANON` clear) must stay on the unchecked fast path: a
    /// large, entirely newline-free payload — the shape of the 1.9 MiB JPEG
    /// from ADR-0059's slow-upload report — is delivered byte-for-byte,
    /// with no refusal.
    #[tokio::test(flavor = "current_thread")]
    async fn raw_mode_delivers_a_large_newline_free_payload_intact() {
        let cmd = CommandBuilder::new("cat");
        let (mut pty_rx, input_tx, mut pty) =
            spawn_pty(cmd, 80, 24).expect("spawn cat under a real pty");

        // Enter raw mode on the shared master fd, the same way a TUI or a
        // shell with readline in raw mode would from the slave side — the
        // discipline is a property of the pty, not of which end changed it.
        // ECHO is cleared too so only `cat`'s own forwarded copy reaches
        // this test's reader (a realistic raw-mode program disables both
        // together).
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
        let (completion_tx, completed) = std::sync::mpsc::channel();
        input_tx
            .try_send(EncodedInputRequest::acknowledged(
                payload.clone(),
                completion_tx,
            ))
            .expect("writer mailbox has room");

        let outcome =
            tokio::task::spawn_blocking(move || completed.recv_timeout(DELIVERY_DEADLINE))
                .await
                .expect("blocking task")
                .expect("writer thread must reply");
        assert_eq!(outcome, WriteCompletion::Delivered);

        let mut received = Vec::new();
        while received.len() < payload.len() {
            let chunk = tokio::time::timeout(DELIVERY_DEADLINE, pty_rx.recv())
                .await
                .expect("must not hang")
                .expect("pty output channel open");
            match chunk {
                PtyEvent::Bytes(bytes) => received.extend_from_slice(&bytes),
                PtyEvent::Eof => panic!("pty closed before full delivery"),
            }
        }
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
        let _ = pty.child.kill();
    }
}
