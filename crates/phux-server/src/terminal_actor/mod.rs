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
use std::collections::{HashMap, HashSet, VecDeque};
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
use crate::mailbox::{Outbound, TerminalInput};

mod construct;
mod consumers;
mod events;
mod io;
mod native;
mod osc133;
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

pub use requests::*;
pub use spawn::*;
pub use sync::*;
pub use tick::*;

/// Line half of [`DEFAULT_SCROLLBACK`]: a tmux-style mid-range value.
const DEFAULT_MAX_SCROLLBACK: u32 = 10_000;

/// Per-pane scrollback bounds used by the no-config convenience constructors
/// ([`TerminalActor::new`] / [`TerminalActor::new_with_command`]). The runtime
/// path overrides both halves with `defaults.history-limit` and
/// `defaults.history-bytes` via [`TerminalActor::build_with_token`]; the byte
/// half is the shipped schema default, because on any but a narrow grid it is
/// the bound that actually binds (ADR-0094).
const DEFAULT_SCROLLBACK: phux_config::ScrollbackLimits =
    phux_config::ScrollbackLimits::new(DEFAULT_MAX_SCROLLBACK, phux_config::DEFAULT_HISTORY_BYTES);

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

/// Maximum independent native history cuts retained for one terminal.
///
/// The release contract exercises eight simultaneous clients; keeping this
/// fixed preserves a hard per-terminal memory/lease bound.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const MAX_NATIVE_HISTORY_CLIENTS: usize = 8;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const MAX_NATIVE_REPLAY_BYTES: usize = 4 * 1024 * 1024;

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
        // Ground-state fast skip, for the same reason as
        // [`osc133::Osc133Scanner::feed`]: in `Ground` the machine reacts to
        // exactly two bytes (`ESC` and 8-bit `OSC`), so a chunk of plain
        // output is a long run of no-ops that used to be stepped one match
        // arm at a time. Resuming mid-OSC still walks every byte, because
        // those bytes are the OSC payload.
        let bytes = if matches!(self.state, ColorQueryState::Ground) {
            let Some(start) = memchr::memchr2(b'\x1b', 0x9d, bytes) else {
                return;
            };
            &bytes[start..]
        } else {
            bytes
        };
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
/// The v1 ask-trigger is OSC-driven. The safe libghostty-vt wrapper does not
/// expose OSC 9 / OSC 777 desktop notifications; phux's bounded raw scanner
/// handles OSC 9;4 progress only. The title therefore remains the explicit ask
/// signal. An agent that has blocked for input sets its title to:
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
/// edge-filter: a re-asserted identical marker is not a new report.
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

/// Ceiling on how long the pane-kill path will wait to reap the child after
/// it has been signalled, and on how long it will wait for either bridge
/// thread to exit (phux-l96p.12).
///
/// Reached only when something has already gone wrong: by this point the
/// child has been sent `SIGHUP` and then `SIGKILL`, so it should be a zombie
/// within a poll or two. The budget exists because the alternative — a
/// blocking `waitpid`, or a bare `JoinHandle::join` — turns "one child we
/// failed to kill" into "every pane on the runtime is frozen" (ADR-0003).
/// Generous relative to [`PANE_KILL_POLL`] so ordinary scheduling delay never
/// trips it.
///
/// **What expiry costs, stated honestly.** Giving up here is not free, and
/// nothing downstream cleans up after it:
///
/// * The child is handed to a detached thread that blocks in `waitpid`
///   (`io::spawn_detached_reaper`). phux installs no `SIGCHLD` handler and has
///   no central reaper — the adopted-PTY child collects only on an explicit
///   poll — so without that thread the process would stay a zombie for the
///   lifetime of the server. One parked thread is the cheaper leak.
/// * A bridge thread that misses the budget is detached, not stopped. Rust
///   cannot cancel a thread, so it lives until its descriptor closes. That is
///   at worst one parked thread per abandoned pane.
///
/// Both are bounded leaks traded against an unbounded stall, which is the
/// right trade on a shared current-thread runtime; neither is a leak we would
/// accept if the deadlock were avoidable some other way.
const PANE_KILL_REAP_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const NATIVE_HISTORY_TTL: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
struct NativeCursorOwner {
    cursor: crate::native_state::OpaqueHistoryCursor,
    record_index: usize,
    touched: tokio::time::Instant,
    next_page_seq: u64,
    terminal_id: phux_protocol::ids::TerminalId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
struct PendingNativeBootstrap {
    capture: Option<crate::native_state::NativeManagedCapture<'static>>,
    waiters: Vec<NativeBootstrapRequest>,
    records: Vec<Bytes>,
    retained_bytes: usize,
    capture_bytes: usize,
    scratch: Vec<u8>,
    max_chunks: usize,
    chunk_bytes: usize,
    base_seq: u64,
    chunk_count: usize,
    limits: phux_protocol::caps::BootstrapLimits,
    replay: VecDeque<(u64, Bytes)>,
    replay_bytes: usize,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
struct NativePublicationGeneration {
    base_seq: u64,
    replay: VecDeque<(u64, Bytes)>,
    replay_bytes: usize,
    waiting: HashSet<u64>,
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_NATIVE_HOST_ALLOC: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static PANIC_NEXT_NATIVE_HOST_ALLOC: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn reserve_native_bytes(capacity: usize) -> Result<Vec<u8>, crate::native_state::NativeStateError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(test)]
        assert!(
            !PANIC_NEXT_NATIVE_HOST_ALLOC.with(|panic| panic.replace(false)),
            "injected native host allocation panic"
        );
        #[cfg(test)]
        if FAIL_NEXT_NATIVE_HOST_ALLOC.with(|fail| fail.replace(false)) {
            return Err(crate::native_state::NativeStateError::OutOfMemory);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| crate::native_state::NativeStateError::OutOfMemory)?;
        Ok(bytes)
    }))
    .unwrap_or(Err(crate::native_state::NativeStateError::OutOfMemory))
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn native_step_bytes(
    capture_bytes: usize,
    retained_bytes: usize,
    chunk_bytes: usize,
) -> Result<usize, crate::native_state::NativeStateError> {
    capture_bytes
        .checked_sub(retained_bytes)
        .filter(|bytes| *bytes != 0)
        .map(|remaining| remaining.min(chunk_bytes))
        .ok_or(crate::native_state::NativeStateError::LimitExceeded)
}

#[derive(Debug)]
enum CanonicalTerminal {
    Plain(Option<GhosttyTerminal<'static, 'static>>),
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    Native(crate::native_state::NativeTerminalManager),
}

impl CanonicalTerminal {
    #[allow(
        clippy::expect_used,
        reason = "Plain is temporarily None only while native_manager holds the actor-local mutable borrow"
    )]
    const fn terminal(&self) -> &GhosttyTerminal<'static, 'static> {
        match self {
            Self::Plain(terminal) => terminal
                .as_ref()
                .expect("plain canonical terminal is present"),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.terminal(),
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "Plain is temporarily None only while native_manager holds the actor-local mutable borrow"
    )]
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

    #[allow(
        clippy::expect_used,
        reason = "Plain is temporarily None only while native_manager holds the actor-local mutable borrow"
    )]
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
            match crate::native_state::NativeTerminalManager::new(
                terminal,
                MAX_NATIVE_HISTORY_CLIENTS,
            ) {
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
    Publication(NativePublicationRequest),
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
    publication: mpsc::Receiver<NativePublicationRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    history: mpsc::Receiver<NativeHistoryRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    release: mpsc::Receiver<NativeReleaseRequest>,
}

impl NativeRequestReceivers {
    async fn recv(&mut self) -> NativeActorRequest {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            tokio::select! {
                Some(request) = self.bootstrap.recv() => NativeActorRequest::Bootstrap(request),
                Some(request) = self.publication.recv() => NativeActorRequest::Publication(request),
                Some(request) = self.history.recv() => NativeActorRequest::History(request),
                Some(request) = self.release.recv() => NativeActorRequest::Release(request),
                else => std::future::pending().await,
            }
        }
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        {
            let _disabled = NativeActorRequest::Disabled;
            std::future::pending::<NativeActorRequest>().await
        }
    }
}

enum NativeOrPty {
    Native(NativeActorRequest),
    Pty(Option<PtyEvent>),
}

async fn recv_native_or_pty(
    native: &mut NativeRequestReceivers,
    pty: Option<&mut mpsc::Receiver<PtyEvent>>,
    prefer_native: bool,
) -> NativeOrPty {
    if prefer_native {
        tokio::select! {
            biased;
            request = native.recv() => NativeOrPty::Native(request),
            event = recv_or_pending(pty) => NativeOrPty::Pty(event),
        }
    } else {
        tokio::select! {
            biased;
            event = recv_or_pending(pty) => NativeOrPty::Pty(event),
            request = native.recv() => NativeOrPty::Native(request),
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
    /// When input bytes were last handed to the PTY writer, consumed by the
    /// next output burst to sample `echo.server` (`crate::perf`).
    last_input_at: std::cell::Cell<Option<std::time::Instant>>,
    /// When this pane last produced output; gates `echo.server` arming to a
    /// pane that was quiet (`crate::perf::ECHO_QUIET_WINDOW`).
    last_output_at: std::cell::Cell<Option<std::time::Instant>>,
    /// Actor-global raw PTY sequence; never resets across bootstrap generations.
    raw_seq: u64,
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
    native_cursor_owners: HashMap<u64, NativeCursorOwner>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pending_native_bootstrap: Option<PendingNativeBootstrap>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_bootstrap_backlog: VecDeque<NativeBootstrapRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_publications:
        HashMap<crate::native_state::OpaqueHistoryCursor, NativePublicationGeneration>,
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
    pty_rx: Option<mpsc::Receiver<PtyEvent>>,
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
    /// Latest OSC 9;4 payload, mirrored from the raw PTY stream for detection.
    last_progress: String,
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
    /// Last in-pane "ask" marker observed — the actor's mirror of the
    /// `phux-ask` title sentinel, and a transport edge filter only.
    ///
    /// The v1 ask-trigger is OSC-driven: an in-pane agent signals a pending
    /// human-answerable question by setting the terminal title (OSC 0 / OSC 2)
    /// to a `phux-ask` sentinel (see [`AskMarker`]). The actor does not decide
    /// whether that reaches a client: it reports each marker *change* — a new
    /// or changed marker, and the pane retitling away from one — as an
    /// [`AgentDetectEvent::AskSentinel`], and [`crate::agent_asked`] out in
    /// `ServerState` ranks it against the other ADR-0036 sources and owns the
    /// coalescing. This field exists so a pane sitting on a stable `phux-ask`
    /// title does not push a message per PTY chunk; `None` means the pane is
    /// not currently displaying a marker.
    ///
    /// The raw scanner mirrors OSC 9;4 for state detection, but does not treat
    /// generic OSC 9 / OSC 777 desktop notifications as asks. The title
    /// sentinel remains the explicit v1 ask signal.
    last_ask: Option<AskMarker>,
    /// Whether an ask edge was derived but refused by a full agent-state
    /// sink, so the next PTY chunk must re-derive and retry it.
    ///
    /// The retry used to be implicit: the marker was re-parsed on every
    /// chunk, so a stale `last_ask` simply re-attempted next time. Parsing is
    /// now gated on a title change (the only thing that can move the answer),
    /// which makes the owed retry something the actor has to remember rather
    /// than rediscover.
    ask_retry_owed: bool,
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
    /// Incremental OSC scanner over the raw PTY byte stream. Sources
    /// `command_started` / `command_finished` (with the
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
