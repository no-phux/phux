//! The attach entry points (`run_*`), the outer re-attach loop
//! (`attach_session`), and the `LoopExit` vocabulary it shares with
//! `main_loop`.

use std::cell::RefCell;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::OutputMode;
use phux_protocol::wire::frame::{AttachTarget, FrameKind};
use tracing::Instrument as _;

use crate::attach::connection::{Connection, Dial};
use crate::attach::input_dispatch::ReattachTarget;
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::paint::StatusBarPaint;
use crate::attach::record::{SessionRecorder, TeeSink};
use crate::predict::PredictiveConfig;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};

use super::main_loop::main_loop;
use super::session_io::{attach_client_caps, attach_client_name, send_attach, wait_for_attached};
use super::terminal::{RawModeGuard, exit_after_detach, install_panic_hook_once};

/// Production attach: wrap stdout in the off-loop [`StdoutSink`](crate::attach::stdout_writer)
/// so a slow terminal never blocks the select loop (phux-fysb), then run the
/// session. Tests use the synchronous [`run_with_stdout`] seam directly.
///
/// `rec` is the `phux --rec` session recorder (ADR-0060). When present the
/// render sink is wrapped in a [`TeeSink`] *above* the `StdoutSink`, so the
/// recording sees every composited byte even in the moments the backlog cap
/// makes the glass drop frames.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(super) async fn run_buffered(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    rec: Option<Rc<RefCell<SessionRecorder>>>,
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    let (mut sink, writer) = crate::attach::stdout_writer::spawn_stdout_writer();
    // Cloned BEFORE any wrap: the resync flag belongs to the StdoutSink, not
    // to whatever is layered on top of it.
    let resync = Arc::clone(&sink.needs_resync);
    if let Some(rec) = rec {
        let mut tee = TeeSink {
            inner: &mut sink,
            rec: Rc::clone(&rec),
        };
        attach_session(
            dial,
            target,
            &mut tee,
            predict,
            Some(resync.as_ref()),
            Some(writer),
            true,
            initial_notice,
            Some(rec),
        )
        .await
    } else {
        attach_session(
            dial,
            target,
            &mut sink,
            predict,
            Some(resync.as_ref()),
            Some(writer),
            true,
            initial_notice,
            None,
        )
        .await
    }
}

/// Dial-aware production attach (UDS *or* QUIC) with predictive echo config.
///
/// The CLI builds a [`Dial`] from its flags (a UDS path or a remote
/// `--quic` target) and the same off-loop-stdout production path runs
/// regardless of byte plumbing. Blocks until the server sends `DETACHED`
/// or the user detaches.
///
/// The function is `async` because it relies on tokio; embedders must
/// drive it on a tokio runtime. Per ADR-0003 the canonical runtime is
/// `tokio::runtime::Builder::new_current_thread` — the returned future
/// is intentionally `!Send` because libghostty's `Terminal` is `!Send`
/// and lives on the attach task's stack across `await` points. The
/// single-threaded runtime never moves the future between threads.
///
/// `predict.enabled = false` bypasses prediction entirely;
/// `predict.enabled = true` engages the Mosh-class prediction layer
/// documented in [`crate::predict`] (`phux-9gw.1`).
///
/// # Ordering (`phux-roz`)
///
/// The expensive pre-handshake work — connect, `HELLO`, `ATTACH`,
/// and the `ATTACHED` wait — runs on the *cooked* outer terminal.
/// Failures there propagate as `Err(_)` without ever entering raw mode
/// or the alt screen, so a missing server / bad session name / Ctrl-C
/// during connect prints a one-line error on the normal screen and
/// exits cleanly. Only after the server's `ATTACHED` frame arrives do
/// we flip the terminal into raw + alt screen via [`RawModeGuard`].
///
/// `initial_notice` (phux-i0e8.2.3) is a transient status-bar message shown
/// once the session is attached and painting — the CLI's reconnect loop
/// passes `re-attached after server restart` so the recovery is visible
/// *inside* the TUI (a cooked-terminal eprintln is alt-screened over within
/// milliseconds). `None` on a first attach.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_predict_dial(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    run_buffered(dial, target, predict, None, initial_notice).await
}

/// As [`run_with_predict_dial`], but tees the composited output stream into
/// `rec` — the `phux --rec` session recording (ADR-0060).
///
/// The recorder is passed in (rather than opened here) because the CLI must
/// keep the SAME recording across a graceful-upgrade reconnect: the attach
/// loop can return and be re-entered, and a recorder created per attempt
/// would truncate the cast at every reconnect.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_recorded_dial(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    rec: Rc<RefCell<SessionRecorder>>,
    initial_notice: Option<Notice>,
) -> Result<AttachEnd, AttachError> {
    run_buffered(dial, target, predict, Some(rec), initial_notice).await
}

/// UDS attach that writes the entire composited output stream to a
/// caller-supplied [`RenderSink`](crate::attach::RenderSink) (any `Write`).
///
/// The stream covers alt-screen enter, cursor hide, every pane's per-row
/// CUP/SGR, the status bar, overlays, and cleanup.
///
/// The renderer and all chrome painters are generic over `Write`, and the
/// driver threads this one sink through `main_loop` into
/// `handle_server_frame`, `paint_full_frame`, and `dispatch_input_events`.
/// So the whole attach render path is injectable: production passes real
/// stdout via [`run_with_predict_dial`]; tests and the headless agent
/// surface pass a `Vec<u8>` (or any other `Write`) and read back the
/// captured VT.
///
/// Exposed so tests can capture the byte stream and assert on it — in
/// particular, the regression guard for `phux-roz` asserts that the
/// pre-handshake failure path NEVER emits `\x1b[?1049h`. The stdin /
/// signal / termios cleanup paths run on real stdout regardless of the
/// injected sink (Drop / signal handlers can't reach it).
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_stdout<W: crate::attach::RenderSink>(
    socket: &Path,
    target: AttachTarget,
    out: &mut W,
) -> Result<AttachEnd, AttachError> {
    run_with_stdout_predict(socket, target, out, PredictiveConfig::disabled()).await
}

/// As [`run_with_stdout`], but with an explicit predictive-echo config.
/// Production callers should reach for [`run_with_predict_dial`]; this is
/// the test-injectable variant.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_stdout_predict<W: crate::attach::RenderSink>(
    socket: &Path,
    target: AttachTarget,
    out: &mut W,
    predict: PredictiveConfig,
) -> Result<AttachEnd, AttachError> {
    // Synchronous-sink test seam: no off-loop writer, no resync flag.
    attach_session(
        &Dial::uds(socket),
        target,
        out,
        predict,
        None,
        None,
        false,
        None,
        None,
    )
    .await
}

/// STAGE 1 — the pre-handshake, on the cooked outer terminal: HELLO ->
/// ATTACH -> ATTACHED.
///
/// We deliberately do NOT install `RawModeGuard` around this. If anything
/// here fails (no server, refused, signal during connect) the user's terminal
/// stays in its original state and `Err(_)` carries the actionable cause up
/// to the CLI.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn handshake(
    dial: &Dial,
    target: AttachTarget,
    probe_default_colors: bool,
) -> Result<(Connection, FrameKind, OutputMode), AttachError> {
    let default_colors = probe_default_colors
        .then(crate::attach::terminal_probe::default_colors)
        .flatten();
    let client_caps = attach_client_caps(default_colors, dial);
    let conn = Connection::connect_dial_with_hello(dial, attach_client_name(), client_caps).await?;
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol(
            "production connection returned before bootstrap negotiation".to_owned(),
        )
    })?;
    let output_mode = if matches!(
        negotiated.profile,
        phux_protocol::BootstrapProfile::SynthesizedVtStateSync
    ) {
        OutputMode::StateSync
    } else {
        OutputMode::Raw
    };
    let mut conn = conn;
    let attach_id = send_attach(&mut conn, target).await?;
    let attached = wait_for_attached(&mut conn, attach_id).await?;
    Ok((conn, attached, output_mode))
}

/// ADR-0048: read the `mouse` config (default on) to decide whether the
/// [`RawModeGuard`] also enables the client's own outer-terminal mouse
/// tracking, so divider drag-to-resize works by default. A load failure or an
/// explicit `mouse = false` falls back to pass-through-only — no DECSET, host
/// native selection untouched.
fn mouse_capture_enabled() -> bool {
    phux_config::loader::load()
        .map(|c| c.defaults.mouse)
        .unwrap_or(true)
}

/// Drain queued output and stop the off-loop stdout writer, if this attach
/// has one, so nothing is lost and later terminal-reset writes aren't
/// garbled by an in-flight frame.
fn stop_writer(writer: &mut Option<crate::attach::stdout_writer::WriterHandle>) {
    if let Some(writer) = writer.take() {
        writer.shutdown_and_join();
    }
}

/// Re-handshake against `target` on the SAME connection and clear the glass
/// for the session it lands on.
///
/// Returns the new `ATTACHED` frame for the next `main_loop` entry, which
/// rebuilds ALL session-scoped state fresh (pane mirrors, workspace, predict,
/// overlays, pending-spawn maps, layout subscription) from it, then repaints.
/// A full repaint of the new session's grid happens via the replayed
/// negotiated bootstrap frames inside the loop.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn switch_session<W: crate::attach::RenderSink>(
    conn: &mut Connection,
    out: &mut W,
    target: ReattachTarget,
    pending_window: &mut Option<usize>,
    pending_pane: &mut Option<usize>,
) -> Result<FrameKind, AttachError> {
    // Lifecycle transition (info): switching sessions on the same
    // connection. `?target` names the destination.
    tracing::info!(?target, "attach loop: SWITCH_TO; re-attaching");
    let attached = reattach_on_same_connection(conn, target, pending_window, pending_pane).await?;
    let _ = write_terminal_clear(out);
    Ok(attached)
}

/// The attach session body shared by the production
/// ([`run_with_predict_dial`]) and test-injectable
/// ([`run_with_stdout_predict`]) entry points.
///
/// `resync` is the [`StdoutSink`](crate::attach::stdout_writer) backpressure flag
/// (`None` for the synchronous test sink); `main_loop` polls it to repaint
/// the latest state after the writer dropped a stale backlog. `writer` is the
/// off-loop stdout writer's handle (`None` for the test sink); it is drained
/// and joined before the terminal-reset writes on every exit path so output
/// isn't lost and the reset isn't garbled.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "per-invocation knobs from the run_* entry points; a builder for one internal fn would be ceremony"
)]
async fn attach_session<W: crate::attach::RenderSink>(
    dial: &Dial,
    target: AttachTarget,
    out: &mut W,
    predict: PredictiveConfig,
    resync: Option<&AtomicBool>,
    mut writer: Option<crate::attach::stdout_writer::WriterHandle>,
    probe_default_colors: bool,
    // phux-i0e8.2.3: transient status-bar notice for the FIRST `main_loop`
    // entry only (an in-invocation session switch must not re-show it) —
    // the reconnect loop's "re-attached after server restart".
    initial_notice: Option<Notice>,
    recorder: Option<Rc<RefCell<SessionRecorder>>>,
) -> Result<AttachEnd, AttachError> {
    // Attach-handshake timing (info): HELLO -> ATTACH -> ATTACHED. The
    // span's CLOSE duration is the end-to-end attach latency a trace reader
    // wants for "why was the first paint slow." Lifecycle-rate, so info.
    crate::perf::mark_started();
    let handshake_span = tracing::info_span!("attach_handshake", ?target);
    let (mut conn, attached, output_mode) = handshake(dial, target, probe_default_colors)
        .instrument(handshake_span)
        .await?;
    // The output mode is a per-connection HELLO property; construction
    // negotiates exactly once and the re-attach loop below reuses the same
    // `conn`, so this bool is stable across an in-connection session switch.
    // `FRAME_ACK`s feed the server's per-seq RTT/backpressure accounting;
    // a raw consumer's acks are dropped server-side, so the loop skips them.
    let wants_state_sync = output_mode == OutputMode::StateSync;

    // STAGE 2 — server accepted the attach. Now and only now do we flip
    // the outer terminal into raw + alt screen. The guard's Drop runs
    // on unwinding; the signal-handler path inside `main_loop` runs
    // `write_terminal_reset` explicitly to cover SIGINT/SIGTERM/SIGHUP.
    let mouse_capture = mouse_capture_enabled();
    // Register the fatal-signal handler BEFORE raw mode is entered. The
    // handler snapshots termios at install time, so installing it after
    // `RawModeGuard` would capture the *raw* flags and "restore" the user
    // into raw mode on crash — the exact wedge it exists to prevent.
    //
    // This arms the termios-only variant; the escape-code resets are added
    // by `RawModeGuard` once the alt screen is actually up, so a crash
    // before that point does not emit stray DECSETs to a normal screen.
    phux_crash::install_terminal_restore_only();

    let _guard = RawModeGuard::install_with_stdout(out, mouse_capture)?;

    // Install a panic hook so an unexpected panic inside `main_loop`
    // (renderer bug, libghostty FFI surprise, etc.) still restores the
    // terminal before the default hook prints its backtrace. The hook
    // is global, so we only register it once per process.
    //
    // The hook covers panics only — those unwind, so `Drop` and the hook
    // both run. A *fatal* signal (SIGSEGV/SIGBUS/SIGABRT) does neither,
    // which is why `phux_crash` is armed separately above. Our own code is
    // `#![forbid(unsafe_code)]`, so that path is reachable essentially only
    // through the native libghostty-vt FFI boundary — the "FFI surprise"
    // this comment already anticipated, in the one form the hook can't see.
    install_panic_hook_once();

    // phux-eb0: outer re-attach loop. `main_loop` is single-session by
    // construction (it builds ~15 session-scoped locals and replays the
    // ATTACHED frame once on entry). When the user picks another session
    // via `<leader> a` the loop returns `LoopExit::SwitchTo(name)`; here
    // we detach from the current session, re-run the ATTACH handshake
    // against `ByName(name)` on the SAME transport connection (a session
    // switch is within one server, so the UDS connection — bound to the
    // server, not to any single session — is reused, not reconnected),
    // and re-enter `main_loop` with the new ATTACHED frame. The
    // `RawModeGuard` stays installed across the switch (it lives in this
    // outer scope) so the alt screen never flickers and the terminal is
    // never left in a bad state. On `Detached` the loop exits via
    // `exit_after_detach` (which never returns — see its doc comment).
    let mut attached = attached;
    // First-use guidance is profile-scoped, versioned, and best-effort. The
    // moment is decided once per process attach and consumed by the first loop
    // entry, so in-process session switches never repeat it.
    let onboarding_path = crate::attach::onboarding::state_path();
    let mut onboarding_claim = crate::attach::onboarding::begin_attach(&onboarding_path);
    // phux-foz.8: window index to select after a one-step cross-session
    // window pick (`switch-session { name, window }`) re-attaches. `None`
    // on the first attach and after plain switches; set per-iteration by
    // the SwitchTo arm below, consumed by `main_loop` once the target's
    // persisted layout loads. phux-jpqd: `pending_pane` is the pane half
    // of a one-step cross-session pane pick (`switch-session { .., pane }`).
    let mut pending_window: Option<usize> = None;
    let mut pending_pane: Option<usize> = None;
    // The window sidebar's runtime on/off state, handed back by each
    // `LoopExit::SwitchTo` and fed into the next `main_loop` entry. `None` on
    // the first attach so `[sidebar] enabled` decides. Unlike `pending_window`
    // / `pending_pane` this is deliberately NOT `take`n — it persists for the
    // life of the attach, across any number of switches.
    let mut carried_sidebar_enabled: Option<bool> = None;
    // phux-i0e8.2.3: hand the reconnect notice to the first `main_loop`
    // entry only (same `take` pattern as the onboarding hint above): a
    // session switch re-enters `main_loop` but is not a reconnect.
    let mut initial_notice = initial_notice;
    loop {
        let claim = onboarding_claim.take();
        let exit = match main_loop(
            &mut conn,
            attached,
            predict,
            out,
            resync,
            wants_state_sync,
            claim,
            initial_notice.take(),
            pending_window.take(),
            pending_pane.take(),
            carried_sidebar_enabled,
        )
        .await
        {
            Ok(exit) => exit,
            Err(err) => {
                // Drain + stop the off-loop writer before propagating; the
                // RawModeGuard's Drop restores the terminal as we unwind.
                stop_writer(&mut writer);
                return Err(err);
            }
        };
        match exit {
            LoopExit::Detached {
                end,
                locally_requested,
            } => {
                // Lifecycle transition (info): the attach loop is exiting.
                tracing::info!(?end, "attach loop: DETACHED; exiting");
                // The session's own numbers, one line, always: what a user
                // pastes into a report about a laggy session.
                tracing::info!("{}", crate::perf::summary_line());
                // The session ended (user detach, server `DETACHED`, or a
                // detach-intended disconnect). Restore the terminal and
                // exit now rather than returning up the stack: a returning
                // `Ok(())` would let the tokio runtime drop block forever
                // on the uncancellable stdin read thread (see
                // `exit_after_detach`'s doc comment).
                //
                // Drain queued output + stop the writer FIRST so nothing is
                // lost and the reset writes in `exit_after_detach` aren't
                // garbled by an in-flight frame.
                stop_writer(&mut writer);
                exit_after_detach(end, locally_requested, &onboarding_path, recorder.as_ref());
            }
            LoopExit::SwitchTo {
                target,
                sidebar_enabled,
            } => {
                // The sidebar is the human's chrome, not the session's. Carry
                // the toggle into the next entry so the strip does not blink
                // shut on every space switch.
                carried_sidebar_enabled = Some(sidebar_enabled);
                attached = switch_session(
                    &mut conn,
                    out,
                    target,
                    &mut pending_window,
                    &mut pending_pane,
                )
                .await?;
            }
        }
    }
}

/// Detach the current session and re-handshake against `target` on the SAME
/// connection, returning the new `ATTACHED` frame.
///
/// Tears down the current session on the server first so it frees our
/// per-consumer reference grid and reaps the detached consumer rather than
/// leaking it. `DETACH` does not close the connection server-side (the
/// server's DETACH arm emits `DETACHED` and keeps its read loop alive), so
/// the same `conn` is reusable — no reconnect, because one server owns all
/// the sessions and the transport is bound to the server, not to any single
/// session.
///
/// An existing session re-attaches by name; a new-session request creates it
/// (or attaches, if the name is already taken) via `CreateIfMissing`.
/// phux-foz.8: a one-step window pick carries a target window, stashed in
/// `pending_window` for the next `main_loop` entry, which resolves it once the
/// new session's layout loads. phux-jpqd: a foreign fleet row also carries a
/// target pane, resolved after the window select.
async fn reattach_on_same_connection(
    conn: &mut Connection,
    target: ReattachTarget,
    pending_window: &mut Option<usize>,
    pending_pane: &mut Option<usize>,
) -> Result<phux_protocol::wire::frame::FrameKind, AttachError> {
    detach_and_drain(conn).await?;
    let attach_target = match target {
        ReattachTarget::Existing { name, window, pane } => {
            *pending_window = window;
            *pending_pane = pane;
            AttachTarget::ByName(name)
        }
        ReattachTarget::Create(name) => create_session_target(name),
    };
    let attach_id = send_attach(conn, attach_target).await?;
    let attached = wait_for_attached(conn, attach_id).await?;
    tracing::info!("attach loop: re-attach handshake complete");
    Ok(attached)
}

/// Build the `CreateIfMissing` target for an in-TUI session create (the
/// session picker's "new session" row, phux-0db).
///
/// Carries the client process's current working directory instead of
/// `cwd: None`: `None` on the wire seeds the pane in the *daemon's* CWD
/// (typically `$HOME` for a long-lived server), which breaks tools whose
/// persistence is keyed by directory (e.g. `claude --resume`). The server
/// validates the path and falls back to its default spawn directory when
/// it is not an enterable directory on the server host, so a stale or
/// remote-client path can never fail the create.
pub(super) fn create_session_target(name: String) -> AttachTarget {
    AttachTarget::CreateIfMissing {
        name,
        command: None,
        cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
    }
}

/// phux-eb0: send `DETACH` and drain frames until `DETACHED` arrives, so
/// the server-side per-consumer state (reference grid, subscriber lists)
/// is released before the next `ATTACH` on the same connection.
///
/// Frames that arrive between our `DETACH` and the server's `DETACHED`
/// (a `TERMINAL_OUTPUT` already in flight, a late `METADATA_CHANGED`) are
/// discarded — we are tearing the session down and rebuilding all
/// session-scoped state on the next attach, so nothing in this window is
/// worth applying. A server-initiated disconnect during the drain is a
/// genuine error (the switch can't complete), surfaced as
/// `AttachError::Disconnected`.
async fn detach_and_drain(conn: &mut Connection) -> Result<(), AttachError> {
    conn.send(&FrameKind::Detach).await?;
    loop {
        match conn.recv().await? {
            // Any reason ends the drain: we asked for this detach, and the
            // next ATTACH rebuilds every session-scoped thing the reason
            // could have qualified.
            FrameKind::Detached { .. } => return Ok(()),
            other => {
                tracing::trace!(kind = ?other, "draining frame during session switch");
            }
        }
    }
}

/// phux-eb0: clear the alt screen between sessions so the previous
/// session's grid doesn't briefly show under the new session's first
/// paint. The new bootstrap repaint lands immediately after, so this is a
/// one-frame clear, not a flicker.
fn write_terminal_clear<W: Write>(out: &mut W) -> io::Result<()> {
    out.write_all(b"\x1b[2J\x1b[H")?;
    out.flush()
}

/// phux-eb0: how the `main_loop` `select!` loop terminated.
///
/// `main_loop` is single-session by construction — it builds all the
/// session-scoped locals up front and replays one ATTACHED frame. Rather
/// than tear down and rebuild that state in place, the loop signals its
/// caller ([`run_with_stdout_predict`]'s outer loop) which way it exited:
///
/// * `Detached(end)` — the user detached, the server sent `DETACHED`, or
///   the last pane closed (phux-i0e8.2.2 — `end` carries which, plus the
///   dead pane's exit status). The outer loop runs `exit_after_detach(end)`
///   on this path, which prints the last-pane explanation on the cooked
///   terminal and exits the process.
/// * `SwitchTo(target)` — the user committed `switch-session { name }`
///   (via the `<leader> a` picker / palette) or `new-session`. The outer
///   loop detaches from the current session and re-runs the handshake
///   against the target (`ByName` for an existing session, `CreateIfMissing`
///   for a new one), then re-enters `main_loop` with the new ATTACHED frame
///   and freshly-rebuilt session state.
#[derive(Debug)]
pub(super) enum LoopExit {
    /// The session ended (detach / server DETACHED / last pane closed).
    /// Carries WHY (phux-i0e8.2.2) so the teardown path can explain a
    /// last-pane death on the cooked terminal. The process exits.
    Detached {
        end: AttachEnd,
        locally_requested: bool,
    },
    /// Re-attach on the same connection — to an existing session or a
    /// newly-created one.
    SwitchTo {
        /// Where to re-attach.
        target: ReattachTarget,
        /// The window sidebar's RUNTIME on/off state at the moment of the
        /// switch.
        ///
        /// `main_loop` rebuilds every session-scoped local on re-entry, which
        /// is right for session state and wrong for this: the sidebar is the
        /// human's chrome, not the session's, and switching spaces does not
        /// change which window they are looking at. Without carrying it out,
        /// the next entry re-seeds the strip from `[sidebar] enabled` and
        /// silently reverts a `toggle-sidebar` the user made.
        sidebar_enabled: bool,
    },
}

/// Classify a detach independently from the wire frame that completed it.
/// A server `DETACHED` is local only when this client had already requested
/// detach; pane death remains its own ending even if the events race.
pub(super) const fn is_local_detach(end: AttachEnd, local_intent: bool) -> bool {
    local_intent && matches!(end, AttachEnd::Detached { .. })
}

pub(super) const fn detached_loop_exit(end: AttachEnd, local_intent: bool) -> LoopExit {
    LoopExit::Detached {
        end,
        locally_requested: is_local_detach(end, local_intent),
    }
}

/// The window sidebar's enabled flag at `main_loop` entry.
///
/// A first attach carries nothing, so `[sidebar] enabled` decides. An
/// in-process session switch carries the RUNTIME value — `toggle-sidebar` may
/// have flipped it since attach — and that wins over the config default in
/// BOTH directions: a strip opened by hand stays open across
/// `switch-session`, and one closed by hand stays closed even under a config
/// that defaults it on.
///
/// This is the client-local half of the driver's reset convention.
/// Session-scoped locals (`zoomed`, the pane map, `attention_navigation`) are
/// rebuilt on every entry because they name things that belong to the session
/// being left. The sidebar names something that belongs to the human's
/// window, and it was the only chrome toggle on the wrong side of that line.
///
/// Deliberately not `Option::unwrap_or`, which is not `const`.
pub(super) const fn seed_sidebar_enabled(carried: Option<bool>, configured: bool) -> bool {
    match carried {
        Some(enabled) => enabled,
        None => configured,
    }
}

pub(super) fn finish_onboarding_claim(
    claim: Option<crate::attach::onboarding::AttachClaim>,
    delivery_accepted: bool,
) {
    if delivery_accepted && let Some(claim) = claim {
        let _ = claim.commit();
    }
}

pub(super) fn finish_return_onboarding_after_paint(
    claim: &mut Option<crate::attach::onboarding::AttachClaim>,
    status_bar: Option<&StatusBarPainter>,
    paint: StatusBarPaint,
) {
    if paint.delivered(status_bar, crate::attach::onboarding::RETURN_NOTICE) {
        finish_onboarding_claim(claim.take(), true);
    }
}
