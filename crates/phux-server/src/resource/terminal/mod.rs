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
use libghostty_vt::terminal::SizeReportSize;
use libghostty_vt::{RenderState, Terminal as GhosttyTerminal};
use phux_protocol::ClientId;
use phux_protocol::wire::frame::{
    AgentEvent, ControlAction, FrameKind, ResourceLifecycle, TerminalSignal,
};
use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::{mpsc, oneshot, watch};
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
use crate::resource::{ResourceCore, ResourceFacetHandle, ResourceHandle, ResourceKind};

mod construct;
mod consumers;
mod events;
mod fd_shrink;
mod io;
mod native;
mod osc133;
mod park;
mod process_facet;
pub(crate) mod program_status;
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

/// Finite history replay for resource attachment and replacement generations.
pub(crate) const DEFAULT_REPLAY_SCROLLBACK_LINES: u32 = 1_000;

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

/// Streaming recognizer for `OSC 10/11 ; ? ST` color queries, which the
/// pinned engine does not answer; the actor answers from canonical state.
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
        // In `Ground` only ESC and 8-bit OSC matter, so skip plain runs with
        // `memchr` (re-armed on every return to `Ground`). Mid-OSC bytes are
        // payload and are walked.
        let mut index = 0;
        while index < bytes.len() {
            if matches!(self.state, ColorQueryState::Ground) {
                let Some(start) = memchr::memchr2(b'\x1b', 0x9d, &bytes[index..]) else {
                    return;
                };
                index += start;
            }
            let byte = bytes[index];
            index += 1;
            match self.state {
                ColorQueryState::Ground => match byte {
                    b'\x1b' => self.state = ColorQueryState::Escape,
                    0x9d => self.start_osc(),
                    _ => {}
                },
                ColorQueryState::Escape => match byte {
                    b']' => self.start_osc(),
                    b'\x1b' => {}
                    _ => self.state = ColorQueryState::Ground,
                },
                ColorQueryState::Osc => match byte {
                    b'\x07' | 0x9c => self.finish_osc(&mut on_query),
                    b'\x1b' => self.state = ColorQueryState::OscEscape,
                    value => self.push_payload(value),
                },
                ColorQueryState::OscEscape => {
                    if byte == b'\\' {
                        self.finish_osc(&mut on_query);
                    } else {
                        // An unrecognized OSC: find its terminator, never
                        // treat its suffix as a query.
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

#[cfg(test)]
mod color_query_tests {
    use super::ColorQueryScanner;

    fn queries(chunks: &[&[u8]]) -> Vec<u8> {
        let mut scanner = ColorQueryScanner::default();
        let mut seen = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, |selector| seen.push(selector));
        }
        seen
    }

    #[test]
    fn recognises_both_query_forms_and_split_chunks() {
        assert_eq!(queries(&[b"\x1b]10;?\x07"]), vec![10]);
        assert_eq!(queries(&[b"\x9d11;?\x9c"]), vec![11]);
        assert_eq!(queries(&[b"\x1b]1", b"1;", b"?\x1b\\"]), vec![11]);
        assert_eq!(
            queries(&[b"\x1b]10;#ff0000\x07\x1b]12;?\x07"]),
            Vec::<u8>::new()
        );
    }

    /// Queries separated by long plain runs are all answered.
    #[test]
    fn the_skip_rearms_after_each_return_to_ground() {
        let mut chunk = vec![b'x'; 100_000];
        chunk.extend_from_slice(b"\x1b]10;?\x07");
        chunk.extend(std::iter::repeat_n(b'y', 100_000));
        chunk.extend_from_slice(b"\x1b[0m");
        chunk.extend(std::iter::repeat_n(b'z', 100_000));
        chunk.extend_from_slice(b"\x1b]11;?\x1b\\");
        assert_eq!(queries(&[&chunk]), vec![10, 11]);
    }
}

fn color_query_reply(selector: u8, color: libghostty_vt::style::RgbColor) -> Vec<u8> {
    let r = u16::from(color.r) * 0x101;
    let g = u16::from(color.g) * 0x101;
    let b = u16::from(color.b) * 0x101;
    format!("\x1b]{selector};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\").into_bytes()
}

/// Title prefix an in-pane agent sets (OSC 0/2) to ask a human a question:
///
/// ```text
/// ESC ] 2 ; phux-ask:<question>                         ST
/// ESC ] 2 ; phux-ask[<id>]:<question>                   ST
/// ESC ] 2 ; phux-ask[<id>]:<question>?s=opt1|opt2|opt3  ST
/// ```
///
/// Retitling away from a `phux-ask` title clears the ask.
const ASK_TITLE_PREFIX: &str = "phux-ask";

/// A parsed `phux-ask` title marker. Equality is by content so the actor can
/// edge-filter re-asserted markers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AskMarker {
    /// Correlation id; empty when the title omits `[id]`.
    id: String,
    /// The question text presented to the human.
    question: String,
    /// Suggested answers, in presentation order; empty when none were given.
    suggestions: Vec<String>,
}

impl AskMarker {
    /// Parse a `phux-ask` title (see [`ASK_TITLE_PREFIX`]); `None` for any
    /// other title, including a bare `phux-ask` without `:`.
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

/// Max ready PTY chunks coalesced into one `vt_write` + broadcast per pump
/// wakeup, so an unbroken stream cannot monopolize the loop.
const MAX_PTY_COALESCE: usize = 64;

/// Byte cap on one coalesced `vt_write`: each is a synchronous parse that
/// blocks the loop, so this bounds how long a queued keystroke waits. The
/// parser is streaming, so splitting here loses nothing.
pub(crate) const MAX_PTY_COALESCE_BYTES: usize = 48 * 1024;

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
/// One native checkpoint binding: one client's pump on one stream. Two pumps
/// from one client use different streams; only recapture of the same pair
/// tombstones the prior generation.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NativeCursorKey {
    owner: u64,
    stream_id: phux_protocol::ids::StreamId,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
impl NativeCursorKey {
    const fn new(owner: u64, stream_id: phux_protocol::ids::StreamId) -> Self {
        Self { owner, stream_id }
    }
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
struct NativeCursorOwner {
    cursor: crate::native_state::OpaqueHistoryCursor,
    record_index: usize,
    touched: tokio::time::Instant,
    next_page_seq: u64,
    terminal_id: phux_protocol::ids::ResourceId,
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
    started_at: tokio::time::Instant,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
struct PendingNativeHistory {
    request: NativeHistoryRequest,
    started_at: tokio::time::Instant,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
struct NativePublicationGeneration {
    base_seq: u64,
    replay: VecDeque<(u64, Bytes)>,
    replay_bytes: usize,
    waiting: HashSet<NativeCursorKey>,
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
    /// The canonical terminal, or `None` while it is on loan (to the native
    /// manager or a snapshot capture); callers degrade rather than abort.
    pub(super) const fn try_terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match self {
            Self::Plain(terminal) => terminal.as_ref(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.try_terminal(),
        }
    }

    /// Degrades rather than aborts if the terminal is on loan.
    fn vt_write(&mut self, bytes: &[u8]) {
        match self {
            Self::Plain(Some(terminal)) => terminal.vt_write(bytes),
            Self::Plain(None) => {
                tracing::error!(
                    bytes = bytes.len(),
                    "vt_write with the plain canonical terminal taken; dropping the write"
                );
            }
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.vt_write(bytes),
        }
    }

    /// Refuses rather than aborts when the terminal is taken.
    fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        match self {
            Self::Plain(Some(terminal)) => {
                terminal.resize(cols, rows, cell_width_px, cell_height_px)
            }
            Self::Plain(None) => {
                tracing::error!("resize with the plain canonical terminal taken; refusing it");
                Err(libghostty_vt::error::Error::InvalidValue)
            }
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.resize(cols, rows, cell_width_px, cell_height_px),
        }
    }

    fn reset_for_new_child(&mut self) {
        match self {
            Self::Plain(Some(terminal)) => terminal.reset(),
            Self::Plain(None) => {}
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(manager) => manager.reset(),
        }
    }

    fn reinstall_pty_write(
        &mut self,
        size_report: &Rc<Cell<SizeReportSize>>,
        pty_tx: Option<&mpsc::Sender<EncodedInputRequest>>,
    ) -> Result<(), TerminalActorError> {
        match self {
            Self::Plain(Some(terminal)) => {
                TerminalActor::install_effects(terminal, size_report, pty_tx)
            }
            Self::Plain(None) => Ok(()),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            Self::Native(_) => Ok(()),
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
    /// Native checkpoint bindings keyed by `(owner, stream_id)`.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_cursor_owners: HashMap<NativeCursorKey, NativeCursorOwner>,
    /// Native pumps the last reflow tombstoned, owed a resync.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    reflow_tombstoned: Vec<crate::resource::ResyncTarget>,
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
    event_sink: Option<crate::resource::event_sink::EventSink>,
    /// Last OSC 0/2 title, for `title_changed`. Refreshed on every chunk even
    /// if unwatched: the detector reads it (ADR-0046).
    last_title: String,
    /// Latest OSC 9;4 payload, mirrored from the raw PTY stream for detection.
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
    agent_session_append: Option<mpsc::Sender<crate::resource::agent_session::AppendRequest>>,
    /// Grid-mutation flag for the detector's slower tick; distinct from
    /// `terminal_dirty_since_tick`, which `tick_emit` clears far more often.
    agent_dirty_since_detect: bool,
    /// Last `phux-ask` title marker, an edge filter so a stable title does
    /// not report per chunk. Changes go out as
    /// [`AgentDetectEvent::AskSentinel`]; [`crate::agent_asked`] ranks them.
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
    /// OSC 133 scanner over raw PTY bytes (keeps the `D` exit code the grid
    /// drops); survives marks split across chunks.
    osc133: osc133::Osc133Scanner,
    /// OSC 133 prompt state behind the `process.prompt` facet.
    prompt: osc133::PromptTracker,
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
