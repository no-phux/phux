//! Per-pane actor (`phux-byc.5`).
//!
//! Owns a `libghostty_vt::Terminal`, a backing `portable_pty` master,
//! and per-pane input encoders. Drives a `select!` loop that forwards
//! PTY output to subscribed clients and writes client-originated input
//! back to the PTY.
//!
//! See ADR-0014 for the placement rationale. In short: `Terminal` is
//! `!Send + !Sync`, so it can't live behind a `tokio::spawn` future. It
//! lives inside a `spawn_local` task that runs on the server's existing
//! current-thread runtime via a `LocalSet`. All cross-task coordination
//! flows through channel handles ([`TerminalHandle`]) that are `Send` —
//! the actor itself never crosses a thread boundary.
//!
//! # PTY async wrapper choice
//!
//! `portable_pty::MasterPty::try_clone_reader` / `take_writer` hand out
//! `Box<dyn Read + Send>` and `Box<dyn Write + Send>` — both **blocking**
//! I/O handles. We bridge them to async with two dedicated `std::thread`s
//! (one for reads, one for writes) that talk to the actor over
//! `tokio::sync::mpsc` channels. This avoids OS-specific `AsyncFd`
//! plumbing for a feature whose value (a few PTY fds, not hundreds)
//! doesn't justify the complexity. At typical phux pane counts (1–20)
//! the per-pane thread cost is invisible against everything else the
//! server does.
//!
//! # Why `bytes::Bytes` for the output broadcast
//!
//! `tokio::sync::broadcast::Sender` requires `Clone` payloads (every
//! subscriber receives a copy of the same value). `bytes::Bytes` is the
//! standard cheap-clone byte buffer in the tokio ecosystem; `Vec<u8>`
//! would also work but at the cost of a full clone per subscriber.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use bytes::Bytes;
use libghostty_vt::terminal::SizeReportSize;
use libghostty_vt::{RenderState, Terminal as GhosttyTerminal, TerminalOptions};
use phux_protocol::ClientId;
use phux_protocol::wire::frame::{
    AgentEvent, ControlAction, FrameKind, TerminalEventType, TerminalLifecycle, TerminalSignal,
};
use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

use crate::agent_detect::{AgentDetectEvent, AgentDetector, DetectOutcome};
use crate::grid::{ConsumerReference, SnapshotBytes, SnapshotSynthesizer};
use crate::input::paste::PasteOutcome;
use crate::input::{
    InputEncoderSnapshot, PerTerminalFocusEncoder, PerTerminalKeyEncoder, PerTerminalMouseEncoder,
    PerTerminalPasteEncoder,
};
use crate::state::{Outbound, TerminalInput};

mod osc133;
pub mod requests;
pub mod spawn;
pub mod sync;
pub mod tick;

pub use requests::*;
pub use spawn::*;
pub use sync::*;
pub use tick::*;

/// Per-Terminal scrollback cap used by the no-config convenience
/// constructors ([`TerminalActor::new`] / [`TerminalActor::new_with_command`]).
/// A tmux-style mid-range value; the runtime path overrides it with
/// `defaults.history-limit` via [`TerminalActor::build_with_token`].
const DEFAULT_MAX_SCROLLBACK: u32 = 10_000;

/// Fallback per-cell pixel size `(width, height)` used to derive the PTY
/// `winsize` pixel fields and XTWINOPS size reports until a client announces
/// a viewport with usable pixel metrics. A program inside the pane that calls
/// `TIOCGWINSZ` or queries `CSI 14 t` must read nonzero pixel dimensions:
/// pixel probes such as `kitten icat` refuse to run against a terminal that
/// reports `0x0` ("Terminal does not support reporting screen sizes in
/// pixels"). `8x16` is a conventional terminal cell at ~96 DPI; it is only a
/// placeholder, replaced the moment a real client reports its display's cell
/// size via [`ResizeRequest::cell_px`]. Cells (cols/rows) stay authoritative;
/// pixels are always derived as `cells x cell size`.
const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

/// Streaming recognizer for the OSC 10/11 query form used by terminal-aware
/// applications (`OSC 10 ; ? ST` / `OSC 11 ; ? ST`). libghostty tracks the
/// effective colors but the pinned engine does not currently emit replies for
/// these two OSC queries, so the actor answers them from that canonical state.
#[derive(Debug, Default)]
struct ColorQueryScanner {
    state: ColorQueryState,
    payload: [u8; 4],
    len: usize,
    valid: bool,
}

#[derive(Debug, Default, Clone, Copy)]
enum ColorQueryState {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
}

impl ColorQueryScanner {
    fn feed(&mut self, bytes: &[u8], mut on_query: impl FnMut(u8)) {
        for byte in bytes {
            match self.state {
                ColorQueryState::Ground => match *byte {
                    b'\x1b' => self.state = ColorQueryState::Escape,
                    0x9d => self.start_osc(),
                    _ => {}
                },
                ColorQueryState::Escape => match *byte {
                    b']' => self.start_osc(),
                    b'\x1b' => {}
                    _ => self.state = ColorQueryState::Ground,
                },
                ColorQueryState::Osc => match *byte {
                    b'\x07' | 0x9c => self.finish_osc(&mut on_query),
                    b'\x1b' => self.state = ColorQueryState::OscEscape,
                    value => self.push_payload(value),
                },
                ColorQueryState::OscEscape => {
                    if *byte == b'\\' {
                        self.finish_osc(&mut on_query);
                    } else {
                        // An ESC not followed by `\\` is part of an OSC we do
                        // not recognize. Keep scanning for its terminator but
                        // never mistake its suffix for a query.
                        self.valid = false;
                        self.state = ColorQueryState::Osc;
                    }
                }
            }
        }
    }

    const fn start_osc(&mut self) {
        self.state = ColorQueryState::Osc;
        self.len = 0;
        self.valid = true;
    }

    const fn push_payload(&mut self, byte: u8) {
        if self.len < self.payload.len() {
            self.payload[self.len] = byte;
            self.len += 1;
        } else {
            self.valid = false;
        }
    }

    fn finish_osc(&mut self, on_query: &mut impl FnMut(u8)) {
        if self.valid && self.len == self.payload.len() {
            match &self.payload {
                b"10;?" => on_query(10),
                b"11;?" => on_query(11),
                _ => {}
            }
        }
        self.state = ColorQueryState::Ground;
        self.len = 0;
        self.valid = false;
    }
}

fn color_query_reply(selector: u8, color: libghostty_vt::style::RgbColor) -> Vec<u8> {
    let r = u16::from(color.r) * 0x101;
    let g = u16::from(color.g) * 0x101;
    let b = u16::from(color.b) * 0x101;
    format!("\x1b]{selector};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\").into_bytes()
}

/// Sentinel prefix an in-pane agent writes into the terminal title (OSC 0 /
/// OSC 2) to signal a pending human-answerable question (phux-2sl6).
///
/// The v1 ask-trigger is OSC-driven. libghostty-vt does not surface OSC 9 /
/// OSC 777 desktop-notification escapes through its Rust API (title, pwd, and
/// bell are the only user-notification signals it exposes), so the title is
/// the honest closest signal an agent can drive and the server can observe
/// without disturbing the per-consumer state-sync synthesizer. An agent that
/// has blocked for input sets its title to:
///
/// ```text
/// ESC ] 2 ; phux-ask:<question>                         ST
/// ESC ] 2 ; phux-ask[<id>]:<question>                   ST
/// ESC ] 2 ; phux-ask[<id>]:<question>?s=opt1|opt2|opt3  ST
/// ```
///
/// i.e. the literal prefix [`ASK_TITLE_PREFIX`], an optional `[id]`, the
/// question text, and an optional `?s=` suffix carrying `|`-separated
/// suggested answers. Retitling away from a `phux-ask` title clears the ask.
/// Full agent-state detection (manifests / hooks / OSC-9 surfacing) is the
/// follow-up phux-2sl6.4.
const ASK_TITLE_PREFIX: &str = "phux-ask";

/// A parsed in-pane "ask" marker (phux-2sl6), sourced from the terminal title.
///
/// Construct with [`AskMarker::parse`], which returns `None` for any title that
/// is not a `phux-ask` sentinel. Equality is by content so the actor can
/// coalesce: a re-asserted identical marker does not re-fire `Asked`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AskMarker {
    /// Stable id the answer correlates against. Defaults to the empty string
    /// when the title omits the `[id]` segment.
    id: String,
    /// The question text presented to the human.
    question: String,
    /// Suggested answers, in presentation order; empty when none were given.
    suggestions: Vec<String>,
}

impl AskMarker {
    /// Parse an in-pane ask marker out of a terminal title, or `None` if the
    /// title is not a `phux-ask` sentinel.
    ///
    /// Grammar (see [`ASK_TITLE_PREFIX`]): `phux-ask` then an optional
    /// `[<id>]`, then `:`, then the question, then an optional `?s=a|b|c`
    /// suggestion suffix. A bare `phux-ask` with no `:` is rejected (it is a
    /// degenerate marker carrying no question).
    fn parse(title: &str) -> Option<Self> {
        let rest = title.strip_prefix(ASK_TITLE_PREFIX)?;
        // Optional `[id]` segment immediately after the prefix.
        let (id, rest) = if let Some(after_bracket) = rest.strip_prefix('[') {
            let close = after_bracket.find(']')?;
            (
                after_bracket[..close].to_owned(),
                &after_bracket[close + 1..],
            )
        } else {
            (String::new(), rest)
        };
        // The question is introduced by ':'. Without it there is no ask.
        let body = rest.strip_prefix(':')?;
        // Optional `?s=opt1|opt2` suggestion suffix.
        let (question, suggestions) = match body.split_once("?s=") {
            Some((q, sugg)) => (
                q.to_owned(),
                sugg.split('|')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            None => (body.to_owned(), Vec::new()),
        };
        Some(Self {
            id,
            question,
            suggestions,
        })
    }
}

/// Upper bound on consecutive ready PTY chunks coalesced into a single
/// `vt_write` + broadcast frame per pump wakeup (phux-ahk burst path). A
/// heavy neovim redraw or p10k repaint arrives as many ~4KB reads; coalescing
/// collapses the per-chunk Terminal write, broadcast frame, and downstream
/// socket write into one. Bounded so a process emitting an unbroken stream
/// can't monopolize the actor's `select!` loop (starving input / snapshot
/// requests) — at 4KB/chunk this caps one drain at ~256KB.
const MAX_PTY_COALESCE: usize = 64;

/// Byte cap on one coalesced `vt_write` payload. A heavy redraw arrives
/// as many ~4KB reads; coalescing still collapses them into one frame,
/// but a single `vt_write` is a synchronous libghostty parse that blocks
/// the actor loop (and thus the input arm polled before it) for its full
/// duration. Capping at 48KB keeps a typical neovim / p10k repaint in one
/// frame while bounding the worst-case parse, so a queued keystroke
/// interleaves after at most one capped parse. libghostty's VT parser is
/// a streaming state machine, so splitting the byte stream on this
/// boundary loses no escape sequence — bytes are never reordered. Paired
/// with `MAX_INPUT_COALESCE`, this is the load-bearing bound on the output
/// arm: the two consts together keep either direction from monopolizing
/// the single-thread actor loop.
pub(crate) const MAX_PTY_COALESCE_BYTES: usize = 48 * 1024;

/// Upper bound on input events drained in a single `input_rx` wakeup
/// before returning to the `select!`. Input events are tiny (one encode +
/// channel send each) and `input_rx` is a bounded, low-rate single-client
/// mailbox that empties in microseconds, so in steady state the PTY-output
/// arm wins as soon as the mailbox drains. This cap bounds a single
/// pathological batch (a paste that the encoder expands, or a burst of
/// queued keys) so it cannot inflate one `input_rx` turn without limit; it
/// does not by itself force a yield to output. The structural output bound
/// is `MAX_PTY_COALESCE_BYTES`.
const MAX_INPUT_COALESCE: usize = 16;

/// Grace window a still-running PTY child gets to flush and exit after a
/// `SIGHUP` on pane teardown before we escalate to `SIGKILL` (phux-sw1).
/// Sized so a foreground agent (`claude`) can persist its transcript; kept
/// short so pane close / server shutdown stays snappy (idle shells exit on
/// the hangup well inside it).
// ponytail: fixed 500ms grace + 20ms poll; promote to a config knob only if a
// slow-flushing agent actually needs longer.
const PANE_KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(500);
const PANE_KILL_POLL: std::time::Duration = std::time::Duration::from_millis(20);
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const NATIVE_HISTORY_TTL: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn native_bytes(data: &[u8]) -> Result<Bytes, crate::native_state::NativeStateError> {
    let mut bytes = bytes::BytesMut::new();
    bytes
        .try_reserve(data.len())
        .map_err(|_| crate::native_state::NativeStateError::OutOfMemory)?;
    bytes.extend_from_slice(data);
    Ok(bytes.freeze())
}

#[derive(Debug)]
enum CanonicalTerminal {
    Plain(Option<GhosttyTerminal<'static, 'static>>),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Native(crate::native_state::NativeTerminalManager),
}

impl CanonicalTerminal {
    fn terminal(&self) -> &GhosttyTerminal<'static, 'static> {
        match self {
            Self::Plain(terminal) => terminal
                .as_ref()
                .expect("plain canonical terminal is present"),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.terminal(),
        }
    }

    fn vt_write(&mut self, bytes: &[u8]) {
        match self {
            Self::Plain(terminal) => terminal
                .as_mut()
                .expect("plain canonical terminal is present")
                .vt_write(bytes),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.vt_write(bytes),
        }
    }

    fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        match self {
            Self::Plain(terminal) => terminal
                .as_mut()
                .expect("plain canonical terminal is present")
                .resize(cols, rows, cell_width_px, cell_height_px),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.resize(cols, rows, cell_width_px, cell_height_px),
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_manager(
        &mut self,
    ) -> Result<
        &mut crate::native_state::NativeTerminalManager,
        crate::native_state::NativeStateError,
    > {
        if let Self::Plain(slot) = self {
            let terminal = slot
                .take()
                .ok_or(crate::native_state::NativeStateError::InvalidState)?;
            match crate::native_state::NativeTerminalManager::new(terminal, 4) {
                Ok(manager) => *self = Self::Native(manager),
                Err(failure) => {
                    let error = failure.error;
                    *self = Self::Plain(Some(failure.terminal));
                    return Err(error);
                }
            }
        }
        match self {
            Self::Native(manager) => Ok(manager),
            Self::Plain(_) => Err(crate::native_state::NativeStateError::InvalidState),
        }
    }
}

impl std::ops::Deref for CanonicalTerminal {
    type Target = GhosttyTerminal<'static, 'static>;

    fn deref(&self) -> &Self::Target {
        self.terminal()
    }
}

enum NativeActorRequest {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Bootstrap(NativeBootstrapRequest),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    History(NativeHistoryRequest),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Release(NativeReleaseRequest),
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    Disabled,
}

struct NativeRequestReceivers {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    bootstrap: mpsc::Receiver<NativeBootstrapRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    history: mpsc::Receiver<NativeHistoryRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    release: mpsc::Receiver<NativeReleaseRequest>,
}

impl NativeRequestReceivers {
    async fn recv(&mut self) -> Option<NativeActorRequest> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            tokio::select! {
                request = self.bootstrap.recv() => request.map(NativeActorRequest::Bootstrap),
                request = self.history.recv() => request.map(NativeActorRequest::History),
                request = self.release.recv() => request.map(NativeActorRequest::Release),
            }
        }
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        {
            std::future::pending::<Option<NativeActorRequest>>().await
        }
    }
}

/// Per-pane actor. Owns the `Terminal`, the PTY master, the per-pane
/// input encoders, and serves the channels exposed via [`TerminalHandle`].
///
/// `GhosttyTerminal<'static, 'static>` because we use [`GhosttyTerminal::new`] (NULL
/// allocator) — the lifetime parameters degenerate to `'static`. A
/// future custom allocator path would tie this to the surrounding
/// arena's lifetime; not needed for `phux-byc.5`.
///
/// `Terminal`, encoders, and the `SnapshotSynthesizer` are stashed
/// inside `RefCell` so the `select!` arms (which conceptually borrow
/// `&mut self`) can each take what they need without fighting the
/// borrow checker over disjoint field access.
#[allow(
    clippy::struct_excessive_bools,
    reason = "DEC mode bits and internal state flags are independent; collapsing them would obscure individual semantics"
)]
pub struct TerminalActor {
    terminal: RefCell<CanonicalTerminal>,
    synth: RefCell<SnapshotSynthesizer<'static>>,
    /// Cheap idle short-circuit for [`Self::tick_emit`] (phux-4l0).
    ///
    /// `true` whenever the canonical [`libghostty_vt::Terminal`] has been mutated
    /// (`vt_write`, resize) since the last `tick_emit`. Set at every
    /// mutation point, cleared at the top of each `tick_emit`. When this
    /// is `false` AND no consumer is awaiting its first emission, the
    /// per-consumer row walk is skipped entirely — an idle pane with N
    /// consumers then costs O(1) per tick instead of O(N * rows) row
    /// renders + allocations.
    ///
    /// Deliberately independent of libghostty's `RenderState`/`Snapshot`
    /// dirty bits: those are *consumed* (cleared) by ANY `RenderState::update`
    /// on the shared terminal (see
    /// [`crate::grid::SnapshotSynthesizer::synthesize_against_reference`]),
    /// including the one-shot updates in snapshot/screen/attach handling,
    /// so probing them here could miss a write a sibling handler already
    /// consumed. A self-owned flag cannot be clobbered that way.
    terminal_dirty_since_tick: bool,
    color_query_scanner: ColorQueryScanner,
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
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_cursor_owners:
        HashMap<crate::native_state::OpaqueHistoryCursor, (u64, tokio::time::Instant, u64)>,
    set_default_colors_rx: mpsc::Receiver<SetDefaultColorsRequest>,
    screen_rx: mpsc::Receiver<ScreenRequest>,
    upgrade_rx: mpsc::Receiver<UpgradeHandleRequest>,
    pwd_rx: mpsc::Receiver<PwdRequest>,
    resize_rx: mpsc::Receiver<ResizeRequest>,
    consumer_attach_rx: mpsc::Receiver<ConsumerAttachRequest>,
    consumer_detach_rx: mpsc::Receiver<ConsumerDetachRequest>,
    /// Per-consumer state-sync `FRAME_ACK` channel (phux-q0e.4). Drained
    /// by a select! arm that walks `consumer_states[client_id]` and
    /// advances `last_acked_seq` (the reference itself advances on emit).
    consumer_ack_rx: mpsc::Receiver<ConsumerAckRequest>,
    /// Per-consumer state-sync cache (ADR-0018, phux-q0e.2). Keyed by
    /// the [`ClientId`] the runtime uses for subscription tracking in
    /// [`crate::state::ServerState`]; entries are inserted by the
    /// ATTACH handler and removed by DETACH. `!Send` because the actor
    /// holds the `!Send` `Terminal` — fine; the whole actor lives on the
    /// `LocalSet` thread (ADR-0014).
    consumer_states: HashMap<ClientId, ConsumerSyncState>,
    /// Whether the per-consumer state-sync tick (phux-q0e.3) is the live
    /// emitter of `TerminalOutput` frames (ADR-0018).
    ///
    /// `false` in production for human TUI attach (phux-yeca). Raw PTY
    /// bytes are the byte-faithful, low-latency human path; synthesized
    /// per-consumer ticks are reserved for explicitly negotiated
    /// state-sync consumers. When `true`, the tick is the live
    /// server->client emission path: per attached consumer it diffs the
    /// live `Terminal` against that consumer's own
    /// [`crate::grid::ConsumerReference`] (via the actor's shared
    /// [`SnapshotSynthesizer`]) and pushes only the delta with a
    /// per-consumer monotonic `seq`. The reference advances on emit
    /// (emit-once); the runtime suppresses its broadcast pump for any
    /// tick-managed consumer so exactly one emitter serves each consumer.
    ///
    /// Three prerequisites had to land before this can be enabled for a
    /// negotiated consumer: all are met mechanically, but human attach stays
    /// raw until phux-fseo adds an explicit mode boundary.
    ///
    /// 1. **Single emitter (phux-3uv).** The runtime's `handle_attach`
    ///    suppresses its raw PTY-byte broadcast pump for any consumer this
    ///    actor reports as tick-managed (via
    ///    [`ConsumerAttachOutcome::tick_managed`]). Without that a
    ///    tick-emitted `TerminalOutput` and the broadcast pump's would both
    ///    land on the same consumer mailbox with independent `seq` —
    ///    double-paint, non-monotonic `seq` (proto.md §8.2).
    /// 2. **Client `FRAME_ACK` loop (phux-3uv).** The client drives
    ///    `FRAME_ACK`, advancing the server's `last_acked_seq` for
    ///    backpressure accounting (proto.md §8.2).
    /// 3. **Per-consumer dirty isolation (phux-ia4).** `RenderState::update`
    ///    *consumes* the shared `Terminal` dirty state on first read each
    ///    tick (libghostty `render.zig`), which starved all-but-one
    ///    consumer on a shared pane under the old per-consumer-`RenderState`
    ///    dirty model. Resolved by diffing each consumer against its own
    ///    [`crate::grid::ConsumerReference`] (rendered row bodies), which
    ///    never reads the shared dirty bits — full per-consumer isolation
    ///    regardless of attach/ack divergence.
    ///
    /// Tests may set it either way via the test-only setters; production
    /// leaves it `false` until output mode negotiation exists.
    consumer_tick_emits: bool,
    /// Bytes streaming in from the PTY reader thread. `None` when this
    /// actor is the no-PTY test variant (`TerminalActor::new`); the select!
    /// branch becomes a no-op via `Option::as_mut`.
    pty_rx: Option<mpsc::UnboundedReceiver<PtyEvent>>,
    /// Outbound bytes destined for the PTY writer thread. `None` for
    /// the no-PTY test variant.
    pty_tx: Option<mpsc::Sender<EncodedInputRequest>>,
    /// PTY backing resources. Kept alive for the actor's lifetime;
    /// dropped on shutdown to send EOF to the slave and tear down the
    /// reader/writer threads.
    pty: Option<PtyOwned>,
    output_tx: broadcast::Sender<PaneOutput>,
    /// One-shot fired when the actor observes PTY EOF. Paired with the
    /// matching receiver in [`TerminalActorBundle::exit_notify`]; the
    /// runtime uses it to drive client-detach on shell exit (phux-it8).
    ///
    /// `Option` so the actor can `.take()` it after firing — sending on
    /// a `oneshot::Sender` is a by-value move. `None` after the first
    /// fire or if the bundle's receiver was never created (the test
    /// constructor [`TerminalActor::new_with_seed`] leaves it `Some` too,
    /// but no consumer subscribes; the `.ok()` swallow is benign).
    ///
    /// Carries the child's exit status when known: `Some(code)` for a
    /// normal `_exit(n)`, `None` for signal-killed children or
    /// otherwise-unknown exits (phux-4li.11; the structured exit code
    /// flows into the `TERMINAL_CLOSED` wire frame the runtime emits on
    /// PTY EOF).
    exit_notify: Option<oneshot::Sender<Option<i32>>>,
    /// Cancellation token watched by the actor's `select!`. Cancel to
    /// ask the actor to shut down cleanly (drains the PTY, reaps the
    /// child, and exits). A child token of the per-server root token
    /// when constructed via [`Self::build_with_token`]; an unlinked
    /// fresh token when constructed via [`TerminalActor::new`] et al.
    /// Dropping the token does NOT cancel — call `.cancel()` explicitly
    /// (this is intentional; the prior `oneshot::Sender::drop` semantics
    /// were a hidden lifecycle coupling we want gone).
    token: CancellationToken,
    /// Optional sink for agent events the actor sources from the PTY
    /// stream (SPEC §7.5, phux-y2t): `bell`, `title_changed`, `dirty`,
    /// `idle`, and the OSC-133-sourced `command_started` / `command_finished`.
    /// `None` for actors that no one watches (most tests); set by the
    /// runtime's spawn path via [`Self::set_event_sink`]. The runtime
    /// drains this channel and fans each event out to event-stream
    /// subscribers scoped to this pane (it owns the wire `TerminalId`,
    /// which the actor does not know).
    ///
    /// `try_send` semantics: a full sink drops the event rather than
    /// stalling the hot PTY-pump loop — the event stream is an
    /// accelerator, not a guarantee (a dropped event just falls back to
    /// the CLI poll floor).
    event_sink: Option<mpsc::Sender<AgentEvent>>,
    /// Last terminal title observed (OSC 0 / OSC 2), for change detection.
    /// `title_changed` fires only when the polled title differs from this.
    ///
    /// Refreshed UNCONDITIONALLY by [`Self::refresh_title`] on every PTY
    /// chunk — including for a pane nobody is watching — because the
    /// agent-state detector reads it on its own timer and the OSC title is
    /// its highest-priority signal (ADR-0046 §B).
    last_title: String,
    /// Level-triggered agent-state detector (ADR-0046). `Some` only for a
    /// PTY-backed actor with a wired `agent_state_sink` and a non-empty rule
    /// set; constructed in [`Self::run`], so no existing constructor or test
    /// actor grows one.
    agent_detect: Option<crate::agent_detect::AgentDetector>,
    /// Sink for edge-filtered detector outputs. Drained by
    /// `runtime::client::spawn_agent_state_drain`, which owns `ServerState`
    /// and performs the arbitration + `metadata_set`.
    agent_state_sink: Option<mpsc::Sender<AgentDetectEvent>>,
    /// Grid-mutation flag scoped to the DETECTOR's tick (100-500 ms).
    ///
    /// Deliberately distinct from `terminal_dirty_since_tick`, which
    /// `tick_emit` clears every ~30 ms: a detector reading that flag would
    /// see `false` on nearly every tick and its "skip the scan when nothing
    /// changed" fast path would skip *every* scan, so it would never see the
    /// screen at all. Set at the same three mutation sites, cleared by
    /// [`Self::detect_tick`].
    agent_dirty_since_detect: bool,
    /// Last in-pane "ask" marker observed, for coalescing `AgentEvent::Asked`.
    ///
    /// The v1 ask-trigger is OSC-driven: an in-pane agent signals a pending
    /// human-answerable question by setting the terminal title (OSC 0 / OSC 2)
    /// to a `phux-ask` sentinel (see [`AskMarker`]). Like `dirty`/`idle`, the
    /// ask is coalesced — we emit `Asked` once when the marker first appears
    /// and again only when the marker *content* changes, so an agent that keeps
    /// re-asserting the same title does not re-fire. `None` means no ask is
    /// currently pending; clearing the marker (retitling to anything else)
    /// resets it so the next distinct ask fires.
    ///
    /// NOTE (phux-2sl6): libghostty-vt does not surface OSC 9 / OSC 777
    /// desktop-notification escapes through its API — title (OSC 0/2), pwd
    /// (OSC 7), and bell are the only user-notification signals it exposes —
    /// so the title sentinel is the honest closest signal for v1. Full
    /// agent-state detection (manifests / hooks / OSC-9 surfacing) is the
    /// follow-up phux-2sl6.4.
    last_ask: Option<AskMarker>,
    /// Whether the pane is currently in an active output "burst": a
    /// `dirty` event has been emitted and no settling `idle` has followed.
    /// Drives the dirty/idle coalescing — at most one `dirty` per burst,
    /// then one `idle` when a tick observes the grid has settled.
    in_output_burst: bool,
    /// Whether a PTY output chunk arrived since the preceding idle-check
    /// tick. Self-owned by dirty/idle bookkeeping so settling a burst does
    /// not depend on [`Self::tick_emit`] consuming its state-sync mutation
    /// flag (that emitter is deliberately gated off for raw consumers).
    output_since_idle_tick: bool,
    /// Event subscribers for this pane. When semantic state changes occur
    /// (command started, grid changed, etc.), broadcast to all subscribers
    /// whose `event_types` filter matches. `Vec` guarded by `RefCell` for
    /// interior mutability (single-threaded actor, no lock contention).
    /// Subscribers added by `handle_subscribe_terminal_events` and removed
    /// implicitly on detach.
    event_subscribers: RefCell<Vec<TerminalEventSubscriber>>,
    /// Last known working directory for this pane. Used to detect CWD
    /// changes and emit `CwdChanged` events (phux-foz.4). Queried lazily at
    /// OSC-133 prompt boundaries and on output-idle via `process_cwd`
    /// (`proc_pidinfo` on macOS, `/proc/PID/cwd` on Linux).
    last_known_cwd: RefCell<String>,
    /// phux-foz.4: incremental OSC-133 prompt-mark scanner over the raw PTY
    /// byte stream. Sources `command_started` / `command_finished` (with the
    /// `D`-mark exit code libghostty's grid projection does not retain) and
    /// triggers the prompt-boundary cwd re-query. Stateful so a mark split
    /// across two PTY read chunks is still recognised.
    osc133: osc133::Osc133Scanner,
    /// Whether we've already emitted a Dirty event in the current output
    /// burst. Coalesces multiple grid mutations into one event per burst
    /// (matching the `in_output_burst` coalescing for `AgentEvent`).
    dirty_event_emitted_this_burst: bool,
    /// Inbound subscription request channel. Drained by a select! arm
    /// that calls `subscribe_to_events`.
    /// Supervisory control mailbox (ADR-0033): lease-change broadcasts and
    /// process signals. Drained by a `select!` arm.
    control_rx: mpsc::Receiver<ControlRequest>,
    /// Process lifecycle as the supervisory surface sees it (ADR-0033):
    /// `Running` until a `Freeze` (SIGSTOP) flips it to `Frozen`, back to
    /// `Running` on `Resume` (SIGCONT). Natural/terminal exits are reported
    /// by the existing `TERMINAL_CLOSED` / `PaneClosed` path, not here.
    lifecycle: TerminalLifecycle,
    subscribe_to_events_rx: mpsc::Receiver<SubscribeToEventsRequest>,
    /// Inbound unsubscription request channel. Drained by a select! arm
    /// that calls `unsubscribe_from_events`.
    unsubscribe_from_events_rx: mpsc::Receiver<UnsubscribeFromEventsRequest>,
    /// Wire-level terminal id (for Event frames). Set by the runtime
    /// during subscription registration. `0` until a subscriber arrives.
    wire_terminal_id: u32,
    cols: u16,
    rows: u16,
    /// Per-cell pixel size `(width, height)` used to derive the PTY winsize
    /// pixel fields and XTWINOPS size reports. Seeded to [`DEFAULT_CELL_PX`]
    /// so the geometry is never zero, then overwritten by the most recent
    /// [`ResizeRequest`] that carries usable pixel metrics. Sticky: a
    /// pixel-less resize (agent `TERMINAL_RESIZE`) keeps the established
    /// value. Nonzero on both axes at all times, so pixel probes inside the
    /// pane (`kitten icat`, sixel sizers) always read a real cell size.
    cell_px: (u16, u16),
    /// Current grid + cell geometry shared with the libghostty `on_size`
    /// callback, which answers XTWINOPS size queries (CSI 14/16/18 t)
    /// synchronously inside `vt_write` — while `handle_resize` is the
    /// writer. Updated after every applied resize.
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
    /// Could not spawn the command on the PTY slave.
    #[error("spawn failed: {0}")]
    Spawn(String),
    /// Could not take the master reader or writer half, or start the
    /// bridge threads.
    #[error("pty io setup failed: {0}")]
    PtyIo(String),
}

/// Bundle returned from [`TerminalActor::new`]: the actor itself plus a
/// [`CancellationToken`] that, when cancelled, fires the actor's
/// shutdown branch.
///
/// The token is **clone-shared** with the actor: callers can clone it
/// before handing the actor off to `spawn_local`, hold the clone, and
/// call `.cancel()` to ask the actor to exit. Unlike the prior
/// `oneshot::Sender<()>`-shaped bundle, dropping `token` does NOT
/// cancel the actor — cancellation must be explicit.
#[must_use]
pub struct TerminalActorBundle {
    /// The actor; pass to `tokio::task::spawn_local`.
    pub actor: TerminalActor,
    /// Cross-task handle to the actor.
    pub handle: TerminalHandle,
    /// Cancellation token. Call `.cancel()` to ask the actor to shut
    /// down cleanly. Cloneable; shares cancellation state with the
    /// actor's internal copy.
    pub token: CancellationToken,
    /// One-shot receiver that fires when the actor observes PTY EOF
    /// (the child process exited, the pane is dying). The runtime
    /// pairs this with the terminal's [`phux_core::ids::TerminalId`] and uses
    /// it to drive client-detach on shell-`exit` (phux-it8).
    ///
    /// Used by the runtime's per-pane EOF watcher task; tests that
    /// don't care about lifecycle simply drop it. Receiver-drop is
    /// benign for the sender side — the actor uses `send().ok()`.
    ///
    /// `Option` so callers can `take()` it out of the bundle;
    /// `None` after the first take.
    ///
    /// The payload is the child's exit status: `Some(code)` on a normal
    /// `_exit(n)` (or where the kernel reports a code at all), `None`
    /// for signal-killed children or unknown-cause exits. Mirrors the
    /// `TERMINAL_CLOSED.exit_status` wire field exactly (phux-4li.11).
    pub exit_notify: Option<oneshot::Receiver<Option<i32>>>,
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
    /// Re-adopt a PTY master fd + child PID inherited across a graceful-upgrade
    /// `execve` (ADR-0032).
    Adopt {
        /// Inherited master descriptor (`FD_CLOEXEC` cleared before the exec).
        master_fd: std::os::fd::RawFd,
        /// Surviving child PID on the slave side.
        child_pid: i32,
    },
}

impl TerminalActor {
    /// Build a fresh actor of the given dimensions **without** a backing
    /// PTY. Used by tests that exercise snapshot / shutdown semantics
    /// without driving a real process.
    ///
    /// The `GhosttyTerminal` is allocated via libghostty's default allocator
    /// (NULL alloc → `'static` lifetimes). `max_scrollback` is
    /// `DEFAULT_MAX_SCROLLBACK` — a tmux-style mid-range value the
    /// runtime overrides with `defaults.history-limit` via
    /// [`Self::build_with_token`].
    #[allow(clippy::new_ret_no_self, reason = "bundle-shaped constructor")]
    pub fn new(cols: u16, rows: u16) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            PtySource::None,
            DEFAULT_MAX_SCROLLBACK,
            CancellationToken::new(),
            None,
        )
    }

    /// Build a fresh actor backed by a real PTY running `cmd`.
    ///
    /// Spawns the command on the slave side, kicks off the reader and
    /// writer bridge threads, and returns the bundle. The caller hands
    /// `actor` to `spawn_local` and keeps `handle` + `token` to talk
    /// to and tear down the actor.
    pub fn new_with_command(
        cmd: CommandBuilder,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            PtySource::Spawn(cmd),
            DEFAULT_MAX_SCROLLBACK,
            CancellationToken::new(),
            None,
        )
    }

    /// Convenience: spawn the user's default shell (`$SHELL` or
    /// `/bin/sh`; no server config in scope here) in a fresh PTY.
    pub fn new_with_default_shell(
        cols: u16,
        rows: u16,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::new_with_command(default_shell_command(&resolve_shell(None)), cols, rows)
    }

    /// Build an actor whose cancellation token is `token` (typically a
    /// `root_token.child_token()` from [`crate::runtime::ServerRuntime`]).
    /// The bundle's `token` field is a clone of the same token, so
    /// cancelling either propagates to the actor.
    ///
    /// This is the path the runtime uses; tests use [`Self::new`] /
    /// [`Self::new_with_command`] which generate an unlinked fresh
    /// token internally.
    pub fn build_with_token(
        cols: u16,
        rows: u16,
        cmd: Option<CommandBuilder>,
        max_scrollback: u32,
        token: CancellationToken,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            cmd.map_or(PtySource::None, PtySource::Spawn),
            max_scrollback,
            token,
            None,
        )
    }

    /// Runtime constructor that seeds host default colors before the PTY is
    /// spawned and any child output can be parsed.
    pub fn build_with_token_and_colors(
        cols: u16,
        rows: u16,
        cmd: Option<CommandBuilder>,
        max_scrollback: u32,
        token: CancellationToken,
        default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            cmd.map_or(PtySource::None, PtySource::Spawn),
            max_scrollback,
            token,
            default_colors,
        )
    }

    /// Build an actor around a PTY master fd + child PID inherited across a
    /// graceful-upgrade `execve` (ADR-0032), then replay `seed` (the pane's
    /// snapshot from the [`StateBlob`](crate::upgrade::blob::StateBlob)) into
    /// the fresh `Terminal` so the grid matches what the old image showed.
    ///
    /// The PTY is not re-opened and the child is not re-spawned — both kept
    /// running across the exec; this rebuilds only the server-side plumbing.
    pub fn new_with_adopted_pty(
        master_fd: std::os::fd::RawFd,
        child_pid: i32,
        cols: u16,
        rows: u16,
        max_scrollback: u32,
        token: CancellationToken,
        seed: &[u8],
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        let bundle = Self::build(
            cols,
            rows,
            PtySource::Adopt {
                master_fd,
                child_pid,
            },
            max_scrollback,
            token,
            None,
        )?;
        bundle.actor.terminal.borrow_mut().vt_write(seed);
        bundle.actor.publish_input_snapshot();
        Ok(bundle)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "straight-line wiring: one channel pair per request mailbox, then a single actor + handle struct literal. Splitting on an arbitrary boundary separates a channel's two halves from where the actor/handle consume them, which is harder to follow than the linear form."
    )]
    fn build(
        cols: u16,
        rows: u16,
        pty_source: PtySource,
        max_scrollback: u32,
        token: CancellationToken,
        default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        let mut terminal = GhosttyTerminal::new(TerminalOptions {
            cols,
            rows,
            // `defaults.history-limit` is a `u32` on the wire/config; the
            // libghostty option is `usize`. The widen is lossless on all
            // supported targets.
            max_scrollback: max_scrollback as usize,
        })?;
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal)?;
        if let Some(colors) = default_colors {
            Self::install_default_colors(&mut terminal, colors)?;
        }
        let size_report = Rc::new(Cell::new(SizeReportSize {
            rows,
            columns: cols,
            cell_width: u32::from(DEFAULT_CELL_PX.0),
            cell_height: u32::from(DEFAULT_CELL_PX.1),
        }));
        let synth = SnapshotSynthesizer::new()?;
        let key_enc = PerTerminalKeyEncoder::new()?;
        let mouse_enc = PerTerminalMouseEncoder::new()?;
        let initial_input_snapshot = InputEncoderSnapshot::capture(&terminal, DEFAULT_CELL_PX)?;
        let (input_tx, input_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (encoded_input_tx, encoded_input_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (input_snapshot_tx, input_snapshot_rx) = watch::channel(initial_input_snapshot);
        let (snapshot_tx, snapshot_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_bootstrap_tx, native_bootstrap_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_history_tx, native_history_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_release_tx, native_release_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let native_requests = NativeRequestReceivers {
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            bootstrap: native_bootstrap_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            history: native_history_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            release: native_release_rx,
        };
        let (set_default_colors_tx, set_default_colors_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (screen_tx, screen_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (upgrade_tx, upgrade_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (pwd_tx, pwd_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (resize_tx, resize_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (consumer_attach_tx, consumer_attach_rx) =
            mpsc::channel::<ConsumerAttachRequest>(DEFAULT_INPUT_MAILBOX);
        let (consumer_detach_tx, consumer_detach_rx) =
            mpsc::channel::<ConsumerDetachRequest>(DEFAULT_INPUT_MAILBOX);
        let (consumer_ack_tx, consumer_ack_rx) =
            mpsc::channel::<ConsumerAckRequest>(DEFAULT_INPUT_MAILBOX);
        let (subscribe_to_events_tx, subscribe_to_events_rx) =
            mpsc::channel::<SubscribeToEventsRequest>(DEFAULT_INPUT_MAILBOX);
        let (unsubscribe_from_events_tx, unsubscribe_from_events_rx) =
            mpsc::channel::<UnsubscribeFromEventsRequest>(DEFAULT_INPUT_MAILBOX);
        let (control_tx, control_rx) = mpsc::channel::<ControlRequest>(DEFAULT_INPUT_MAILBOX);
        let (output_tx, _output_rx_seed) = broadcast::channel(DEFAULT_OUTPUT_BROADCAST);
        let (exit_tx, exit_rx) = oneshot::channel::<Option<i32>>();
        let bundle_token = token.clone();

        let (pty_rx, pty_tx, pty) = match pty_source {
            PtySource::None => (None, None, None),
            PtySource::Spawn(cmd) => {
                let (rx, tx, owned) = spawn_pty(cmd, cols, rows)?;
                (Some(rx), Some(tx), Some(owned))
            }
            PtySource::Adopt {
                master_fd,
                child_pid,
            } => {
                let (rx, tx, owned) = adopt_pty(master_fd, child_pid)?;
                (Some(rx), Some(tx), Some(owned))
            }
        };
        Self::install_effects(&mut terminal, &size_report, pty_tx.as_ref())?;

        let actor = Self {
            terminal: RefCell::new(CanonicalTerminal::Plain(Some(terminal))),
            synth: RefCell::new(synth),
            // A pane may carry initial content (PTY banner, restored
            // scrollback); start dirty so the first tick always emits.
            terminal_dirty_since_tick: true,
            color_query_scanner: ColorQueryScanner::default(),
            key_enc: RefCell::new(key_enc),
            mouse_enc: RefCell::new(mouse_enc),
            focus_enc: RefCell::new(PerTerminalFocusEncoder::new()),
            paste_enc: RefCell::new(PerTerminalPasteEncoder::new()),
            input_rx,
            encoded_input_rx,
            input_snapshot_tx,
            snapshot_rx,
            native_requests,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_cursor_owners: HashMap::new(),
            set_default_colors_rx,
            screen_rx,
            upgrade_rx,
            pwd_rx,
            resize_rx,
            consumer_attach_rx,
            consumer_detach_rx,
            consumer_ack_rx,
            consumer_states: HashMap::new(),
            // phux-yeca: keep the human attach path on the raw PTY
            // broadcast pump by default. The per-consumer synthesized-VT
            // tick path is correct for state-sync experiments, but as the
            // sole emitter it adds a visible 20-30 ms floor to local typing
            // and can lose byte-exact styling that interactive shells/TUIs
            // rely on. Tests can still flip this on explicitly with
            // `enable_tick_emit_for_test`; production needs a negotiated
            // consumer mode before making synthesized ticks the human path.
            consumer_tick_emits: false,
            pty_rx,
            pty_tx,
            pty,
            output_tx: output_tx.clone(),
            exit_notify: Some(exit_tx),
            token,
            event_sink: None,
            last_title: String::new(),
            agent_detect: None,
            agent_state_sink: None,
            agent_dirty_since_detect: false,
            last_ask: None,
            in_output_burst: false,
            output_since_idle_tick: false,
            event_subscribers: RefCell::new(Vec::new()),
            last_known_cwd: RefCell::new(std::env::var("HOME").unwrap_or_default()),
            osc133: osc133::Osc133Scanner::new(),
            dirty_event_emitted_this_burst: false,
            subscribe_to_events_rx,
            unsubscribe_from_events_rx,
            control_rx,
            lifecycle: TerminalLifecycle::Running,
            wire_terminal_id: 0,
            cols,
            rows,
            cell_px: DEFAULT_CELL_PX,
            size_report,
        };
        let handle = TerminalHandle {
            input: input_tx,
            encoded_input: encoded_input_tx,
            input_snapshot: input_snapshot_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap: native_bootstrap_tx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_history: native_history_tx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_release: native_release_tx,
            snapshot: snapshot_tx,
            set_default_colors: set_default_colors_tx,
            screen: screen_tx,
            upgrade: upgrade_tx,
            pwd: pwd_tx,
            output: output_tx,
            resize: resize_tx,
            consumer_attach: consumer_attach_tx,
            consumer_detach: consumer_detach_tx,
            consumer_ack: consumer_ack_tx,
            subscribe_to_events: subscribe_to_events_tx,
            unsubscribe_from_events: unsubscribe_from_events_tx,
            control: control_tx,
            cols,
            rows,
        };
        Ok(TerminalActorBundle {
            actor,
            handle,
            token: bundle_token,
            exit_notify: Some(exit_rx),
        })
    }

    fn install_default_colors(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        colors: phux_protocol::caps::TerminalDefaultColors,
    ) -> Result<(), TerminalActorError> {
        use libghostty_vt::style::RgbColor;

        terminal.set_default_fg_color(Some(RgbColor {
            r: colors.foreground.r,
            g: colors.foreground.g,
            b: colors.foreground.b,
        }))?;
        terminal.set_default_bg_color(Some(RgbColor {
            r: colors.background.r,
            g: colors.background.g,
            b: colors.background.b,
        }))?;
        Ok(())
    }

    /// Answer OSC 10/11 queries found in a just-parsed PTY chunk from the
    /// canonical terminal's effective colors. The scanner persists across PTY
    /// reads, so an escape sequence split at any byte boundary still works.
    fn answer_color_queries(&mut self, bytes: &[u8]) {
        let mut queries = 0_u8;
        self.color_query_scanner.feed(bytes, |selector| {
            queries |= match selector {
                10 => 1,
                11 => 2,
                _ => 0,
            };
        });
        if queries == 0 {
            return;
        }
        let terminal = self.terminal.borrow();
        let foreground = (queries & 1 != 0)
            .then(|| terminal.fg_color().ok().flatten())
            .flatten();
        let background = (queries & 2 != 0)
            .then(|| terminal.bg_color().ok().flatten())
            .flatten();
        drop(terminal);

        let Some(pty_tx) = &self.pty_tx else {
            return;
        };
        if let Some(color) = foreground {
            let _ = pty_tx.try_send(EncodedInputRequest::legacy(color_query_reply(10, color)));
        }
        if let Some(color) = background {
            let _ = pty_tx.try_send(EncodedInputRequest::legacy(color_query_reply(11, color)));
        }
    }

    /// Install the libghostty effect handlers the actor relies on.
    ///
    /// `on_size` answers XTWINOPS size queries (CSI 14/16/18 t) from the
    /// shared geometry cell; `handle_resize` keeps it current. Without
    /// this callback libghostty silently drops the query and pixel-aware
    /// programs (`kitten icat` preflights, sixel sizers) see a mute
    /// terminal even though the kernel winsize carries pixel dims.
    ///
    /// `on_pty_write` routes terminal-generated replies (XTWINOPS size
    /// reports, DECRQM mode reports, CSI 21 t title reports, mode-2048
    /// in-band resize notifications) back to the child through the same
    /// writer bridge that carries client input; libghostty discards
    /// every reply when it is absent. The callback fires synchronously
    /// inside `vt_write` while the actor holds the `Terminal` borrow, so
    /// it must not touch the terminal — a channel send is safe. The
    /// sender it captures is WEAK: the closure lives in the Terminal's
    /// vtable, which the actor owns through `shutdown_pty` — a strong
    /// clone there would keep the writer-bridge channel open while
    /// `shutdown_pty` joins the writer thread (which only exits on
    /// channel close), deadlocking teardown.
    fn install_effects(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        size_report: &Rc<Cell<SizeReportSize>>,
        pty_tx: Option<&mpsc::Sender<EncodedInputRequest>>,
    ) -> Result<(), TerminalActorError> {
        terminal.on_size({
            let size_report = Rc::clone(size_report);
            move |_term| Some(size_report.get())
        })?;
        if let Some(tx) = pty_tx {
            let tx = tx.downgrade();
            terminal.on_pty_write(move |_term, bytes| {
                // No upgrade ⇒ the writer bridge (and child) are gone;
                // the reply has no recipient. Drop it.
                if let Some(tx) = tx.upgrade() {
                    let _ = tx.try_send(EncodedInputRequest::legacy(bytes.to_vec()));
                }
            })?;
        }
        Ok(())
    }

    /// Test-only constructor: write `bytes` into the actor's `Terminal`
    /// before the actor starts running. Useful for unit and integration
    /// tests that want the snapshot/incremental synthesis path to
    /// return non-trivial content without wiring up a PTY pump.
    ///
    /// Public (rather than `#[cfg(test)]`) so integration tests under
    /// `crates/phux-server/tests/` can call it. Not exercised by
    /// production code; the name + doc make the intent clear.
    pub fn new_with_seed(
        cols: u16,
        rows: u16,
        bytes: &[u8],
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        let bundle = Self::new(cols, rows)?;
        bundle.actor.terminal.borrow_mut().vt_write(bytes);
        bundle.actor.publish_input_snapshot();
        Ok(bundle)
    }

    /// Register `client_id` as an attached consumer (phux-q0e.2).
    ///
    /// Allocates a fresh `RenderState`, primes it against the live
    /// terminal (`update` + manual `set_dirty(Clean)` walk), and stores
    /// the resulting [`ConsumerSyncState`] in `consumer_states`.
    ///
    /// Why prime + clear: the runtime's ATTACH path emits a
    /// `TERMINAL_SNAPSHOT` immediately after this call returns, which
    /// brings the consumer's mirror Terminal up to the current
    /// canonical state. The per-consumer reference must reflect that same
    /// reference point — otherwise the first incremental emission would
    /// treat every row as changed and re-paint the screen the snapshot
    /// just installed.
    ///
    /// Idempotent: re-attaching the same `client_id` (e.g. on a runtime
    /// bug) overwrites the prior entry.
    fn register_consumer_generation(
        &mut self,
        client_id: ClientId,
        outbound: mpsc::Sender<Outbound>,
        wire_terminal_id: u32,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        wants_state_sync: bool,
    ) -> Result<(), ConsumerAttachError> {
        // Priming the per-consumer reference + cursor/mode capture costs two
        // full-grid render passes, but a raw broadcast-pump consumer (the
        // human attach path) never reads either: the tick serves only
        // tick-managed consumers and `FRAME_ACK` is dropped for raw ones, and
        // `wants_state_sync` is fixed at registration with no flip path. So
        // do the work only when this consumer is actually tick-managed; a raw
        // consumer attaches with an empty reference and a placeholder capture.
        // (If it were ever tick-served, `needs_initial_emit` forces a full
        // pass that primes both — see the tick emit gate.)
        let tick_managed = wants_state_sync || self.consumer_tick_emits;
        let (last_cursor_mode, reference) = if tick_managed {
            let terminal = self.terminal.borrow();
            // Cursor + DEC mode capture happens against a one-shot
            // `RenderState` so we don't conflict with the shared
            // synthesizer's borrow used to prime the reference below.
            let last_cursor_mode = {
                let mut render_state = RenderState::new()?;
                let snapshot = render_state.update(&terminal)?;
                LastAckedCursorMode::capture(&terminal, &snapshot)
            };
            // Prime the reference against the live terminal so the next
            // `synthesize_against_reference` emits only deltas from *now* —
            // the `TERMINAL_SNAPSHOT` the runtime emits right after this call
            // already brings the consumer's mirror to this same point.
            let mut reference = ConsumerReference::new();
            self.synth
                .borrow_mut()
                .prime_reference(&terminal, &mut reference)?;
            (last_cursor_mode, reference)
        } else {
            (LastAckedCursorMode::unprimed(), ConsumerReference::new())
        };
        self.consumer_states.insert(
            client_id,
            ConsumerSyncState {
                reference,
                outbound,
                wire_terminal_id,
                stream_id,
                bootstrap_id,
                // First emission gets `seq == 1`. `0` is reserved for
                // the "empty initial frame" sentinel matching
                // `FrameId::ZERO` in [`LastAckedCursorMode`]'s doc.
                next_seq: 1,
                last_acked_seq: 0,
                last_cursor_mode,
                // Force one synthesis pass on the next tick even if the
                // terminal is Clean since the previous tick (phux-4l0).
                needs_initial_emit: true,
                // Fresh consumer is not behind; `needs_initial_emit` already
                // guarantees its first pass runs.
                behind: false,
                // No RTT sample yet — runs at the cold-start default until the
                // first FRAME_ACK round-trip lands (phux-q0e.5).
                rtt: RttEstimator::default(),
                emit_instants: std::collections::BTreeMap::new(),
                wants_state_sync,
                // Loss-tolerance is opt-in and off by default (phux-v45.8):
                // the reliable-transport emit-once model is the norm. The
                // runtime flips it on via `enable_loss_tolerance` for a
                // forwarded/lossy leg right after registration.
                loss_tolerant: false,
                acked_reference: ConsumerReference::new(),
                pending_refs: std::collections::BTreeMap::new(),
            },
        );
        Ok(())
    }

    #[cfg(test)]
    fn register_consumer(
        &mut self,
        client_id: ClientId,
        outbound: mpsc::Sender<Outbound>,
        wire_terminal_id: u32,
        wants_state_sync: bool,
    ) -> Result<(), ConsumerAttachError> {
        self.register_consumer_generation(
            client_id,
            outbound,
            wire_terminal_id,
            phux_protocol::ids::StreamId::new(u64::from(client_id.get()) + 1)
                .expect("test stream id"),
            phux_protocol::ids::BootstrapId::new(1).expect("test bootstrap id"),
            wants_state_sync,
        )
    }

    /// Switch an already-registered consumer to the advance-on-ack
    /// loss-tolerant emission model (phux-v45.8, ADR-0042).
    ///
    /// Idempotent-ish: sets [`ConsumerSyncState::loss_tolerant`] and primes
    /// [`ConsumerSyncState::acked_reference`] to the live grid so the first
    /// post-enable tick emits only deltas from *now* (the consumer's
    /// `TERMINAL_SNAPSHOT` brought its mirror to this same point). Silent no-op
    /// if the consumer is not registered (raced against detach). A failure to
    /// prime the reference leaves the consumer on the emit-once path (the
    /// reference stays empty, so the first tick would repaint everything — safe,
    /// just not yet loss-tolerant); logged, not fatal.
    fn enable_loss_tolerance(&mut self, client_id: ClientId) {
        // Disjoint field borrows: `self.terminal` / `self.synth` (RefCell
        // interior) vs `self.consumer_states` (via `get_mut`).
        let terminal = self.terminal.borrow();
        let synth = &self.synth;
        let Some(state) = self.consumer_states.get_mut(&client_id) else {
            trace!(
                ?client_id,
                "enable_loss_tolerance for unregistered consumer; dropping"
            );
            return;
        };
        match synth
            .borrow_mut()
            .prime_reference(&terminal, &mut state.acked_reference)
        {
            Ok(()) => {
                state.loss_tolerant = true;
                trace!(?client_id, "loss-tolerant state-sync enabled for consumer");
            }
            Err(err) => {
                warn!(
                    ?client_id,
                    error = %err,
                    "enable_loss_tolerance: priming acked reference failed; staying emit-once",
                );
            }
        }
    }

    /// Drop the per-consumer state for `client_id` if present
    /// (phux-q0e.2). Silent no-op if absent — matches the idempotency
    /// of `ServerState::detach`.
    fn unregister_consumer(&mut self, client_id: ClientId) {
        // `HashMap::remove` returns the entry; dropping it frees the
        // per-consumer reference grid.
        let _ = self.consumer_states.remove(&client_id);
    }

    /// Handle an inbound `FRAME_ACK` from `client_id` carrying cumulative
    /// `seq` (phux-q0e.4, ADR-0018 addendum).
    ///
    /// Under the v0.1 emit-once model (phux-ia4) the per-consumer
    /// reference advances on *emit*, not on ack: a given change is shipped
    /// exactly once and the reference is committed before the frame goes
    /// out (see
    /// [`crate::grid::SnapshotSynthesizer::synthesize_against_reference`]).
    /// `FRAME_ACK` therefore no longer drives cache eviction; it tracks
    /// `last_acked_seq` for backpressure accounting (proto.md §8.2) and
    /// refreshes the informational cursor/mode capture. The loss-tolerance
    /// "re-diff against an older reference on a dropped frame" property is
    /// a future lossy-transport concern (ADR-0018), not wired on the
    /// reliable v0.1 transports.
    ///
    /// Per proto.md §8.2 acks are cumulative: an ack for `seq = N` implies
    /// all prior emissions up to `N`. Older / duplicate / out-of-order
    /// acks (`seq <= last_acked_seq`) are silently dropped.
    ///
    /// Silent no-op if `client_id` is not currently registered. This
    /// races cleanly against detach: the runtime may dispatch an
    /// in-flight ack just as the consumer is being torn down, and the
    /// ack should evaporate rather than recreate a dropped entry.
    ///
    /// Returns `true` when this ack folded a fresh RTT sample into the
    /// consumer's [`RttEstimator`] (phux-q0e.5) — the `run` loop uses that as
    /// a cue to recompute the shared adaptive tick cadence. `false` when no
    /// sample was produced (no matching emit instant, older/duplicate ack,
    /// or unregistered consumer).
    fn on_generation_frame_ack(
        &mut self,
        client_id: ClientId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        seq: u64,
    ) -> bool {
        let Some(consumer) = self.consumer_states.get(&client_id) else {
            return false;
        };
        if consumer.stream_id != stream_id || consumer.bootstrap_id != bootstrap_id {
            trace!(
                ?client_id,
                ?stream_id,
                ?bootstrap_id,
                seq,
                "FRAME_ACK for stale stream generation; dropping",
            );
            return false;
        }
        self.on_frame_ack(client_id, seq)
    }

    fn on_frame_ack(&mut self, client_id: ClientId, seq: u64) -> bool {
        // Captured before the `&mut` borrow below: the global test override
        // that forces every consumer onto the tick.
        let force_all_consumers = self.consumer_tick_emits;
        let Some(consumer) = self.consumer_states.get_mut(&client_id) else {
            // Race against detach (or an ack for an unknown client). No
            // bookkeeping; no warning — this is a steady-state event,
            // not a misuse.
            trace!(
                ?client_id,
                seq, "FRAME_ACK for unregistered consumer; dropping"
            );
            return false;
        };
        // phux-38k6: only a tick-managed consumer's acks belong to this
        // per-consumer seq space. A raw (broadcast-pump) consumer acks the
        // pump's *local* seq, which is unrelated to this state's `next_seq` /
        // `emit_instants`; folding it in would set `last_acked_seq` from a
        // foreign counter and skew the RTT/backpressure accounting once the
        // consumer is (or becomes) state-sync. Drop it — the pump owns no
        // per-consumer state to update (phux-fseo made modes negotiable, so
        // this is now reachable, not just defensive).
        if !force_all_consumers && !consumer.wants_state_sync {
            trace!(
                ?client_id,
                seq, "FRAME_ACK for raw-broadcast consumer; not a tick ack, dropping"
            );
            return false;
        }
        if seq <= consumer.last_acked_seq {
            // Older or duplicate ack — acks are cumulative (proto.md
            // §8.2), so `seq <= last_acked_seq` carries no new information.
            trace!(
                ?client_id,
                seq,
                last_acked_seq = consumer.last_acked_seq,
                "FRAME_ACK older/duplicate; dropping",
            );
            return false;
        }
        consumer.last_acked_seq = seq;

        // Loss-tolerant reference advance (phux-v45.8, ADR-0042). A cumulative
        // ack for `seq` acknowledges every emission up to and including it, so
        // the consumer's acked reference advances to the grid snapshot of the
        // highest emitted `seq` this ack covers, and every pending snapshot at
        // or below `seq` is dropped. This is the eviction the emit-once path
        // (below, comment retained for contrast) deliberately does NOT do: on a
        // lossy leg the reference must trail the ack so a dropped frame re-diffs
        // against the last state the consumer provably has.
        if consumer.loss_tolerant {
            if let Some((&covered, _)) = consumer.pending_refs.range(..=seq).next_back()
                && let Some(snapshot) = consumer.pending_refs.remove(&covered)
            {
                consumer.acked_reference = snapshot;
            }
            // Keep only strictly-newer pending snapshots (still in flight).
            consumer.pending_refs = consumer.pending_refs.split_off(&seq.saturating_add(1));
        }

        // RTT sample (phux-q0e.5). Acks are cumulative, so `seq` acknowledges
        // every emission up to and including it. Find the emit instant for
        // the highest emitted seq that is `<= seq` (the most recent frame
        // this ack covers) and time it against now. Then prune every emit
        // instant `<= seq`: those frames are acked and can never produce a
        // future sample, so the map stays bounded by the in-flight window.
        let now = tokio::time::Instant::now();
        let rtt_sample = consumer
            .emit_instants
            .range(..=seq)
            .next_back()
            .map(|(_, &emitted_at)| now.saturating_duration_since(emitted_at));
        // `split_off(&(seq + 1))` keeps only the strictly-greater keys; the
        // returned (acked) half is dropped. `seq + 1` cannot overflow in
        // practice (u64 seq at the clamped cadence) but saturate for safety.
        let still_in_flight = consumer.emit_instants.split_off(&seq.saturating_add(1));
        consumer.emit_instants = still_in_flight;
        let sampled = if let Some(sample) = rtt_sample {
            consumer.rtt.observe(sample);
            trace!(
                ?client_id,
                seq,
                rtt_ms = sample.as_secs_f64() * 1000.0,
                srtt_ms = consumer.rtt.smoothed().map(|d| d.as_secs_f64() * 1000.0),
                "FRAME_ACK: RTT sample folded into EMA",
            );
            true
        } else {
            false
        };

        // Refresh the informational cursor/mode capture. Uses a one-shot
        // `RenderState` so it doesn't disturb the per-consumer reference.
        let terminal = self.terminal.borrow();
        let cursor_mode = match RenderState::new() {
            Ok(mut rs) => match rs.update(&terminal) {
                Ok(snapshot) => Some(LastAckedCursorMode::capture(&terminal, &snapshot)),
                Err(err) => {
                    warn!(
                        ?client_id,
                        seq,
                        error = %err,
                        "FRAME_ACK: cursor/mode capture update failed; keeping prior capture",
                    );
                    None
                }
            },
            Err(err) => {
                warn!(
                    ?client_id,
                    seq,
                    error = %err,
                    "FRAME_ACK: cursor/mode RenderState alloc failed; keeping prior capture",
                );
                None
            }
        };
        if let Some(cm) = cursor_mode {
            consumer.last_cursor_mode = cm;
        }

        trace!(
            ?client_id,
            seq, "FRAME_ACK applied: last_acked_seq advanced"
        );
        sampled
    }

    /// The shared adaptive tick interval for this actor: the minimum over
    /// every attached consumer's desired interval (phux-q0e.5).
    ///
    /// One `tokio::time::Interval` drives the whole pane, but RTT is
    /// per-consumer. Taking the *minimum* means the most-demanding (lowest
    /// half-RTT) consumer sets the cadence: a fast local peer keeps its 50 Hz
    /// feel even when sharing the pane with a slow satellite peer, and the
    /// slow peer simply sees more empty/short diffs per tick (harmless — the
    /// per-consumer reference advances only on a real delta). With no
    /// consumers, or none yet sampled, this is [`DEFAULT_TICK_INTERVAL`].
    fn adaptive_tick_interval(&self) -> std::time::Duration {
        self.consumer_states
            .values()
            .map(|s| s.rtt.desired_tick_interval())
            .min()
            .unwrap_or(DEFAULT_TICK_INTERVAL)
    }

    /// Rebuild the shared state-sync timer to fire at `desired` if it differs
    /// from the currently-armed `current` by more than [`TICK_RESET_DEADBAND`]
    /// (phux-q0e.5).
    ///
    /// The deadband keeps a steady RTT from churning the scheduler on every
    /// sub-millisecond EMA wobble. `tokio::time::Interval::reset_after`
    /// re-anchors only the next deadline; the recurring `period` is fixed at
    /// construction, so changing the cadence means rebuilding the interval.
    /// The first new tick is anchored one full `desired` out so the cadence
    /// change doesn't fire a tick immediately.
    fn rearm_tick(
        tick: &mut tokio::time::Interval,
        current: &mut std::time::Duration,
        desired: std::time::Duration,
    ) {
        if current.abs_diff(desired) < TICK_RESET_DEADBAND {
            return;
        }
        *current = desired;
        let mut next = tokio::time::interval_at(tokio::time::Instant::now() + desired, desired);
        next.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        *tick = next;
    }

    /// Test-only: number of consumers currently registered.
    #[cfg(test)]
    pub fn consumer_count(&self) -> usize {
        self.consumer_states.len()
    }

    /// Test-only: borrow the per-consumer state for `client_id`.
    #[cfg(test)]
    pub fn consumer_state(&self, client_id: ClientId) -> Option<&ConsumerSyncState> {
        self.consumer_states.get(&client_id)
    }

    /// Wire an agent-event sink (SPEC §7.5, phux-y2t). The actor emits
    /// `bell` / `title_changed` / `dirty` / `idle` / `command_*` events to
    /// `sink`; the runtime drains it and fans each event out to
    /// event-stream subscribers scoped to this pane. Called by the spawn
    /// path before the actor is handed to `spawn_local`.
    pub fn set_event_sink(&mut self, sink: mpsc::Sender<AgentEvent>) {
        self.event_sink = Some(sink);
    }

    /// Best-effort agent-event emission (SPEC §7.5). `try_send` so a full
    /// sink drops the event rather than stalling the actor — the event
    /// stream is an accelerator, never a guarantee. No-op when no sink is
    /// wired (the common test path).
    fn emit_event(&self, event: AgentEvent) {
        if let Some(sink) = self.event_sink.as_ref() {
            let _ = sink.try_send(event);
        }
    }

    /// Wire the agent-state detector's sink (ADR-0046). The actor's detector
    /// timer emits edge-filtered [`AgentDetectEvent`]s here; the runtime's
    /// `spawn_agent_state_drain` owns `ServerState` and performs the
    /// arbitration + `metadata_set`. Called by the spawn path before the
    /// actor is handed to `spawn_local`, exactly like [`Self::set_event_sink`].
    ///
    /// `pub(crate)`, unlike `set_event_sink`: `AgentEvent` is a wire type, but
    /// [`AgentDetectEvent`] is deliberately server-internal — the detector
    /// introduces no wire surface.
    pub(crate) fn set_agent_state_sink(&mut self, sink: mpsc::Sender<AgentDetectEvent>) {
        self.agent_state_sink = Some(sink);
    }

    /// Best-effort detector emission. `try_send`: a full sink drops the
    /// event rather than stalling the actor. Safe to drop — the detector is
    /// level-triggered, so the next tick re-derives and re-publishes.
    fn emit_agent_state(&self, event: AgentDetectEvent) {
        if let Some(sink) = self.agent_state_sink.as_ref() {
            let _ = sink.try_send(event);
        }
    }

    /// Sync `last_title` with libghostty's tracked OSC 0/2 title. Returns
    /// `true` on a real change.
    ///
    /// The `RefCell` borrow of `self.terminal` MUST be released before
    /// `self.last_title` is written, hence the two-step. No allocation on the
    /// steady path: the common case compares and returns `false`.
    fn refresh_title(&mut self) -> bool {
        let next: Option<String> = {
            let terminal = self.terminal.borrow();
            let current = terminal.title().unwrap_or("");
            (current != self.last_title).then(|| current.to_owned())
        }; // borrow released here — required
        match next {
            Some(title) => {
                self.last_title = title;
                true
            }
            None => false,
        }
    }

    /// Right-trimmed live-viewport rows, top to bottom. The detector's ONLY
    /// grid read.
    ///
    /// Routes through the synthesizer's fresh-`RenderState` projection
    /// (`scrollback = None`, so the LIVE screen and never history). That
    /// projection deliberately allocates its own `RenderState` per call
    /// rather than reusing the pooled one, precisely so a read like this does
    /// not consume the shared libghostty dirty bits the per-consumer
    /// state-sync tick needs (the phux-ia4 bug). Do not "optimize" it.
    fn viewport_lines(&self) -> Option<Vec<String>> {
        let terminal = self.terminal.borrow();
        let synth = self.synth.borrow();
        match synth.screen_state_with_scrollback(&terminal, 0, None, false) {
            Ok(state) => Some(state.lines),
            Err(err) => {
                trace!(error = %err, "agent-detect: viewport read failed; skipping tick");
                None
            }
        }
    }

    /// One agent-detector tick (ADR-0046). Returns the interval the caller
    /// should re-arm the detector timer at.
    ///
    /// The detector is taken out of its `Option` for the duration: it needs
    /// `&mut` while `viewport_lines` needs `&self`, and the borrow checker
    /// will not have both. Put back before returning, always.
    fn detect_tick(&mut self) -> Option<std::time::Duration> {
        let mut detector = self.agent_detect.take()?;
        let now = std::time::Instant::now();
        // The detector's OWN dirty flag, not `terminal_dirty_since_tick`:
        // that one is cleared every ~30 ms by `tick_emit`, so a detector
        // ticking at 100-500 ms would observe it as `false` almost always and
        // skip every single scan.
        //
        // The flag is CONSUMED only by a scan that actually happened. A tick
        // that skips the scan must not eat the evidence that a scan is owed:
        // `wants_screen` is false for the whole of a pane's unidentified life,
        // so consuming it unconditionally threw away every grid mutation an
        // agent made before we noticed it existed — including the one that
        // painted the permission dialog we were supposed to see. Likewise a
        // failed projection leaves the flag set, so the next tick retries.
        let dirty = self.agent_dirty_since_detect;
        let screen = if detector.wants_screen(dirty) {
            let lines = self.viewport_lines();
            if lines.is_some() {
                self.agent_dirty_since_detect = false;
            }
            lines
        } else {
            None
        };
        let master_fd = self
            .pty
            .as_ref()
            .and_then(|p| p.master.lock().ok().and_then(|m| m.as_raw_fd()));
        let outcome = detector.tick(now, master_fd, &self.last_title, screen.as_deref());
        let next = detector.interval();
        self.agent_detect = Some(detector);

        match outcome {
            DetectOutcome::Quiet => {}
            DetectOutcome::Publish(report) => {
                self.emit_agent_state(AgentDetectEvent::State(report));
            }
            DetectOutcome::Retract => self.emit_agent_state(AgentDetectEvent::Retract),
        }
        Some(next)
    }

    /// Source agent events from a freshly-applied PTY chunk (phux-y2t),
    /// called right after `vt_write`. Sources, in order:
    ///
    /// - `bell` — a BEL (`0x07`) anywhere in the chunk. Emitted once per
    ///   chunk even if several BELs arrive together (a burst of bells is
    ///   one alert from the consumer's perspective).
    /// - `title_changed` — the libghostty-tracked OSC 0 / OSC 2 title now
    ///   differs from the last observed value.
    /// - `dirty` — the chunk mutated the grid (a new output burst began).
    ///   Coalesced: at most one `dirty` per burst; the settling `idle`
    ///   fires from the tick arm.
    /// - `OutputReceived` — broadcast to semantic event subscribers.
    /// - `GridChanged` — broadcast to semantic event subscribers.
    ///
    /// `command_started` / `command_finished` (phux-foz.4) — sourced from a
    /// direct OSC-133 scan of the raw chunk (see [`osc133`]): `C` emits
    /// `command_started`, `D` emits `command_finished` with the shell's
    /// exit code when reported. The grid projection cannot yield the
    /// `D`-mark exit code, so the byte stream is the honest source. Each
    /// `D` mark is also a prompt boundary: the pane's kernel cwd is
    /// re-queried there and a change emits `cwd_changed`.
    fn source_events_from_chunk(&mut self, chunk: &[u8]) {
        // UNCONDITIONAL, and deliberately ahead of the no-listener guard
        // below (ADR-0046). The agent-state detector reads `last_title` on
        // its own timer, and the OSC title is its highest-priority signal —
        // but this function used to return early for a pane nobody was
        // watching, which left `last_title` stale forever for exactly the
        // panes the sidebar most wants to describe. Refreshing here keeps one
        // title parser (libghostty's) and one mirror. Costs one FFI read plus
        // one compare per chunk, and allocates only when the title changes.
        let title_changed = self.refresh_title();
        if self.event_sink.is_none() && self.event_subscribers.borrow().is_empty() {
            return;
        }
        self.output_since_idle_tick = true;
        // OSC-133 prompt marks (phux-foz.4). Scanned before the coalesced
        // dirty/title sources below so a command boundary and its dirty
        // burst arrive in stream order.
        for mark in self.osc133.feed(chunk) {
            match mark {
                osc133::PromptMark::CommandStart => {
                    self.emit_event(AgentEvent::CommandStarted);
                    self.broadcast_agent_event(&AgentEvent::CommandStarted);
                }
                osc133::PromptMark::CommandEnd { exit_code } => {
                    self.emit_event(AgentEvent::CommandFinished { exit_code });
                    self.broadcast_agent_event(&AgentEvent::CommandFinished { exit_code });
                    // The command just finished: the shell is back at a
                    // prompt and any `cd` has landed. Re-query the kernel
                    // cwd and announce a change.
                    self.check_cwd_changed();
                }
            }
        }
        if chunk.contains(&0x07) {
            self.emit_event(AgentEvent::Bell);
        }
        // Title: `refresh_title` (above) already synced the mirror; emit on a
        // real change.
        if title_changed {
            self.emit_event(AgentEvent::TitleChanged {
                title: self.last_title.clone(),
            });
        }
        // Asked: source a pending human-answerable question from a `phux-ask`
        // title sentinel (phux-2sl6). Coalesced like dirty/idle — emit once
        // when the marker first appears and again only when its content
        // changes; retitling away from a `phux-ask` title clears the ask so
        // the next distinct one fires. The v1 trigger is OSC-driven; full
        // agent-state detection (manifests / hooks) is phux-2sl6.4.
        let current_ask = AskMarker::parse(&self.last_title);
        if current_ask != self.last_ask {
            if let Some(ask) = current_ask.as_ref() {
                self.emit_event(AgentEvent::Asked {
                    id: ask.id.clone(),
                    question: ask.question.clone(),
                    suggestions: ask.suggestions.clone(),
                    // Elapsed-since-ask is not tracked server-side in v1; the
                    // consumer renders a live waiting counter from receipt.
                    elapsed_seconds: None,
                });
            }
            self.last_ask = current_ask;
        }
        // Dirty: a chunk arrived, so the grid mutated. Coalesce to one
        // `dirty` per burst; `idle` (from the tick arm) closes the burst.
        if !self.in_output_burst {
            self.in_output_burst = true;
            self.emit_event(AgentEvent::Dirty);
            if !self.dirty_event_emitted_this_burst {
                self.broadcast_agent_event(&AgentEvent::Dirty);
                self.dirty_event_emitted_this_burst = true;
            }
        }
    }

    /// Emit `idle` when an output burst has settled (phux-y2t), called from
    /// the tick arm. A burst is "settled" when no PTY output chunk arrived
    /// since the previous tick. Idempotent: only the first settled tick after
    /// a `dirty` emits `idle`; subsequent idle ticks are silent until the next
    /// burst.
    fn maybe_emit_idle(&mut self) {
        let had_output = std::mem::take(&mut self.output_since_idle_tick);
        if self.in_output_burst && !had_output {
            self.in_output_burst = false;
            self.dirty_event_emitted_this_burst = false;
            self.emit_event(AgentEvent::Idle);
            self.broadcast_agent_event(&AgentEvent::Idle);
            // phux-foz.4: an output burst settling is the fallback prompt
            // boundary for shells without OSC-133 integration — a `cd`
            // echoes a prompt (burst), settles (idle), and the kernel cwd
            // re-query below announces the change. One best-effort syscall
            // per settled burst.
            self.check_cwd_changed();
        }
    }

    /// phux-foz.4: re-query the PTY child's kernel cwd and emit
    /// [`AgentEvent::CwdChanged`] when it differs from the last
    /// observation. Best-effort and coalesced: no PTY / dead child /
    /// denied query all yield silence, and an unchanged directory emits
    /// nothing. Called at OSC-133 `D` prompt boundaries and on output
    /// settle.
    fn check_cwd_changed(&self) {
        let Some(pid) = self.pty.as_ref().and_then(|p| p.child.process_id()) else {
            return;
        };
        let Some(cwd) = crate::cwd_query::process_cwd(pid) else {
            return;
        };
        let cwd = cwd.to_string_lossy().into_owned();
        if *self.last_known_cwd.borrow() == cwd {
            return;
        }
        self.last_known_cwd.borrow_mut().clone_from(&cwd);
        self.emit_event(AgentEvent::CwdChanged { cwd: cwd.clone() });
        self.broadcast_agent_event(&AgentEvent::CwdChanged { cwd });
    }

    /// Register a new event subscriber to receive semantic terminal events.
    /// Non-blocking: failure to send is silently dropped (accelerator semantics).
    /// Also updates the actor's `wire_terminal_id` for use in Event frames.
    fn subscribe_to_events(&mut self, request: SubscribeToEventsRequest) {
        self.wire_terminal_id = request.wire_terminal_id;
        self.event_subscribers.borrow_mut().push(request.subscriber);
    }

    /// Unsubscribe from semantic terminal events by removing the subscriber
    /// whose outbound mailbox pointer matches the provided reference.
    /// Silent no-op if the subscriber is not found.
    fn unsubscribe_from_events(&self, request: &UnsubscribeFromEventsRequest) {
        let mut subs = self.event_subscribers.borrow_mut();
        subs.retain(|sub| (&raw const sub.outbound) as usize != request.outbound_addr);
    }

    /// Broadcast an `AgentEvent` to all interested subscribers based on the
    /// event type. Uses `try_send`: drops events if a subscriber's mailbox is full.
    fn broadcast_agent_event(&self, event: &AgentEvent) {
        let subs = self.event_subscribers.borrow();
        for subscriber in subs.iter() {
            // Check if this subscriber is interested in this event type.
            // Map AgentEvent variants to TerminalEventType for filtering.
            let event_type = match event {
                AgentEvent::CommandStarted => Some(TerminalEventType::CommandStarted),
                AgentEvent::CommandFinished { .. } => Some(TerminalEventType::CommandEnded),
                AgentEvent::CwdChanged { .. } => Some(TerminalEventType::CwdChanged),
                AgentEvent::Dirty => Some(TerminalEventType::GridChanged),
                AgentEvent::Idle => Some(TerminalEventType::OutputReceived),
                // Other event types don't map to semantic filters yet
                _ => None,
            };

            // Supervisory control events (ADR-0033) bypass the semantic-type
            // filter: "who has the wheel" and "frozen" are not grid activity,
            // and every subscriber needs them to render an honest state.
            let interested = matches!(event, AgentEvent::TerminalControl { .. })
                || event_type.is_some_and(|et| {
                    subscriber.event_types.is_empty() || subscriber.event_types.contains(&et)
                });

            if interested {
                let frame = FrameKind::Event {
                    terminal: if self.wire_terminal_id != 0 {
                        Some(phux_protocol::ids::TerminalId::local(self.wire_terminal_id))
                    } else {
                        None
                    },
                    event: event.clone(),
                };
                let _ = subscriber.outbound.try_send(Outbound::Frame(frame));
            }
        }
    }

    /// Handle a supervisory [`ControlRequest`] (ADR-0033): a lease-change
    /// broadcast or a process signal. The input lease lives in `ServerState`;
    /// this actor is the emitter (it owns the event-subscriber list and the
    /// lifecycle) and the signal deliverer (it owns the PTY child pid).
    fn handle_control_request(&mut self, req: ControlRequest) {
        match req {
            ControlRequest::LeaseChanged {
                input_holder,
                action,
                actor,
            } => {
                self.emit_terminal_control(action, input_holder, Some(actor), None);
            }
            ControlRequest::AgentRecordInvalidated => {
                if let Some(detector) = self.agent_detect.as_mut() {
                    detector.invalidate_published();
                }
            }
            ControlRequest::Signal {
                signal,
                input_holder,
                by,
                reply,
            } => {
                let result = self.deliver_signal(signal);
                if result.is_ok() {
                    // Reflect the reversible brake in the lifecycle the next
                    // broadcast reports. Terminating signals leave it
                    // `Running` until the EOF path fires `PaneClosed`.
                    match signal {
                        TerminalSignal::Freeze => self.lifecycle = TerminalLifecycle::Frozen,
                        TerminalSignal::Resume => self.lifecycle = TerminalLifecycle::Running,
                        TerminalSignal::Interrupt
                        | TerminalSignal::Terminate
                        | TerminalSignal::Kill => {}
                    }
                    let action = match signal {
                        TerminalSignal::Interrupt => ControlAction::Interrupted,
                        TerminalSignal::Freeze => ControlAction::Frozen,
                        TerminalSignal::Resume => ControlAction::Resumed,
                        TerminalSignal::Terminate => ControlAction::Terminated,
                        TerminalSignal::Kill => ControlAction::Killed,
                    };
                    self.emit_terminal_control(action, input_holder, Some(by), None);
                }
                let _ = reply.send(result);
            }
        }
    }

    /// Deliver a POSIX signal to the pane's process group (ADR-0033).
    ///
    /// `portable_pty` spawns the child as a session/process-group leader (it
    /// calls `setsid` + `TIOCSCTTY` to give the PTY a controlling terminal),
    /// so the child's pid *is* its process-group id. Signaling the group
    /// (`killpg`) reaches the child and every subprocess it spawned — the
    /// agent and all its descendants — which is what "freeze/kill the agent"
    /// must mean.
    fn deliver_signal(&self, signal: TerminalSignal) -> Result<(), String> {
        use nix::sys::signal::{Signal as NixSignal, killpg};
        use nix::unistd::Pid;

        let pid = self
            .pty
            .as_ref()
            .and_then(|p| p.child.process_id())
            .and_then(|id| i32::try_from(id).ok())
            .ok_or_else(|| "no PTY child to signal".to_owned())?;

        let nix_signal = match signal {
            TerminalSignal::Interrupt => NixSignal::SIGINT,
            TerminalSignal::Freeze => NixSignal::SIGSTOP,
            TerminalSignal::Resume => NixSignal::SIGCONT,
            TerminalSignal::Terminate => NixSignal::SIGTERM,
            TerminalSignal::Kill => NixSignal::SIGKILL,
        };

        killpg(Pid::from_raw(pid), nix_signal).map_err(|err| format!("killpg failed: {err}"))
    }

    /// Build and broadcast an [`AgentEvent::TerminalControl`] (ADR-0033)
    /// stamped with this actor's current lifecycle.
    fn emit_terminal_control(
        &self,
        action: ControlAction,
        input_holder: Option<phux_protocol::ClientId>,
        actor: Option<phux_protocol::ClientId>,
        exit_status: Option<i32>,
    ) {
        self.broadcast_agent_event(&AgentEvent::TerminalControl {
            lifecycle: self.lifecycle,
            exit_status,
            input_holder,
            action,
            actor,
        });
    }

    /// Test-only: this actor's current shared adaptive tick interval
    /// (phux-q0e.5). Exposes [`Self::adaptive_tick_interval`] so tests can
    /// assert the cadence the `run` loop would arm without driving real time.
    #[cfg(test)]
    pub fn adaptive_tick_interval_for_test(&self) -> std::time::Duration {
        self.adaptive_tick_interval()
    }

    /// Test-only: drive `on_frame_ack` synchronously and report whether it
    /// produced a fresh RTT sample (phux-q0e.5). The `run` loop uses the
    /// return value to decide whether to re-arm the shared tick.
    #[cfg(test)]
    pub fn on_frame_ack_for_test(&mut self, client_id: ClientId, seq: u64) -> bool {
        self.on_frame_ack(client_id, seq)
    }

    /// Test-only: enable the per-consumer tick emission gate
    /// (`consumer_tick_emits`). Production defaults this OFF for human
    /// attach; this setter lets state-sync tests opt into the synthesized
    /// output path explicitly.
    #[cfg(test)]
    pub const fn enable_tick_emit_for_test(&mut self) {
        self.consumer_tick_emits = true;
    }

    /// Test-only: disable the per-consumer tick emission gate so the
    /// `tick_emit`-stays-silent path can be asserted locally, independent
    /// of the production default.
    #[cfg(test)]
    pub const fn disable_tick_emit_for_test(&mut self) {
        self.consumer_tick_emits = false;
    }

    /// Test-only: flip an already-registered state-sync consumer to the
    /// advance-on-ack loss-tolerant model (phux-v45.8), mirroring what the
    /// runtime does for a forwarded/lossy-leg consumer right after attach.
    #[cfg(test)]
    pub fn enable_loss_tolerance_for_test(&mut self, client_id: ClientId) {
        self.enable_loss_tolerance(client_id);
    }

    /// Test-only: backdate this consumer's emit instants by `by` so the
    /// loss-tolerant retransmit timer reads as elapsed on the next tick
    /// (phux-v45.8), letting a retransmit be exercised without sleeping real
    /// time. Saturates at the process epoch rather than underflowing.
    #[cfg(test)]
    pub fn backdate_emit_instants_for_test(
        &mut self,
        client_id: ClientId,
        by: std::time::Duration,
    ) {
        if let Some(state) = self.consumer_states.get_mut(&client_id) {
            let now = tokio::time::Instant::now();
            let past = now.checked_sub(by).unwrap_or(now);
            for instant in state.emit_instants.values_mut() {
                *instant = past;
            }
        }
    }

    /// Test-only: write `bytes` into the actor's `Terminal` and mark the
    /// per-tick dirty flag, mirroring the production PTY-byte path so the
    /// phux-4l0 idle short-circuit sees the mutation. Tests must use this
    /// rather than poking `terminal.borrow_mut().vt_write` directly, or
    /// the next `tick_emit` would short-circuit and skip the write.
    #[cfg(test)]
    pub fn vt_write_for_test(&mut self, bytes: &[u8]) {
        self.terminal.borrow_mut().vt_write(bytes);
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
    }

    /// Test-only: install in-memory PTY channels on a no-PTY actor so a
    /// test can inject a PTY-output burst (the returned
    /// [`mpsc::UnboundedSender<PtyEvent>`]) and observe the encoded input
    /// the actor forwards toward the PTY writer thread (the returned
    /// [`mpsc::Receiver<EncodedInputRequest>`]). Faithful to production
    /// wiring: queued output is consumed by `vt_write`; serviced input
    /// surfaces on the writer receiver. `pty` stays `None` — the run loop
    /// only reads `pty_tx` for input forwarding and `pty` for cwd/EOF,
    /// neither of which this seam exercises.
    #[cfg(test)]
    pub(crate) fn install_test_pty_channels(
        &mut self,
    ) -> (
        mpsc::UnboundedSender<PtyEvent>,
        mpsc::Receiver<EncodedInputRequest>,
    ) {
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<PtyEvent>();
        let (writer_tx, writer_rx) = mpsc::channel::<EncodedInputRequest>(DEFAULT_INPUT_MAILBOX);
        self.pty_rx = Some(evt_rx);
        self.pty_tx = Some(writer_tx);
        (evt_tx, writer_rx)
    }

    /// Synthesize a snapshot of the current `Terminal` state. Exposed
    /// for tests that want to drive the synthesis path synchronously
    /// without going through the actor's `select!` loop.
    fn synthesize(&self) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        self.synthesize_with_scrollback(None)
    }

    /// Synthesize an ATTACH snapshot, optionally priming the client's
    /// scrollback with retained history rows (`phux-9q5f`). `scrollback`
    /// follows the [`crate::grid::SnapshotSynthesizer::synthesize_with_scrollback`]
    /// convention. Exposed for tests that drive the synthesis path
    /// synchronously without the actor's `select!` loop.
    fn synthesize_with_scrollback(
        &self,
        scrollback: Option<u32>,
    ) -> Result<SnapshotBytes, crate::grid::SynthesisError> {
        let terminal = self.terminal.borrow();
        // phux-uow0: the full snapshot uses a fresh RenderState internally, so
        // it needs only a shared borrow — taking `&mut` here would falsely
        // serialize it against the per-consumer tick path that also reads
        // `self.synth`.
        let synth = self.synth.borrow();
        synth.synthesize_with_scrollback(&terminal, scrollback)
    }

    /// Project the current `Terminal` grid into a structured
    /// [`phux_core::screen::ScreenState`], stamping `pane` as the
    /// wire-local id. Side-effect-free — the read path for `GET_SCREEN`.
    fn screen_state(
        &self,
        pane: u32,
        scrollback: Option<u32>,
        cells: bool,
    ) -> Result<phux_core::screen::ScreenState, crate::grid::SynthesisError> {
        let terminal = self.terminal.borrow();
        // Shared borrow: the read goes through a fresh per-call
        // `RenderState` (see the synthesizer body), so it never contends
        // with the tick path's `&mut` use of the pooled state.
        let synth = self.synth.borrow();
        synth.screen_state_with_scrollback(&terminal, pane, scrollback, cells)
    }

    /// Publish the complete terminal-derived encoder state after a terminal
    /// mutation. Capture failures retain the previous good snapshot.
    fn publish_input_snapshot(&self) {
        let terminal = self.terminal.borrow();
        match InputEncoderSnapshot::capture(&terminal, self.cell_px) {
            Ok(snapshot) => {
                self.input_snapshot_tx.send_replace(snapshot);
            }
            Err(err) => warn!(error = %err, "input encoder snapshot capture failed"),
        }
    }

    /// Translate a [`TerminalInput`] into PTY bytes via the per-pane
    /// encoders + the current terminal state.
    ///
    /// Returns `Ok(None)` when the event was deliberately dropped
    /// (e.g., focus events while DEC 1004 is off; rejected untrusted
    /// pastes). Returns `Err` on encoder failure; the caller logs and
    /// continues — a single bad input must not kill the actor.
    fn encode_input(&self, input: &TerminalInput) -> Result<Option<Vec<u8>>, libghostty_vt::Error> {
        let terminal = self.terminal.borrow();
        match input {
            TerminalInput::Key(event) => {
                let mut enc = self.key_enc.borrow_mut();
                let bytes = enc.encode(event, &terminal)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Mouse(event) => {
                let mut enc = self.mouse_enc.borrow_mut();
                let bytes = enc.encode(event, &terminal, self.cell_px)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Focus(event) => {
                let mut enc = self.focus_enc.borrow_mut();
                let bytes = enc.encode(*event, &terminal)?;
                Ok(bytes.map(<[u8]>::to_vec))
            }
            TerminalInput::Paste(event) => {
                let mut enc = self.paste_enc.borrow_mut();
                match enc.encode(event, &terminal)? {
                    PasteOutcome::Encoded(bytes) => Ok(Some(bytes.to_vec())),
                    PasteOutcome::Rejected => Ok(None),
                }
            }
        }
    }

    /// Encode one input event and forward it to the PTY writer thread.
    /// Shared by the bounded `input_rx` drain in [`Self::run`]. A failed
    /// encode or a closed writer logs and is dropped — a single bad event
    /// must not kill the actor.
    fn service_input(&self, input: &TerminalInput) {
        // Every arm below logs at debug or above: a dropped or empty input
        // is invisible to the caller (ROUTE_INPUT acks Ok regardless, per
        // SPEC §9 fire-and-forget), so this log is the only witness when a
        // key vanishes between the mailbox and the PTY.
        match self.encode_input(input) {
            Ok(Some(bytes)) => {
                if bytes.is_empty() {
                    debug!(?input, "input encoded to zero bytes; nothing to write");
                    return;
                }
                self.service_encoded_input(EncodedInputRequest::legacy(bytes));
            }
            Ok(None) => {
                debug!(?input, "input gated/dropped by encoder");
            }
            Err(err) => {
                warn!(error = %err, "input encode failed; dropping event");
            }
        }
    }

    /// Forward bytes encoded by the dedicated input lane to the PTY writer.
    fn service_encoded_input(&self, request: EncodedInputRequest) {
        if request.bytes.is_empty() && request.completion.is_none() {
            return;
        }
        if let Some(tx) = self.pty_tx.as_ref() {
            let len = request.bytes.len();
            match tx.try_send(request) {
                Ok(()) => debug!(len, "input queued to PTY writer"),
                // Dropping input is fire-and-forget per SPEC L1 §9, but it
                // is not a debug-level event: the bytes are gone, nothing
                // downstream reports it, and the caller is still acked `Ok`.
                // At `debug!` this was invisible by default, which is how a
                // payload split across several events could lose an
                // interior one and corrupt mid-stream with no trace
                // (phux-oxd7).
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!(len, "PTY writer queue full; dropping input");
                }
                // The writer thread is gone. Every subsequent byte for this
                // pane goes nowhere while output, snapshots, and acks all
                // keep working — the pane looks alive and is not.
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    error!(len, "PTY writer channel closed; pane input is dead");
                }
            }
        } else {
            debug!("no PTY; encoded input discarded");
        }
    }

    /// Apply a resize to both the libghostty `Terminal` and the PTY
    /// kernel-side winsize. Idempotent; logs and continues on errors.
    fn handle_resize(&mut self, cols: u16, rows: u16, cell_px: Option<(u16, u16)>) {
        // libghostty has no concept of a zero-dimension grid: a 0-col or
        // 0-row resize fails with `InvalidValue` and leaves the grid at its
        // prior size. SPEC §10.5 already treats a zero-dimension viewport as
        // a no-op at the ATTACH path; clamp the live VIEWPORT_RESIZE path to
        // the same 1-cell minimum here so a `0x0` from a client (a host
        // terminal collapsing to nothing) can never reach libghostty.
        let cols = cols.max(1);
        let rows = rows.max(1);
        // Sticky cell size: see the `ResizeRequest::cell_px` doc.
        if let Some(cell) = cell_px {
            self.cell_px = cell;
        }
        let (cell_w, cell_h) = self.cell_px;

        // `Terminal::resize` takes the per-cell pixel size and derives the
        // terminal's pixel dimensions (`cells x cell size`) for XTWINOPS
        // size reports, mode-2048 in-band notifications, and image
        // protocols. Seeded to `DEFAULT_CELL_PX` and replaced by a client's
        // reported cell size, so it is always nonzero — pixel probes inside
        // the pane never see a zero text area.
        //
        // A both-axes shrink in a single resize() call once overflowed
        // libghostty's `PageList.resizeCols` (phux-y06, the SIGABRT
        // reproduced by the resize-extremes storm); libghostty-vt 0.2.0
        // covers that path, so a both-shrink is a single safe call.
        let applied = {
            let mut term = self.terminal.borrow_mut();
            let result = term.resize(cols, rows, u32::from(cell_w), u32::from(cell_h));
            if let Err(err) = result {
                warn!(?err, cols, rows, "terminal resize failed");
            }
            // Cache the dims libghostty actually settled on, never the
            // requested dims: on error (e.g. a clamped 0 that still failed)
            // the grid is unchanged, so caching the request would desync
            // the cache from the real grid size.
            (term.cols().unwrap_or(cols), term.rows().unwrap_or(rows))
        };
        self.cols = applied.0;
        self.rows = applied.1;
        self.size_report.set(SizeReportSize {
            rows: applied.1,
            columns: applied.0,
            cell_width: u32::from(cell_w),
            cell_height: u32::from(cell_h),
        });
        self.publish_input_snapshot();
        // A resize reflows the grid: every consumer reference is rebuilt
        // on the next diff, so force the next tick to walk (phux-4l0), and
        // force the next detector tick to re-scan (ADR-0046) — a reflow can
        // move the prompt box, which is a region the rules depend on.
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
        if let Some(pty) = &self.pty {
            // The kernel `winsize` pixel fields are the whole text area;
            // saturate rather than wrap if an enormous grid on a dense
            // display overflows the u16 (the kernel field is no wider).
            let size = PtySize {
                rows: applied.1,
                cols: applied.0,
                pixel_width: applied.0.saturating_mul(cell_w),
                pixel_height: applied.1.saturating_mul(cell_h),
            };
            if let Ok(master) = pty.master.lock()
                && let Err(err) = master.resize(size)
            {
                warn!(
                    ?err,
                    cols = applied.0,
                    rows = applied.1,
                    "pty resize ioctl failed"
                );
            }
        }
    }

    /// Broadcast a full synthesized snapshot of the canonical `Terminal`'s
    /// current grid to every attached client, as an in-band
    /// [`PaneOutput::Resync`].
    ///
    /// Two callers: a resize that reflowed the grid (phux-8v1, below), and a
    /// `resync_only` request from an output pump that dropped bytes past the
    /// broadcast buffer (`RecvError::Lagged`) and needs its consumer's mirror
    /// rebuilt. Both want the same thing — the authoritative grid re-sent on the
    /// ordered output channel so it cleanly supersedes whatever the client last
    /// applied, with no double-apply or lost output.
    ///
    /// Why this is needed: a resize triggers an *independent* reflow on
    /// both the server's canonical `Terminal` and each client's mirror
    /// `Terminal`. Those reflows can diverge — libghostty's cols-shrink
    /// reflow does not reproduce the client mirror's content identically,
    /// dropping rows — so after a resize the client mirror and the server
    /// grid disagree. The live output path (the PTY-byte broadcast fanned
    /// out by the per-attach pump in `runtime.rs`) only carries *new* PTY
    /// bytes, so the historical grid content is never re-sent and the
    /// divergence is permanent: the user sees lost / duplicated rows
    /// ("repeating/duplicated characters on resize").
    ///
    /// The synthesized bytes from [`SnapshotSynthesizer::synthesize`] open
    /// with a `DECSTR + ED2 + home` reset preamble, so feeding them to the
    /// client mirror via the ordinary `TERMINAL_OUTPUT` → `vt_write` path
    /// resets that mirror and repaints it from authoritative state. We
    /// reuse the existing output broadcast rather than the per-consumer
    /// state-sync path (`consumer_states`) because the runtime drives the
    /// broadcast/pump path; the q0e per-consumer tick is not wired into
    /// the runtime today.
    fn broadcast_resync(&self, reason: ResyncReason) {
        // No subscribers → nothing to resync. `receiver_count` is the
        // broadcast channel's live-subscriber count; the seed receiver
        // held by the actor was dropped at construction, so this is the
        // attached-pump count.
        if self.output_tx.receiver_count() == 0 {
            return;
        }
        match self.synthesize() {
            Ok(snap) => {
                debug!(
                    bytes = snap.bytes.len(),
                    "resize resync: snapshot broadcast"
                );
                // A `Lagged`/no-receiver send error is benign here — the
                // next PTY output or a re-attach snapshot re-syncs.
                // phux-3ns5: ship the post-reflow grid as a `Resync` (→
                // `TERMINAL_SNAPSHOT`) carrying the settled dims, so the
                // client mirror resizes to `(cols, rows)` before applying
                // the replay. Delivered as raw output it could not resize
                // the mirror, stranding a resize-grow with blank space.
                let _ = self.output_tx.send(PaneOutput::Resync {
                    cols: self.cols,
                    rows: self.rows,
                    reason,
                    bytes: Bytes::from(snap.bytes),
                });
            }
            Err(err) => {
                warn!(
                    error = %err,
                    "resize resync: snapshot synthesis failed; clients recover on next output",
                );
            }
        }
    }

    /// Best-effort reap the child if it has already exited. Called on
    /// PTY EOF — at that point the child has almost certainly exited
    /// (EOF on the master fd indicates the slave has been closed,
    /// which usually means the child has exited or detached). We try
    /// `try_wait` first to avoid blocking; if it returns `None` we
    /// leave the child alone (it might still be alive doing something
    /// odd; the shutdown path will deal with it).
    ///
    /// Returns the exit status in the shape the `TERMINAL_CLOSED` wire
    /// frame wants (phux-4li.11): `Some(code)` for a normal `_exit(n)`,
    /// `None` for signal-killed children or otherwise-unknown exits.
    /// `portable_pty::ExitStatus.signal` is the discriminator — a
    /// non-`None` signal name means the kernel reports the death as
    /// signal-driven, which collapses to `exit_status = None` on the
    /// wire per the SPEC §10.1 compact-subset rule.
    fn reap_child_if_any(&mut self) -> Option<i32> {
        let pty = self.pty.as_mut()?;
        // PTY EOF races the child becoming waitable: the master reads EOF
        // the moment the last slave fd closes, which can be a hair before
        // the kernel marks the process reapable. A single `try_wait` here
        // reported `exit_status: None` for children that exited cleanly
        // microseconds later, so TERMINAL_CLOSED lied to agents reading
        // exit codes. Retry briefly. The blocking sleep is deliberate:
        // this runs on the single current-thread runtime, but only once
        // per pane lifetime, and the budget is small; an async retry would
        // need to move `child` out of `self.pty`, which the shutdown path
        // still owns. A child that closed its slave but keeps running
        // (a daemonizer) exhausts the budget and reports `None`, as before.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        loop {
            match pty.child.try_wait() {
                Ok(Some(status)) => {
                    debug!(?status, "child reaped on PTY EOF");
                    return exit_status_to_wire(&status);
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(None) => {
                    trace!("PTY EOF but child still alive — leaving to shutdown path");
                    return None;
                }
                Err(err) => {
                    debug!(?err, "child try_wait failed on PTY EOF");
                    return None;
                }
            }
        }
    }

    /// React to PTY EOF (the child went away): detach the PTY-read branch
    /// and notify the runtime so it can broadcast `TERMINAL_CLOSED`.
    ///
    /// Dropping `pty_rx` parks the pump's `select!` arm forever, but the
    /// actor deliberately stays alive — it must remain reachable for
    /// late-arriving `SnapshotRequest`s (a client attaching just after the
    /// child exited) and for orderly shutdown via the cancellation token.
    /// The child is reaped here so we don't leave a zombie waiting for the
    /// explicit shutdown signal. (phux-it8: firing `exit_notify` is what
    /// lets attached clients learn the shell exited instead of freezing in
    /// alt-screen.)
    ///
    /// TODO(phux-9gw): multi-pane lifecycle — when a session has more than
    /// one pane, a single EOF should switch focus to a sibling rather than
    /// detach the whole session. Today sessions are 1:1 with panes in
    /// practice so the simpler "EOF → detach attached" model is correct.
    fn handle_pty_eof(&mut self) {
        debug!("PTY EOF; firing exit_notify and keeping actor alive for late snapshot/input drain");
        self.pty_rx = None;
        let exit_status = self.reap_child_if_any();
        if let Some(tx) = self.exit_notify.take() {
            let _ = tx.send(exit_status);
        }
    }

    /// Tear down the PTY: gracefully stop the child if still alive, drop
    /// the master (which sends EOF to the slave and unblocks the reader
    /// thread), and join the bridge threads. Best-effort: errors are
    /// logged, not propagated, because we're on the shutdown path.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn shutdown_pty(&mut self) {
        let Some(mut pty) = self.pty.take() else {
            return;
        };
        // If the child is still alive, tear it down *gracefully* so a
        // foreground process (e.g. `claude`) gets a chance to flush before
        // we pull the rug — see `terminate_child_group`. If it already
        // exited this is a no-op; `wait` below reaps the zombie.
        match pty.child.try_wait() {
            Ok(Some(_status)) => {
                trace!("pty child already exited");
            }
            Ok(None) => {
                Self::terminate_child_group(&mut pty).await;
            }
            Err(err) => {
                debug!(?err, "pty child try_wait failed");
            }
        }
        // Drop the master so the reader thread sees EOF and exits.
        // We drop pty_tx so the writer thread sees a closed channel
        // and exits. Both happen automatically when `self.pty` /
        // `self.pty_tx` are dropped at the end of `run`, but doing it
        // here makes the thread joins below predictable.
        drop(self.pty_tx.take());
        // Reap the child so the OS releases its slot.
        match pty.child.wait() {
            Ok(status) => debug!(?status, "pty child reaped"),
            Err(err) => debug!(?err, "pty child wait failed"),
        }
        // We can't drop `pty.master` separately because it's behind an
        // Arc<Mutex<_>> — the Arc strong count drops when `pty` falls
        // out of scope at the end of this function.
        if let Some(handle) = pty.reader_thread.take() {
            // Bounded wait: if the reader hasn't exited inside a small
            // budget we move on. Joining unconditionally would hang
            // the shutdown path if the reader is wedged in a blocking
            // read on a pty fd that won't EOF (rare but possible).
            //
            // In practice EOF arrives immediately after `master` drops
            // at the end of this function; we accept the join-on-drop
            // ordering risk because the reader thread is owned and
            // bounded by us. The unwrap below is safe because the
            // thread itself can't panic — it's a tight loop on Read.
            let _ = handle.join();
        }
        if let Some(handle) = pty.writer_thread.take() {
            let _ = handle.join();
        }
        drop(pty);
    }

    /// Gracefully stop a still-running PTY child on pane teardown (phux-sw1).
    ///
    /// A pane close is a hangup: send `SIGHUP` to both the PTY's foreground
    /// process group and the session-leading shell group. Interactive shells
    /// put foreground jobs such as `claude` in a separate process group, so
    /// signaling only the shell group misses the process that needs to flush.
    /// Poll for both groups to exit within [`PANE_KILL_GRACE`], then `SIGKILL`
    /// any survivors as a backstop. The PTY master stays open for the duration,
    /// so the foreground process can still write during the grace window.
    ///
    /// This replaces an immediate `std::process::Child::kill` (a `SIGKILL` of
    /// the shell pid alone, with no grace), which killed a foreground agent
    /// before it could persist its transcript. The foreground group is
    /// snapshotted from the PTY before signaling to avoid losing it when the
    /// shell exits. Falls back to the library kill if no group can be found.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn terminate_child_group(pty: &mut PtyOwned) {
        use nix::errno::Errno;
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;

        let shell_group = pty
            .child
            .process_id()
            .and_then(|id| i32::try_from(id).ok())
            .map(Pid::from_raw);
        let foreground_group = pty
            .master
            .lock()
            .ok()
            .and_then(|master| master.process_group_leader())
            .filter(|id| *id > 0)
            .map(Pid::from_raw);

        // Signal the foreground job first. The shell may exit immediately on
        // SIGHUP, at which point tcgetpgrp can no longer recover this group.
        let mut groups = Vec::with_capacity(2);
        if let Some(group) = foreground_group {
            groups.push(group);
        }
        if let Some(group) = shell_group
            && !groups.contains(&group)
        {
            groups.push(group);
        }
        if groups.is_empty() {
            let _ = pty.child.kill();
            return;
        }

        let mut delivered = false;
        for &group in &groups {
            match killpg(group, Signal::SIGHUP) {
                Ok(()) => delivered = true,
                Err(Errno::ESRCH) => {}
                Err(err) => debug!(?err, ?group, "SIGHUP to pane group failed"),
            }
        }
        if !delivered {
            let _ = pty.child.kill();
            return;
        }

        // Poll every snapshotted group, not just the shell child: the shell can
        // exit while a foreground job remains alive. Reap the shell as it exits
        // so its zombie does not keep the shell process group looking alive for
        // the entire grace period.
        let deadline = tokio::time::Instant::now() + PANE_KILL_GRACE;
        while tokio::time::Instant::now() < deadline {
            if let Err(err) = pty.child.try_wait() {
                debug!(?err, "try_wait during pane-kill grace failed");
            }
            if groups
                .iter()
                .all(|&group| matches!(killpg(group, None), Err(Errno::ESRCH)))
            {
                return;
            }
            tokio::time::sleep(PANE_KILL_POLL).await;
        }

        // Backstop: a group ignored the hangup (or is mid-flush past the
        // budget). Hard-kill every surviving snapshotted group.
        for group in groups {
            if !matches!(killpg(group, None), Err(Errno::ESRCH)) {
                let _ = killpg(group, Signal::SIGKILL);
            }
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn handle_native_bootstrap(&mut self, req: NativeBootstrapRequest) {
        let NativeBootstrapRequest {
            outbound,
            owner,
            terminal_id,
            stream_id,
            bootstrap_id,
            limits,
            base_seq,
            reply,
        } = req;
        let max_bytes = usize::try_from(limits.max_chunk_bytes())
            .expect("negotiated native chunk limit fits usize");
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(max_bytes).is_err() {
            let _ = reply.send(Err(crate::native_state::NativeStateError::OutOfMemory));
            return;
        }
        buffer.resize(max_bytes, 0);
        self.release_native_owner(owner);

        let mut began = false;
        let mut tombstoned = false;
        let result = (|| -> Result<
            crate::native_state::OpaqueHistoryCursor,
            crate::native_state::NativeStateError,
        > {
            let mut host = self.terminal.borrow_mut();
            let manager = host.native_manager()?;
            let mut capture = manager.capture(limits)?;
            outbound
                .try_send(Outbound::Frame(FrameKind::BootstrapBegin {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    profile: phux_protocol::caps::BootstrapStreamProfile::NativeState {
                        codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
                    },
                    cols: self.cols,
                    rows: self.rows,
                    base_seq,
                }))
                .map_err(|_| crate::native_state::NativeStateError::InvalidHandle)?;
            began = true;

            let mut chunk_seq = 0_u32;
            loop {
                let event = match capture.step(&mut buffer) {
                    Ok(event) => event,
                    Err(error) => {
                        let _ = outbound
                            .try_send(Outbound::Frame(FrameKind::BootstrapTombstone {
                                terminal_id: terminal_id.clone(),
                                stream_id,
                                bootstrap_id,
                                reason: phux_protocol::wire::frame::TombstoneReason::CodecFailure,
                                last_valid_seq: base_seq,
                            }));
                        tombstoned = true;
                        return Err(error);
                    }
                };
                let ready = matches!(
                    event.kind,
                    crate::native_state::NativeCheckpointChunkKind::Ready
                );
                let payload = native_bytes(event.bytes)?;
                outbound
                    .try_send(Outbound::Frame(FrameKind::BootstrapChunk {
                        terminal_id: terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        chunk_seq,
                        payload,
                    }))
                    .map_err(|_| crate::native_state::NativeStateError::InvalidHandle)?;
                chunk_seq = chunk_seq
                    .checked_add(1)
                    .ok_or(crate::native_state::NativeStateError::LimitExceeded)?;
                if ready {
                    let cursor = *capture
                        .ready_cursor()
                        .ok_or(crate::native_state::NativeStateError::InvalidState)?;
                    let cursor_bytes = match native_bytes(&cursor) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            drop(capture);
                            manager.release(&cursor)?;
                            return Err(error);
                        }
                    };
                    let ready_frame = FrameKind::BootstrapReady {
                        terminal_id: terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        history_cursor: Some(cursor_bytes),
                    };
                    if outbound.try_send(Outbound::Frame(ready_frame)).is_err() {
                        drop(capture);
                        manager.release(&cursor)?;
                        return Err(crate::native_state::NativeStateError::InvalidHandle);
                    }
                    return Ok(cursor);
                }
            }
        })();
        if result.is_err() && began && !tombstoned {
            let _ = outbound.try_send(Outbound::Frame(FrameKind::BootstrapTombstone {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reason: phux_protocol::wire::frame::TombstoneReason::CodecFailure,
                last_valid_seq: base_seq,
            }));
        }
        if let Ok(cursor) = result.as_ref() {
            self.native_cursor_owners
                .insert(*cursor, (owner, tokio::time::Instant::now(), base_seq));
        }
        let _ = reply.send(result.map(|_| ()));
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn handle_native_history(&mut self, req: NativeHistoryRequest) {
        let NativeHistoryRequest {
            outbound,
            owner,
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            max_bytes,
            limits,
            reply,
        } = req;
        let cursor: crate::native_state::OpaqueHistoryCursor = match cursor.as_ref().try_into() {
            Ok(cursor) => cursor,
            Err(_) => {
                let _ = reply.send(Err(crate::native_state::NativeStateError::InvalidHandle));
                return;
            }
        };
        let result = (|| -> Result<bool, crate::native_state::NativeStateError> {
            let Some((cursor_owner, _, last_valid_seq)) =
                self.native_cursor_owners.get(&cursor).copied()
            else {
                return Err(crate::native_state::NativeStateError::InvalidHandle);
            };
            if cursor_owner != owner {
                return Err(crate::native_state::NativeStateError::InvalidHandle);
            }
            let bound = max_bytes.min(limits.max_history_page_bytes());
            let size = usize::try_from(bound)
                .map_err(|_| crate::native_state::NativeStateError::LimitExceeded)?;
            if size == 0 {
                return Err(crate::native_state::NativeStateError::LimitExceeded);
            }
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(size)
                .map_err(|_| crate::native_state::NativeStateError::OutOfMemory)?;
            buffer.resize(size, 0);
            let mut host = self.terminal.borrow_mut();
            let manager = host.native_manager()?;
            let event = match manager.next(&cursor, limits, bound, &mut buffer) {
                Ok(event) => event,
                Err(error) => {
                    let reason = match error {
                        crate::native_state::NativeStateError::Resize => {
                            phux_protocol::wire::frame::TombstoneReason::Resize
                        }
                        _ => phux_protocol::wire::frame::TombstoneReason::CodecFailure,
                    };
                    let _ = outbound.try_send(Outbound::Frame(FrameKind::BootstrapTombstone {
                        terminal_id: terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        reason,
                        last_valid_seq,
                    }));
                    if matches!(
                        error,
                        crate::native_state::NativeStateError::Reset
                            | crate::native_state::NativeStateError::Resize
                            | crate::native_state::NativeStateError::Pruned
                            | crate::native_state::NativeStateError::Stale
                    ) {
                        let _ = manager.release(&cursor);
                    }
                    return Err(error);
                }
            };
            let ended = matches!(&event, crate::native_state::NativeHistoryEvent::End);
            match event {
                crate::native_state::NativeHistoryEvent::Page {
                    bytes, next_cursor, ..
                } => {
                    outbound
                        .try_send(Outbound::Frame(FrameKind::HistoryPage {
                            terminal_id,
                            stream_id,
                            bootstrap_id,
                            cursor: native_bytes(&cursor)?,
                            next_cursor: Some(native_bytes(&next_cursor)?),
                            payload: native_bytes(bytes)?,
                        }))
                        .map_err(|_| crate::native_state::NativeStateError::InvalidHandle)?;
                }
                crate::native_state::NativeHistoryEvent::End => {
                    outbound
                        .try_send(Outbound::Frame(FrameKind::HistoryPage {
                            terminal_id,
                            stream_id,
                            bootstrap_id,
                            cursor: native_bytes(&cursor)?,
                            next_cursor: None,
                            payload: Bytes::new(),
                        }))
                        .map_err(|_| crate::native_state::NativeStateError::InvalidHandle)?;
                    manager.release(&cursor)?;
                }
            }
            Ok(ended)
        })();
        let invalidated = match &result {
            Err(error) => matches!(
                error,
                crate::native_state::NativeStateError::Reset
                    | crate::native_state::NativeStateError::Resize
                    | crate::native_state::NativeStateError::Pruned
                    | crate::native_state::NativeStateError::Stale
            ),
            Ok(_) => false,
        };
        if matches!(result, Ok(true)) || invalidated {
            self.native_cursor_owners.remove(&cursor);
        } else if result.is_ok()
            && let Some(entry) = self.native_cursor_owners.get_mut(&cursor)
        {
            entry.1 = tokio::time::Instant::now();
        }
        let _ = reply.send(result.map(|_| ()));
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn release_native_owner(&mut self, owner: u64) {
        let cursors: Vec<_> = self
            .native_cursor_owners
            .iter()
            .filter_map(|(cursor, (cursor_owner, _, _))| {
                (*cursor_owner == owner).then_some(*cursor)
            })
            .collect();
        if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
            for cursor in &cursors {
                let _ = manager.release(cursor);
            }
        }
        for cursor in cursors {
            self.native_cursor_owners.remove(&cursor);
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn expire_native_cursors(&mut self) {
        let cutoff = tokio::time::Instant::now() - NATIVE_HISTORY_TTL;
        let cursors: Vec<_> = self
            .native_cursor_owners
            .iter()
            .filter_map(|(cursor, (_, touched, _))| (*touched <= cutoff).then_some(*cursor))
            .collect();
        if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
            for cursor in &cursors {
                let _ = manager.release(cursor);
            }
        }
        for cursor in cursors {
            self.native_cursor_owners.remove(&cursor);
        }
    }

    /// Run the actor's event loop until shutdown.
    ///
    /// Branches:
    /// - `shutdown` — biased first so a clean shutdown beats a hot PTY.
    /// - `pty_rx` — PTY bytes: write to `Terminal`, broadcast to
    ///   subscribed clients.
    /// - `input_rx` — wire input events: encode via per-pane encoders
    ///   and forward to the PTY writer thread.
    /// - `snapshot_rx` — ATTACH snapshots: synthesize from `Terminal`.
    /// - `resize_rx` — viewport size changes: update `Terminal` +
    ///   PTY winsize.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "single select! loop; arms are short and inlined for locality"
    )]
    #[allow(
        clippy::cognitive_complexity,
        reason = "select! macro expansion inflates the score; arms are individually small and locality wins over decomposition"
    )]
    pub async fn run(mut self) {
        debug!(
            cols = self.cols,
            rows = self.rows,
            has_pty = self.pty.is_some(),
            "TerminalActor started",
        );

        // State-sync tick driver (phux-q0e.3 / phux-q0e.5). RTT-adaptive
        // cadence: starts at the `DEFAULT_TICK_INTERVAL` cold-start value and
        // is rebuilt toward each consumer's measured `RTT/2` (clamped to
        // [`MIN_TICK_INTERVAL`, `MAX_TICK_INTERVAL`]) as `FRAME_ACK`
        // round-trips land. The shared timer runs at the minimum desired
        // interval across consumers (see [`Self::adaptive_tick_interval`]).
        // `MissedTickBehavior::Delay` — if the actor falls behind under heavy
        // PTY traffic we want subsequent ticks spaced by the interval from
        // when they ran, not bunched up to "catch up" (which would defeat the
        // rate limit's purpose). `Burst` (the default) would spam emissions
        // when a long PTY chunk delays us past several tick boundaries.
        let mut tick_interval = DEFAULT_TICK_INTERVAL;
        let mut tick = tokio::time::interval(tick_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Eat the first immediate tick (Interval fires synchronously on
        // first poll). Without this, the very first iteration would
        // tick before any other branch has a chance to react.
        let _ = tick.tick().await;

        // Agent-state detector (ADR-0046). Constructed HERE, not in `build`,
        // for two reasons: `started` then anchors the startup grace window at
        // the moment the child actually begins painting, and no existing
        // constructor or test actor grows a detector it never asked for. Only
        // a PTY-backed actor with a wired sink and a non-empty rule set gets
        // one — everything else pays exactly nothing.
        let rules = crate::agent_detect::rules::global();
        if self.pty.is_some() && self.agent_state_sink.is_some() && !rules.is_empty() {
            self.agent_detect = Some(AgentDetector::new(rules, std::time::Instant::now()));
        }
        let mut detect_interval = crate::agent_detect::TICK_UNIDENTIFIED;
        let mut detect_tick = tokio::time::interval(detect_interval);
        detect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let _ = detect_tick.tick().await;

        // phux-8v1 drag fix: debounce timer for the post-resize client
        // resync. (Re)armed on each resync-requesting resize; when it
        // fires we broadcast ONE snapshot at the settled size. Init far
        // out — `resync_pending` is false until a resize arms it, and we
        // always `reset()` the deadline when arming, so the initial
        // instant is never observed.
        let resync_debounce = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::pin!(resync_debounce);
        let mut resync_pending = false;
        let mut resync_reason = ResyncReason::Resize;

        loop {
            // For panes without a PTY, the `pty_rx` branch needs an
            // always-pending future. We construct that with
            // `recv_or_pending`: when the receiver is `Some`, it polls
            // it; when `None`, it parks forever (so the select! arm
            // never fires and the other arms are the only ones live).
            tokio::select! {
                biased;

                () = self.token.cancelled() => {
                    debug!("TerminalActor cancellation token fired");
                    self.shutdown_pty().await;
                    return;
                }

                // Bytes already encoded on the dedicated input lane. This
                // bounded mailbox is the production input path and shares the
                // actor's highest scheduling priority.
                Some(request) = self.encoded_input_rx.recv() => {
                    self.service_encoded_input(request);
                    for _ in 1..MAX_INPUT_COALESCE {
                        match self.encoded_input_rx.try_recv() {
                            Ok(next) => self.service_encoded_input(next),
                            Err(_) => break,
                        }
                    }
                }

                // Legacy inline input → PTY, retained for direct-drive tests
                // that intentionally construct the runtime without a lane.
                // Polled before the PTY-output arm (biased
                // order) so a queued keystroke is serviced this turn
                // rather than waiting behind an output burst — the fix for
                // load-correlated input starvation. Bounded by
                // `MAX_INPUT_COALESCE`: the arm fires on the first ready
                // event, then drains up to a capped batch via `try_recv`
                // so a paste the encoder expands cannot inflate one turn
                // without limit. The PTY-output arm's structural bound is
                // `MAX_PTY_COALESCE_BYTES`.
                Some(input) = self.input_rx.recv() => {
                    self.service_input(&input);
                    for _ in 1..MAX_INPUT_COALESCE {
                        match self.input_rx.try_recv() {
                            Ok(next) => self.service_input(&next),
                            // Empty (nothing more ready) or Disconnected —
                            // stop draining.
                            Err(_) => break,
                        }
                    }
                }

                // PTY → Terminal + broadcast. Polled after the input arm:
                // when this arm finishes one bounded payload and returns to
                // the `select!`, the biased re-poll offers `input_rx`
                // first, so a keystroke queued during the burst is serviced
                // before the next `vt_write`. When the byte cap stops the
                // drain mid-burst the arm additionally yields to the
                // scheduler before re-entering, so sibling tasks on the
                // LocalSet (and a freshly-queued keystroke) get a turn
                // between bounded parses rather than after the whole burst.
                evt = recv_or_pending(self.pty_rx.as_mut()) => {
                    match evt {
                        Some(PtyEvent::Bytes(first)) => {
                            // Coalesce any chunks already queued behind this one
                            // into a single Terminal write + broadcast frame
                            // (phux-ahk burst path). A lone chunk takes the
                            // fast path below: its `Vec` moves into `Bytes`
                            // with no copy. Only a genuine burst (several
                            // reads queued) allocates a join buffer. The drain
                            // stops on the chunk-count cap, on EOF, or once the
                            // payload would cross `MAX_PTY_COALESCE_BYTES` — in
                            // the byte-cap case the crossing chunk is left
                            // queued for the next turn (mpsc has no peek, so the
                            // length is checked before `try_recv`).
                            let mut coalesced: Vec<u8> = Vec::new();
                            let mut saw_eof = false;
                            // `true` when the drain stopped because the next
                            // chunk would cross the byte cap (more output is
                            // likely queued) rather than because the queue
                            // emptied. Drives the post-broadcast yield so a
                            // sustained burst hands the scheduler a turn
                            // between bounded parses.
                            let mut hit_byte_cap = false;
                            for _ in 0..MAX_PTY_COALESCE {
                                // Length so far: the lone `first` chunk before
                                // any coalescing, else the join buffer. Stop
                                // before consuming a chunk that would push the
                                // payload past the byte cap so each `vt_write`
                                // is a bounded synchronous parse. The first
                                // chunk always lands; only coalescing is capped.
                                let current_len = if coalesced.is_empty() {
                                    first.len()
                                } else {
                                    coalesced.len()
                                };
                                if current_len >= MAX_PTY_COALESCE_BYTES {
                                    hit_byte_cap = true;
                                    break;
                                }
                                match self.pty_rx.as_mut().map(mpsc::UnboundedReceiver::try_recv) {
                                    Some(Ok(PtyEvent::Bytes(more))) => {
                                        if coalesced.is_empty() {
                                            coalesced.reserve(first.len() + more.len());
                                            coalesced.extend_from_slice(&first);
                                        }
                                        coalesced.extend_from_slice(&more);
                                    }
                                    // A queued EOF: flush the coalesced bytes
                                    // first, then handle EOF below.
                                    Some(Ok(PtyEvent::Eof)) => {
                                        saw_eof = true;
                                        break;
                                    }
                                    // Empty (nothing more ready) or the sender
                                    // dropped — stop draining. A dropped sender
                                    // surfaces as EOF on the next pump wakeup.
                                    _ => break,
                                }
                            }
                            let payload: Bytes = if coalesced.is_empty() {
                                Bytes::from(first)
                            } else {
                                Bytes::from(coalesced)
                            };
                            // Debug level deliberately (was trace): this is
                            // the pump's only witness line, and the lost-echo
                            // forensics (phux-dacb follow-up) need it inside
                            // the test capture's debug filter. Per-wakeup, so
                            // it costs one line per coalesced read, not per
                            // byte.
                            debug!(bytes = payload.len(), "vt_write: PTY chunk(s) -> Terminal");
                            self.terminal.borrow_mut().vt_write(&payload);
                            self.answer_color_queries(&payload);
                            self.publish_input_snapshot();
                            // The grid changed: let the next tick walk
                            // the rows (phux-4l0 idle short-circuit) and let
                            // the next detector tick re-scan (ADR-0046).
                            self.terminal_dirty_since_tick = true;
                            self.agent_dirty_since_detect = true;
                            // phux-y2t: source agent events (bell, title,
                            // dirty) from the just-applied bytes for any
                            // event-stream subscriber. No-op without a sink.
                            self.source_events_from_chunk(&payload);
                            // Broadcast send fails only when no
                            // subscribers exist; that's a normal
                            // steady-state (no attached clients) and
                            // we silently drop.
                            let _ = self.output_tx.send(PaneOutput::Live(payload));
                            if saw_eof {
                                self.handle_pty_eof();
                            } else if hit_byte_cap {
                                // A capped payload with more output queued:
                                // yield so the runtime re-polls (input arm
                                // first) and sibling LocalSet tasks advance,
                                // bounding the output arm at the thread level.
                                // The next loop turn coalesces the next
                                // bounded payload, so throughput is preserved.
                                tokio::task::yield_now().await;
                            }
                        }
                        Some(PtyEvent::Eof) | None => {
                            self.handle_pty_eof();
                        }
                    }
                }

                Some(req) = self.native_requests.recv() => {
                    match req {
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        NativeActorRequest::Bootstrap(req) => {
                            self.handle_native_bootstrap(req).await;
                        }
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        NativeActorRequest::History(req) => {
                            self.handle_native_history(req).await;
                        }
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        NativeActorRequest::Release(req) => {
                            self.release_native_owner(req.owner);
                        }
                        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
                        NativeActorRequest::Disabled => unreachable!("disabled native receiver"),
                    }
                }


                Some(req) = self.snapshot_rx.recv() => {
                    let snap = match self.synthesize_with_scrollback(req.scrollback) {
                        Ok(s) => s,
                        Err(err) => {
                            warn!(error = %err, "snapshot synthesis failed; replying with empty");
                            SnapshotBytes {
                                cols: self.cols,
                                rows: self.rows,
                                bytes: Vec::new(),
                                scrollback: Vec::new(),
                            }
                        }
                    };
                    let _ = req.reply.send(snap);
                }

                Some(req) = self.set_default_colors_rx.recv() => {
                    let result = match &mut *self.terminal.borrow_mut() {
                        CanonicalTerminal::Plain(Some(terminal)) => {
                            Self::install_default_colors(terminal, req.colors)
                        }
                        CanonicalTerminal::Plain(None) => Ok(()),
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        CanonicalTerminal::Native(_) => {
                            warn!("default palette update deferred while native cuts are active");
                            Ok(())
                        }
                    };
                    if let Err(err) = result {
                        warn!(error = %err, "failed to install client default colors");
                    }
                    let _ = req.reply.send(());
                }

                Some(req) = self.screen_rx.recv() => {
                    let want_cells = req.cells;
                    let screen = self.screen_state(req.pane, req.scrollback, req.cells).unwrap_or_else(|err| {
                        warn!(error = %err, "screen projection failed; replying with empty");
                        phux_core::screen::ScreenState {
                            schema_version: phux_core::screen::SCHEMA_VERSION,
                            pane: req.pane,
                            cols: self.cols,
                            rows: self.rows,
                            cursor: None,
                            lines: Vec::new(),
                            scrollback: Vec::new(),
                            // Honour the request shape even on the error
                            // path: an empty cells vec, not a misleading
                            // `None`, when the caller asked for cells.
                            cells: want_cells.then(Vec::new),
                        }
                    });
                    let _ = req.reply.send(screen);
                }

                Some(req) = self.upgrade_rx.recv() => {
                    // ADR-0032: hand the upgrade producer this pane's PTY
                    // descriptors + a full replay snapshot. Read-only; mirrors
                    // the snapshot/pwd paths.
                    let snap = self.synthesize_with_scrollback(Some(0)).unwrap_or_else(|err| {
                        warn!(error = %err, "upgrade snapshot synthesis failed; replying empty");
                        SnapshotBytes {
                            cols: self.cols,
                            rows: self.rows,
                            bytes: Vec::new(),
                            scrollback: Vec::new(),
                        }
                    });
                    let pty = self.pty.as_ref();
                    let master_fd = pty
                        .and_then(|p| p.master.lock().ok().and_then(|m| m.as_raw_fd()));
                    let child_pid = pty
                        .and_then(|p| p.child.process_id())
                        .and_then(|id| i32::try_from(id).ok());
                    let cwd = pty
                        .and_then(|p| p.child.process_id())
                        .and_then(crate::cwd_query::process_cwd)
                        .map(|p| p.to_string_lossy().into_owned())
                        .or_else(|| {
                            let last = self.last_known_cwd.borrow().clone();
                            (!last.is_empty()).then_some(last)
                        });
                    let cell_px = (self.cell_px != (0, 0)).then_some(self.cell_px);
                    let title = (!self.last_title.is_empty()).then(|| self.last_title.clone());
                    let _ = req.reply.send(PaneUpgradeHandle {
                        master_fd,
                        child_pid,
                        cols: self.cols,
                        rows: self.rows,
                        cell_px,
                        title,
                        cwd,
                        vt_replay_bytes: snap.bytes,
                        scrollback_bytes: snap.scrollback,
                    });
                }

                Some(req) = self.pwd_rx.recv() => {
                    // Resolve the pane's live working directory by asking
                    // the kernel for the PTY child's CWD (the shell's
                    // directory *now*, after any `cd`). `None` when there
                    // is no PTY (no-PTY actor), the child has no pid, or
                    // the query is unsupported/denied — the caller then
                    // falls back to a non-inherited default.
                    let cwd = self
                        .pty
                        .as_ref()
                        .and_then(|p| p.child.process_id())
                        .and_then(crate::cwd_query::process_cwd)
                        .map(|p| p.to_string_lossy().into_owned());
                    let _ = req.reply.send(cwd);
                }

                Some(req) = self.resize_rx.recv() => {
                    // A `resync_only` request (from a lagged output pump)
                    // carries no geometry — skip the resize and only schedule
                    // the resync broadcast below.
                    if !req.resync_only {
                        self.handle_resize(req.cols, req.rows, req.cell_px);
                    }
                    // phux-8v1: re-broadcast a full snapshot for live
                    // resizes so client mirrors reconverge after their
                    // independent reflow. Suppressed for the ATTACH-time
                    // resize (the handshake snapshot covers it). Debounced
                    // (RESIZE_RESYNC_DEBOUNCE) so a drag storm — or a burst of
                    // lag-resync requests — coalesces into a single snapshot
                    // rather than flooding the client.
                    if req.resync_clients {
                        resync_pending = true;
                        resync_reason = if req.resync_only {
                            ResyncReason::OutboundGap
                        } else {
                            ResyncReason::Resize
                        };
                        resync_debounce
                            .as_mut()
                            .reset(tokio::time::Instant::now() + RESIZE_RESYNC_DEBOUNCE);
                    }
                }

                // phux-8v1: debounced resize resync — fires once the
                // resize storm settles (RESIZE_RESYNC_DEBOUNCE after the
                // last resync-requesting resize). Guarded by
                // `resync_pending` so the idle far-future timer never
                // fires spuriously.
                () = &mut resync_debounce, if resync_pending => {
                    resync_pending = false;
                    self.broadcast_resync(resync_reason);
                }

                Some(req) = self.consumer_attach_rx.recv() => {
                    let ConsumerAttachRequest {
                        client_id,
                        outbound,
                        wire_terminal_id,
                        stream_id,
                        bootstrap_id,
                        wants_state_sync,
                        loss_tolerant,
                        reply,
                    } = req;
                    // phux-3uv / phux-fseo: map register success to an outcome
                    // that tells the runtime whether this actor is
                    // tick-managing the consumer. Tick-managed ⇒ the runtime
                    // suppresses its broadcast pump for this pane (single
                    // emitter). A consumer is tick-managed if it negotiated
                    // `OutputMode::StateSync` (`wants_state_sync`), OR the
                    // global test gate forces every consumer onto the tick.
                    let tick_managed = self.consumer_tick_emits || wants_state_sync;
                    let result = self
                        .register_consumer_generation(
                            client_id,
                            outbound,
                            wire_terminal_id,
                            stream_id,
                            bootstrap_id,
                            wants_state_sync,
                        )
                        .map(|()| ConsumerAttachOutcome { tick_managed });
                    // phux-v45.8: a forwarded/lossy-leg state-sync consumer
                    // opts into the advance-on-ack loss-tolerant model right
                    // after a successful registration. No-op for a direct
                    // reliable-transport consumer (`loss_tolerant == false`).
                    if result.is_ok() && loss_tolerant && tick_managed {
                        self.enable_loss_tolerance(client_id);
                    }
                    if let Err(err) = &result {
                        warn!(
                            ?client_id,
                            wire_terminal_id,
                            error = %err,
                            "consumer attach: per-consumer synthesizer setup failed",
                        );
                    } else {
                        trace!(
                            ?client_id,
                            wire_terminal_id,
                            tick_managed,
                            "consumer attached: per-consumer synthesizer primed"
                        );
                    }
                    let _ = reply.send(result);
                }

                Some(req) = self.consumer_detach_rx.recv() => {
                    let ConsumerDetachRequest { client_id, reply } = req;
                    self.unregister_consumer(client_id);
                    trace!(?client_id, "consumer detached: per-consumer RenderState freed");
                    // phux-q0e.5: losing a consumer can raise the minimum
                    // desired interval (e.g. the fastest peer left), so
                    // re-evaluate the shared cadence.
                    Self::rearm_tick(&mut tick, &mut tick_interval, self.adaptive_tick_interval());
                    let _ = reply.send(());
                }

                // ADR-0018 / phux-q0e.4: inbound FRAME_ACK. Clears the
                // per-consumer dirty cache so the next tick re-diffs
                // against the just-acked reference. Loss tolerance: a
                // dropped ack just means the next tick re-emits a larger
                // diff against the same older reference — no
                // retransmit machinery here.
                Some(req) = self.consumer_ack_rx.recv() => {
                    let ConsumerAckRequest {
                        client_id,
                        stream_id,
                        bootstrap_id,
                        seq,
                    } = req;
                    // phux-q0e.5: a fresh RTT sample may shift the adaptive
                    // cadence. Rebuild the shared tick only when the new
                    // minimum-desired interval moves beyond the deadband, so
                    // a steady RTT does not churn the scheduler.
                    if self.on_generation_frame_ack(client_id, stream_id, bootstrap_id, seq) {
                        Self::rearm_tick(&mut tick, &mut tick_interval, self.adaptive_tick_interval());
                    }
                }

                // Semantic event subscription request. Register the subscriber
                // and begin broadcasting matching events to their outbound mailbox.
                Some(req) = self.subscribe_to_events_rx.recv() => {
                    self.subscribe_to_events(req);
                }

                // Semantic event unsubscription request. Remove the subscriber
                // from the broadcast list. Silent no-op if already unsubscribed.
                Some(req) = self.unsubscribe_from_events_rx.recv() => {
                    self.unsubscribe_from_events(&req);
                }

                // Supervisory control (ADR-0033): lease-change broadcasts and
                // process signals. The lease itself lives in `ServerState`; the
                // actor is the emitter (it owns the subscriber list + lifecycle)
                // and the signal deliverer (it owns the PTY child pid).
                Some(req) = self.control_rx.recv() => {
                    self.handle_control_request(req);
                }

                // State-sync tick driver (phux-q0e.3, phux-ia4, ADR-0018).
                // Iterates each attached consumer, diffs the live terminal
                // against that consumer's own reference grid, and pushes a
                // `TerminalOutput` frame onto its outbound mailbox whenever
                // `synthesize_against_reference` returns non-empty bytes.
                _ = tick.tick() => {
                    // phux-y2t: close an output burst with an `idle` event
                    // when no PTY output arrived since the previous tick.
                    // This bookkeeping is independent of the state-sync
                    // emitter gate, so headless watchers settle raw panes too.
                    self.maybe_emit_idle();
                    self.tick_emit();
                    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                    self.expire_native_cursors();
                }

                // Agent-state detector (ADR-0046). This interval is the SOLE
                // driver: PTY bytes deliberately do NOT wake it. A chatty
                // agent spewing megabytes must cost zero extra detector work
                // — the whole design is a periodic re-derivation, not a
                // reaction to output. The cadence is adaptive (500 ms while
                // unidentified, 300 ms once identified, 100 ms while
                // confirming a working -> idle transition) and is re-armed
                // through the existing `rearm_tick`, whose deadband keeps a
                // steady cadence from churning the scheduler.
                _ = detect_tick.tick(), if self.agent_detect.is_some() => {
                    if let Some(next) = self.detect_tick() {
                        Self::rearm_tick(&mut detect_tick, &mut detect_interval, next);
                    }
                }

                else => break,
            }
        }
    }

    /// One tick of the state-sync emission driver (phux-q0e.3, phux-ia4).
    ///
    /// Walks every attached consumer in turn. For each:
    ///
    /// 1. Call [`SnapshotSynthesizer::synthesize_against_reference`] using
    ///    the actor's shared synthesizer and the consumer's *own*
    ///    reference grid. The reference is per-consumer and independent of
    ///    the shared `Terminal` dirty bits, so every consumer on a shared
    ///    pane gets its own correct diff this tick — even though
    ///    libghostty's `RenderState::update` consumes the shared dirty
    ///    state on the first read (the phux-ia4 fix). Synthesis errors are
    ///    logged and that consumer is skipped for this tick (no kill: a
    ///    transient FFI error on one consumer must not poison the others).
    /// 2. If the body is empty, skip — the viewport is byte-identical to
    ///    that consumer's reference (steady state between writes).
    /// 3. Stamp the per-consumer monotonic `seq` (starting at `1`,
    ///    incrementing per emission) and ship a `TerminalOutput` frame
    ///    via the per-consumer outbound mailbox.
    ///
    /// Emit-once (phux-ia4): `synthesize_against_reference` advances the
    /// consumer's reference before returning a non-empty body, so a given
    /// change is emitted exactly once and an unchanged terminal produces no
    /// re-emission on the next tick. This is the v0.1 reliable-transport
    /// model (proto.md §8); the loss-tolerance re-diff property is a future
    /// lossy-transport concern (ADR-0018) and is not wired here.
    #[allow(
        clippy::too_many_lines,
        reason = "single cohesive per-tick emission: the length is inline \
                  safety rationale (permit reservation, emit-once, \
                  backpressure) that splitting would scatter and endanger"
    )]
    fn tick_emit(&mut self) {
        // Per-tick observation span (hot path, so debug level: the default
        // `phux=info` filter leaves it disabled and effectively free —
        // `tracing` skips a disabled span without evaluating its fields).
        // The correlation fields a trace reader greps for to localize
        // server-side lag: how many consumers this tick must serve and
        // whether the grid is dirty. `consumer_count` is read before the
        // gate so the span is consistent on the gated-off / idle-skip
        // return paths too; `emitted` + `total_out_bytes` are recorded at
        // the end of a productive tick.
        let tick_span = tracing::debug_span!(
            "tick_emit",
            consumer_count = self.consumer_states.len(),
            dirty = self.terminal_dirty_since_tick,
            // Filled in at the end of a productive tick via `record`; declared
            // `Empty` so they exist on the span for later assignment.
            emitted = tracing::field::Empty,
            total_out_bytes = tracing::field::Empty,
        )
        .entered();

        // Emission gate (phux-0q8 / phux-3uv / phux-ia4 / phux-fseo). The tick
        // emits only for a *tick-managed* consumer — one that negotiated
        // `OutputMode::StateSync` (`state.wants_state_sync`), or any consumer
        // when the global test gate forces it; the runtime suppresses its
        // broadcast pump for exactly those (see `ConsumerAttachOutcome`). A
        // raw consumer is served by the pump, so the tick stays silent for it
        // to avoid double-painting. `force_all_consumers` is captured here so
        // the loop below reads it without re-borrowing `self` while it holds
        // `&mut self.consumer_states`.
        let force_all_consumers = self.consumer_tick_emits;
        if !force_all_consumers && !self.consumer_states.values().any(|s| s.wants_state_sync) {
            // No tick-managed consumer: nothing to emit (dirty flag untouched).
            return;
        }

        // Idle short-circuit (phux-4l0). The per-consumer reference diff
        // walks + renders every viewport row into a throwaway `Vec<u8>`
        // for every consumer, every tick — pure waste when nothing has
        // changed. Take and reset the "mutated since last tick" flag here;
        // if the terminal is unchanged AND no consumer is awaiting its
        // first emission, skip the entire per-consumer loop.
        //
        // Correctness: a `Clean` terminal cannot have diverged from any
        // consumer's last-emitted reference (the reference advanced to the
        // terminal state on the prior emit, and nothing has mutated the
        // terminal since), so skipping is sound. Two carve-outs suppress the
        // short-circuit even on a `Clean` terminal:
        //
        // - `needs_initial_emit` preserves the phux-ia4 multi-consumer
        //   guarantee: a consumer registered *after* the last write sits on a
        //   clean terminal yet has never had a synthesis pass, so it must be
        //   walked once even though the global flag is clear.
        // - `behind` preserves the backpressure retry: a consumer skipped on
        //   a prior tick because its mailbox was full has a reference behind
        //   the live grid. The grid can stay `Clean` indefinitely, so without
        //   this the held-back delta would never be retried once the client
        //   drains (the wave-hunt/server-lifecycle backpressure leak).
        let mutated = self.terminal_dirty_since_tick;
        self.terminal_dirty_since_tick = false;
        if !mutated
            && !self.consumer_states.values().any(|s| {
                // phux-v45.8: a loss-tolerant consumer with un-acked frames in
                // flight must keep being walked even on a Clean terminal, so its
                // retransmit timer can fire and re-diff a suspected-lost frame
                // against the acked reference. `pending_refs` is empty for the
                // reliable emit-once path, so this adds nothing there.
                s.needs_initial_emit || s.behind || !s.pending_refs.is_empty()
            })
        {
            return;
        }

        // Borrow the terminal + shared synthesizer once per tick. The
        // synthesizer's `RenderState`/iterators are reused across
        // consumers; the per-consumer state lives in each `reference`.
        let terminal = self.terminal.borrow();
        let mut synth = self.synth.borrow_mut();
        // phux-ahk.2: render the grid ONCE for this tick (the consumer-
        // independent snapshot + per-row cell render + cursor/mode FFI +
        // epilogue/screen-toggle precompute). Each consumer below then only
        // DIFFS against the shared result via `diff_consumer`, so a pane with
        // N state-sync consumers renders once, not N times.
        let (tick_cols, tick_rows, tick_live_cm) = match synth.prepare_tick(&terminal) {
            Ok(t) => t,
            Err(err) => {
                warn!(error = %err, "state-sync tick: prepare_tick failed; skipping tick");
                return;
            }
        };
        // Consumers whose outbound mailbox is `Closed` (receiver dropped)
        // are reaped after the loop so a missed detach (phux-ddg) does not
        // leave a dead `ConsumerReference` to be re-rendered forever.
        let mut closed: Vec<ClientId> = Vec::new();
        // Per-tick emission tally recorded onto the tick span on the way out
        // (frames actually shipped + their total byte volume) — the headline
        // "frame N for terminal T was Y bytes" reconstruction signal.
        let mut emitted: u64 = 0;
        let mut total_out_bytes: usize = 0;
        for (client_id, state) in &mut self.consumer_states {
            // phux-fseo: serve only tick-managed consumers. A raw consumer
            // sharing this pane is served by the broadcast pump; emitting here
            // too would double-paint it, so skip it (reference left untouched
            // for a later mode flip).
            if !force_all_consumers && !state.wants_state_sync {
                continue;
            }
            // Captured before the `behind` reset below: whether a prior tick
            // held this consumer's delta back for a full mailbox. The
            // loss-tolerant emit gate treats a drained-after-backpressure
            // consumer as "has new content to ship" (phux-v45.8).
            let was_behind = state.behind;
            // This consumer is being serviced this tick; it no longer needs
            // a forced first pass.
            state.needs_initial_emit = false;
            // Reserve an outbound permit BEFORE synthesizing
            // (phux-wave-hunt/server-lifecycle). `synthesize_against_reference`
            // commits the per-consumer reference to the just-rendered grid
            // *before* it returns the bytes (emit-once, grid.rs), so once we
            // synthesize the delta is the only copy and the reference has
            // moved past it. If the send then failed `Full` we would drop the
            // delta and never re-emit it (the next tick diffs against the
            // already-advanced reference), silently losing content and
            // diverging the client mirror forever.
            //
            // Reserving first inverts the ordering: a `Full` mailbox means we
            // skip this consumer entirely this tick WITHOUT synthesizing, so
            // the reference (and `next_seq`) stay put and the delta is
            // re-diffed intact on the next tick once the client drains. A
            // `Closed` mailbox reaps the entry (phux-ddg self-heal). Only when
            // we hold a permit — which guarantees the subsequent send cannot
            // fail — do we synthesize, advance the reference, and ship.
            let permit = match state.outbound.try_reserve() {
                Ok(permit) => permit,
                Err(tokio::sync::mpsc::error::TrySendError::Full(())) => {
                    // Backpressure: the consumer mailbox is wedged. Skip
                    // without advancing the reference so no content is lost;
                    // the next tick retries the same delta. Mark `behind` so
                    // the idle short-circuit keeps walking this consumer even
                    // if the grid goes `Clean` before the client drains — the
                    // retry must not depend on a fresh write. At debug so a
                    // stall is visible at the recommended `phux=debug` level.
                    state.behind = true;
                    debug!(
                        ?client_id,
                        wire_terminal_id = state.wire_terminal_id,
                        "state-sync tick: consumer mailbox full; skipping (reference held, retries next tick)",
                    );
                    continue;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(())) => {
                    // The receiver is gone. A `ConsumerDetachRequest` may have
                    // been dropped (best-effort `try_send` on a full detach
                    // mailbox, runtime.rs) so `unregister_consumer` never ran.
                    // Self-heal: reap the entry now so we stop re-rendering a
                    // dead consumer every tick (phux-ddg).
                    debug!(
                        ?client_id,
                        wire_terminal_id = state.wire_terminal_id,
                        "state-sync tick: consumer mailbox closed; reaping entry",
                    );
                    closed.push(*client_id);
                    continue;
                }
            };
            // We hold a permit: the mailbox has room, so this consumer is
            // about to be fully serviced this tick (a delta ships, or the
            // diff is empty and the reference is already at the live grid).
            // Either way it is no longer behind.
            state.behind = false;
            // Per-consumer synthesis span (debug; the per-tick CPU sink —
            // its duration is the key server-side lag signal). Carries the
            // consumer correlation fields; the diff size lands in
            // `synthesize_against_reference`'s own child span.
            let _synth_span = tracing::debug_span!(
                "synthesize",
                ?client_id,
                wire_terminal_id = state.wire_terminal_id,
            )
            .entered();
            // diff_consumer is infallible (the fallible render happened once in
            // `prepare_tick` above); it returns this consumer's delta bytes.
            // phux-v45.8: a loss-tolerant consumer re-diffs against its
            // last-ACKED reference (which does NOT advance on emit), so a
            // dropped/un-acked frame self-heals — its rows still differ from the
            // acked reference on the next emission. The reliable-transport
            // default stays on the emit-once `diff_consumer` path (reference
            // advances on emit), byte-for-byte unchanged.
            let bytes = if state.loss_tolerant {
                synth.diff_against_base(tick_cols, tick_rows, tick_live_cm, &state.acked_reference)
            } else {
                synth
                    .diff_consumer(tick_cols, tick_rows, tick_live_cm, &mut state.reference)
                    .bytes
            };
            if bytes.is_empty() {
                // Byte-identical to this consumer's reference; nothing to
                // send this tick. The reserved permit drops unused. A closed
                // mailbox was already reaped by the `try_reserve` arm above,
                // so no extra liveness probe is needed here.
                continue;
            }
            // phux-v45.8 loss-tolerant emit gate. Because a loss-tolerant diff
            // is against the last-acked (not last-emitted) reference, it stays
            // non-empty every tick while a frame is un-acked. Only actually
            // (re)ship when there is genuinely new content this tick
            // (`mutated`), a drained-after-backpressure delta to flush
            // (`was_behind`), or a retransmit is due for a still-un-acked frame
            // (suspected loss). Otherwise hold: re-shipping the same cumulative
            // delta every tick would flood the leg. The reserved permit drops.
            if state.loss_tolerant {
                let now = tokio::time::Instant::now();
                let retransmit_due = !state.pending_refs.is_empty()
                    && state.emit_instants.values().next_back().is_none_or(|last| {
                        now.saturating_duration_since(*last)
                            >= tick::retransmit_timeout(state.rtt.smoothed())
                    });
                if !mutated && !was_behind && !retransmit_due {
                    continue;
                }
            }
            let seq = state.next_seq;
            let out_bytes = bytes.len();
            // Wrapping_add for paranoia; `u64` will not realistically
            // roll over at 33 Hz, but the existing `runtime.rs` pump
            // uses the same idiom and we match it.
            state.next_seq = state.next_seq.wrapping_add(1);
            let frame = FrameKind::TerminalOutput {
                terminal_id: phux_protocol::ids::TerminalId::local(state.wire_terminal_id),
                stream_id: state.stream_id,
                bootstrap_id: state.bootstrap_id,
                seq,
                bytes: bytes.into(),
            };
            // Infallible: we hold a reserved permit, so this cannot block,
            // drop, or fail. This preserves the actor's single-poll-budget
            // invariant (the tick arm never yields the loop) while keeping
            // emit-once consistent — a synthesized delta always ships.
            permit.send(Outbound::Frame(frame));
            // Stamp the emit instant for this seq so the matching FRAME_ACK
            // can be turned into an RTT sample (phux-q0e.5). Recorded only
            // for shipped frames — empty/skipped ticks have no round-trip to
            // measure. Pruned on ack, so the map stays as small as the
            // in-flight window.
            state.emit_instants.insert(seq, tokio::time::Instant::now());
            // Defensive bound: ack-pruning keeps this map tiny for a
            // well-behaved consumer, but one that opts into state sync and
            // never sends FRAME_ACK (or a transport that drops acks) would
            // otherwise grow it one entry per emitted tick without bound
            // (~50/s at the 20ms floor cadence). Evict the oldest (lowest-seq)
            // samples past the cap; an unacked sample this stale is already
            // useless for RTT, so dropping it costs nothing and bounds the
            // map to a few KB per consumer. See `MAX_EMIT_INSTANTS`.
            while state.emit_instants.len() > MAX_EMIT_INSTANTS {
                state.emit_instants.pop_first();
            }
            // phux-v45.8: for a loss-tolerant consumer, snapshot the grid state
            // this `seq` shipped so a later cumulative `FRAME_ACK` can advance
            // the acked reference to exactly it. Bounded the same way as
            // `emit_instants` (oldest-evicted past the cap) so a wedged leg
            // cannot grow it without bound. Empty/untouched on the emit-once
            // path.
            if state.loss_tolerant {
                let snapshot = synth.snapshot_tick_reference(tick_cols, tick_rows, tick_live_cm);
                state.pending_refs.insert(seq, snapshot);
                while state.pending_refs.len() > MAX_EMIT_INSTANTS {
                    state.pending_refs.pop_first();
                }
            }
            emitted += 1;
            total_out_bytes += out_bytes;
            trace!(
                ?client_id,
                wire_terminal_id = state.wire_terminal_id,
                seq,
                out_bytes,
                "state-sync tick: TERMINAL_OUTPUT emitted",
            );
        }
        drop(synth);
        drop(terminal);
        // Record the per-tick emission tally on the tick span so a reader
        // can reconstruct "tick served N consumers, shipped M frames /
        // B bytes" without re-deriving it from the per-consumer trace lines.
        tick_span.record("emitted", emitted);
        tick_span.record("total_out_bytes", total_out_bytes);
        for client_id in closed {
            self.consumer_states.remove(&client_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ceiling for "this task has already been told to stop, so joining it
    /// should return immediately" waits.
    ///
    /// The number is deliberately NOT load-bearing. Nothing here measures how
    /// fast an actor shuts down — the assertion is that it shuts down at all,
    /// and every one of these joins resolves in single-digit milliseconds on
    /// an idle box. What the timeout buys is a bounded failure for a genuine
    /// hang instead of a wedged test binary.
    ///
    /// It used to be 500ms, which made it a latent flake: these tests share a
    /// machine with the rest of the suite, and a current-thread runtime that
    /// loses its core for half a second turns "the actor exited" into "the
    /// actor did not exit within 500ms". Two of them (phux-br1f:
    /// `rapid_resizes_coalesce_into_one_resync_snapshot` and
    /// `parent_token_cancel_cascades_to_pane_actor`) failed exactly that way
    /// on a saturated laptop and passed in ~0.1s when re-run alone. A
    /// suite that cries wolf gets ignored, which is how a real regression
    /// ships.
    ///
    /// Raising it does not weaken anything: a real hang still fails, just 30s
    /// later, and the panic message names the actor that would not exit.
    const ACTOR_EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

    /// How long one `recv` in the drain loops below waits before the loop
    /// re-checks its predicate (and, where relevant, re-pokes the PTY).
    ///
    /// A granularity knob, not a bound: the loops that use it run until
    /// `ACTOR_EXIT_DEADLINE`. They used to run for a fixed ITERATION count
    /// instead, which is a wall-clock deadline wearing a disguise — `0..32`
    /// ticks of 100ms is a 3.2s budget that a starved runtime blows through
    /// without the actor being at fault, and it also caps how many frames the
    /// loop may consume, so a chatty actor could exhaust it before the frame
    /// under test even arrived.
    const DRAIN_POLL_TICK: std::time::Duration = std::time::Duration::from_millis(100);

    #[test]
    fn seeded_default_colors_are_installed_before_actor_run() {
        use phux_protocol::caps::{TerminalColor, TerminalDefaultColors};

        let colors = TerminalDefaultColors {
            foreground: TerminalColor {
                r: 208,
                g: 208,
                b: 208,
            },
            background: TerminalColor {
                r: 18,
                g: 24,
                b: 27,
            },
        };
        let bundle = TerminalActor::build_with_token_and_colors(
            80,
            24,
            None,
            100,
            CancellationToken::new(),
            Some(colors),
        )
        .expect("actor");
        let mut actor = bundle.actor;
        {
            let terminal = actor.terminal.borrow();
            assert_eq!(
                terminal.default_fg_color().expect("foreground"),
                Some(libghostty_vt::style::RgbColor {
                    r: 208,
                    g: 208,
                    b: 208,
                })
            );
            assert_eq!(
                terminal.default_bg_color().expect("background"),
                Some(libghostty_vt::style::RgbColor {
                    r: 18,
                    g: 24,
                    b: 27,
                })
            );
        }

        let (_pty_output, mut pty_input) = actor.install_test_pty_channels();
        let first = b"\x1b]10;?\x1b";
        let second = b"\\\x1b]11;?\x1b\\";
        actor.terminal.borrow_mut().vt_write(first);
        actor.answer_color_queries(first);
        actor.terminal.borrow_mut().vt_write(second);
        actor.answer_color_queries(second);
        assert_eq!(
            pty_input.try_recv().expect("OSC 10 reply").bytes,
            b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\"
        );
        assert_eq!(
            pty_input.try_recv().expect("OSC 11 reply").bytes,
            b"\x1b]11;rgb:1212/1818/1b1b\x1b\\"
        );
    }

    /// phux-07y: `shell_command` runs the user's command via
    /// `<resolved shell> -c <command>` so quoting / args work and the
    /// pane closes when the command exits. phux-i0e8.4.1: the shell is
    /// the resolved default (`defaults.shell` → `$SHELL` → `/bin/sh`),
    /// passed in by the caller.
    #[test]
    fn shell_command_wraps_in_shell_dash_c() {
        let cmd = shell_command("/opt/fancy/fish", "btop --utf-force");
        let argv = cmd.get_argv();
        assert_eq!(argv.len(), 3, "expected [shell, -c, command]");
        assert_eq!(argv[0], "/opt/fancy/fish");
        assert_eq!(argv[1], "-c");
        assert_eq!(argv[2], "btop --utf-force");
    }

    /// phux-2sl6: a non-`phux-ask` title is not an ask marker.
    #[test]
    fn ask_marker_rejects_non_ask_titles() {
        assert_eq!(AskMarker::parse(""), None);
        assert_eq!(AskMarker::parse("vim README.md"), None);
        // Bare prefix with no `:` carries no question — not a marker.
        assert_eq!(AskMarker::parse("phux-ask"), None);
        assert_eq!(AskMarker::parse("phux-ask[q1]"), None);
    }

    /// phux-2sl6: the minimal `phux-ask:<question>` form yields an empty id,
    /// the question, and no suggestions.
    #[test]
    fn ask_marker_parses_bare_question() {
        let marker = AskMarker::parse("phux-ask:Proceed?").expect("a phux-ask marker");
        assert_eq!(marker.id, "");
        assert_eq!(marker.question, "Proceed?");
        assert!(marker.suggestions.is_empty());
    }

    /// phux-2sl6: the full `phux-ask[<id>]:<question>?s=a|b|c` form yields
    /// the id, the question, and the `|`-delimited suggestions in order.
    #[test]
    fn ask_marker_parses_id_and_suggestions() {
        let marker = AskMarker::parse("phux-ask[q1]:Deploy to prod??s=Yes|No|Hold")
            .expect("a phux-ask marker");
        assert_eq!(marker.id, "q1");
        assert_eq!(marker.question, "Deploy to prod?");
        assert_eq!(
            marker.suggestions,
            vec!["Yes".to_owned(), "No".to_owned(), "Hold".to_owned()],
        );
    }

    /// phux-2sl6: an empty `?s=` suffix (or empty options) yields no
    /// suggestions, never a vector with empty strings.
    #[test]
    fn ask_marker_drops_empty_suggestions() {
        let marker = AskMarker::parse("phux-ask:Ready??s=").expect("a phux-ask marker");
        assert_eq!(marker.question, "Ready?");
        assert!(marker.suggestions.is_empty());

        let marker = AskMarker::parse("phux-ask:Pick??s=a||b").expect("a phux-ask marker");
        assert_eq!(
            marker.suggestions,
            vec!["a".to_owned(), "b".to_owned()],
            "empty inter-pipe segments are dropped",
        );
    }

    /// Direct synchronous test: snapshot-of-blank-Terminal yields the
    /// expected reset preamble. Doesn't spawn the actor; exercises the
    /// synthesis helper directly.
    #[test]
    fn synthesize_blank_pane_returns_reset_preamble() {
        let bundle = TerminalActor::new(80, 24).expect("new");
        let snap = bundle.actor.synthesize().expect("synthesize");
        assert_eq!(snap.cols, 80);
        assert_eq!(snap.rows, 24);
        assert!(snap.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));
    }

    /// Synchronous test: seed bytes flow through to the synthesized
    /// snapshot. Exercises [`TerminalActor::new_with_seed`].
    #[test]
    fn synthesize_seeded_pane_carries_visible_text() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let snap = bundle.actor.synthesize().expect("synthesize");
        let body = String::from_utf8_lossy(&snap.bytes);
        assert!(
            body.contains("hello"),
            "synthesized bytes should contain seeded text, got: {body:?}"
        );
    }

    /// Async test: the actor responds to `SnapshotRequest` over the
    /// `LocalSet` and ships back the same bytes the synchronous
    /// synthesizer would.
    #[tokio::test(flavor = "current_thread")]
    async fn actor_responds_to_snapshot_request_on_localset() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle =
                    TerminalActor::new_with_seed(20, 5, b"hi there").expect("new_with_seed");
                let handle = bundle.handle.clone();
                // Hold the token; under new semantics dropping it does
                // NOT cancel, so the actor is alive regardless. Keep
                // the binding for parallel structure with the other
                // tests in this module.
                let _token = bundle.token;
                tokio::task::spawn_local(bundle.actor.run());

                let (reply_tx, reply_rx) = oneshot::channel();
                handle
                    .snapshot
                    .send(SnapshotRequest {
                        scrollback: None,
                        reply: reply_tx,
                    })
                    .await
                    .expect("send snapshot request");
                let snap = reply_rx.await.expect("snapshot reply");
                assert_eq!(snap.cols, 20);
                assert_eq!(snap.rows, 5);
                let body = String::from_utf8_lossy(&snap.bytes);
                assert!(
                    body.contains("hi there"),
                    "actor-synthesized bytes should contain seeded text"
                );
            })
            .await;
    }

    /// A no-PTY actor answers `UpgradeHandleRequest` with the replay snapshot
    /// and dims but no descriptors — there is no child to hand off.
    #[tokio::test(flavor = "current_thread")]
    async fn upgrade_handle_no_pty_has_snapshot_but_no_descriptors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new_with_seed(20, 5, b"seeded").expect("new_with_seed");
                let handle = bundle.handle.clone();
                let _token = bundle.token;
                tokio::task::spawn_local(bundle.actor.run());

                let (reply_tx, reply_rx) = oneshot::channel();
                handle
                    .upgrade
                    .send(UpgradeHandleRequest { reply: reply_tx })
                    .await
                    .expect("send upgrade request");
                let h = reply_rx.await.expect("upgrade reply");
                assert_eq!(h.master_fd, None);
                assert_eq!(h.child_pid, None);
                assert_eq!((h.cols, h.rows), (20, 5));
                assert!(
                    String::from_utf8_lossy(&h.vt_replay_bytes).contains("seeded"),
                    "replay snapshot should carry the seeded text"
                );
            })
            .await;
    }

    /// A PTY-backed actor answers `UpgradeHandleRequest` with the live master
    /// fd and child PID — the descriptors the re-exec'd image re-adopts.
    #[tokio::test(flavor = "current_thread")]
    async fn upgrade_handle_with_pty_exposes_fd_and_pid() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut cmd = portable_pty::CommandBuilder::new("sleep");
                cmd.arg("30");
                let bundle =
                    TerminalActor::new_with_command(cmd, 80, 24).expect("new_with_command");
                let handle = bundle.handle.clone();
                let token = bundle.token.clone();
                tokio::task::spawn_local(bundle.actor.run());

                let (reply_tx, reply_rx) = oneshot::channel();
                handle
                    .upgrade
                    .send(UpgradeHandleRequest { reply: reply_tx })
                    .await
                    .expect("send upgrade request");
                let h = reply_rx.await.expect("upgrade reply");
                assert!(h.master_fd.is_some(), "PTY actor should expose a master fd");
                assert!(h.child_pid.is_some(), "PTY actor should expose a child pid");

                // Cancel so the actor reaps the `sleep` child instead of
                // leaking it past the test.
                token.cancel();
            })
            .await;
    }

    /// `new_with_adopted_pty` rebuilds a working actor around an inherited PTY:
    /// it replays the seed snapshot into the fresh grid, exposes the adopted
    /// child, and surfaces the live child's output — proving the resume path's
    /// actor construction end to end.
    #[tokio::test(flavor = "current_thread")]
    async fn adopted_actor_replays_seed_and_serves_live_pty() {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::io::Write;
        use std::os::fd::{BorrowedFd, IntoRawFd};

        #[allow(
            clippy::future_not_send,
            reason = "current-thread test helper; the actor's TerminalHandle is intentionally !Sync"
        )]
        async fn snapshot(handle: &TerminalHandle) -> String {
            let (reply, rx) = oneshot::channel();
            handle
                .snapshot
                .send(SnapshotRequest {
                    scrollback: None,
                    reply,
                })
                .await
                .expect("send snapshot");
            String::from_utf8_lossy(&rx.await.expect("snapshot reply").bytes).into_owned()
        }

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // A real PTY with a `cat` child that echoes input.
                let sys = native_pty_system();
                let pair = sys
                    .openpty(PtySize {
                        rows: 5,
                        cols: 20,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .expect("openpty");
                let child = pair
                    .slave
                    .spawn_command(CommandBuilder::new("cat"))
                    .expect("spawn cat");
                drop(pair.slave);
                let pid = i32::try_from(child.process_id().expect("pid")).expect("pid fits i32");
                let master_fd = pair.master.as_raw_fd().expect("master fd");
                // An owned duplicate of the master for the actor to adopt; the
                // test keeps `pair.master` to write into the PTY. `std`-only
                // (no `libc`, which is macOS-gated in this crate).
                // SAFETY: `master_fd` is open and outlives this borrow.
                let dup_fd = unsafe { BorrowedFd::borrow_raw(master_fd) }
                    .try_clone_to_owned()
                    .expect("dup master")
                    .into_raw_fd();
                let mut writer = pair.master.take_writer().expect("take writer");
                drop(child); // the adopted actor becomes the sole reaper.

                let bundle = TerminalActor::new_with_adopted_pty(
                    dup_fd,
                    pid,
                    20,
                    5,
                    1000,
                    CancellationToken::new(),
                    b"resumed",
                )
                .expect("new_with_adopted_pty");
                let handle = bundle.handle.clone();
                let token = bundle.token.clone();
                tokio::task::spawn_local(bundle.actor.run());

                // Seed replayed synchronously into the rebuilt grid.
                assert!(
                    snapshot(&handle).await.contains("resumed"),
                    "adopted actor should replay the seed snapshot"
                );

                // The adopted child is live and wired into the actor.
                let (reply, rx) = oneshot::channel();
                handle
                    .upgrade
                    .send(UpgradeHandleRequest { reply })
                    .await
                    .expect("send upgrade");
                let h = rx.await.expect("upgrade reply");
                assert_eq!(h.child_pid, Some(pid));
                assert!(h.master_fd.is_some());

                // Live byte flow: write into the PTY; `cat` echoes; the adopted
                // actor's grid surfaces it.
                writer.write_all(b"ping\n").expect("write to pty");
                writer.flush().expect("flush");
                let mut saw = false;
                for _ in 0..40 {
                    if snapshot(&handle).await.contains("ping") {
                        saw = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                assert!(saw, "adopted actor should surface the child's echo");

                token.cancel();
            })
            .await;
    }

    /// ADR-0033 end-to-end: a `Freeze` (SIGSTOP) flips the pane to `Frozen`
    /// and broadcasts a `TerminalControl` event; `Resume` (SIGCONT) returns it
    /// to `Running`; `Kill` (SIGKILL) actually terminates the child (its EOF
    /// fires the exit notification). Exercises the actor's signal deliverer
    /// (`killpg`) and the control-event broadcast over a real PTY child.
    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::too_many_lines,
        reason = "end-to-end PTY signal test: three signal round-trips plus subscriber setup"
    )]
    async fn signal_freezes_resumes_and_kills_the_child() {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::os::fd::{BorrowedFd, IntoRawFd};

        #[allow(
            clippy::future_not_send,
            reason = "current-thread test helper; the actor's TerminalHandle is intentionally !Sync"
        )]
        async fn next_control(
            rx: &mut mpsc::Receiver<Outbound>,
        ) -> (ControlAction, TerminalLifecycle) {
            // Scan past any incidental grid events (Dirty/Idle) for the next
            // supervisory TerminalControl broadcast.
            let scan = async {
                loop {
                    let Outbound::Frame(frame) = rx.recv().await.expect("event channel open");
                    if let FrameKind::Event {
                        event:
                            AgentEvent::TerminalControl {
                                action, lifecycle, ..
                            },
                        ..
                    } = frame
                    {
                        return (action, lifecycle);
                    }
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(5), scan)
                .await
                .expect("a TerminalControl event should arrive")
        }

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let sys = native_pty_system();
                let pair = sys
                    .openpty(PtySize {
                        rows: 5,
                        cols: 20,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .expect("openpty");
                let child = pair
                    .slave
                    .spawn_command(CommandBuilder::new("cat"))
                    .expect("spawn cat");
                drop(pair.slave);
                let pid = i32::try_from(child.process_id().expect("pid")).expect("pid fits i32");
                let master_fd = pair.master.as_raw_fd().expect("master fd");
                // SAFETY: `master_fd` is open and outlives this borrow.
                let dup_fd = unsafe { BorrowedFd::borrow_raw(master_fd) }
                    .try_clone_to_owned()
                    .expect("dup master")
                    .into_raw_fd();
                drop(child); // the adopted actor becomes the sole reaper.

                let bundle = TerminalActor::new_with_adopted_pty(
                    dup_fd,
                    pid,
                    20,
                    5,
                    1000,
                    CancellationToken::new(),
                    b"",
                )
                .expect("new_with_adopted_pty");
                let handle = bundle.handle.clone();
                let token = bundle.token.clone();
                let mut exit_rx = bundle.exit_notify.expect("exit notify");
                tokio::task::spawn_local(bundle.actor.run());

                // Subscribe to the agent-event stream so we observe the
                // TerminalControl broadcasts.
                let (evt_tx, mut evt_rx) = mpsc::channel::<Outbound>(64);
                handle
                    .subscribe_to_events
                    .send(SubscribeToEventsRequest {
                        subscriber: TerminalEventSubscriber {
                            outbound: evt_tx,
                            event_types: Vec::new(),
                        },
                        wire_terminal_id: 1,
                    })
                    .await
                    .expect("subscribe");

                let by = phux_protocol::ids::ClientId::new(7);

                // Freeze → Frozen.
                let (reply, ack) = oneshot::channel();
                handle
                    .control
                    .send(ControlRequest::Signal {
                        signal: TerminalSignal::Freeze,
                        input_holder: None,
                        by,
                        reply,
                    })
                    .await
                    .expect("send freeze");
                ack.await.expect("freeze ack").expect("freeze delivered");
                let (action, lifecycle) = next_control(&mut evt_rx).await;
                assert_eq!(action, ControlAction::Frozen);
                assert_eq!(lifecycle, TerminalLifecycle::Frozen);

                // Resume → Running.
                let (reply, ack) = oneshot::channel();
                handle
                    .control
                    .send(ControlRequest::Signal {
                        signal: TerminalSignal::Resume,
                        input_holder: None,
                        by,
                        reply,
                    })
                    .await
                    .expect("send resume");
                ack.await.expect("resume ack").expect("resume delivered");
                let (action, lifecycle) = next_control(&mut evt_rx).await;
                assert_eq!(action, ControlAction::Resumed);
                assert_eq!(lifecycle, TerminalLifecycle::Running);

                // Kill → the child actually dies; its EOF fires the exit notify.
                let (reply, ack) = oneshot::channel();
                handle
                    .control
                    .send(ControlRequest::Signal {
                        signal: TerminalSignal::Kill,
                        input_holder: None,
                        by,
                        reply,
                    })
                    .await
                    .expect("send kill");
                ack.await.expect("kill ack").expect("kill delivered");
                tokio::time::timeout(std::time::Duration::from_secs(5), &mut exit_rx)
                    .await
                    .expect("killed child should exit and notify")
                    .expect("exit notify channel");

                token.cancel();
            })
            .await;
    }

    /// phux-sw1: killing a pane (cancel the actor token → `shutdown_pty`)
    /// must give a foreground job in a process group distinct from the shell a
    /// chance to flush before it dies. This reproduces interactive job-control
    /// topology rather than testing a shell and child that share one group.
    #[tokio::test(flavor = "current_thread")]
    async fn pane_kill_lets_foreground_process_flush_before_death() {
        use portable_pty::CommandBuilder;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let marker = dir.path().join("flushed");
                let armed = dir.path().join("armed");

                // The script announces that its HUP trap is installed. Without
                // that handshake the test races the shell: a distinct process
                // group is established at exec, but `trap` runs afterwards, so
                // a SIGHUP arriving in between is handled by the DEFAULT
                // disposition — the shell dies silently and the marker is
                // never written. That gap is why this test failed under a
                // fully parallel `just test` (phux-2390) with an empty marker
                // in 0.7s: not a timeout, a missing synchronization point.
                let script = dir.path().join("foreground.sh");
                std::fs::write(
                    &script,
                    "trap 'printf flushed > \"$PHUX_TEST_MARKER\"; exit 0' HUP\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     while :; do sleep 30; done\n",
                )
                .expect("write foreground script");

                // Monitor mode makes the script a foreground job in its own
                // process group, matching an interactive shell running Claude.
                let mut cmd = CommandBuilder::new("/bin/sh");
                cmd.arg("-c");
                cmd.arg(format!("set -m; /bin/sh {}", script.display()));
                cmd.env("PHUX_TEST_MARKER", &marker);
                cmd.env("PHUX_TEST_ARMED", &armed);

                let token = CancellationToken::new();
                let bundle = TerminalActor::build_with_token(20, 5, Some(cmd), 1000, token.clone())
                    .expect("build actor");
                let actor = bundle.actor;
                let pty = actor.pty.as_ref().expect("test actor has PTY");
                let shell_group = i32::try_from(pty.child.process_id().expect("shell pid"))
                    .expect("shell pid fits i32");
                let master = std::sync::Arc::clone(&pty.master);
                let run = tokio::task::spawn_local(actor.run());

                let foreground_group =
                    tokio::time::timeout(std::time::Duration::from_secs(2), async {
                        loop {
                            let group = master.lock().expect("master lock").process_group_leader();
                            if let Some(group) = group
                                && group != shell_group
                            {
                                break group;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    })
                    .await
                    .expect("foreground job must acquire a distinct process group");
                assert_ne!(foreground_group, shell_group);

                // Wait for the trap to be armed before signalling. This is
                // the synchronization the test was missing; the process-group
                // check above proves the job exists, not that it can handle a
                // signal yet.
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while !armed.exists() {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("foreground job must install its SIGHUP trap");

                // Kill the pane. The actor's shutdown runs SIGHUP + grace.
                token.cancel();
                tokio::time::timeout(std::time::Duration::from_secs(5), run)
                    .await
                    .expect("actor shutdown timed out")
                    .expect("actor task failed");

                let body = std::fs::read_to_string(&marker).unwrap_or_default();
                assert!(
                    body.contains("flushed"),
                    "foreground process must run its SIGHUP flush handler before \
                     the pane is killed; marker={body:?}",
                );
            })
            .await;
    }

    /// Interactive-latency regression gate: a queued keystroke must
    /// interleave with a large pending PTY-output burst rather than wait
    /// for the entire burst to drain. Pre-queues ~800KB of output (far
    /// exceeding `MAX_PTY_COALESCE_BYTES`) plus one input event, runs the
    /// actor, and asserts the input reaches the PTY writer channel while
    /// the burst is still draining (the cumulative broadcast bytes seen
    /// at that moment are far below the full burst). Fails if input is
    /// serviced only after the entire burst drains (output-first ordering
    /// or an unbounded coalesce that never yields).
    #[tokio::test(flavor = "current_thread")]
    async fn input_interleaves_with_a_large_pty_output_burst() {
        use phux_protocol::input::paste::{PasteEvent, PasteTrust};

        const CHUNK_LEN: usize = 4096;
        const CHUNK_COUNT: usize = 200;
        const BURST_BYTES: usize = CHUNK_LEN * CHUNK_COUNT;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new(80, 24).expect("new");
                let handle = bundle.handle.clone();
                let token = bundle.token.clone();
                let mut actor = bundle.actor;
                let (pty_evt_tx, mut writer_rx) = actor.install_test_pty_channels();

                // Subscribe before spawning so no broadcast frame is
                // missed. This is the deterministic ordering gate: the
                // cumulative output bytes observed at the instant input
                // lands must be far below the full burst.
                let mut out_rx = handle.output.subscribe();

                // Pre-queue a burst far larger than MAX_PTY_COALESCE_BYTES
                // so it spans many capped vt_writes.
                let chunk = vec![b'x'; CHUNK_LEN];
                for _ in 0..CHUNK_COUNT {
                    pty_evt_tx
                        .send(PtyEvent::Bytes(chunk.clone()))
                        .expect("queue burst");
                }
                // Queue ONE input event. With bracketed-paste mode 2004
                // off (a fresh Terminal's default) a Trusted paste of
                // b"x" encodes to exactly b"x" on the writer channel.
                handle
                    .input
                    .send(TerminalInput::Paste(PasteEvent {
                        trust: PasteTrust::Trusted,
                        data: b"x".to_vec(),
                    }))
                    .await
                    .expect("queue input");

                tokio::task::spawn_local(actor.run());

                // The keystroke must be serviced mid-burst: it interleaves
                // rather than waiting for the whole burst to drain. The
                // timeout is only a backstop against a wedged actor — the
                // byte-ordering check below is what actually proves
                // "mid-burst", so the duration carries no meaning and is
                // sized to be unreachable under load (see
                // `ACTOR_EXIT_DEADLINE`).
                let got = tokio::time::timeout(ACTOR_EXIT_DEADLINE, writer_rx.recv())
                    .await
                    .expect("input must be serviced mid-burst, not after it");
                assert_eq!(
                    got.map(|request| request.bytes),
                    Some(b"x".to_vec()),
                    "queued keystroke should reach the PTY writer while the burst drains",
                );

                // Count broadcast bytes the actor has emitted so far.
                // Account Lagged-skipped frames toward the total (the
                // broadcast channel is bounded; a fast burst can lag this
                // receiver) so the ordering gate cannot under-report and
                // pass spuriously.
                let mut emitted: usize = 0;
                loop {
                    match out_rx.try_recv() {
                        Ok(PaneOutput::Live(bytes) | PaneOutput::Resync { bytes, .. }) => {
                            emitted += bytes.len();
                        }
                        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                            // Each lagged frame is one coalesced payload of
                            // at most MAX_PTY_COALESCE_BYTES; bound the
                            // skipped volume by that cap so the assertion
                            // stays conservative (never under-reports).
                            let skipped = usize::try_from(n).unwrap_or(usize::MAX);
                            emitted += skipped.saturating_mul(MAX_PTY_COALESCE_BYTES);
                        }
                        Err(_) => break,
                    }
                }
                token.cancel();
                assert!(
                    emitted < BURST_BYTES,
                    "input must land mid-burst: cumulative output {emitted} should be \
                     below the full burst {BURST_BYTES}",
                );
            })
            .await;
    }

    /// phux-cs6: the actor answers a `PwdRequest` with its PTY child's
    /// live working directory. A shell is spawned that `cd`s into a
    /// freshly-created temp dir and then blocks (`read`), so its CWD is
    /// the temp dir when the kernel query runs. This is the actor-level
    /// proof of the inherit-focused acceptance criterion.
    #[tokio::test(flavor = "current_thread")]
    async fn actor_responds_to_pwd_request_with_pty_child_cwd() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                // Canonicalize: macOS hands back the realpath
                // (/private/var/... for /var/...), which is what the
                // kernel query returns too.
                let dir_path = dir.path().canonicalize().expect("canonicalize tempdir");

                let mut cmd = CommandBuilder::new("/bin/sh");
                cmd.arg("-c");
                cmd.arg(format!("cd '{}' && read _", dir_path.display()));
                let bundle = TerminalActor::build_with_token(
                    20,
                    5,
                    Some(cmd),
                    DEFAULT_MAX_SCROLLBACK,
                    CancellationToken::new(),
                )
                .expect("build_with_token");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let join = tokio::task::spawn_local(bundle.actor.run());

                // Poll the actor until the shell has executed the `cd`.
                // The query races the child's startup, so retry briefly.
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
                let mut got: Option<String> = None;
                while tokio::time::Instant::now() < deadline {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    handle
                        .pwd
                        .send(PwdRequest { reply: reply_tx })
                        .await
                        .expect("send pwd request");
                    got = reply_rx.await.expect("pwd reply");
                    if got.as_deref() == Some(dir_path.to_str().expect("utf8 path")) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                assert_eq!(
                    got.as_deref(),
                    Some(dir_path.to_str().expect("utf8 path")),
                    "actor should report the PTY child's live CWD",
                );

                token.cancel();
                let _ = tokio::time::timeout(ACTOR_EXIT_DEADLINE, join).await;
            })
            .await;
    }

    /// phux-cs6: a no-PTY actor has no child to query, so `pwd` is `None`
    /// and the spawn path falls back to a non-inherited default.
    #[tokio::test(flavor = "current_thread")]
    async fn actor_pwd_request_is_none_without_pty() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new_with_seed(20, 5, b"no pty here").expect("seed");
                let handle = bundle.handle.clone();
                let _token = bundle.token;
                tokio::task::spawn_local(bundle.actor.run());

                let (reply_tx, reply_rx) = oneshot::channel();
                handle
                    .pwd
                    .send(PwdRequest { reply: reply_tx })
                    .await
                    .expect("send pwd request");
                assert_eq!(reply_rx.await.expect("pwd reply"), None);
            })
            .await;
    }

    /// The actor stops promptly when its cancellation token fires,
    /// even if input/snapshot channels stay open.
    #[tokio::test(flavor = "current_thread")]
    async fn actor_exits_on_cancellation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new(20, 5).expect("new");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let join = tokio::task::spawn_local(bundle.actor.run());

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");

                let (reply_tx, reply_rx) = oneshot::channel();
                let _ = handle.snapshot.try_send(SnapshotRequest {
                    scrollback: None,
                    reply: reply_tx,
                });
                drop(reply_rx);
            })
            .await;
    }

    /// A parent token's `.cancel()` propagates to a `child_token()`-
    /// linked `TerminalActor`, which exits within a short deadline. Pins
    /// down the hierarchical cascade introduced by the
    /// `CancellationToken` refactor.
    #[tokio::test(flavor = "current_thread")]
    async fn parent_token_cancel_cascades_to_pane_actor() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let parent = CancellationToken::new();
                let child = parent.child_token();
                let bundle =
                    TerminalActor::build_with_token(20, 5, None, DEFAULT_MAX_SCROLLBACK, child)
                        .expect("build_with_token");
                let join = tokio::task::spawn_local(bundle.actor.run());

                parent.cancel();

                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after its parent token was cancelled")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// Test helper: build a throwaway outbound mailbox + receiver pair
    /// shaped like the production [`crate::state::AttachedClient::tx`].
    /// The receiver is returned so callers can hold it open (otherwise
    /// the actor's `try_send` would see a closed channel).
    fn dummy_outbound() -> (mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
        mpsc::channel(16)
    }

    /// phux-q0e.2: ATTACH allocates a per-consumer `RenderState` and
    /// `register_consumer` stores it keyed by `ClientId`. Two attaches
    /// land two entries; one detach removes only that entry; a second
    /// detach of the same id is a no-op.
    #[test]
    fn register_unregister_consumer_drives_lifecycle_map() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        assert_eq!(actor.consumer_count(), 0, "starts empty");

        let a = ClientId(1);
        let b = ClientId(2);
        let (tx_a, _rx_a) = dummy_outbound();
        let (tx_b, _rx_b) = dummy_outbound();
        actor
            .register_consumer(a, tx_a, 1, false)
            .expect("register a");
        assert_eq!(actor.consumer_count(), 1);
        actor
            .register_consumer(b, tx_b, 2, false)
            .expect("register b");
        assert_eq!(actor.consumer_count(), 2);

        actor.unregister_consumer(a);
        assert_eq!(actor.consumer_count(), 1, "one entry after first detach");
        assert!(actor.consumer_state(a).is_none(), "a removed");
        assert!(actor.consumer_state(b).is_some(), "b retained");

        // Idempotent detach: re-detaching `a` is a no-op.
        actor.unregister_consumer(a);
        assert_eq!(actor.consumer_count(), 1);

        actor.unregister_consumer(b);
        assert_eq!(actor.consumer_count(), 0, "both removed");
    }

    /// phux-q0e.2: right after `register_consumer` returns, the
    /// per-consumer state has `last_acked_seq == 0` (no `FRAME_ACK`s yet
    /// — wired by phux-q0e.4) and the cursor/mode capture matches the
    /// live terminal. The dirty-bit reset is a best-effort FFI call
    /// (phux-l0t notes the libghostty surface is unreliable on
    /// repeated updates); we assert the observable contract — the
    /// `ConsumerSyncState` is in place and primed against the live
    /// terminal — rather than the post-reset dirty value itself, which
    /// the tick driver (phux-q0e.3) will re-read on its first tick.
    #[test]
    fn register_consumer_initial_state_matches_terminal() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let mut actor = bundle.actor;
        let client = ClientId(7);
        let (tx, _rx) = dummy_outbound();
        // A tick-managed (state-sync) consumer: priming runs, so the
        // capture must reflect the live terminal.
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");

        let state = actor.consumer_state(client).expect("state present");
        assert_eq!(state.last_acked_seq, 0, "no acks yet");
        assert_eq!(state.next_seq, 1, "first emission gets seq=1");
        assert_eq!(
            state.wire_terminal_id, 11,
            "wire id stored on the per-consumer entry"
        );
        // Seeded "hello" advances the cursor to (5, 0). The capture
        // must reflect that — proves the RenderState was actually
        // updated against the live terminal, not left blank.
        assert_eq!(state.last_cursor_mode.cursor_x, Some(5));
        assert_eq!(state.last_cursor_mode.cursor_y, Some(0));
    }

    /// A raw broadcast-pump consumer (the human attach path) skips the two
    /// full-grid render passes priming would cost: its reference and
    /// cursor/mode capture are never read (the tick serves only tick-managed
    /// consumers). So it registers with the `unprimed` placeholder, not a
    /// live capture (phux-ahk register-prime gating).
    #[test]
    fn register_raw_consumer_skips_priming() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let mut actor = bundle.actor;
        let client = ClientId(7);
        let (tx, _rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");

        let state = actor.consumer_state(client).expect("state present");
        // Not primed: the placeholder capture, not the seeded cursor at (5, 0).
        assert_eq!(state.last_cursor_mode.cursor_x, None);
        assert_eq!(state.last_cursor_mode.cursor_y, None);
    }

    /// phux-q0e.2: end-to-end across the actor's `select!` loop —
    /// ATTACH then DETACH over the channels handle the lifecycle on
    /// the same `LocalSet` thread the `Terminal` lives on. Drives the
    /// actor through `spawn_local`, so the `!Send` `RenderState`
    /// stays on its owning thread.
    #[tokio::test(flavor = "current_thread")]
    async fn consumer_attach_detach_round_trip_over_channels() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new(20, 5).expect("new");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let join = tokio::task::spawn_local(bundle.actor.run());

                let client = ClientId(42);
                let (out_tx, _out_rx) = dummy_outbound();
                let (tx_a, rx_a) = oneshot::channel();
                handle
                    .consumer_attach
                    .send(ConsumerAttachRequest {
                        client_id: client,
                        outbound: out_tx,
                        wire_terminal_id: 99,
                        stream_id: phux_protocol::ids::StreamId::new(43).expect("test stream id"),
                        bootstrap_id: phux_protocol::ids::BootstrapId::new(1)
                            .expect("test bootstrap id"),
                        wants_state_sync: false,
                        loss_tolerant: false,
                        reply: tx_a,
                    })
                    .await
                    .expect("send attach");
                rx_a.await.expect("attach reply").expect("attach succeeded");

                let (tx_d, rx_d) = oneshot::channel();
                handle
                    .consumer_detach
                    .send(ConsumerDetachRequest {
                        client_id: client,
                        reply: tx_d,
                    })
                    .await
                    .expect("send detach");
                rx_d.await.expect("detach reply");

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// phux-q0e.4: `on_frame_ack` advances `last_acked_seq` monotonically
    /// for in-order acks. Three acks (1, 2, 3) walk the field forward.
    #[test]
    fn on_frame_ack_advances_last_acked_seq_in_order() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, _rx) = dummy_outbound();
        // State-sync consumer: its acks belong to the per-consumer tick seq
        // space, so `on_frame_ack` folds them in (phux-38k6).
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");

        for seq in 1..=3 {
            actor.on_frame_ack(client, seq);
            assert_eq!(
                actor.consumer_state(client).expect("state").last_acked_seq,
                seq,
                "in-order ack must advance last_acked_seq",
            );
        }
    }

    /// phux-q0e.4: older or duplicate acks (`seq <= last_acked_seq`) MUST
    /// be silently dropped — they carry no new state information under
    /// SPEC §12.2's cumulative-ack semantics. After ack=5 then ack=3, the
    /// field must stay at 5.
    #[test]
    fn on_frame_ack_older_or_duplicate_is_dropped() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, _rx) = dummy_outbound();
        // State-sync consumer so its acks are processed (phux-38k6).
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");

        actor.on_frame_ack(client, 5);
        assert_eq!(actor.consumer_state(client).unwrap().last_acked_seq, 5);

        // Older ack.
        actor.on_frame_ack(client, 3);
        assert_eq!(
            actor.consumer_state(client).unwrap().last_acked_seq,
            5,
            "older ack must NOT regress last_acked_seq",
        );

        // Duplicate ack.
        actor.on_frame_ack(client, 5);
        assert_eq!(
            actor.consumer_state(client).unwrap().last_acked_seq,
            5,
            "duplicate ack must NOT touch last_acked_seq",
        );

        // Higher ack still progresses.
        actor.on_frame_ack(client, 6);
        assert_eq!(actor.consumer_state(client).unwrap().last_acked_seq, 6);
    }

    /// phux-38k6: a `FRAME_ACK` from a raw (broadcast-pump) consumer carries a
    /// pump-local seq unrelated to this per-consumer tick state, so
    /// `on_frame_ack` drops it — `last_acked_seq` must NOT move. Otherwise a
    /// foreign counter would skew the RTT/backpressure accounting if the
    /// consumer later went state-sync.
    #[test]
    fn on_frame_ack_for_raw_consumer_is_dropped() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let mut actor = bundle.actor;
        // Global gate OFF (production human-attach default) and a raw consumer.
        let client = ClientId(1);
        let (tx, _rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");

        let folded = actor.on_frame_ack(client, 7);
        assert!(!folded, "raw-consumer ack produces no RTT sample");
        assert_eq!(
            actor.consumer_state(client).expect("state").last_acked_seq,
            0,
            "raw-pump ack must not advance the per-consumer last_acked_seq",
        );
    }

    /// phux-q0e.4: `on_frame_ack` for an unregistered client is a silent
    /// no-op — no panic, no entry created. Mirrors the rest of the
    /// consumer lifecycle's idempotency.
    #[test]
    fn on_frame_ack_for_unregistered_consumer_is_noop() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let mut actor = bundle.actor;

        let stranger = ClientId(999);
        assert_eq!(actor.consumer_count(), 0);
        actor.on_frame_ack(stranger, 42);
        assert_eq!(actor.consumer_count(), 0, "no entry created by stray ack");
        assert!(actor.consumer_state(stranger).is_none());
    }

    /// phux-q0e.4: register, ack, then detach, then re-ack — the re-ack
    /// after detach is a no-op (no panic, no resurrection of the entry).
    #[test]
    fn on_frame_ack_after_detach_is_noop() {
        let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
        let mut actor = bundle.actor;
        let client = ClientId(7);
        let (tx, _rx) = dummy_outbound();
        // State-sync so the pre-detach ack is folded in (phux-38k6); the
        // point of the test is that a *post*-detach ack does not resurrect.
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");
        actor.on_frame_ack(client, 2);
        assert_eq!(actor.consumer_state(client).unwrap().last_acked_seq, 2);

        actor.unregister_consumer(client);
        assert!(actor.consumer_state(client).is_none());

        // Late ack after detach: must not resurrect the entry.
        actor.on_frame_ack(client, 9);
        assert!(actor.consumer_state(client).is_none());
        assert_eq!(actor.consumer_count(), 0);
    }

    /// phux-0q8 coexistence gate: with a consumer registered but the
    /// emission gate forced OFF (`consumer_tick_emits == false`),
    /// `tick_emit` MUST NOT push any frame onto the consumer's outbound
    /// mailbox — even with dirty seeded content. This is the invariant
    /// that lets the per-consumer lifecycle run live alongside the
    /// broadcast pump without double-painting the client when the gate is
    /// off. Production defaults the gate OFF for human attach (phux-yeca),
    /// but this test still disables it explicitly so the invariant is local.
    #[test]
    fn tick_emit_is_silent_while_gate_is_off() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        actor.disable_tick_emit_for_test();
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");
        // Make the grid genuinely dirty AFTER register so a non-gated tick
        // would have something to emit — proving the gate, not an empty diff.
        actor.vt_write_for_test(b"dirty-content");

        // Several ticks: the gate must keep every one silent.
        for _ in 0..3 {
            actor.tick_emit();
        }
        assert!(
            rx.try_recv().is_err(),
            "gate off: tick_emit must not emit while the broadcast pump is the live path",
        );
        // The per-consumer entry is still live (lifecycle is active) —
        // only emission is suppressed.
        assert_eq!(actor.consumer_count(), 1);
        assert_eq!(
            actor.consumer_state(client).expect("state").next_seq,
            1,
            "no emission means the per-consumer seq never advanced",
        );
    }

    /// phux-bowo: dirty/idle settling is independent of the state-sync
    /// emitter gate. A raw-only pane must produce a fresh pair for each
    /// output burst even though `tick_emit` stays gated and therefore leaves
    /// `terminal_dirty_since_tick` set.
    #[test]
    fn raw_only_consumer_gets_repeatable_dirty_idle_cycles() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let raw_client = ClientId(1);
        let (outbound, mut output_rx) = dummy_outbound();
        actor
            .register_consumer(raw_client, outbound, 11, false)
            .expect("register raw consumer");
        let (event_tx, mut event_rx) = mpsc::channel(8);
        actor.set_event_sink(event_tx);

        for chunk in [b"first".as_slice(), b"\r\nsecond".as_slice()] {
            actor.vt_write_for_test(chunk);
            actor.source_events_from_chunk(chunk);
            assert!(
                matches!(event_rx.try_recv(), Ok(AgentEvent::Dirty)),
                "each output burst must begin with dirty",
            );

            // The first tick observes output and keeps the burst open. The
            // gated tick emits no synthesized output and does not consume its
            // state-sync dirty flag.
            actor.maybe_emit_idle();
            actor.tick_emit();
            assert!(event_rx.try_recv().is_err(), "first tick is not idle");

            // A following quiet tick settles independently of that still-set
            // state-sync flag, re-arming dirty for the next burst.
            actor.maybe_emit_idle();
            actor.tick_emit();
            assert!(
                matches!(event_rx.try_recv(), Ok(AgentEvent::Idle)),
                "quiet tick must close the burst with idle",
            );
        }

        assert!(
            output_rx.try_recv().is_err(),
            "raw consumer must remain on the byte-faithful broadcast path",
        );
        assert!(
            actor.terminal_dirty_since_tick,
            "gated tick must not weaken state-sync dirty bookkeeping",
        );
    }

    /// phux-yeca: production defaults the synthesized tick emitter OFF so
    /// human TUI attach stays on the immediate raw PTY broadcast path.
    #[test]
    fn tick_emit_gate_defaults_off_for_human_attach() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");
        actor.vt_write_for_test(b"dirty-content");

        actor.tick_emit();

        assert!(
            rx.try_recv().is_err(),
            "default human attach path must not wait for synthesized tick output",
        );
    }

    /// phux-fseo: a consumer that negotiated `OutputMode::StateSync`
    /// (`wants_state_sync == true`) is served by the tick even with the
    /// global test gate OFF — the per-consumer opt-in is the production
    /// path. Proves the negotiation actually reaches `tick_emit` without
    /// relying on `enable_tick_emit_for_test`.
    #[test]
    fn tick_emit_serves_negotiated_state_sync_consumer_with_gate_off() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        // NB: global gate left at its production default (OFF).
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");
        actor.vt_write_for_test(b"state-sync-marker");

        actor.tick_emit();

        let frame = rx
            .try_recv()
            .expect("state-sync consumer must be served by the tick even with the gate off");
        let Outbound::Frame(FrameKind::TerminalOutput { seq, .. }) = frame else {
            panic!("expected a TerminalOutput frame for the state-sync consumer");
        };
        assert_eq!(seq, 1, "first tick emission stamps seq=1");
    }

    /// phux-fseo: with the global gate OFF and two consumers sharing one
    /// pane — one `StateSync`, one `Raw` — the tick serves ONLY the
    /// state-sync consumer. The raw consumer is served by the runtime's
    /// broadcast pump; emitting to it here too would double-paint it.
    #[test]
    fn tick_emit_mixed_mode_serves_only_state_sync_consumer() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let sync_client = ClientId(1);
        let raw_client = ClientId(2);
        let (sync_tx, mut sync_rx) = dummy_outbound();
        let (raw_tx, mut raw_rx) = dummy_outbound();
        actor
            .register_consumer(sync_client, sync_tx, 11, true)
            .expect("register state-sync");
        actor
            .register_consumer(raw_client, raw_tx, 12, false)
            .expect("register raw");
        actor.vt_write_for_test(b"shared-pane-write");

        actor.tick_emit();

        assert!(
            matches!(
                sync_rx.try_recv(),
                Ok(Outbound::Frame(FrameKind::TerminalOutput { .. })),
            ),
            "state-sync consumer must receive the synthesized delta",
        );
        assert!(
            raw_rx.try_recv().is_err(),
            "raw consumer must stay on the broadcast pump — tick must not double-paint it",
        );
    }

    // ---- phux-v45.8 / ADR-0042: loss-tolerant (advance-on-ack) state sync ----

    /// Render a `Terminal`'s viewport into right-trimmed rows, skipping
    /// wide-cell tails — enough to assert grid equivalence between a
    /// state-sync mirror and the canonical after applying deltas.
    fn render_viewport(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
        use libghostty_vt::render::{CellIterator, RowIterator};
        use libghostty_vt::screen::CellWide;
        let mut rs = RenderState::new().expect("RenderState::new");
        let snap = rs.update(t).expect("update");
        let rows_n = snap.rows().expect("rows");
        let mut rows = RowIterator::new().expect("RowIterator::new");
        let mut cells = CellIterator::new().expect("CellIterator::new");
        let mut row_iter = rows.update(&snap).expect("row update");
        let mut out: Vec<String> = Vec::with_capacity(usize::from(rows_n));
        let mut i: u16 = 0;
        while let Some(row) = row_iter.next() {
            if i >= rows_n {
                break;
            }
            let mut line = String::new();
            let mut cell_iter = cells.update(row).expect("cell update");
            while let Some(cell) = cell_iter.next() {
                if matches!(
                    cell.raw_cell().expect("rc").wide().expect("wide"),
                    CellWide::SpacerTail
                ) {
                    continue;
                }
                let g = cell.graphemes().expect("graphemes");
                if g.is_empty() {
                    line.push(' ');
                } else {
                    line.extend(g);
                }
            }
            out.push(line.trim_end().to_owned());
            i += 1;
        }
        out
    }

    /// Drain every currently-queued `TERMINAL_OUTPUT` body from a consumer's
    /// mailbox (its `seq` and bytes).
    fn drain_outputs(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(u64, Vec<u8>)> {
        let mut frames = Vec::new();
        while let Ok(Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. })) = rx.try_recv()
        {
            frames.push((seq, bytes.to_vec()));
        }
        frames
    }

    /// A loss-tolerant consumer's reference advances on `FRAME_ACK`, not on
    /// emit: after emitting a delta the acked reference is unchanged and the
    /// frame is retained in `pending_refs`; the matching ack advances the
    /// reference and prunes the pending snapshot.
    #[test]
    fn loss_tolerant_reference_advances_only_on_ack() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");
        actor.enable_loss_tolerance_for_test(client);

        actor.vt_write_for_test(b"hello");
        actor.tick_emit();

        let frames = drain_outputs(&mut rx);
        assert_eq!(frames.len(), 1, "one delta shipped for the new content");
        let seq = frames[0].0;
        {
            let state = actor.consumer_state(client).expect("state");
            assert!(
                state.pending_refs.contains_key(&seq),
                "the emitted frame's grid snapshot is retained until acked",
            );
            assert_eq!(
                state.last_acked_seq, 0,
                "no ack yet: the acked reference has not advanced",
            );
        }

        // Idle tick with no new content and no elapsed time: the loss-tolerant
        // gate must NOT re-ship the same cumulative delta (no flood).
        actor.tick_emit();
        assert!(
            drain_outputs(&mut rx).is_empty(),
            "an un-acked delta must not re-ship every idle tick",
        );

        // Ack it: the reference advances and the pending snapshot is pruned.
        actor.on_frame_ack(client, seq);
        {
            let state = actor.consumer_state(client).expect("state");
            assert!(
                state.pending_refs.is_empty(),
                "ack prunes the covered pending snapshot",
            );
            assert_eq!(state.last_acked_seq, seq);
        }
        // Post-ack idle tick: nothing to send (acked == live).
        actor.tick_emit();
        assert!(
            drain_outputs(&mut rx).is_empty(),
            "post-ack idle tick is silent"
        );
    }

    /// The core v45.8 property: a dropped/un-acked frame self-heals. Emit
    /// delta 1 (simulated dropped — never applied to the mirror, never acked),
    /// then emit delta 2 for later content. Because delta 2 is re-diffed
    /// against the last-ACKED reference (still empty), it re-includes delta 1's
    /// rows, so applying ONLY delta 2 to the mirror converges it to canonical.
    #[test]
    fn loss_tolerant_dropped_frame_rediffs_against_acked_and_converges() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");
        actor.enable_loss_tolerance_for_test(client);

        // Mirror starts at the same point the acked reference was primed to
        // (an empty grid) — what the consumer's TERMINAL_SNAPSHOT establishes.
        let mut mirror = GhosttyTerminal::new(TerminalOptions {
            cols: 20,
            rows: 5,
            max_scrollback: 100,
        })
        .expect("mirror");

        // Content A → delta 1. SIMULATE A DROP: do not apply it, do not ack.
        actor.vt_write_for_test(b"AAAA");
        actor.tick_emit();
        let dropped = drain_outputs(&mut rx);
        assert_eq!(dropped.len(), 1, "delta 1 emitted");

        // Content B (a second row) → delta 2. Re-diffed against acked (empty).
        actor.vt_write_for_test(b"\r\nBBBB");
        actor.tick_emit();
        let delivered = drain_outputs(&mut rx);
        assert_eq!(delivered.len(), 1, "delta 2 emitted");

        // Apply ONLY delta 2 (delta 1 was lost).
        mirror.vt_write(&delivered[0].1);

        let canonical_grid = render_viewport(&actor.terminal.borrow());
        let mirror_grid = render_viewport(&mirror);
        assert_eq!(
            canonical_grid, mirror_grid,
            "applying only the post-drop cumulative delta must converge the \
             mirror to canonical (self-heal);\ncanonical = {canonical_grid:?}\n\
             mirror    = {mirror_grid:?}",
        );
        assert_eq!(mirror_grid[0], "AAAA");
        assert_eq!(mirror_grid[1], "BBBB");
    }

    /// After a `FRAME_ACK`, subsequent deltas are diffed against the newly
    /// advanced acked reference (incremental, not cumulative-from-empty), and
    /// the mirror — brought current by the acked frames then the new one —
    /// still converges. Exercises the steady-state ack loop.
    #[test]
    fn loss_tolerant_incremental_after_ack_converges() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");
        actor.enable_loss_tolerance_for_test(client);
        let mut mirror = GhosttyTerminal::new(TerminalOptions {
            cols: 20,
            rows: 5,
            max_scrollback: 100,
        })
        .expect("mirror");

        // Round 1: content, deliver + ack.
        actor.vt_write_for_test(b"row-one");
        actor.tick_emit();
        let f1 = drain_outputs(&mut rx);
        assert_eq!(f1.len(), 1);
        mirror.vt_write(&f1[0].1);
        actor.on_frame_ack(client, f1[0].0);

        // Round 2: more content, delivered + acked; diffed against the acked
        // reference from round 1.
        actor.vt_write_for_test(b"\r\nrow-two");
        actor.tick_emit();
        let f2 = drain_outputs(&mut rx);
        assert_eq!(f2.len(), 1);
        mirror.vt_write(&f2[0].1);
        actor.on_frame_ack(client, f2[0].0);

        let canonical_grid = render_viewport(&actor.terminal.borrow());
        let mirror_grid = render_viewport(&mirror);
        assert_eq!(
            canonical_grid, mirror_grid,
            "steady-state ack loop converges"
        );
        assert_eq!(mirror_grid[0], "row-one");
        assert_eq!(mirror_grid[1], "row-two");
    }

    /// A retransmit heals a lost final frame on an otherwise idle terminal.
    /// Emit content (dropped, un-acked), backdate the emit clock past the
    /// retransmit timeout, then run an idle tick: the consumer retransmits a
    /// cumulative delta (re-diffed against the acked reference) that converges
    /// the mirror even though no new content arrived.
    #[test]
    fn loss_tolerant_retransmits_lost_frame_when_idle() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");
        actor.enable_loss_tolerance_for_test(client);
        let mut mirror = GhosttyTerminal::new(TerminalOptions {
            cols: 20,
            rows: 5,
            max_scrollback: 100,
        })
        .expect("mirror");

        // Content → delta. Simulate a drop: discard it, never ack.
        actor.vt_write_for_test(b"lonely");
        actor.tick_emit();
        let dropped = drain_outputs(&mut rx);
        assert_eq!(dropped.len(), 1, "initial delta emitted");

        // No new content. Backdate the emit instant past the retransmit RTO
        // so the next idle tick re-ships (suspected loss).
        actor.backdate_emit_instants_for_test(client, std::time::Duration::from_secs(2));
        actor.tick_emit();
        let retransmit = drain_outputs(&mut rx);
        assert_eq!(
            retransmit.len(),
            1,
            "an idle terminal with an un-acked frame must retransmit after the RTO",
        );

        // The retransmit (re-diffed against the acked reference) converges the
        // mirror that never saw the original frame.
        mirror.vt_write(&retransmit[0].1);
        let canonical_grid = render_viewport(&actor.terminal.borrow());
        let mirror_grid = render_viewport(&mirror);
        assert_eq!(
            canonical_grid, mirror_grid,
            "retransmit heals the lost frame on an idle terminal",
        );
        assert_eq!(mirror_grid[0], "lonely");
    }

    /// The emit-once (non-loss-tolerant) default is untouched: a state-sync
    /// consumer that did NOT opt into loss-tolerance keeps no `pending_refs`
    /// and advances its reference on emit (`on_frame_ack` does not evict a
    /// pending snapshot because there is none).
    #[test]
    fn emit_once_default_keeps_no_pending_refs() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");
        // NB: loss-tolerance NOT enabled.
        actor.vt_write_for_test(b"content");
        actor.tick_emit();
        let frames = drain_outputs(&mut rx);
        assert_eq!(frames.len(), 1);
        let state = actor.consumer_state(client).expect("state");
        assert!(!state.loss_tolerant, "default consumer is emit-once");
        assert!(
            state.pending_refs.is_empty(),
            "emit-once path allocates no pending reference snapshots",
        );
    }

    /// phux-0q8 / phux-q0e.3 / phux-3uv / phux-ia4: with the gate ON for
    /// a SINGLE consumer, `tick_emit` diffs the
    /// dirty seeded grid against the consumer's reference and ships exactly
    /// one `TerminalOutput` carrying the content, stamping `seq = 1`.
    ///
    /// Emit-once (phux-ia4): the consumer's reference advances on emit, so
    /// a second tick with no further writes is SILENT — the change is
    /// delivered exactly once, not re-emitted every tick. A subsequent
    /// write produces a fresh single emission (`seq = 2`).
    #[test]
    fn tick_emit_emits_once_when_gate_is_on() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        // Register against the (blank) terminal: the reference is primed
        // so deltas are measured "from now." Writing AFTER register is what
        // makes the next tick produce a diff.
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");
        actor.vt_write_for_test(b"q0e-marker");

        actor.tick_emit();
        let frame = rx
            .try_recv()
            .expect("gate on: first tick must emit the changed grid");
        let Outbound::Frame(FrameKind::TerminalOutput {
            terminal_id,
            seq,
            bytes,
            ..
        }) = frame
        else {
            panic!("expected a TerminalOutput frame from tick_emit");
        };
        assert_eq!(seq, 1, "first tick emission stamps seq=1");
        assert_eq!(
            terminal_id.local_id(),
            Some(11),
            "tick frame carries the registered wire terminal id",
        );
        assert!(
            contains_subslice(&bytes, b"q0e-marker"),
            "tick emission must carry the seeded grid content; got {:?}",
            String::from_utf8_lossy(&bytes),
        );

        // Emit-once: with no further writes, the reference now matches the
        // live grid, so the next tick is silent — NO re-emission of the
        // already-delivered change.
        actor.tick_emit();
        assert!(
            rx.try_recv().is_err(),
            "gate on: emit-once — an already-emitted change is not re-sent on the next tick",
        );

        // A fresh write produces a new single emission with the next seq.
        actor.vt_write_for_test(b" more");
        actor.tick_emit();
        let frame = rx
            .try_recv()
            .expect("gate on: a new write must emit a fresh diff");
        let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) = frame else {
            panic!("expected a TerminalOutput frame on the new write");
        };
        assert_eq!(seq, 2, "second distinct change stamps seq=2");
        assert!(
            contains_subslice(&bytes, b"more"),
            "second emission must carry the newly-written content; got {:?}",
            String::from_utf8_lossy(&bytes),
        );

        // FRAME_ACK advances last_acked_seq but does not itself trigger any
        // emission; the grid is unchanged so the next tick stays silent.
        actor.on_frame_ack(client, 2);
        actor.tick_emit();
        assert!(
            rx.try_recv().is_err(),
            "gate on: an unchanged grid stays silent after ack",
        );
        assert_eq!(
            actor.consumer_state(client).expect("state").next_seq,
            3,
            "two emissions advanced next_seq to 3; the post-ack tick was silent",
        );
        assert_eq!(
            actor.consumer_state(client).expect("state").last_acked_seq,
            2,
            "FRAME_ACK advanced last_acked_seq",
        );
    }

    /// phux-ia4 regression: TWO consumers sharing one pane. A single tick
    /// of new output MUST deliver the incremental to BOTH consumers — not
    /// just the first one walked.
    ///
    /// This is the exact starvation the ticket is about. Under the old
    /// per-consumer-`RenderState` dirty model, the first consumer's
    /// `RenderState::update` consumed the shared `Terminal` dirty bits, so
    /// the second consumer that tick observed `Dirty::Clean` and emitted
    /// nothing. The per-consumer reference grid removes that coupling: each
    /// consumer diffs against its own last-synced rows, so both receive the
    /// change in the same tick regardless of walk order.
    #[test]
    fn tick_emit_serves_every_consumer_on_a_shared_pane() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        // Gate ON (production default).
        actor.enable_tick_emit_for_test();

        // Two consumers on the same pane, primed against the same blank
        // terminal.
        let client_a = ClientId(1);
        let client_b = ClientId(2);
        let (tx_a, mut rx_a) = dummy_outbound();
        let (tx_b, mut rx_b) = dummy_outbound();
        actor
            .register_consumer(client_a, tx_a, 11, false)
            .expect("register a");
        actor
            .register_consumer(client_b, tx_b, 11, false)
            .expect("register b");

        // One tick of new output AFTER both are primed.
        actor.vt_write_for_test(b"shared-marker");
        actor.tick_emit();

        // BOTH consumers must receive a TerminalOutput carrying the marker.
        let recv_marker = |rx: &mut mpsc::Receiver<Outbound>, who: &str| {
            let frame = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("consumer {who} starved: no frame this tick"));
            let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) = frame else {
                panic!("consumer {who}: expected a TerminalOutput frame");
            };
            assert_eq!(seq, 1, "consumer {who}: first emission stamps seq=1");
            assert!(
                contains_subslice(&bytes, b"shared-marker"),
                "consumer {who}: incremental must carry the shared marker; got {:?}",
                String::from_utf8_lossy(&bytes),
            );
        };
        recv_marker(&mut rx_a, "A");
        recv_marker(&mut rx_b, "B");

        // Emit-once per consumer: with no further writes, neither consumer
        // gets a re-emission on the next tick.
        actor.tick_emit();
        assert!(
            rx_a.try_recv().is_err(),
            "consumer A: emit-once — no re-emission on an unchanged tick",
        );
        assert!(
            rx_b.try_recv().is_err(),
            "consumer B: emit-once — no re-emission on an unchanged tick",
        );

        // Per-consumer independence: a consumer that detaches does not
        // perturb the other. A fresh write reaches the survivor exactly
        // once.
        actor.unregister_consumer(client_a);
        actor.vt_write_for_test(b" again");
        actor.tick_emit();
        let frame = rx_b.try_recv().expect("consumer B: must get the new write");
        let Outbound::Frame(FrameKind::TerminalOutput { seq, bytes, .. }) = frame else {
            panic!("consumer B: expected a TerminalOutput frame");
        };
        assert_eq!(seq, 2, "consumer B: second distinct change stamps seq=2");
        assert!(
            contains_subslice(&bytes, b"again"),
            "consumer B: must carry the second write; got {:?}",
            String::from_utf8_lossy(&bytes),
        );
        assert!(
            rx_a.try_recv().is_err(),
            "consumer A detached: must receive nothing further",
        );
    }

    /// phux-4l0: an idle tick (no write since the last tick, no consumer
    /// awaiting its first emission) short-circuits and emits nothing.
    #[test]
    fn idle_tick_short_circuits_and_emits_nothing() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");

        // First tick: the consumer needs its initial pass, so it is walked
        // (returns empty here — primed against a blank terminal) and the
        // dirty flag set at construction is consumed.
        actor.tick_emit();
        // Drain whatever the first tick produced (expected: nothing, since
        // the reference was primed to the same blank state).
        while rx.try_recv().is_ok() {}

        // Now write, tick, drain: the consumer receives the write.
        actor.vt_write_for_test(b"hello");
        actor.tick_emit();
        let got = rx.try_recv().expect("write must reach the consumer");
        let Outbound::Frame(FrameKind::TerminalOutput { bytes, .. }) = got else {
            panic!("expected TerminalOutput");
        };
        assert!(contains_subslice(&bytes, b"hello"));

        // Many idle ticks (no further writes): the short-circuit must keep
        // each one silent and must not perturb the consumer entry.
        for _ in 0..5 {
            actor.tick_emit();
            assert!(
                rx.try_recv().is_err(),
                "idle tick must emit nothing (short-circuit)",
            );
        }
        assert_eq!(actor.consumer_count(), 1, "consumer entry intact");
    }

    /// phux-4l0: a consumer registered AFTER the last write sits on a
    /// terminal that is `Clean` since the previous tick, yet has never had
    /// a synthesis pass. The `needs_initial_emit` carve-out must keep the
    /// short-circuit from starving it: the next tick must still walk it
    /// (here the write predates the attach, so it is already primed and
    /// the body is empty — the point is the entry is serviced, not
    /// skipped, preserving the phux-ia4 multi-consumer guarantee).
    #[test]
    fn new_consumer_served_even_when_terminal_clean() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        // Consumer A attaches, a write lands, a tick delivers it, then a
        // steady-state tick clears the dirty flag.
        let client_a = ClientId(1);
        let (tx_a, mut rx_a) = dummy_outbound();
        actor
            .register_consumer(client_a, tx_a, 11, false)
            .expect("reg a");
        actor.vt_write_for_test(b"first");
        actor.tick_emit();
        while rx_a.try_recv().is_ok() {}
        // Steady-state tick: terminal now Clean since last tick.
        actor.tick_emit();
        assert!(rx_a.try_recv().is_err(), "A steady-state: nothing");

        // Consumer B attaches with NO intervening write. The terminal is
        // Clean, but B has needs_initial_emit set, so the short-circuit
        // must NOT fire — B must be walked. (Primed to current state, so
        // the body is empty, but the entry is serviced and the flag
        // cleared.)
        let client_b = ClientId(2);
        let (tx_b, mut rx_b) = dummy_outbound();
        actor
            .register_consumer(client_b, tx_b, 11, false)
            .expect("reg b");
        assert!(
            actor
                .consumer_state(client_b)
                .expect("b present")
                .needs_initial_emit,
            "B should be awaiting its first emission",
        );
        actor.tick_emit();
        // B primed to current state ⇒ empty body, but the pass ran:
        // needs_initial_emit is now cleared.
        assert!(
            !actor
                .consumer_state(client_b)
                .expect("b present")
                .needs_initial_emit,
            "B's first pass must have run despite the Clean terminal",
        );
        assert!(rx_b.try_recv().is_err(), "B primed ⇒ empty first pass");

        // A fresh write after both are primed reaches BOTH.
        actor.vt_write_for_test(b" again");
        actor.tick_emit();
        let frame_a = rx_a.try_recv().expect("A gets the new write");
        let frame_b = rx_b.try_recv().expect("B gets the new write");
        for (who, frame) in [("A", frame_a), ("B", frame_b)] {
            let Outbound::Frame(FrameKind::TerminalOutput { bytes, .. }) = frame else {
                panic!("{who}: expected TerminalOutput");
            };
            assert!(
                contains_subslice(&bytes, b"again"),
                "{who} must carry the new write",
            );
        }
    }

    /// phux-ddg: a consumer whose outbound receiver has been dropped (a
    /// detach whose `ConsumerDetachRequest` never reached the actor — full
    /// mailbox) must be reaped by `tick_emit` rather than re-rendered every
    /// tick forever. The tick is self-healing: a `Closed` mailbox removes
    /// the entry.
    #[test]
    fn tick_emit_reaps_consumer_with_closed_mailbox() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        let client = ClientId(1);
        let (tx, rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");
        assert_eq!(actor.consumer_count(), 1);

        // Simulate the dropped-detach leak: the client's receiver goes
        // away (disconnect) but the detach request was lost, so the
        // per-consumer entry is still present.
        drop(rx);

        // A write makes the tick try to emit to the dead consumer; the
        // send fails Closed and the entry is reaped.
        actor.vt_write_for_test(b"content");
        actor.tick_emit();
        assert_eq!(
            actor.consumer_count(),
            0,
            "closed-mailbox consumer must be reaped by the tick",
        );

        // Subsequent ticks are stable no-ops.
        actor.vt_write_for_test(b"more");
        actor.tick_emit();
        assert_eq!(actor.consumer_count(), 0, "stays reaped");
    }

    /// phux-ddg: a consumer with a closed mailbox is reaped even when the
    /// diff body is empty (idle dead consumer). Without this, an idle but
    /// dead consumer would never hit the `try_send` Closed arm and would
    /// linger until pane teardown.
    #[test]
    fn tick_emit_reaps_idle_consumer_with_closed_mailbox() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        let client = ClientId(1);
        let (tx, rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");

        // Prime past the initial-emit pass with one tick (empty body).
        actor.tick_emit();
        assert_eq!(actor.consumer_count(), 1);

        // Receiver drops (disconnect). A write keeps the per-consumer loop
        // running this tick; whether the diff body is empty or not, the
        // `is_closed()` probe on the empty-body path and the `Closed` arm
        // on the send path both reap the entry.
        drop(rx);
        actor.vt_write_for_test(b"x");
        actor.tick_emit();
        assert_eq!(
            actor.consumer_count(),
            0,
            "dead consumer reaped even though it never acked",
        );
    }

    /// Resize updates both the libghostty `Terminal` and (when present)
    /// the PTY winsize. We only assert the Terminal side here — the
    /// PTY ioctl path is exercised in the integration test.
    #[tokio::test(flavor = "current_thread")]
    async fn resize_updates_terminal_dims() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new(80, 24).expect("new");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let join = tokio::task::spawn_local(bundle.actor.run());

                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 120,
                        rows: 40,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
                    })
                    .await
                    .expect("send resize");
                // Give the actor a moment to process the resize before
                // we shut it down. A bounded `yield_now` loop is the
                // current-thread-friendly version of `sleep(0)`.
                for _ in 0..16 {
                    tokio::task::yield_now().await;
                }

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// A resize carrying cell pixel metrics must land in the kernel
    /// winsize: `ws_xpixel`/`ws_ypixel` = cells x cell size. TIOCGWINSZ
    /// is the first thing pixel-aware programs (`kitten icat`, sixel
    /// sizers) consult; without the `cell_px` plumbing it reads 0x0.
    /// A later pixel-less resize (agent `TERMINAL_RESIZE`) must keep the
    /// established cell size rather than zeroing the pixel fields.
    #[tokio::test(flavor = "current_thread")]
    async fn resize_with_cell_px_updates_pty_winsize_pixels() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let cmd = CommandBuilder::new("/bin/cat");
                let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
                let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
                let handle = bundle.handle.clone();
                let token = bundle.token.clone();
                let join = tokio::task::spawn_local(bundle.actor.run());

                // Poll the kernel winsize until it reaches `want` (the
                // resize mailbox is async); bail out after a bounded wait.
                let wait_for = async |want: (u16, u16, u16, u16)| {
                    let read = || {
                        let got = master
                            .lock()
                            .expect("master lock")
                            .get_size()
                            .expect("size");
                        (got.cols, got.rows, got.pixel_width, got.pixel_height)
                    };
                    for _ in 0..200 {
                        if read() == want {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    panic!(
                        "winsize never reached {want:?}; kernel reports {:?}",
                        read()
                    );
                };

                // 100x40 cells at 9x18 px per cell: 900x720 px text area.
                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 100,
                        rows: 40,
                        cell_px: Some((9, 18)),
                        resync_clients: false,
                        resync_only: false,
                    })
                    .await
                    .expect("send resize");
                wait_for((100, 40, 900, 720)).await;

                // Pixel-less resize: grid changes, cell size sticks.
                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 90,
                        rows: 30,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
                    })
                    .await
                    .expect("send resize");
                wait_for((90, 30, 810, 540)).await;

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// With no client ever reporting pixel metrics, the PTY winsize must
    /// still carry nonzero pixel dimensions derived from [`DEFAULT_CELL_PX`]:
    /// at spawn (before any resize) and after a pixel-less resize. This is
    /// the proximate `kitten icat` unblock — its preflight refuses a `0x0`
    /// pixel report, and most clients announce cells but not pixels.
    #[tokio::test(flavor = "current_thread")]
    async fn winsize_pixels_default_when_no_client_reports_metrics() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let cmd = CommandBuilder::new("/bin/cat");
                let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
                let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
                let handle = bundle.handle.clone();
                let token = bundle.token.clone();
                let join = tokio::task::spawn_local(bundle.actor.run());

                let (cell_w, cell_h) = DEFAULT_CELL_PX;

                // Spawn-time winsize: derived from the fallback cell size,
                // never zero. 80x24 cells at 8x16 px -> 640x384 px.
                let spawned = master
                    .lock()
                    .expect("master lock")
                    .get_size()
                    .expect("size");
                assert_eq!(
                    (spawned.pixel_width, spawned.pixel_height),
                    (80 * cell_w, 24 * cell_h),
                    "spawn-time winsize must carry nonzero default pixel dims",
                );

                let wait_for = async |want: (u16, u16, u16, u16)| {
                    let read = || {
                        let got = master
                            .lock()
                            .expect("master lock")
                            .get_size()
                            .expect("size");
                        (got.cols, got.rows, got.pixel_width, got.pixel_height)
                    };
                    for _ in 0..200 {
                        if read() == want {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    panic!(
                        "winsize never reached {want:?}; kernel reports {:?}",
                        read()
                    );
                };

                // A pixel-less resize keeps deriving pixels from the default
                // cell size: 100x40 cells at 8x16 px -> 800x640 px.
                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 100,
                        rows: 40,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
                    })
                    .await
                    .expect("send resize");
                wait_for((100, 40, 100 * cell_w, 40 * cell_h)).await;

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// End-to-end XTWINOPS: a PTY child queries `CSI 14 t` (text area in
    /// pixels) and `CSI 18 t` (text area in cells) and must receive the
    /// geometry the most recent resize established. Exercises the whole
    /// reply path — libghostty parses the query from PTY output, the
    /// `on_size` callback supplies the shared geometry, and `on_pty_write`
    /// routes the encoded reply back into the PTY writer bridge. The
    /// asserted bytes come back via tty echo of the child's input.
    #[tokio::test(flavor = "current_thread")]
    async fn xtwinops_size_queries_answered_from_resized_geometry() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut cmd = CommandBuilder::new("/bin/sh");
                // One query pair per input line, so the test can re-trigger
                // if an early line raced the resize.
                cmd.args(["-c", r"while read _; do printf '\033[14t\033[18t'; done"]);
                let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
                let pty_in = bundle.actor.pty_tx.clone().expect("pty writer");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let mut out = handle.output.subscribe();
                let join = tokio::task::spawn_local(bundle.actor.run());

                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 100,
                        rows: 40,
                        cell_px: Some((9, 18)),
                        resync_clients: false,
                        resync_only: false,
                    })
                    .await
                    .expect("send resize");
                // Let the actor drain the resize before the first query.
                for _ in 0..16 {
                    tokio::task::yield_now().await;
                }

                // CSI 14 t reply: ESC [ 4 ; height_px ; width_px t.
                // CSI 18 t reply: ESC [ 8 ; rows ; cols t.
                // The replies surface as tty ECHO of the child's input, and
                // ECHOCTL (in the default lflags) renders the ESC byte in
                // caret notation — `^[` — so accept either spelling.
                let seen = |acc: &[u8], tail: &[u8]| {
                    contains_subslice(acc, &[b"\x1b[", tail].concat())
                        || contains_subslice(acc, &[b"^[[", tail].concat())
                };
                let mut acc: Vec<u8> = Vec::new();
                let mut found = false;
                let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
                let mut round = 0_usize;
                while tokio::time::Instant::now() < deadline {
                    // Re-poke the shell periodically: the first `go` can land
                    // before the child has finished starting, and then nothing
                    // ever queries the terminal.
                    if round.is_multiple_of(16) {
                        pty_in
                            .try_send(EncodedInputRequest::legacy(b"go\n".to_vec()))
                            .expect("pty write");
                    }
                    round += 1;
                    match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                        Ok(Ok(PaneOutput::Live(bytes))) => acc.extend_from_slice(&bytes),
                        Ok(Ok(PaneOutput::Resync { bytes, .. })) => {
                            acc.extend_from_slice(&bytes);
                        }
                        Ok(Err(_)) => break, // channel closed
                        Err(_) => {}         // poll tick; retry
                    }
                    if seen(&acc, b"4;720;900t") && seen(&acc, b"8;40;100t") {
                        found = true;
                        break;
                    }
                }
                assert!(
                    found,
                    "XTWINOPS replies never observed; output so far: {:?}",
                    String::from_utf8_lossy(&acc),
                );

                // The writer bridge exits on channel close; `shutdown_pty`
                // joins it, so the test's sender clone must drop first.
                drop(pty_in);
                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// phux-8v1 regression: a resize must re-broadcast a full snapshot of
    /// the post-reflow grid so attached clients (whose mirror reflowed
    /// independently and may have dropped rows) reconverge on the
    /// canonical content. We assert the broadcast that follows a resize
    /// carries the snapshot reset preamble (`ESC [ ! p`, DECSTR) AND the
    /// content that was on the grid before the resize — without this fix
    /// the only post-resize bytes are new PTY output, so prior content is
    /// never re-sent and the client shows lost/duplicated rows.
    #[tokio::test]
    async fn resize_rebroadcasts_grid_snapshot_for_phux_8v1() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new_with_seed(80, 24, b"phux8v1-marker").expect("seed");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                // Subscribe BEFORE the actor runs so we don't miss the
                // resize broadcast.
                let mut out = handle.output.subscribe();
                let join = tokio::task::spawn_local(bundle.actor.run());

                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 40,
                        rows: 10,
                        cell_px: None,
                        resync_clients: true,
                        resync_only: false,
                    })
                    .await
                    .expect("send resize");

                // Collect broadcast bytes for a bounded window and look
                // for the snapshot. `recv` resolves as soon as the resize
                // broadcast lands. phux-3ns5: the resync rides a
                // `PaneOutput::Resync` carrying the post-reflow dims, so
                // also capture them to assert the client mirror is told to
                // resize to 40x10.
                let mut acc: Vec<u8> = Vec::new();
                let mut resync_dims: Option<(u16, u16)> = None;
                let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
                while tokio::time::Instant::now() < deadline {
                    match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                        Ok(Ok(PaneOutput::Resync {
                            cols, rows, bytes, ..
                        })) => {
                            resync_dims = Some((cols, rows));
                            acc.extend_from_slice(&bytes);
                            if contains_subslice(&acc, b"\x1b[!p")
                                && contains_subslice(&acc, b"phux8v1-marker")
                            {
                                break;
                            }
                        }
                        Ok(Ok(PaneOutput::Live(bytes))) => acc.extend_from_slice(&bytes),
                        Ok(Err(_)) => break, // channel closed
                        Err(_) => tokio::task::yield_now().await, // poll tick
                    }
                }
                assert_eq!(
                    resync_dims,
                    Some((40, 10)),
                    "resize resync must carry the post-reflow grid dims (phux-3ns5)",
                );

                assert!(
                    contains_subslice(&acc, b"\x1b[!p"),
                    "resize broadcast missing DECSTR snapshot preamble; got {:?}",
                    String::from_utf8_lossy(&acc),
                );
                assert!(
                    contains_subslice(&acc, b"phux8v1-marker"),
                    "resize broadcast did not re-send pre-resize grid content; got {:?}",
                    String::from_utf8_lossy(&acc),
                );

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// phux-y8v6: a `resync_only` request (sent by a lagged output pump)
    /// must re-broadcast a full grid snapshot WITHOUT resizing the grid —
    /// the recovery path for a consumer that dropped bytes past the broadcast
    /// buffer. We assert the broadcast carries the snapshot preamble + the
    /// seeded content, and that the resync dims are the UNCHANGED grid size
    /// (proving no resize happened).
    #[tokio::test]
    async fn resync_only_request_rebroadcasts_snapshot_without_resizing() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle =
                    TerminalActor::new_with_seed(80, 24, b"resync-only-marker").expect("seed");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let mut out = handle.output.subscribe();
                let join = tokio::task::spawn_local(bundle.actor.run());

                // resync_only: geometry fields are ignored, so the bogus 0x0
                // must NOT become the grid size.
                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 0,
                        rows: 0,
                        cell_px: None,
                        resync_clients: true,
                        resync_only: true,
                    })
                    .await
                    .expect("send resync_only");

                let mut acc: Vec<u8> = Vec::new();
                let mut resync_dims: Option<(u16, u16)> = None;
                let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
                while tokio::time::Instant::now() < deadline {
                    match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                        Ok(Ok(PaneOutput::Resync {
                            cols, rows, bytes, ..
                        })) => {
                            resync_dims = Some((cols, rows));
                            acc.extend_from_slice(&bytes);
                            if contains_subslice(&acc, b"\x1b[!p")
                                && contains_subslice(&acc, b"resync-only-marker")
                            {
                                break;
                            }
                        }
                        Ok(Ok(PaneOutput::Live(bytes))) => acc.extend_from_slice(&bytes),
                        Ok(Err(_)) => break,
                        Err(_) => tokio::task::yield_now().await,
                    }
                }

                assert_eq!(
                    resync_dims,
                    Some((80, 24)),
                    "resync_only must keep the grid size, not adopt the ignored 0x0",
                );
                assert!(
                    contains_subslice(&acc, b"\x1b[!p"),
                    "resync_only broadcast missing DECSTR snapshot preamble; got {:?}",
                    String::from_utf8_lossy(&acc),
                );
                assert!(
                    contains_subslice(&acc, b"resync-only-marker"),
                    "resync_only broadcast did not re-send grid content; got {:?}",
                    String::from_utf8_lossy(&acc),
                );

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// phux-8v1 drag fix: a STORM of rapid live resizes (a window drag)
    /// must COALESCE into a single resync snapshot, not one per resize.
    /// Without the debounce the client gets flooded with snapshots
    /// synthesized at successive widths, and a stale-width one corrupts
    /// the mirror (the duplicated-characters-while-dragging symptom).
    /// We count broadcasts carrying the snapshot preamble (`ESC [ ! p`);
    /// each resync is exactly one such message, so the count is the
    /// snapshot count regardless of any interleaved PTY output.
    #[tokio::test]
    async fn rapid_resizes_coalesce_into_one_resync_snapshot() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle =
                    TerminalActor::new_with_seed(80, 24, b"drag-marker").expect("seed");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let mut out = handle.output.subscribe();
                let join = tokio::task::spawn_local(bundle.actor.run());

                // Fire a storm of live resizes back-to-back, well within
                // the RESIZE_RESYNC_DEBOUNCE window.
                for w in [70u16, 60, 50, 60, 70, 80, 90, 100] {
                    handle
                        .resize
                        .send(ResizeRequest {
                            cols: w,
                            rows: 24,
                            cell_px: None,
                            resync_clients: true,
                            resync_only: false,
                        })
                        .await
                        .expect("send resize");
                }

                // Wait comfortably past the debounce so the single
                // coalesced snapshot has fired.
                tokio::time::sleep(RESIZE_RESYNC_DEBOUNCE * 4).await;

                // Count resync broadcasts. Debounced => exactly 1.
                // phux-3ns5: each resync is a `PaneOutput::Resync`, so the
                // variant itself is the count (no preamble sniffing needed).
                let mut snapshots = 0usize;
                loop {
                    match out.try_recv() {
                        Ok(PaneOutput::Resync { bytes, .. }) => {
                            debug_assert!(contains_subslice(&bytes, b"\x1b[!p"));
                            snapshots += 1;
                        }
                        // Live output and a lagged drop are both "not a
                        // resync" — skip and keep draining.
                        Ok(PaneOutput::Live(_))
                        | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
                assert_eq!(
                    snapshots, 1,
                    "a resize storm must coalesce into exactly one resync snapshot, got {snapshots}",
                );

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked");
            })
            .await;
    }

    /// Crash-hunt: a storm of *degenerate* resizes — `0x0`, `1x1`,
    /// `1x200`, `200x1`, a 1000x1000 monster, and repeated both-axes
    /// shrinks crossing the 1-cell clamp — must NOT panic the actor task.
    /// `handle_resize` clamps to a 1-cell minimum so a zero dimension never
    /// reaches libghostty; the both-axes-shrink overflow in the Zig
    /// `PageList.resizeCols` is covered by libghostty-vt 0.2.0. We assert
    /// the actor is still alive (the `join` unwrap surfaces a panicked task)
    /// and a final sane resize still applies.
    #[tokio::test]
    async fn degenerate_resize_storm_does_not_panic_actor() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = TerminalActor::new_with_seed(80, 24, b"crash-hunt").expect("seed");
                let handle = bundle.handle.clone();
                let token = bundle.token;
                let join = tokio::task::spawn_local(bundle.actor.run());

                let storm: &[(u16, u16)] = &[
                    (0, 0),
                    (1, 1),
                    (1, 200),
                    (200, 1),
                    (0, 0),
                    (1000, 1000),
                    (1, 1),
                    (3, 3),
                    (2, 2),
                    (1, 1),
                    (5, 1),
                    (1, 5),
                    (1, 1),
                ];
                for &(cols, rows) in storm {
                    handle
                        .resize
                        .send(ResizeRequest {
                            cols,
                            rows,
                            cell_px: None,
                            resync_clients: false,
                            resync_only: false,
                        })
                        .await
                        .expect("send resize");
                }
                // Let the actor drain the whole mailbox.
                for _ in 0..64 {
                    tokio::task::yield_now().await;
                }

                // A final sane resize must still take effect — proof the
                // actor survived and is processing, not wedged.
                handle
                    .resize
                    .send(ResizeRequest {
                        cols: 100,
                        rows: 30,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
                    })
                    .await
                    .expect("send final resize");
                for _ in 0..16 {
                    tokio::task::yield_now().await;
                }

                token.cancel();
                tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                    .await
                    .expect("actor did not exit after cancel")
                    .expect("actor task panicked under degenerate resize storm");
            })
            .await;
    }

    /// phux-y06 regression (crash-hunt): a degenerate resize storm that
    /// includes both-axes shrinks (e.g. real `80x24 -> 1x1`) issued as
    /// BARE single `resize()` calls must NOT abort libghostty's
    /// `PageList.resizeCols` with an integer overflow.
    ///
    /// libghostty's `PageList.resizeCols` once overflowed (panic in Zig →
    /// SIGABRT) when cols AND rows shrank in one `resize()` call. This test
    /// proves the engine fix — not any phux-side axis decomposition — carries
    /// the load. It feeds reflowable content, then drives the storm with a
    /// 1-cell clamp only (the same input hygiene `handle_resize` keeps),
    /// issuing each step as one direct `resize()`. It must survive every step
    /// and settle at the final size. (Run as a plain `GhosttyTerminal` test so
    /// a regression aborts THIS test, not a flaky e2e teardown.)
    #[test]
    fn resize_desync_then_both_shrink_does_not_overflow() {
        let mut term = GhosttyTerminal::new(TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 100,
        })
        .expect("term");
        // Enough scrollback content that a cols-reflow actually walks rows
        // (the overflow needs real content to reflow).
        for i in 0..300u32 {
            let line = format!("row-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
            term.vt_write(line.as_bytes());
        }

        // The minimal trigger: a 0x0 (fails, no-op) immediately followed by
        // a both-shrink to 1x1, then the wider degenerate storm.
        let storm: &[(u16, u16)] = &[
            (0, 0),
            (1, 1),
            (1, 200),
            (200, 1),
            (0, 0),
            (1000, 1000),
            (1, 1),
            (3, 3),
            (2, 2),
            (1, 1),
            (100, 30),
        ];
        for &(req_cols, req_rows) in storm {
            for i in 0..40u32 {
                let line = format!("interleave-{i}-bbbbbbbbbbbbbbbbbbbbbbbbbbbb\r\n");
                term.vt_write(line.as_bytes());
            }
            // Mirror `handle_resize`: 1-cell clamp (input hygiene) only,
            // then a BARE single resize() per step — no axis decomposition.
            // libghostty-vt 0.2.0 keeps the both-shrink steps from
            // overflowing.
            let cols = req_cols.max(1);
            let rows = req_rows.max(1);
            let _ = term.resize(cols, rows, 0, 0);
        }

        // Survived without SIGABRT; the grid settled at the final sane size.
        assert_eq!(term.cols().expect("cols"), 100);
        assert_eq!(term.rows().expect("rows"), 30);
    }

    /// Drain every `TerminalOutput` currently queued on `rx`, returning the
    /// concatenated payload bytes and the ordered list of `seq`s.
    fn drain_terminal_output(rx: &mut mpsc::Receiver<Outbound>) -> (Vec<u8>, Vec<u64>) {
        let mut bytes = Vec::new();
        let mut seqs = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            if let Outbound::Frame(FrameKind::TerminalOutput {
                seq, bytes: body, ..
            }) = frame
            {
                seqs.push(seq);
                bytes.extend_from_slice(&body);
            }
        }
        (bytes, seqs)
    }

    /// wave-hunt/server-lifecycle: a consumer whose outbound mailbox fills up
    /// under sustained output MUST NOT lose grid content. Once the client
    /// drains, every written marker must still be reconstructable from the
    /// delivered stream.
    ///
    /// Pre-fix this failed: `tick_emit` synthesized the delta (which commits
    /// the per-consumer reference to the just-rendered grid, emit-once) and
    /// THEN dropped the frame on a `Full` mailbox. The reference had already
    /// advanced past the dropped delta, so the next tick diffed against a
    /// reference that already included the dropped content and never
    /// re-emitted it — silent permanent content loss / mirror divergence.
    /// The fix reserves the outbound permit BEFORE synthesizing, so a full
    /// mailbox skips the consumer without advancing its reference.
    #[test]
    fn backpressured_consumer_loses_no_content_after_draining() {
        // More rounds than the mailbox holds so the tick's send hits `Full`.
        const ROUNDS: usize = 12;
        let bundle = TerminalActor::new(80, 24).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        let client = ClientId(1);
        // Tiny mailbox so a few ticks saturate it — same shape as the
        // production `DEFAULT_CLIENT_MAILBOX` pressure, smaller and faster.
        let (tx, mut rx) = mpsc::channel::<Outbound>(2);
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");

        // Write a distinct marker on its own line and tick to emit it,
        // WITHOUT draining the receiver. Every marker must survive.
        let markers: Vec<String> = (0..ROUNDS).map(|i| format!("MARK{i:03}=")).collect();
        for marker in &markers {
            actor.vt_write_for_test(format!("{marker}\r\n").as_bytes());
            actor.tick_emit();
        }

        // Drain, then keep ticking + draining so any content held back under
        // backpressure flows once there is room.
        let mut delivered = Vec::new();
        let (chunk, _seqs) = drain_terminal_output(&mut rx);
        delivered.extend_from_slice(&chunk);
        for _ in 0..ROUNDS * 2 {
            actor.tick_emit();
            let (chunk, _seqs) = drain_terminal_output(&mut rx);
            delivered.extend_from_slice(&chunk);
        }

        for marker in &markers {
            assert!(
                contains_subslice(&delivered, marker.as_bytes()),
                "marker {marker:?} never reached the consumer: content was lost \
                 under mailbox backpressure (reference advanced past a dropped \
                 frame). delivered={:?}",
                String::from_utf8_lossy(&delivered),
            );
        }
    }

    /// wave-hunt/server-lifecycle: the per-consumer monotonic `seq` must have
    /// no gaps in the delivered stream. A frame that is NOT shipped must NOT
    /// consume a `seq`. Pre-fix, `tick_emit` incremented `next_seq` and then
    /// dropped the frame on `Full`, burning a seq for a frame the consumer
    /// never saw — the client would observe a hole in the otherwise
    /// contiguous reliable-transport stream (SPEC §12.2) and could not
    /// distinguish loss from reorder.
    #[test]
    fn backpressured_consumer_sees_contiguous_seq_stream() {
        const ROUNDS: usize = 10;
        let bundle = TerminalActor::new(80, 24).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        let client = ClientId(1);
        let (tx, mut rx) = mpsc::channel::<Outbound>(2);
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");

        for i in 0..ROUNDS {
            actor.vt_write_for_test(format!("seqmark{i:03}\r\n").as_bytes());
            actor.tick_emit();
        }

        let mut all_seqs = Vec::new();
        let (_b, seqs) = drain_terminal_output(&mut rx);
        all_seqs.extend(seqs);
        for _ in 0..ROUNDS * 2 {
            actor.tick_emit();
            let (_b, seqs) = drain_terminal_output(&mut rx);
            all_seqs.extend(seqs);
        }

        assert!(
            !all_seqs.is_empty(),
            "expected at least one delivered frame"
        );
        for (idx, seq) in all_seqs.iter().enumerate() {
            let expected = u64::try_from(idx).expect("fits") + 1;
            assert_eq!(
                *seq, expected,
                "delivered seq stream must be contiguous from 1 with no gaps; \
                 a dropped-but-seq-burned frame leaves a hole. got={all_seqs:?}",
            );
        }
    }

    /// Naive subsequence search for test assertions on VT byte streams.
    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    // ---- phux-q0e.5: RTT-adaptive tick interval ----

    /// The EMA seeds on the first sample and converges toward a steady RTT
    /// across a handful of samples (TCP-RTO-style `α = 0.125`).
    #[test]
    fn rtt_estimator_seeds_then_converges() {
        let mut est = RttEstimator::default();
        assert_eq!(est.smoothed(), None, "no sample yet");

        // First sample seeds srtt directly.
        est.observe(std::time::Duration::from_millis(100));
        assert_eq!(
            est.smoothed(),
            Some(std::time::Duration::from_millis(100)),
            "first sample seeds srtt exactly",
        );

        // Feed a steady 100ms stream; srtt must stay put (no drift).
        for _ in 0..20 {
            est.observe(std::time::Duration::from_millis(100));
        }
        let srtt = est.smoothed().expect("has srtt");
        assert!(
            srtt.abs_diff(std::time::Duration::from_millis(100))
                < std::time::Duration::from_millis(1),
            "steady 100ms stream keeps srtt ~100ms, got {srtt:?}",
        );

        // Step the RTT up to 300ms; the EMA moves slowly (one step folds in
        // only α of the gap), then converges over many samples.
        let before = est.smoothed().expect("has srtt");
        est.observe(std::time::Duration::from_millis(300));
        let after_one = est.smoothed().expect("has srtt");
        assert!(
            after_one > before && after_one < std::time::Duration::from_millis(150),
            "one 300ms sample nudges srtt but does not jump to it: {before:?} -> {after_one:?}",
        );
        for _ in 0..100 {
            est.observe(std::time::Duration::from_millis(300));
        }
        let converged = est.smoothed().expect("has srtt");
        assert!(
            converged.abs_diff(std::time::Duration::from_millis(300))
                < std::time::Duration::from_millis(2),
            "srtt converges toward the new 300ms RTT, got {converged:?}",
        );
    }

    /// The adaptive interval is `RTT/2` clamped to [20ms, 200ms]: a near-zero
    /// RTT clamps to the 20ms floor (snappier than the 30ms default), and a
    /// huge RTT clamps to the 200ms ceiling.
    #[test]
    fn adaptive_interval_clamps_both_ends() {
        // Near-zero local RTT -> floor (50 Hz), strictly faster than the
        // fixed 33 Hz default this replaces.
        assert_eq!(
            adaptive_tick_interval(std::time::Duration::from_micros(10)),
            MIN_TICK_INTERVAL,
            "near-zero RTT clamps to the 20ms floor",
        );
        assert!(
            MIN_TICK_INTERVAL < DEFAULT_TICK_INTERVAL,
            "the floor must be snappier than the old fixed cadence",
        );

        // Mid-band: 80ms RTT -> 40ms tick (unclamped RTT/2).
        assert_eq!(
            adaptive_tick_interval(std::time::Duration::from_millis(80)),
            std::time::Duration::from_millis(40),
            "mid-band RTT maps to exactly RTT/2",
        );

        // Satellite-class RTT -> ceiling (5 Hz).
        assert_eq!(
            adaptive_tick_interval(std::time::Duration::from_secs(2)),
            MAX_TICK_INTERVAL,
            "huge RTT clamps to the 200ms ceiling",
        );
    }

    /// An estimator with no sample reports the cold-start default; once a
    /// sample lands, `desired_tick_interval` tracks the clamped RTT/2.
    #[test]
    fn desired_interval_defaults_then_adapts() {
        let mut est = RttEstimator::default();
        assert_eq!(
            est.desired_tick_interval(),
            DEFAULT_TICK_INTERVAL,
            "no sample -> cold-start default",
        );
        est.observe(std::time::Duration::from_millis(120));
        assert_eq!(
            est.desired_tick_interval(),
            std::time::Duration::from_millis(60),
            "after a 120ms sample -> 60ms tick",
        );
    }

    /// End-to-end through the actor: a `FRAME_ACK` measured against a large
    /// simulated transit time backs the shared cadence off toward the 200ms
    /// ceiling; a near-zero transit time pins it to the 20ms floor. Uses
    /// paused tokio time so the emit->ack gap is exact and deterministic.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn actor_cadence_backs_off_on_high_rtt_and_floors_on_low() {
        let bundle = TerminalActor::new(80, 24).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        // Slow peer: a write + tick emits seq=1 and stamps its emit instant.
        let slow = ClientId(1);
        let (tx_slow, _rx_slow) = mpsc::channel::<Outbound>(16);
        actor
            .register_consumer(slow, tx_slow, 11, false)
            .expect("register slow");
        assert_eq!(
            actor.adaptive_tick_interval_for_test(),
            DEFAULT_TICK_INTERVAL,
            "no sample yet -> cold-start cadence",
        );

        actor.vt_write_for_test(b"hello\r\n");
        actor.tick_emit();
        // Simulate a 400ms round trip before the client acks seq=1.
        tokio::time::advance(std::time::Duration::from_millis(400)).await;
        assert!(
            actor.on_frame_ack_for_test(slow, 1),
            "ack of an emitted seq produces an RTT sample",
        );
        // 400ms RTT -> 200ms RTT/2 -> clamps to the 200ms ceiling.
        assert_eq!(
            actor.adaptive_tick_interval_for_test(),
            MAX_TICK_INTERVAL,
            "high-RTT consumer backs the cadence off to the ceiling",
        );

        // Fast peer joins; the shared cadence is the MINIMUM desired, so the
        // near-zero-RTT peer pulls it back down to the floor regardless of
        // the slow peer.
        let fast = ClientId(2);
        let (tx_fast, _rx_fast) = mpsc::channel::<Outbound>(16);
        actor
            .register_consumer(fast, tx_fast, 12, false)
            .expect("register fast");
        actor.vt_write_for_test(b"world\r\n");
        actor.tick_emit();
        // Near-instant ack: advance time by a sub-millisecond sliver.
        tokio::time::advance(std::time::Duration::from_micros(50)).await;
        // Both consumers were emitted to on the tick above; the fast peer's
        // first emitted seq is 1.
        assert!(
            actor.on_frame_ack_for_test(fast, 1),
            "fast peer ack produces a sample",
        );
        assert_eq!(
            actor.adaptive_tick_interval_for_test(),
            MIN_TICK_INTERVAL,
            "the fastest consumer pins the shared cadence to the floor",
        );

        // The slow peer leaving must not regress the floor (fast peer still
        // present), and dropping the fast peer reverts to the cold-start
        // default (no samples left to consult).
        actor.unregister_consumer(slow);
        assert_eq!(
            actor.adaptive_tick_interval_for_test(),
            MIN_TICK_INTERVAL,
            "fast peer still present -> still at the floor",
        );
        actor.unregister_consumer(fast);
        assert_eq!(
            actor.adaptive_tick_interval_for_test(),
            DEFAULT_TICK_INTERVAL,
            "no consumers left -> cold-start default",
        );
    }

    /// An ack that matches no recorded emit instant (e.g. the consumer never
    /// had a frame shipped) yields no RTT sample and leaves the cadence at
    /// the default — the round-trip machinery is inert without an emission.
    #[test]
    fn ack_without_emit_instant_produces_no_sample() {
        let bundle = TerminalActor::new(80, 24).expect("new");
        let mut actor = bundle.actor;
        actor.enable_tick_emit_for_test();

        let client = ClientId(1);
        let (tx, _rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, false)
            .expect("register");

        // No tick_emit ran, so no emit instant was stamped. An ack here
        // advances last_acked_seq but cannot time a round trip.
        assert!(
            !actor.on_frame_ack_for_test(client, 5),
            "ack with no matching emit instant produces no RTT sample",
        );
        assert_eq!(
            actor.adaptive_tick_interval_for_test(),
            DEFAULT_TICK_INTERVAL,
            "cadence stays at the default without a sample",
        );
    }

    /// phux-ahk: a state-sync consumer that never sends `FRAME_ACK` must not
    /// grow `emit_instants` without bound. Ack-pruning never runs for it, so
    /// the per-tick insert is bounded only by the defensive
    /// [`MAX_EMIT_INSTANTS`] cap (oldest-evicted). Drive many more emitting
    /// ticks than the cap, never acking, and assert the map stays capped and
    /// retains the newest (highest-`seq`) samples rather than the stale ones.
    #[test]
    fn emit_instants_is_capped_for_never_acking_consumer() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let client = ClientId(1);
        let (tx, mut rx) = dummy_outbound();
        actor
            .register_consumer(client, tx, 11, true)
            .expect("register");

        // Far more emitting ticks than the cap. Distinct content each tick
        // keeps the grid dirty so the diff is non-empty and the tick actually
        // emits (and inserts). Drain the mailbox each tick so the send keeps
        // succeeding — a full mailbox would backpressure and skip the insert,
        // hiding the growth this test pins.
        let ticks = MAX_EMIT_INSTANTS + 64;
        for i in 0..ticks {
            actor.vt_write_for_test(&[b'a' + u8::try_from(i % 26).expect("0..26 fits u8")]);
            actor.tick_emit();
            while rx.try_recv().is_ok() {}
        }

        let state = actor.consumer_state(client).expect("state present");
        assert!(
            state.emit_instants.len() <= MAX_EMIT_INSTANTS,
            "emit_instants must stay capped at {} for a never-acking consumer; got {}",
            MAX_EMIT_INSTANTS,
            state.emit_instants.len(),
        );
        // Eviction drops the oldest seqs, so emission must actually have run
        // past the cap (otherwise this test proves nothing) and the lowest
        // retained key is well above the first seq.
        let lowest = *state.emit_instants.keys().next().expect("non-empty map");
        assert!(
            lowest > 1,
            "oldest emit instants should have been evicted; lowest retained seq = {lowest}",
        );
    }

    // --- agent-detector dirty-flag accounting (ADR-0046) -------------------

    /// `agent_dirty_since_detect` is the ONLY record that the grid changed
    /// since the detector last looked. `detect_tick` must consume it only on a
    /// tick that actually scanned.
    ///
    /// While no agent is identified, `wants_screen` is unconditionally false —
    /// there is nothing to derive against. So a `detect_tick` in that window
    /// performs no scan, and eating the flag there discards every grid mutation
    /// the agent made before we noticed it existed: the permission dialog it
    /// painted, and then went silent behind, is exactly such a mutation. The
    /// detector then derives `idle` from a screen it never read and latches
    /// there, because `wants_screen` sees `current == Some(Idle)` and never
    /// asks for the scan that would correct it.
    #[test]
    fn detect_tick_keeps_the_dirty_flag_when_it_performs_no_scan() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        actor.agent_detect = Some(AgentDetector::new(
            crate::agent_detect::rules::global(),
            std::time::Instant::now(),
        ));

        // The agent paints. No agent is identified yet (no PTY, so identity
        // never resolves), so this tick cannot scan.
        actor.vt_write_for_test(b"a permission dialog");
        assert!(actor.agent_dirty_since_detect, "the grid changed");

        assert!(actor.detect_tick().is_some(), "the detector ran");
        assert!(
            actor.agent_dirty_since_detect,
            "a tick that performed no scan must not consume the evidence that a scan is owed",
        );
    }

    /// The converse, so the flag is not simply never cleared: once an agent IS
    /// identified, the scan runs and consumes the flag — which is what keeps
    /// the steady state cheap.
    #[test]
    fn detect_tick_consumes_the_dirty_flag_when_it_scans() {
        let bundle = TerminalActor::new(20, 5).expect("new");
        let mut actor = bundle.actor;
        let now = std::time::Instant::now();
        let mut detector = AgentDetector::new(crate::agent_detect::rules::global(), now);
        detector.force_identity("claude", now);
        actor.agent_detect = Some(detector);

        actor.vt_write_for_test(b"some output");
        assert!(actor.agent_dirty_since_detect);

        assert!(actor.detect_tick().is_some(), "the detector ran");
        assert!(
            !actor.agent_dirty_since_detect,
            "a scan consumes the flag; otherwise every tick re-projects the grid forever",
        );
    }

    #[test]
    fn input_snapshot_publishes_after_seed_output_and_resize() {
        let seeded = TerminalActor::new_with_seed(80, 24, b"\x1b[?2004h").expect("seeded");
        assert!(seeded.handle.input_snapshot.borrow().bracketed_paste);

        let mut actor = seeded.actor;
        let mut snapshots = seeded.handle.input_snapshot;
        snapshots.borrow_and_update();
        actor.vt_write_for_test(b"\x1b[?1004h");
        assert!(snapshots.has_changed().expect("publisher alive"));
        assert!(snapshots.borrow_and_update().focus_reporting);

        actor.handle_resize(101, 39, Some((11, 19)));
        assert!(snapshots.has_changed().expect("publisher alive"));
        let resized = *snapshots.borrow_and_update();
        assert_eq!((resized.cols, resized.rows), (101, 39));
        assert_eq!(resized.cell_px, (11, 19));
    }
}
