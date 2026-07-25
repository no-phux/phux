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

/// Resolve the default shell. Reads `$SHELL`; falls back to `/bin/sh`
/// (POSIX-guaranteed) when unset.
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
pub fn default_shell_command() -> CommandBuilder {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
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
/// The command runs via `$SHELL -c <command>` (falling back to
/// `/bin/sh`), so shell quoting and arguments inside `command` behave the
/// same as they would at an interactive prompt, and the pane closes when
/// the command exits. `TERM` is set to match [`default_shell_command`].
#[must_use]
pub fn shell_command(command: &str) -> CommandBuilder {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
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
    let mut writer = master
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
            // A write/flush error is terminal for the pane's input path:
            // the thread exits and every byte queued after it is dropped
            // on the floor. Log loudly — without this line the
            // failure is invisible: output, snapshots, and command acks
            // all keep working while input silently goes nowhere.
            while let Some(request) = input_rx_for_writer.blocking_recv() {
                if let Err(err) = writer.write_all(&request.bytes) {
                    error!(?err, "pty writer: write failed; pane input is now dead");
                    if let Some(completion) = request.completion {
                        let _ = completion.send(false);
                    }
                    break;
                }
                if let Err(err) = writer.flush() {
                    error!(?err, "pty writer: flush failed; pane input is now dead");
                    if let Some(completion) = request.completion {
                        let _ = completion.send(false);
                    }
                    break;
                }
                debug!(len = request.bytes.len(), "pty write flushed");
                if let Some(completion) = request.completion {
                    let _ = completion.send(true);
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
