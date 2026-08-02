//! Submodule for terminal actor internals.

use super::{EncodedInputRequest, TerminalActorError};
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

/// Build the [`CommandBuilder`] for a pane that runs a plain interactive
/// shell — `shell` is the already-resolved program (see
/// [`resolve_shell`]).
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
pub fn default_shell_command(shell: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
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
/// The command runs via `<shell> -c <command>` — `shell` is the resolved
/// default shell (see [`resolve_shell`]: `defaults.shell`, then `$SHELL`,
/// then `/bin/sh`) — so shell quoting and arguments inside `command`
/// behave the same as they would at an interactive prompt, and the pane
/// closes when the command exits. `TERM` is set to match
/// [`default_shell_command`].
#[must_use]
pub fn shell_command(shell: &str, command: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
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
                let len = request.bytes.len();
                let outcome = write_all_resilient(&mut **writer, &request.bytes)
                    .and_then(|()| flush_resilient(&mut **writer));
                match outcome {
                    Ok(()) => {
                        debug!(len, "pty write flushed");
                        if let Some(completion) = request.completion {
                            let _ = completion.send(true);
                        }
                    }
                    Err(WriteError {
                        failure: WriteFailure::PaneGone,
                        source,
                        written,
                    }) => {
                        // The child exited or closed the slave. Routine
                        // teardown, not a fault: the reader thread is
                        // reporting EOF on its own path and the actor will
                        // close the pane. Anything still queued is moot.
                        debug!(
                            ?source,
                            written, len, "pty writer: child gone; input path closing"
                        );
                        if let Some(completion) = request.completion {
                            let _ = completion.send(false);
                        }
                        return;
                    }
                    Err(WriteError {
                        failure: WriteFailure::Fatal,
                        source,
                        written,
                    }) => {
                        // Genuinely unexpected. The pane's input path is
                        // dead and cannot be revived; say so loudly, with
                        // the partial-write count, because output,
                        // snapshots, and command acks all keep working
                        // while input silently goes nowhere.
                        error!(
                            ?source,
                            written, len, "pty writer: write failed; pane input is now dead"
                        );
                        if let Some(completion) = request.completion {
                            let _ = completion.send(false);
                        }
                        return;
                    }
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
||||||| parent of 15b02da0 (feat(config): defaults-table truth: implement defaults.shell, delete refresh-rate/log-filter)

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
        let cmd = default_shell_command(&resolve_shell_from(
            Some("/opt/fancy/fish"),
            Some("/bin/zsh".to_owned()),
        ));
        let argv = cmd.get_argv();
        assert_eq!(argv.len(), 1, "a plain shell takes no arguments");
        assert_eq!(argv[0], "/opt/fancy/fish");
    }
}
