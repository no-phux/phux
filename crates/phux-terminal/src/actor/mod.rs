//! Per-pane Terminal actor.
//!
//! Owns a `libghostty_vt::Terminal`, a `portable_pty` master, and per-pane
//! input encoders, and runs a `select!` loop forwarding PTY output to
//! clients and client input to the PTY. `Terminal` is `!Send`, so the actor
//! runs via `spawn_local` on the server's `LocalSet` (ADR-0014) and is
//! reached only through `Send` channel handles ([`TerminalHandle`]). The
//! writer's half is one thread per pane. Quiet readers share one poller;
//! a pane that is producing output gets its own reader until it goes quiet.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use bytes::Bytes;
use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::terminal::SizeReportSize;
use phux_protocol::ClientId;
use phux_protocol::wire::frame::{
    AgentEvent, ControlAction, FrameKind, ResourceLifecycle, TerminalSignal,
};
use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

use crate::TerminalInput;
use crate::agent_detect::{AgentDetectEvent, AgentDetector, DetectOutcome};
use crate::grid::{ConsumerReference, SnapshotBytes, SnapshotSynthesizer};
use crate::input::paste::PasteOutcome;
use crate::input::{
    InputEncoderSnapshot, PerTerminalFocusEncoder, PerTerminalKeyEncoder, PerTerminalMouseEncoder,
    PerTerminalPasteEncoder,
};
use phux_resource::mailbox::Outbound;
use phux_resource::{ResourceCore, ResourceHandle, ResourceKind};

mod ask_marker;
mod canonical;
mod construct;
mod consumers;
mod effects;
mod events;
pub mod fast_pty;
mod fd_shrink;
mod io;
mod native;
mod park;
mod process_facet;
pub mod program_status;
#[cfg(any(test, feature = "test-support"))]
mod pty_fixture;
pub mod requests;
mod run_loop;
pub mod spawn;
pub mod sync;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_lifecycle;
#[cfg(test)]
mod tests_resize;
#[cfg(test)]
mod tests_state_sync;
pub mod tick;

#[allow(clippy::wildcard_imports, reason = "shared across this module")]
use ask_marker::*;
#[allow(clippy::wildcard_imports, reason = "shared across this module")]
use canonical::*;

pub use fast_pty::{DirectWrite, LaneAhead};
pub use requests::*;
pub use spawn::*;
pub use sync::*;
pub use tick::*;

/// Line half of [`DEFAULT_SCROLLBACK`]: a tmux-style mid-range value.
const DEFAULT_MAX_SCROLLBACK: u32 = 10_000;

/// Finite history replay for resource attachment and replacement generations.
pub const DEFAULT_REPLAY_SCROLLBACK_LINES: u32 = 1_000;

/// Scrollback bounds for the no-config constructors; the runtime passes the
/// configured `defaults.history-limit`/`history-bytes` (ADR-0094).
const DEFAULT_SCROLLBACK: phux_config::ScrollbackLimits =
    phux_config::ScrollbackLimits::new(DEFAULT_MAX_SCROLLBACK, phux_config::DEFAULT_HISTORY_BYTES);

/// Fallback cell size in pixels until a client reports real metrics, so
/// `TIOCGWINSZ` and `CSI 14 t` never report `0x0` (pixel probes such as
/// `kitten icat` refuse that).
const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

/// Maximum native history cuts retained per terminal (a hard memory bound;
/// the release contract exercises eight clients).
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const MAX_NATIVE_HISTORY_CLIENTS: usize = 8;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const MAX_NATIVE_REPLAY_BYTES: usize = 4 * 1024 * 1024;

/// Max ready PTY chunks coalesced into one `vt_write` + broadcast per pump
/// wakeup, so an unbroken stream cannot monopolize the loop.
const MAX_PTY_COALESCE: usize = 64;

/// Byte cap on one coalesced `vt_write`: each is a synchronous parse that
/// blocks the loop, so this bounds how long a queued keystroke waits. The
/// parser is streaming, so splitting here loses nothing.
pub(crate) const MAX_PTY_COALESCE_BYTES: usize = 48 * 1024;

/// Join PTY chunks into one payload, the copy a full actor burst performs.
///
/// The instruction-count guard measures this. Production ingest goes through
/// the actor, which stops before a chunk would cross [`MAX_PTY_COALESCE_BYTES`].
#[doc(hidden)]
#[allow(unreachable_pub, reason = "the icount bench crate is the caller")]
#[must_use]
pub fn icount_join_pty_burst(chunks: Vec<Bytes>) -> Bytes {
    run_loop::icount_join_pty_burst(chunks)
}

/// Max inline input events drained per wakeup, bounding one pathological
/// batch (an expanded paste).
const MAX_INPUT_COALESCE: usize = 16;

/// Grace a PTY child gets after `SIGHUP` on teardown before `SIGKILL`: long
/// enough for an agent to persist its transcript, short enough for snappy
/// close.
const PANE_KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(500);
const PANE_KILL_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Effective hangup grace; production is always [`PANE_KILL_GRACE`]. A
/// deadline, not a sleep: a child that dies at once returns on the first
/// poll. Tests may stretch it.
#[cfg(not(test))]
const fn pane_kill_grace() -> std::time::Duration {
    PANE_KILL_GRACE
}

#[cfg(test)]
fn pane_kill_grace() -> std::time::Duration {
    PANE_KILL_GRACE_OVERRIDE
        .with(Cell::get)
        .unwrap_or(PANE_KILL_GRACE)
}

#[cfg(test)]
thread_local! {
    static PANE_KILL_GRACE_OVERRIDE: Cell<Option<std::time::Duration>> =
        const { Cell::new(None) };
    static PANE_KILL_GRACE_GATE: RefCell<Option<std::path::PathBuf>> =
        const { RefCell::new(None) };
}

/// How long a test may wait for a trap-started marker before the grace
/// ceiling begins.
#[cfg(test)]
const PANE_KILL_GRACE_GATE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Stretch the pane-kill grace for this thread until the guard drops (tests).
#[cfg(test)]
fn stretch_pane_kill_grace(grace: std::time::Duration) -> PaneKillGraceOverride {
    stretch_pane_kill_grace_after(grace, None)
}

/// Like [`stretch_pane_kill_grace`], but start the ceiling only once `gate`
/// exists (tests).
#[cfg(test)]
fn stretch_pane_kill_grace_after(
    grace: std::time::Duration,
    gate: Option<&std::path::Path>,
) -> PaneKillGraceOverride {
    PANE_KILL_GRACE_OVERRIDE.with(|slot| slot.set(Some(grace)));
    PANE_KILL_GRACE_GATE.with(|slot| {
        *slot.borrow_mut() = gate.map(std::path::Path::to_path_buf);
    });
    PaneKillGraceOverride
}

#[cfg(test)]
fn pane_kill_grace_gate() -> Option<std::path::PathBuf> {
    PANE_KILL_GRACE_GATE.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
struct PaneKillGraceOverride;

#[cfg(test)]
impl Drop for PaneKillGraceOverride {
    fn drop(&mut self) {
        PANE_KILL_GRACE_OVERRIDE.with(|slot| slot.set(None));
        PANE_KILL_GRACE_GATE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

/// Ceiling on reaping the child after `SIGHUP`/`SIGKILL` and on joining
/// each bridge thread. Reached only when something already went wrong; the
/// alternative (a blocking `waitpid` or `join`) would freeze every pane on
/// the runtime (ADR-0003).
///
/// Expiry leaks boundedly: the child goes to a detached reaper thread (phux
/// has no `SIGCHLD` handler), and a late bridge thread is detached until its
/// descriptor closes.
const PANE_KILL_REAP_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const NATIVE_HISTORY_TTL: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const NATIVE_CAPTURE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30);
/// The Terminal engine: one per-pane actor.
///
/// Owns the `Terminal`, the PTY, the input encoders, and a [`ResourceCore`], serving [`ResourceHandle`]
/// and [`TerminalHandle`]. Shared pieces sit in `RefCell`s so each `select!`
/// arm can borrow what it needs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "DEC mode bits and internal state flags are independent; collapsing them would obscure individual semantics"
)]
pub struct TerminalActor {
    terminal: RefCell<CanonicalTerminal>,
    synth: RefCell<SnapshotSynthesizer<'static>>,
    /// Set on every canonical mutation, cleared by each `tick_emit`; lets an
    /// idle pane skip the per-consumer walk. Self-owned because libghostty's
    /// dirty bits are consumed by any `RenderState::update`.
    terminal_dirty_since_tick: bool,
    /// When input last reached the PTY writer, for the `echo.server` sample.
    last_input_at: std::cell::Cell<Option<std::time::Instant>>,
    /// When this pane last produced output (gates `echo.server` arming).
    last_output_at: std::cell::Cell<Option<std::time::Instant>>,
    /// Backing-agnostic half: output sequence and broadcast, event fan-out,
    /// lifecycle, control mailbox.
    core: ResourceCore,
    key_enc: RefCell<PerTerminalKeyEncoder>,
    mouse_enc: RefCell<PerTerminalMouseEncoder>,
    focus_enc: RefCell<PerTerminalFocusEncoder>,
    paste_enc: RefCell<PerTerminalPasteEncoder>,
    input_rx: mpsc::Receiver<TerminalInput>,
    /// Bounded lane-to-actor handoff of already encoded PTY bytes.
    encoded_input_rx: mpsc::Receiver<EncodedInputRequest>,
    /// Publishes terminal-derived input modes and dimensions to the input lane.
    input_snapshot_tx: watch::Sender<InputEncoderSnapshot>,
    snapshot_rx: mpsc::Receiver<SnapshotRequest>,
    native_requests: NativeRequestReceivers,
    /// Native checkpoint bindings keyed by `(owner, stream_id)`.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_cursor_owners: HashMap<NativeCursorKey, NativeCursorOwner>,
    /// Native pumps the last reflow tombstoned, owed a resync.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    reflow_tombstoned: Vec<phux_resource::ResyncTarget>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pending_native_bootstrap: Option<PendingNativeBootstrap>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_bootstrap_backlog: VecDeque<NativeBootstrapRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pending_native_history: Option<PendingNativeHistory>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_history_backlog: VecDeque<PendingNativeHistory>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_publications:
        HashMap<crate::native_state::OpaqueHistoryCursor, NativePublicationGeneration>,
    set_default_colors_rx: mpsc::Receiver<SetDefaultColorsRequest>,
    screen_rx: mpsc::Receiver<ScreenRequest>,
    upgrade_rx: mpsc::Receiver<UpgradeHandleRequest>,
    pwd_rx: mpsc::Receiver<PwdRequest>,
    process_rx: mpsc::Receiver<ProcessFacetRequest>,
    resize_rx: ResizeReceiver,
    consumer_attach_rx: mpsc::Receiver<ConsumerAttachRequest>,
    consumer_detach_rx: mpsc::Receiver<ConsumerDetachRequest>,
    /// Per-consumer `FRAME_ACK` channel.
    consumer_ack_rx: mpsc::Receiver<ConsumerAckRequest>,
    /// Per-consumer state-sync cache (ADR-0018), keyed by [`ClientId`];
    /// inserted on attach, removed on detach.
    consumer_states: HashMap<ClientId, ConsumerSyncState>,
    /// Whether the per-consumer tick emits for every consumer (ADR-0018).
    /// `false` in production: human attach gets raw PTY bytes, and only
    /// consumers that negotiated `StateSync` are tick-managed. Test-only
    /// setters flip it.
    consumer_tick_emits: bool,
    /// PTY output from the reader thread; `None` for the no-PTY test actor.
    pty_rx: Option<mpsc::Receiver<PtyEvent>>,
    /// Chunks drained behind the first one of a PTY burst, reused across
    /// bursts so coalescing sizes its one join allocation exactly.
    pty_burst: Vec<Bytes>,
    /// Input bytes for the PTY writer thread; `None` without a PTY.
    pty_tx: Option<mpsc::Sender<EncodedInputRequest>>,
    /// PTY resources; dropped on shutdown to send EOF and stop the threads.
    pty: Option<PtyOwned>,
    /// Sink for agent events sourced from the PTY stream (SPEC §7.5), set by
    /// the runtime's spawn path and drained into the event journal.
    /// Non-blocking: a full sink drops and counts, for a `source_gap`
    /// (ADR-0123).
    event_sink: Option<phux_resource::event_sink::EventSink>,
    /// Last OSC 0/2 title, for `title_changed`. Refreshed on every chunk even
    /// if unwatched: the detector reads it (ADR-0046).
    last_title: String,
    /// Latest OSC 9;4 report as `4;<state>;<percent>`, for detection.
    last_progress: String,
    /// OSC 7501 records belong to the terminal, independently of its screens.
    program_status: program_status::ProgramStatus,
    /// Latest-state publication coalesces bursts without dropping a clear.
    program_status_sink: Option<watch::Sender<Vec<program_status::Record>>>,
    /// Agent-state detector (ADR-0046), built in [`Self::run`] for PTY-backed
    /// actors with a sink and rules.
    agent_detect: Option<crate::agent_detect::AgentDetector>,
    /// Detector output sink, drained by `runtime::client::spawn_agent_state_drain`.
    agent_state_sink: Option<mpsc::Sender<AgentDetectEvent>>,
    /// "Does this pane own a live `AgentSession` child?" (ADR-0103 §5),
    /// handed to the detector when [`Self::run`] builds it.
    live_session_probe: Option<crate::agent_detect::live_session::LiveSessionProbe>,
    /// The bound `AgentSession` child's producer channel. With one bound, a
    /// hook report is appended to its stream (ADR-0103 §6); a closed channel
    /// behaves like no child.
    agent_session_append: Option<mpsc::Sender<phux_resource::agent_session::AppendRequest>>,
    /// Grid-mutation flag for the detector's slower tick; distinct from
    /// `terminal_dirty_since_tick`, which `tick_emit` clears far more often.
    agent_dirty_since_detect: bool,
    /// Last `phux-ask` title marker, an edge filter so a stable title does
    /// not report per chunk. Changes go out as
    /// [`AgentDetectEvent::AskSentinel`]; [`phux_resource::agent_asked`] ranks them.
    last_ask: Option<AskMarker>,
    /// An ask edge a full sink refused; retried on the next chunk (parsing
    /// only reruns on a title change).
    ask_retry_owed: bool,
    /// In an output burst: `dirty` emitted, no `idle` yet.
    in_output_burst: bool,
    /// PTY output arrived since the last idle check (independent of the
    /// state-sync mutation flag).
    output_since_idle_tick: bool,
    /// Last known cwd, for `CwdChanged`. Seeded from the child at build,
    /// re-queried at OSC 133 prompts and on output idle.
    last_known_cwd: RefCell<String>,
    /// Whether the cwd has been announced; the first observation always
    /// emits, so late consumers learn the starting directory.
    cwd_announced: Cell<bool>,
    /// Shell-integration effects the engine's callbacks queued during the
    /// current write, drained in stream order after it.
    effects: effects::PendingEffects,
    /// OSC 133 prompt state behind the `process.prompt` facet.
    prompt: effects::PromptTracker,
    /// PTY child start time (Unix ms), captured at build so `process.child`
    /// still names the right process after reap and pid reuse.
    child_start_ms: Option<u64>,
    /// The child pid kept after a retained pane releases its PTY (ADR-0124).
    released_child_pid: Option<i32>,
    /// The exit facet, recorded at PTY EOF.
    exit: Option<phux_core::process::ProcessExit>,
    /// Supervisory lifecycle (ADR-0033): `Running` or `Frozen`.
    lifecycle: ResourceLifecycle,
    cols: u16,
    rows: u16,
    /// Cell size in pixels for winsize and XTWINOPS; never zero. Updated by
    /// resizes that carry pixel metrics, kept by those that do not.
    cell_px: (u16, u16),
    /// An ioctl failure must remain retryable even when the grid already
    /// has the requested geometry.
    pty_resize_pending: bool,
    /// Geometry shared with libghostty's `on_size` callback, which answers
    /// XTWINOPS queries inside `vt_write`.
    size_report: Rc<Cell<SizeReportSize>>,
}

/// Errors surfaced while constructing a [`TerminalActor`].
#[derive(Debug, thiserror::Error)]
pub enum TerminalActorError {
    /// Libghostty refused to allocate a Terminal or input encoder.
    #[error("libghostty allocation failed: {0}")]
    Terminal(#[from] libghostty_vt::Error),
    /// Failed to allocate the [`SnapshotSynthesizer`].
    #[error("SnapshotSynthesizer::new failed: {0}")]
    Synth(#[from] crate::grid::SynthesisError),
    /// Could not open a PTY pair via `portable_pty`.
    #[error("openpty failed: {0}")]
    OpenPty(String),
    /// Could not spawn the command on the PTY slave. The text is the whole
    /// wire refusal reason; the client already prefixes "spawn failed".
    #[error("{0}")]
    Spawn(String),
    /// Could not take the master halves or start the bridge threads.
    #[error("pty io setup failed: {0}")]
    PtyIo(String),
    /// The organization refused the Terminal's record (a taken session
    /// name, a store failure), so nothing was spawned.
    #[error("{0}")]
    Refused(String),
}

/// The actor plus its [`CancellationToken`]. Cancellation is explicit:
/// dropping the token does not stop the actor.
#[must_use]
pub struct TerminalActorBundle {
    /// The actor; pass to `tokio::task::spawn_local`.
    pub actor: TerminalActor,
    /// Cross-task handle: generic resource channels plus the Terminal facet.
    pub handle: ResourceHandle,
    /// Cancel to shut the actor down.
    pub token: CancellationToken,
    /// Fires with the child's [`ExitOutcome`](phux_core::process::ExitOutcome)
    /// at PTY EOF; the runtime's EOF watcher drives client detach from it.
    /// `Option` so it can be taken once.
    pub exit_notify: Option<oneshot::Receiver<phux_core::process::ExitOutcome>>,
    /// The child process's identity (R12), recorded on its activation so a
    /// later start can tell a surviving orphan from a reused pid. `None`
    /// without a PTY child or when its start time cannot be read.
    pub proc: Option<phux_core::ProcIdentity>,
}

impl std::fmt::Debug for TerminalActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalActor")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("has_pty", &self.pty.is_some())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for TerminalActorBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalActorBundle")
            .field("actor", &self.actor)
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

/// Where a [`TerminalActor`]'s backing PTY comes from.
enum PtySource {
    /// No PTY (test / projection-only actors).
    None,
    /// Open a fresh PTY and spawn `cmd` on the slave.
    Spawn(CommandBuilder),
    /// Re-adopt a PTY master fd and child pid after a graceful-upgrade exec
    /// (ADR-0032).
    Adopt {
        /// Inherited master descriptor (`FD_CLOEXEC` cleared before the exec).
        master_fd: std::os::fd::RawFd,
        /// Surviving child PID on the slave side.
        child_pid: i32,
    },
}
