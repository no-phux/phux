//! Control-plane command, result, and agent-event types — SPEC §5
//! (phux-k61 / ADR-0021) and SPEC §7.5 (phux-y2t / ADR-0022).

use crate::ids::{ClientId, FileUploadId, GroupId, InputOperationId, TerminalId};
use crate::input::InputEvent;
use crate::wire::info::SessionSnapshot;

use super::ErrorCode;

// -----------------------------------------------------------------------------
// Control-plane command types — SPEC §5 (phux-k61 / ADR-0021).
// -----------------------------------------------------------------------------

/// Semantic event type discriminant for filtering in `SubscribeTerminalEvents`.
/// Enables clients to subscribe only to event classes they care about
/// (e.g., command lifecycle without grid chatter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalEventType {
    /// Shell state transition (awaiting input → running → idle).
    ShellStateChanged = 0,
    /// Command started (OSC-133 B marker or equivalent).
    CommandStarted = 1,
    /// Command exited with exit code (OSC-133 D marker).
    CommandEnded = 2,
    /// Output arrived on terminal (PTY bytes detected).
    OutputReceived = 3,
    /// Shell prompt ready for input (no output + OSC-133 C or heuristic).
    PromptReady = 4,
    /// Grid mutated (scroll, output, cursor, clear).
    GridChanged = 5,
    /// Working directory changed.
    CwdChanged = 6,
}

impl TerminalEventType {
    /// Convert to wire byte representation.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Convert from wire byte representation.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::ShellStateChanged),
            1 => Some(Self::CommandStarted),
            2 => Some(Self::CommandEnded),
            3 => Some(Self::OutputReceived),
            4 => Some(Self::PromptReady),
            5 => Some(Self::GridChanged),
            6 => Some(Self::CwdChanged),
            _ => None,
        }
    }
}

/// Scope argument for [`Command::GetState`] (SPEC §5.1).
///
/// `#[non_exhaustive]`: v0.1 exposes only `Server` (the whole-server
/// snapshot, which is what `phux ls` and client-side selector resolution
/// need). Narrower scopes (a single Group, a single Terminal) are
/// additive minor changes when L2 lands — see [ADR-0021](../../../ADR/0021-control-plane-commands.md).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateScope {
    /// Snapshot the entire server (every Terminal the caller may see).
    Server,
}

/// Acquisition mode for [`Command::AcquireInput`] (ADR-0033).
///
/// `Cooperative` grants the input lease only if the Terminal is currently
/// `Open` (unheld) — a polite request that fails with
/// [`ErrorCode::InputLeaseHeld`] if someone else has the wheel.
/// `Seize` preempts the current holder unconditionally — the supervisory
/// "take the wheel now."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputMode {
    /// Grant only if the lease is free; otherwise refuse.
    Cooperative = 0,
    /// Preempt the current holder.
    Seize = 1,
}

impl InputMode {
    /// Wire byte for this mode.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decode from the wire byte; `None` for unknown values.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Cooperative),
            1 => Some(Self::Seize),
            _ => None,
        }
    }
}

/// A POSIX signal to deliver to a Terminal's process group via
/// [`Command::SignalTerminal`] (ADR-0033).
///
/// Distinct from `KILL_TERMINAL` (which removes the pane): these signal the
/// *process* and leave the pane addressable for the post-mortem.
/// `Freeze`/`Resume` is the reversible brake — SIGSTOP halts the agent
/// mid-step, SIGCONT lets it run again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalSignal {
    /// SIGINT — the Ctrl-C equivalent; lets the process clean up.
    Interrupt = 0,
    /// SIGSTOP — pause the process group; fully reversible via `Resume`.
    Freeze = 1,
    /// SIGCONT — resume a frozen process group.
    Resume = 2,
    /// SIGTERM — request graceful termination.
    Terminate = 3,
    /// SIGKILL — force termination.
    Kill = 4,
}

impl TerminalSignal {
    /// Wire byte for this signal.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decode from the wire byte; `None` for unknown values.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Interrupt),
            1 => Some(Self::Freeze),
            2 => Some(Self::Resume),
            3 => Some(Self::Terminate),
            4 => Some(Self::Kill),
            _ => None,
        }
    }
}

/// Lifecycle evidence supplied by an integration hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportedAgentState {
    /// A turn began or resumed.
    Working = 0,
    /// The agent is waiting for human input.
    Blocked = 1,
    /// A turn completed.
    Done = 2,
}

impl ReportedAgentState {
    /// Wire byte for this state.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decode from the wire byte; `None` for unknown values.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Working),
            1 => Some(Self::Blocked),
            2 => Some(Self::Done),
            _ => None,
        }
    }
}

/// Process lifecycle state of a Terminal, carried by
/// [`AgentEvent::TerminalControl`] (ADR-0033).
///
/// `Exited`'s process exit status rides alongside in the event body as an
/// `Option<i32>` (the same shape `TERMINAL_CLOSED.exit_status` uses), so this
/// enum stays a flat discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalLifecycle {
    /// The process group is running normally.
    Running = 0,
    /// The process group is stopped (SIGSTOP); resumable.
    Frozen = 1,
    /// The process exited; the accompanying `exit_status` carries the code.
    Exited = 2,
}

impl TerminalLifecycle {
    /// Wire byte for this lifecycle state.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decode from the wire byte; `None` for unknown values.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Running),
            1 => Some(Self::Frozen),
            2 => Some(Self::Exited),
            _ => None,
        }
    }
}

/// The supervisory action that produced an [`AgentEvent::TerminalControl`]
/// broadcast (ADR-0033).
///
/// Names *what just happened* so consumers can render a log line and the audit
/// trail can record an intent, not just a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlAction {
    /// An input lease was granted to a previously-free Terminal.
    Acquired = 0,
    /// An input lease was taken from a prior holder (`Seize`).
    Seized = 1,
    /// An input lease was released back to `Open`.
    Released = 2,
    /// SIGINT was delivered.
    Interrupted = 3,
    /// SIGSTOP was delivered; the process group is now frozen.
    Frozen = 4,
    /// SIGCONT was delivered; the process group resumed.
    Resumed = 5,
    /// SIGTERM was delivered.
    Terminated = 6,
    /// SIGKILL was delivered.
    Killed = 7,
    /// The process exited (natural or post-signal); lifecycle is now `Exited`.
    Exited = 8,
}

impl ControlAction {
    /// Wire byte for this action.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decode from the wire byte; `None` for unknown values.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Acquired),
            1 => Some(Self::Seized),
            2 => Some(Self::Released),
            3 => Some(Self::Interrupted),
            4 => Some(Self::Frozen),
            5 => Some(Self::Resumed),
            6 => Some(Self::Terminated),
            7 => Some(Self::Killed),
            8 => Some(Self::Exited),
            _ => None,
        }
    }
}

/// A typed control-plane command carried by [`FrameKind::Command`](super::FrameKind::Command) (SPEC §5.1).
///
/// `#[non_exhaustive]`: the spec catalog has seven L1 commands; v0.1 wires
/// the ones the CLI needs — `KILL_TERMINAL`, `GET_STATE`, the
/// side-effect-free `GET_SCREEN` (ADR-0021 §3, ADR-0022 §5), the appended
/// `ROUTE_INPUT` write counterpart, and `KILL_TERMINALS`, the atomic
/// multi-terminal teardown the v0.3.0 "Option B" re-tier left in place of
/// the dissolved L2 lifecycle verbs (ADR-0019 / ADR-0027). Unknown wire
/// tags surface as [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue) rather than coercing
/// to a placeholder.
///
/// Only `PartialEq` (not `Eq`): `RouteInput` carries a [`MouseEvent`](crate::input::mouse::MouseEvent) whose
/// coordinates are not `Eq`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Command {
    /// Subscribe the calling client to one Terminal's content stream
    /// (SPEC §5.1 `ATTACH_TERMINAL`, phux-v45.7): the server registers the
    /// caller as an output subscriber, primes it with a fresh profile-selected
    /// bootstrap generation, and streams generation-bound `TERMINAL_OUTPUT`
    /// from then on — the per-Terminal interactive attach without a
    /// session-scoped `ATTACH` handshake.
    /// Re-attaching replaces the generation without duplicating the stream.
    /// It does NOT resize the Terminal (no viewport rides the command);
    /// callers that want their geometry applied follow with
    /// `TERMINAL_RESIZE`. The catalog's `role_policy` field is not yet
    /// encoded; absence means `{ PRIMARY, takeover: NEVER }` (SPEC §8.1).
    /// Reply: `COMMAND_RESULT { Ok }` (the snapshot MAY precede it, per
    /// SPEC §5 command/stream interleaving), or
    /// `Error { TerminalNotFound }`. This is the verb a federation hub
    /// relays for two-hop attach (ADR-0007 §4, L1 §9.1).
    AttachTerminal {
        /// The Terminal whose content stream to subscribe to.
        terminal_id: TerminalId,
    },
    /// Drop the caller's per-Terminal subscriptions on `terminal_id`
    /// (SPEC §5.1 `DETACH_TERMINAL`, phux-v45.7): the output stream wired
    /// by [`Command::AttachTerminal`] and any per-Terminal event-stream
    /// subscription. The Terminal itself is unaffected. Idempotent — a
    /// no-op (still `Ok`) when the caller holds no subscription or the
    /// Terminal is already gone, so detach can never race a natural close
    /// into an error.
    DetachTerminal {
        /// The Terminal whose subscriptions to drop.
        terminal_id: TerminalId,
    },
    /// Terminate the underlying PTY of `terminal_id`. Asynchronously emits
    /// `TERMINAL_CLOSED`. Backs `phux kill` (one command per resolved
    /// Terminal — see ADR-0021).
    KillTerminal {
        /// The Terminal to terminate.
        terminal_id: TerminalId,
    },
    /// Request a snapshot of server state in `scope`. The reply rides on
    /// `COMMAND_RESULT { Ok_With(State(..)) }`. Backs `phux ls` and the
    /// CLI's client-side selector resolution.
    GetState {
        /// What to snapshot.
        scope: StateScope,
    },
    /// Read `terminal_id`'s current screen as structured data, with no
    /// side effects — the server walks its own `Terminal` grid, so unlike
    /// `ATTACH` this neither resizes the pane nor disturbs the live
    /// session (ADR-0022 §5, `phux-oki`). The reply rides on
    /// `COMMAND_RESULT { Ok_With(Json(..)) }` carrying a serialized
    /// `phux_core::ScreenState`. Backs `phux snapshot` and the poll floor
    /// under `phux wait`/`run`.
    GetScreen {
        /// The Terminal whose screen to project.
        terminal_id: TerminalId,
        /// Requested scrollback history (`phux-o1v`):
        /// - `None` — viewport only (the original v0.2.0-draft.6 shape).
        /// - `Some(0)` — all retained history rows (bare `--scrollback`).
        /// - `Some(n)` — the most-recent `n` history rows.
        ///
        /// Encoded as a trailing presence-byte + `u32` so a decoder reading
        /// the original `GET_SCREEN` body (which ended after `terminal_id`)
        /// would see the `0` presence byte: the field is wire-additive.
        request_scrollback: Option<u32>,
        /// When `true`, the reply's `ScreenState` carries the additive
        /// `cells[]` field: per-cell OSC-133 semantic marks + styles
        /// (`phux-8yl`). Encoded as a trailing `bool` byte *after*
        /// `request_scrollback`; a decoder reading a pre-`phux-8yl` body
        /// (which ended after `request_scrollback`) finds no byte and
        /// defaults it to `false`, so the field is wire-additive.
        cells: bool,
    },
    /// Deliver an already-built input `event` to `terminal_id` without an
    /// attach, subscription, or resize. The write counterpart to the
    /// side-effect-free `GetScreen` read: the server feeds the event
    /// straight into the pane's input pipeline, so unlike `ATTACH` this
    /// never disturbs the live session's dimensions (ADR-0022, `phux-3j3`).
    /// The reply rides `COMMAND_RESULT { Ok }` (or an `Error` if the
    /// Terminal is unknown). Backs `phux send-keys`/`run`.
    RouteInput {
        /// The Terminal to deliver the input to.
        terminal_id: TerminalId,
        /// The structured input event (key/mouse/focus/paste).
        event: InputEvent,
    },
    /// Atomically validate, encode, write, and acknowledge an ordered input
    /// batch. Retries with the same operation id and payload are idempotent.
    ApplyInput {
        /// Non-zero client-generated operation identifier.
        operation_id: InputOperationId,
        /// The Terminal to receive the complete batch.
        terminal_id: TerminalId,
        /// Ordered structured input events.
        events: Vec<InputEvent>,
    },
    /// Atomically terminate every Terminal in `ids` under the server's
    /// single state lock — the one irreducible multi-terminal op left
    /// behind when the L2 collection tier was dissolved in the v0.3.0
    /// "Option B" re-tier (ADR-0019 / ADR-0027). Grouping (which Terminals
    /// belong to a "session") is now client logic over L3 metadata, so the
    /// caller resolves the group to a concrete id list and the server need
    /// only tear them down together.
    ///
    /// Atomicity is local and all-or-nothing in the sense that every
    /// removal happens inside *one* lock acquisition: no other command can
    /// observe a half-killed group on this server. Cross-host atomicity is
    /// out of scope (it would be under any tiering). Killing an already-dead
    /// or unknown id is a no-op (not an error) — the op is idempotent so a
    /// caller racing a natural pane exit still succeeds. The reply rides
    /// `COMMAND_RESULT { Ok }`; the per-pane `TERMINAL_CLOSED` frames follow
    /// asynchronously as the panes reap. Backs `phux kill SESSION`.
    KillTerminals {
        /// The Terminals to terminate. Unknown / already-dead ids are
        /// skipped silently; the op succeeds as long as it is structurally
        /// valid.
        ids: Vec<TerminalId>,
    },
    /// Force-detach clients from *outside* the attach UI — backs `phux detach`.
    /// `session = Some(name)` detaches every client attached to that session;
    /// `session = None` detaches every attached client on the server. Each
    /// target client receives a `DETACHED` frame and its attachment is torn
    /// down, so its TUI exits cleanly. Distinct from `FrameKind::Detach`, which
    /// only detaches the sending connection. Reply: `COMMAND_RESULT { OkWith(
    /// Json(count)) }` where `count` is the number of clients detached.
    DetachClients {
        /// Target session by name, or `None` to detach every attached client.
        session: Option<String>,
    },
    /// Request a comprehensive snapshot of a terminal's full state: grid,
    /// scrollback, shell metadata, cursor, and sequence number (L2 Collection-aware
    /// agent interface). The reply rides `COMMAND_RESULT { Ok_With(Json(..)) }`
    /// carrying a JSON object built server-side. Backs
    /// agent polling and state inspection (ADR-0015 L2, `phux-y2t`).
    GetTerminalState {
        /// The Terminal whose state to snapshot.
        terminal_id: TerminalId,
        /// Whether to include scrollback lines above the viewport.
        /// When `false`, only the viewport is returned.
        include_scrollback: bool,
        /// Maximum number of scrollback lines to return. Ignored if
        /// `include_scrollback` is `false`.
        max_scrollback_lines: u16,
    },
    /// Subscribe to semantic terminal events for a specific pane without
    /// attaching or resizing. The server pushes typed events (`CommandStarted`,
    /// `CommandEnded`, `GridChanged`, `CwdChanged`, `PromptReady`, `OutputReceived`)
    /// as the pane's state changes. Scoped to the Terminal: only events for
    /// that pane flow to the subscriber. Idempotent: re-subscribing updates
    /// the `event_types` filter (empty = all types). Unsubscription is implicit
    /// on detach. Reply: `COMMAND_RESULT { Ok }`; events flow asynchronously as
    /// `Event` frames (SPEC §7.1). Backs agent-protocol `SubscribeTerminalEvents`.
    SubscribeTerminalEvents {
        /// The Terminal (pane) whose events the client subscribes to.
        terminal_id: TerminalId,
        /// Event type filter: which semantic events to forward.
        /// Empty vector = all event types.
        event_types: Vec<TerminalEventType>,
    },
    /// Ask the server to graceful-upgrade itself in place (ADR-0032): snapshot
    /// every pane, re-exec the on-disk binary, and re-adopt the live PTYs so
    /// sessions survive a binary update. A bare trigger — the handoff state
    /// blob is built and passed entirely server-side (it never crosses the
    /// wire). Clients see a brief disconnect and reconnect. Reply:
    /// `COMMAND_RESULT { Ok }` (best-effort, before the re-exec). Backs
    /// `phux upgrade`.
    Upgrade,
    /// Ask the server to stop itself (phux-pimp). A bare trigger, like
    /// [`Self::Upgrade`], and the same shape for the same reason: the work
    /// is entirely server-side and nothing about it belongs on the wire.
    ///
    /// The server acks and then cancels its root token, which is the *same*
    /// signal idle-exit (ADR-0063), the last-pane self-exit, and SIGINT/SIGTERM
    /// already deliver — so every pane gets its SIGHUP-then-grace-then-reap and
    /// the socket is unlinked on the way out. That path also yields exit
    /// status 0, which is what keeps a supervised server *stopped*: launchd's
    /// `KeepAlive{SuccessfulExit: false}` restarts a server killed by a signal
    /// but not one that exited cleanly. A signal-based stop therefore could
    /// not have satisfied ADR-0080's "a deliberately stopped server stays
    /// stopped" on macOS; this can.
    ///
    /// **Local only.** Stopping a server on behalf of a remote peer is a
    /// policy decision phux has not made, so this is refused on any transport
    /// but the UDS. Gated by [`ServerFeature::Shutdown`](crate::caps::ServerFeature::Shutdown):
    /// a client MUST NOT send it unless the bit is advertised, because an
    /// older server drops the unknown tag silently and "nothing happened" is
    /// indistinguishable from "the server ignored me".
    ///
    /// Reply: `COMMAND_RESULT { Ok }`, sent before the teardown begins, then
    /// the connection closes. Backs `phux kill --server`.
    Shutdown,
    /// Assert an exclusive input lease over `terminal_id` (ADR-0033, "take
    /// the wheel"). While a lease is held, only the holder's `INPUT_*`
    /// frames reach the PTY; others are dropped (still acked, preserving the
    /// fire-and-forget input invariant). `mode` chooses cooperative-or-fail
    /// vs. preempt; `ttl_ms` is an advisory lifetime (v1 servers hold the
    /// lease until the holder detaches or its connection drops — see
    /// ADR-0033). Reply: `COMMAND_RESULT { Ok }` on grant, or
    /// `Error { InputLeaseHeld, .. }` when a cooperative acquire loses to an
    /// existing holder.
    AcquireInput {
        /// The Terminal whose input authority to seize.
        terminal_id: TerminalId,
        /// Cooperative (grant only if free) or Seize (preempt).
        mode: InputMode,
        /// Advisory lease lifetime in milliseconds (0 = server default).
        ttl_ms: u32,
    },
    /// Release the input lease over `terminal_id`, returning it to `Open`
    /// (ADR-0033). A no-op if the caller does not hold the lease. Reply:
    /// `COMMAND_RESULT { Ok }`.
    ReleaseInput {
        /// The Terminal whose lease to release.
        terminal_id: TerminalId,
    },
    /// Deliver `signal` to the process group inside `terminal_id` (ADR-0033).
    /// Orthogonal to `KILL_TERMINAL`: this signals the process and leaves the
    /// pane addressable (read its final screen / exit status). `Freeze`
    /// (SIGSTOP) / `Resume` (SIGCONT) is the reversible brake. Reply:
    /// `COMMAND_RESULT { Ok }`, or `Error { TerminalNotFound, .. }`.
    SignalTerminal {
        /// The Terminal whose process group to signal.
        terminal_id: TerminalId,
        /// The signal to deliver.
        signal: TerminalSignal,
    },
    /// Write one acknowledged chunk of a file into the target host's
    /// server-owned upload sandbox (ADR-0059). `terminal_id` selects the host
    /// and proves the destination is useful to an existing pane; the server
    /// chooses the path. Retries with the same upload id, offset, and bytes are
    /// idempotent. The final chunk carries the expected SHA-256 digest.
    PutFile {
        /// Non-zero client-generated identifier stable across chunk retries.
        upload_id: FileUploadId,
        /// A Terminal on the host that must be able to read the completed file.
        terminal_id: TerminalId,
        /// Filename extension without a leading dot.
        extension: String,
        /// Byte offset at which this chunk begins.
        offset: u64,
        /// Raw file bytes for this chunk.
        data: Vec<u8>,
        /// Whether this is the final chunk.
        final_chunk: bool,
        /// Expected whole-file SHA-256 digest; required on the final chunk.
        sha256: Option<[u8; 32]>,
    },
    /// Report that an agent in `terminal_id` is blocked on a human-answerable
    /// question. This is the explicit hook source selected by ADR-0036. The
    /// server validates the payload, then emits [`AgentEvent::Asked`] to the
    /// existing event stream. It does not write to the PTY, attach, resize, or
    /// mutate terminal grid state.
    ReportAsked {
        /// The Terminal/pane that owns the blocked agent.
        terminal_id: TerminalId,
        /// Stable question id for answer correlation.
        id: String,
        /// Human-facing question text.
        question: String,
        /// Suggested answers, in display order.
        suggestions: Vec<String>,
        /// Optional seconds the agent has already been waiting.
        elapsed_seconds: Option<u64>,
    },
    /// Feed hook-sourced lifecycle evidence into the pane's detector without
    /// writing `phux.agent/v1` or disabling subsequent screen derivation.
    ReportAgentState {
        /// Pane whose detected occupant produced the hook.
        terminal_id: TerminalId,
        /// Immediate lifecycle evidence.
        state: ReportedAgentState,
    },
    /// Read the server's in-process performance telemetry: every latency
    /// histogram, throughput counter, and gauge the server keeps, plus its
    /// `getrusage` figures, as one `COMMAND_RESULT { OkWith(Json(report)) }`.
    /// The JSON is a `phux_perf::PerfReport` (`schema_version` inside);
    /// metric names are diagnostic and not part of the wire contract.
    /// Gated on the `GET_PERF` server feature bit.
    GetPerf {
        /// Zero every metric after snapshotting it, so the next report
        /// covers only what happened since.
        reset: bool,
    },
}

/// Acknowledgement for one [`Command::PutFile`] chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUploadAck {
    /// The next byte offset the server expects.
    pub next_offset: u64,
    /// Absolute completed path, present only after the final digest verifies.
    pub path: Option<String>,
}

/// A successful command's payload (SPEC §5, `CommandValue`).
///
/// `#[non_exhaustive]` for forward-compatible additions.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CommandValue {
    /// A Terminal identifier (e.g. the result of a spawn).
    TerminalId(TerminalId),
    /// A Group identifier (opaque grouping key).
    GroupId(GroupId),
    /// A server-state snapshot (reply to `GET_STATE`). Reuses the
    /// `ATTACHED` snapshot shape — see the wire-bytes note in SPEC §7.
    State(SessionSnapshot),
    /// A structured JSON return, for commands whose result is open-shaped.
    Json(String),
    /// Opaque bytes (e.g. an L3 metadata value).
    Bytes(Vec<u8>),
    /// Acknowledgement for a sandboxed file-upload chunk.
    FileUpload(FileUploadAck),
}

/// The outcome of a [`Command`], carried by [`FrameKind::CommandResult`](super::FrameKind::CommandResult)
/// (SPEC §5).
///
/// `#[non_exhaustive]` for forward-compatible additions.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CommandResult {
    /// The command succeeded and returned no value.
    Ok,
    /// The command succeeded and returned a [`CommandValue`].
    OkWith(CommandValue),
    /// The command failed; carries a structured [`ErrorCode`] and a
    /// human-readable UTF-8 diagnostic.
    Error {
        /// Structured failure code.
        code: ErrorCode,
        /// Human-readable diagnostic (UTF-8; unconstrained otherwise).
        message: String,
    },
}

/// A server-pushed agent event carried by [`FrameKind::Event`](super::FrameKind::Event) (SPEC §7.5 /
/// §10.3, phux-y2t).
///
/// The push half of the agent surface: an extensible taxonomy of terminal
/// lifecycle / activity events the server emits to clients that opted in via
/// [`FrameKind::SubscribeEvents`](super::FrameKind::SubscribeEvents). This is an *additive accelerator* of the
/// CLI-side poll-floor `wait` (which already shipped over `GET_SCREEN`) —
/// conditions stay matched client-side, events just cut polling latency.
///
/// # Forward compatibility
///
/// `#[non_exhaustive]`, and the wire encoding is TLV: each event is a `tag:
/// u8` followed by a length-prefixed `body: bytes`. A decoder that does not
/// recognise `tag` reads the declared body length and yields
/// [`AgentEvent::Unknown`] rather than failing the whole frame parse — so a
/// v0.2.x server may add event kinds and an older client skips them
/// cleanly. [`AgentEvent::Unknown`] is *only ever produced by the decoder*;
/// encoders never emit it (encoding it is a no-op-shaped contradiction and
/// is rejected at the match arm).
///
/// Only `PartialEq` / `Eq`: every variant body is a primitive or a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentEvent {
    /// A shell command began executing in the scoped Terminal. Sourced
    /// from OSC-133 `B`/`C` prompt marks (the shell-integration
    /// command-start boundary). Carries no payload — the command text is
    /// not extracted server-side.
    CommandStarted,
    /// A shell command finished in the scoped Terminal. Sourced from the
    /// OSC-133 `D` prompt mark. `exit_code` is `Some(n)` when the shell's
    /// integration reported one (`OSC 133 ; D ; n ST`) and `None`
    /// otherwise — see the wire-spec note on the exit-code gap.
    CommandFinished {
        /// Process exit code reported by the shell's OSC-133 `D` mark, or
        /// `None` when the shell did not include one.
        exit_code: Option<i32>,
    },
    /// The scoped Terminal's title changed (OSC 0 / OSC 2). Carries the
    /// new title as libghostty tracks it.
    TitleChanged {
        /// The new terminal title.
        title: String,
    },
    /// The scoped Terminal received a BEL (`0x07`). The control-plane
    /// counterpart to the `BELL` frame (`0xB0`), delivered on the event
    /// stream so a subscriber need not also attach.
    Bell,
    /// A new Terminal (pane) was spawned. The carried `terminal_id` is on
    /// the [`FrameKind::Event`](super::FrameKind::Event) envelope's `terminal_id` field; this
    /// variant body is empty (the id is the scope).
    PaneSpawned,
    /// A Terminal (pane) closed. Mirrors the L1 `TERMINAL_CLOSED` frame
    /// (`0xA1`); the closed Terminal is the envelope's `terminal_id` and
    /// `exit_status` carries the process exit code (or `None` for signal /
    /// unknown), matching `TERMINAL_CLOSED.exit_status`.
    PaneClosed {
        /// Process exit code (`_exit(n)`), or `None` for signals / unknown.
        exit_status: Option<i32>,
    },
    /// The scoped Terminal's grid mutated since the last `Idle` (output
    /// arrived). Sourced from the per-pane tick's dirty flag; coalesced —
    /// the server emits at most one `Dirty` per active burst, then one
    /// [`AgentEvent::Idle`] when the burst settles.
    Dirty,
    /// The scoped Terminal went quiet: no grid mutation across an idle
    /// window after a `Dirty`. The "output has settled" signal a `wait`
    /// consumer keys on.
    Idle,
    /// A supervisory state change on the scoped Terminal (ADR-0033): the
    /// input lease changed hands, or the process lifecycle moved
    /// (`Running` → `Frozen` → `Exited`). Broadcast to every subscriber so
    /// consumers can render "who has the wheel" and "frozen" without polling,
    /// and so the change is recorded with intent (`action`) and identity
    /// (`actor`) — the seed of the audit trail.
    TerminalControl {
        /// Current process lifecycle of the Terminal.
        lifecycle: TerminalLifecycle,
        /// Process exit status when `lifecycle == Exited`; `None` otherwise
        /// (or for signal-terminated / unknown exits).
        exit_status: Option<i32>,
        /// The client currently holding the input lease, or `None` if the
        /// Terminal is `Open` (any subscriber's input passes).
        input_holder: Option<ClientId>,
        /// What just happened (acquired / seized / released / signalled / …).
        action: ControlAction,
        /// The client that performed `action`, or `None` for server-driven
        /// transitions (e.g. a natural process exit, or a lease expiring on
        /// the holder's disconnect).
        actor: Option<ClientId>,
    },
    /// An agent in the scoped Terminal is waiting on a human answer (phux-2sl6).
    ///
    /// The control-plane carrier for a pending question: an agent that has
    /// blocked for input emits this so a projection consumer can render the
    /// waiting prompt (id, text, suggested answers, how long it has waited)
    /// without re-deriving it from the grid. It mirrors the consumer-side
    /// question model one-for-one. The body is field-tagged TLV, so
    /// `suggestions` and the optional `elapsed_seconds` are additive and an
    /// older decoder skips the whole event as [`AgentEvent::Unknown`].
    Asked {
        /// Stable id the answer correlates against.
        id: String,
        /// The question text presented to the human.
        question: String,
        /// Suggested answers, in presentation order — the *actual options*,
        /// not yes/no. Empty when the agent offered none.
        suggestions: Vec<String>,
        /// Seconds the agent has been waiting, or `None` when not reported.
        elapsed_seconds: Option<u64>,
    },
    /// The scoped Terminal's working directory changed (phux-foz.4).
    ///
    /// Sourced from the kernel cwd of the PTY child process (the same
    /// query the spawn-inheritance path uses — `/proc/<pid>/cwd` on Linux,
    /// `proc_pidinfo` on macOS), polled at OSC-133 prompt boundaries and
    /// on output-idle, and coalesced: emitted only when the directory
    /// actually differs from the last observation. Best-effort like every
    /// event — a consumer seeds from the `ATTACHED` snapshot's
    /// `TerminalInfo::cwd` (the spawn cwd) and refines from this stream.
    CwdChanged {
        /// The Terminal's new working directory (absolute, lossy UTF-8).
        cwd: String,
    },
    /// An event whose `tag` this protocol version does not recognise.
    ///
    /// Produced **only by the decoder** when it reads an `EVENT` frame
    /// whose event tag is outside the known set; the length-prefixed body
    /// is preserved verbatim so a curious consumer can inspect it, but the
    /// common path simply ignores unknown events. Never constructed by an
    /// encoder.
    Unknown {
        /// The unrecognised event tag.
        tag: u8,
        /// The event's opaque body bytes, preserved verbatim.
        body: Vec<u8>,
    },
}
