//! The attach entry points (`run_*`) and the outer re-attach loop
//! (`attach_session`). The `LoopExit` vocabulary it shares with
//! `main_loop` lives in `loop_exit`.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use phux_protocol::caps::OutputMode;
use phux_protocol::wire::frame::{AttachTarget, FrameKind};
use tracing::Instrument as _;

use crate::attach::connection::{Connection, Dial};
use crate::attach::input_dispatch::ReattachTarget;
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::record::{SessionRecorder, TeeSink};
use crate::predict::PredictiveConfig;
use crate::render::chrome::status_bar::Notice;

// Re-exported so `main_loop`, `loop_state`, and the tests keep their
// `entry::` paths.
#[cfg(test)]
pub(super) use super::loop_exit::is_local_detach;
pub(super) use super::loop_exit::{
    CarriedSidebar, EntryPick, LoopExit, SwitchCarry, detached_loop_exit, finish_onboarding_claim,
    finish_return_onboarding_after_paint, seed_sidebar_enabled,
};
use super::main_loop::main_loop;
use super::session_io::{attach_client_caps, attach_client_name, send_attach, wait_for_attached};
use super::terminal::{RawModeGuard, exit_after_detach, install_panic_hook_once};

enum AttachStart<'a> {
    Dial(&'a Dial),
    Connection(Box<Connection>, &'a Dial),
}

/// Production attach: render through the off-loop `StdoutSink` so a slow
/// terminal never blocks the loop. `rec` (ADR-0060) tees the composited
/// bytes above that sink, so a recording keeps frames the glass drops.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn run_from_start(
    start: AttachStart<'_>,
    target: AttachTarget,
    predict: PredictiveConfig,
    rec: Option<Rc<RefCell<SessionRecorder>>>,
    initial_notice: Option<Notice>,
    input_replay: Option<Rc<RefCell<crate::attach::input_replay::InputReplayJournal>>>,
) -> Result<AttachEnd, AttachError> {
    // Best-effort release check, off-thread; the bar tick reads its answer.
    crate::attach::update_notice::spawn_background_refresh();
    let (mut sink, writer) = crate::attach::stdout_writer::spawn_stdout_writer();
    // Cloned BEFORE any wrap: the resync flag belongs to the StdoutSink, not
    // to whatever is layered on top of it.
    let resync = Arc::clone(&sink.needs_resync);
    // A reconnect already probed colors; a second OSC 10/11 query on the
    // cooked screen prints `^[]10;rgb:...` beside the reconnect banner.
    let probe_colors = initial_notice.is_none();
    if let Some(rec) = rec {
        let mut tee = TeeSink {
            inner: &mut sink,
            rec: Rc::clone(&rec),
        };
        attach_session(
            start,
            target,
            &mut tee,
            predict,
            Some(resync.as_ref()),
            Some(writer),
            probe_colors,
            initial_notice,
            Some(rec),
            input_replay,
        )
        .await
    } else {
        attach_session(
            start,
            target,
            &mut sink,
            predict,
            Some(resync.as_ref()),
            Some(writer),
            probe_colors,
            initial_notice,
            None,
            input_replay,
        )
        .await
    }
}

/// Dial-aware production attach (UDS or QUIC) with predictive echo config.
/// Blocks until the session ends.
///
/// With `rec`, the composited output is teed into it (ADR-0060); the
/// recorder is passed in so one cast survives reconnects.
///
/// The future is `!Send` (libghostty's `Terminal` lives across awaits), so
/// drive it on a current-thread runtime (ADR-0003).
///
/// Connect, `HELLO`, `ATTACH`, and the `ATTACHED` wait run on the cooked
/// terminal: a failure there returns `Err` without ever entering raw mode or
/// the alt screen. `initial_notice` is shown once attached (the reconnect
/// loop's "re-attached after server restart").
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_dial(
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    rec: Option<Rc<RefCell<SessionRecorder>>>,
    initial_notice: Option<Notice>,
    input_replay: Option<Rc<RefCell<crate::attach::input_replay::InputReplayJournal>>>,
) -> Result<AttachEnd, AttachError> {
    run_from_start(
        AttachStart::Dial(dial),
        target,
        predict,
        rec,
        initial_notice,
        input_replay,
    )
    .await
}

/// [`run_dial`] over an already connected and HELLO-negotiated transport,
/// avoiding a second dial after a reconnect probe. `dial` is kept for
/// auxiliary control operations.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_connection(
    connection: Connection,
    dial: &Dial,
    target: AttachTarget,
    predict: PredictiveConfig,
    rec: Option<Rc<RefCell<SessionRecorder>>>,
    initial_notice: Option<Notice>,
    input_replay: Option<Rc<RefCell<crate::attach::input_replay::InputReplayJournal>>>,
) -> Result<AttachEnd, AttachError> {
    run_from_start(
        AttachStart::Connection(Box::new(connection), dial),
        target,
        predict,
        rec,
        initial_notice,
        input_replay,
    )
    .await
}

/// UDS attach that writes the whole composited output stream to a sink.
///
/// Covers alt screen, panes, chrome, overlays, and cleanup. Tests and
/// the headless surface capture the VT through it; signal and termios
/// cleanup still target real stdout.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_with_stdout<W: crate::attach::RenderSink>(
    socket: &Path,
    target: AttachTarget,
    out: &mut W,
) -> Result<AttachEnd, AttachError> {
    // Synchronous-sink test seam: no off-loop writer, no resync flag, and no
    // replay journal — the UDS lane never carries one.
    attach_session(
        AttachStart::Dial(&Dial::uds(socket)),
        target,
        out,
        PredictiveConfig::disabled(),
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .await
}

/// Stage 1, on the cooked terminal: HELLO -> ATTACH -> ATTACHED. No raw mode
/// yet, so any failure leaves the user's terminal untouched.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn handshake(
    start: AttachStart<'_>,
    target: AttachTarget,
    probe_default_colors: bool,
) -> Result<(Connection, FrameKind, OutputMode), AttachError> {
    let dial = match start {
        AttachStart::Dial(dial) => dial,
        AttachStart::Connection(conn, _) => {
            let mut conn = *conn;
            let output_mode = negotiated_output_mode(&conn)?;
            let attach_id = send_attach(&mut conn, target).await?;
            let attached = wait_for_attached(&mut conn, attach_id).await?;
            return Ok((conn, attached, output_mode));
        }
    };
    let mut conn = connect_attach_dial(dial, probe_default_colors).await?;
    let output_mode = negotiated_output_mode(&conn)?;
    let attach_id = send_attach(&mut conn, target).await?;
    let attached = wait_for_attached(&mut conn, attach_id).await?;
    Ok((conn, attached, output_mode))
}

/// Establish the production TUI HELLO contract without attaching.
///
/// A reconnect probe's connection can be handed to
/// [`run_connection`] without downgrading capabilities. Colors
/// are not re-probed (a second OSC 10/11 query can print on the cooked
/// terminal).
pub async fn connect_for_attach(dial: &Dial) -> Result<Connection, AttachError> {
    connect_attach_dial(dial, false).await
}

async fn connect_attach_dial(
    dial: &Dial,
    probe_default_colors: bool,
) -> Result<Connection, AttachError> {
    let default_colors = probe_default_colors
        .then(crate::attach::terminal_probe::default_colors)
        .flatten();
    let client_caps = attach_client_caps(default_colors, dial);
    Connection::connect_dial_with_hello(dial, attach_client_name(), client_caps).await
}

fn negotiated_output_mode(conn: &Connection) -> Result<OutputMode, AttachError> {
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol(
            "production connection returned before bootstrap negotiation".to_owned(),
        )
    })?;
    Ok(
        if matches!(
            negotiated.profile,
            phux_protocol::BootstrapProfile::SynthesizedVtStateSync
        ) {
            OutputMode::StateSync
        } else {
            OutputMode::Raw
        },
    )
}

/// ADR-0048: whether the raw-mode guard also enables outer mouse tracking
/// (`mouse` config, default on; a config that fails to load also leaves it
/// on, matching the shipped default).
fn mouse_capture_enabled() -> bool {
    phux_config::loader::load().map_or(true, |config| config.defaults.mouse)
}

/// Stop and join the off-loop stdout writer, if any, before reset writes.
///
/// Shutdown discards pending chunks; this is not a lossless drain.
fn stop_writer(writer: &mut Option<crate::attach::stdout_writer::WriterHandle>) {
    if let Some(writer) = writer.take() {
        writer.shutdown_and_join();
    }
}

/// Re-handshake against `target` on the SAME connection and return the new
/// `ATTACHED` frame for the next `main_loop` entry.
///
/// The glass is NOT cleared here: the previous session's frame stays up
/// through the round trips, and the next entry's held first paint replaces
/// it in one synchronized frame (`handover`). A bare `ED2` here blanked the
/// screen and dragged the visible cursor home for the whole re-attach.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn switch_session(
    conn: &mut Connection,
    target: ReattachTarget,
    pick: &mut EntryPick,
    orphan_kills: &mut super::orphans::OrphanKills,
    review: &mut crate::attach::review::ReviewIndex,
) -> Result<FrameKind, AttachError> {
    // Lifecycle transition (info): switching sessions on the same
    // connection. `?target` names the destination.
    tracing::info!(?target, "attach loop: SWITCH_TO; re-attaching");
    reattach_on_same_connection(conn, target, pick, orphan_kills, review).await
}

/// The attach session body shared by the production and test entry points.
///
/// `resync` is the stdout writer's backpressure flag and `writer` its handle
/// (both `None` for the synchronous test sink); the writer is stopped/joined before
/// every terminal reset.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "per-invocation knobs from the run_* entry points; a builder for one internal fn would be ceremony"
)]
async fn attach_session<W: crate::attach::RenderSink>(
    start: AttachStart<'_>,
    target: AttachTarget,
    out: &mut W,
    predict: PredictiveConfig,
    resync: Option<&AtomicBool>,
    mut writer: Option<crate::attach::stdout_writer::WriterHandle>,
    probe_default_colors: bool,
    // Shown on the first `main_loop` entry only, never on a session switch.
    initial_notice: Option<Notice>,
    recorder: Option<Rc<RefCell<SessionRecorder>>>,
    // ADR-0053 replay journal; outlives any one attach attempt.
    input_replay: Option<Rc<RefCell<crate::attach::input_replay::InputReplayJournal>>>,
) -> Result<AttachEnd, AttachError> {
    // Attach-handshake timing: the span's close is end-to-end attach latency.
    phux_client::perf::mark_started();
    let handshake_span = tracing::info_span!("attach_handshake", ?target);
    // Auxiliary control operations (such as pane moves) retain the original
    // route even when the attach itself reuses a negotiated connection.
    let dial = match &start {
        AttachStart::Dial(dial) | AttachStart::Connection(_, dial) => *dial,
    };
    let (mut conn, attached, output_mode) = handshake(start, target, probe_default_colors)
        .instrument(handshake_span)
        .await?;
    // Output mode is per-connection, so this holds across session switches;
    // raw consumers skip `FRAME_ACK` (the server drops their acks).
    let wants_state_sync = output_mode == OutputMode::StateSync;

    // Stage 2: attached. Only now enter raw mode + alt screen; the guard's
    // Drop and the signal arms both restore the terminal.
    let mouse_capture = mouse_capture_enabled();
    // Arm the fatal-signal handler BEFORE raw mode: it snapshots termios at
    // install, so installing it later would "restore" into raw mode.
    phux_crash::install_terminal_restore_only();

    let _guard = RawModeGuard::install_with_stdout(out, mouse_capture)?;

    // Panics unwind, so the hook (once per process) and Drop restore the
    // terminal; fatal signals are `phux_crash`'s job above.
    install_panic_hook_once();

    // Outer re-attach loop. `main_loop` is single-session; a `SwitchTo`
    // detaches and re-handshakes on the same connection (it is bound to the
    // server, not a session) while the raw-mode guard stays installed, so the
    // alt screen never flickers.
    let mut attached = attached;
    // First-use guidance, decided once per process attach and consumed by the
    // first loop entry.
    let onboarding_path = crate::attach::onboarding::state_path();
    let mut onboarding_claim = crate::attach::onboarding::begin_attach(&onboarding_path);
    // A one-step cross-session pick, consumed by the next `main_loop` entry.
    let mut pick = EntryPick::default();
    // What the last entry handed a switch (`None` on first entry).
    let mut carried: Option<SwitchCarry> = None;
    // First entry only: a session switch is not a reconnect.
    let mut initial_notice = initial_notice;
    // Stray satellite panes still owed a kill; connection-lifetime.
    let mut orphan_kills = super::orphans::OrphanKills::default();
    let mut review = crate::attach::review::ReviewIndex::new();
    loop {
        let claim = onboarding_claim.take();
        // Boxed to keep callers' futures under clippy's `large_futures`.
        let exit = match Box::pin(main_loop(
            &mut conn,
            dial,
            attached,
            predict,
            out,
            resync,
            wants_state_sync,
            claim,
            initial_notice.take(),
            std::mem::take(&mut pick),
            carried.take(),
            input_replay.clone(),
            std::mem::take(&mut orphan_kills),
            std::mem::take(&mut review),
        ))
        .await
        {
            Ok(exit) => exit,
            Err(err) => {
                // Stop and join the off-loop writer before propagating; the
                // RawModeGuard's Drop restores the terminal as we unwind.
                stop_writer(&mut writer);
                tracing::info!(
                    perf = %phux_client::perf::report().to_json(),
                    "{}",
                    phux_client::perf::summary_line()
                );
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
                // Exit rather than return: runtime drop can wait on an
                // uncancellable stdin reader. Stop and join the writer first.
                stop_writer(&mut writer);
                // Include the stopped writer's final samples.
                tracing::info!(
                    perf = %phux_client::perf::report().to_json(),
                    "{}",
                    phux_client::perf::summary_line()
                );
                exit_after_detach(end, locally_requested, &onboarding_path, recorder.as_ref());
            }
            LoopExit::SwitchTo {
                target,
                sidebar,
                orphan_kills: carried_orphans,
                review: carried_review,
                peers,
            } => {
                // The sidebar is the human's chrome; carry it into the next entry.
                carried = Some(SwitchCarry { sidebar, peers });
                orphan_kills = carried_orphans;
                review = carried_review;
                attached =
                    switch_session(&mut conn, target, &mut pick, &mut orphan_kills, &mut review)
                        .await?;
            }
        }
    }
}

/// Detach the current session and re-handshake against `target` on the SAME
/// connection (DETACH keeps the server's read loop alive), returning the new
/// `ATTACHED` frame. A new-session request uses `CreateIfMissing`; a
/// one-step pick's window, pane, or resource is stashed for the next entry.
async fn reattach_on_same_connection(
    conn: &mut Connection,
    target: ReattachTarget,
    pick: &mut EntryPick,
    orphan_kills: &mut super::orphans::OrphanKills,
    review: &mut crate::attach::review::ReviewIndex,
) -> Result<phux_protocol::wire::frame::FrameKind, AttachError> {
    detach_and_drain(conn, orphan_kills, review).await?;
    let attach_target = match target {
        ReattachTarget::Existing {
            name,
            id,
            window,
            pane,
            resource,
        } => {
            *pick = EntryPick {
                window,
                pane,
                resource,
            };
            id.map_or(AttachTarget::ByName(name), AttachTarget::ById)
        }
        ReattachTarget::Create {
            name,
            directory,
            host: _,
        } => create_session_target(name, directory),
    };
    let attach_id = send_attach(conn, attach_target).await?;
    let attached = wait_for_attached(conn, attach_id).await?;
    tracing::info!("attach loop: re-attach handshake complete");
    Ok(attached)
}

/// The `CreateIfMissing` target for an in-TUI session create.
///
/// `directory` is the focused pane's cwd or an explicit pick.
/// `None` leaves the server default. This does not read the client
/// process's working directory.
pub(super) fn create_session_target(name: String, directory: Option<String>) -> AttachTarget {
    AttachTarget::CreateIfMissing {
        name,
        command: None,
        cwd: directory.filter(|path| !path.is_empty()),
    }
}

/// Send `DETACH` and discard frames until `DETACHED`, so the server releases
/// per-consumer state before the next `ATTACH`. `orphan_kills` and the
/// review index still see drained frames: they outlive the session.
async fn detach_and_drain(
    conn: &mut Connection,
    orphan_kills: &mut super::orphans::OrphanKills,
    review: &mut crate::attach::review::ReviewIndex,
) -> Result<(), AttachError> {
    conn.send(&FrameKind::Detach).await?;
    loop {
        match conn.recv().await? {
            FrameKind::Detached { .. } => {
                orphan_kills.finish_switch_drain();
                conn.unbind_all_terminals();
                return Ok(());
            }
            other => {
                tracing::trace!(kind = ?other, "draining frame during session switch");
                review.observe_switch_drain(&other);
                orphan_kills.observe_switch_drain(other, std::time::Instant::now());
            }
        }
    }
}
