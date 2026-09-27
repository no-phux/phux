//! Control-plane command, result, and agent-event types — SPEC §5
//! (ADR-0021) and SPEC §7.5 (ADR-0022).

use crate::ids::{ClientId, FileUploadId, GroupId, InputOperationId, ResourceId, ResourceKind};
use crate::input::InputEvent;
use crate::wire::info::SessionSnapshot;

use super::ErrorCode;

wire_enum! { to_u8 / from_u8;
/// Event class filter for [`Command::SubscribeResourceEvents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceEventType {
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
}

/// Scope argument for [`Command::GetState`] (SPEC §5.1, ADR-0021).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateScope {
    /// Snapshot the entire server (every Terminal the caller may see).
    Server,
}

wire_enum! { to_u8 / from_u8;
/// Acquisition mode for [`Command::AcquireInput`] (ADR-0033).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputMode {
    /// Grant only if the lease is free; otherwise
    /// [`ErrorCode::InputLeaseHeld`].
    Cooperative = 0,
    /// Preempt the current holder.
    Seize = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// A POSIX signal for a Terminal's process group via
/// [`Command::SignalTerminal`] (ADR-0033). Unlike `KILL_RESOURCE`, the pane
/// stays addressable.
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
}

wire_enum! { to_u8 / from_u8;
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
}

wire_enum! { to_u8 / from_u8;
/// Process lifecycle state of a Terminal, carried by
/// [`AgentEvent::TerminalControl`] (ADR-0033). An exit status rides
/// alongside in the event body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResourceLifecycle {
    /// The process group is running normally.
    #[default]
    Running = 0,
    /// The process group is stopped (SIGSTOP); resumable.
    Frozen = 1,
    /// The process exited; the accompanying `exit_status` carries the code.
    Exited = 2,
}
}

wire_enum! { to_u8 / from_u8;
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
    /// An input lease reached its `ttl_ms` and the server returned the
    /// Terminal to `Open` (ADR-0123). A decoder that predates the value
    /// cannot read it, so a server sends it only to a journal-aware
    /// subscription and reports the same transition as `Released` to any
    /// other (`docs/spec/L1.md` §7.1).
    Expired = 9,
    /// The actor re-attached the Terminal with a different declared role
    /// (ADR-0127): a `VIEWER` widened to `PRIMARY`, or the reverse. Like
    /// `Expired`, a pre-`0.9.0-draft.15` decoder fails the frame on it, so
    /// a server sends it only to a journal-aware subscription and withholds
    /// it from any other; a later decoder that predates it reads
    /// `Unknown { tag: 0x08 }`.
    RoleChanged = 10,
}
}

wire_enum! { to_u8 / from_u8;
/// How a held action's approval ended (ADR-0128), carried by
/// [`AgentEvent::ApprovalDecided`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalOutcome {
    /// A connection holding un-held `SIGNAL` on the subject approved it; the
    /// held command then ran once, under the requester's grant.
    Approved = 0,
    /// A connection holding un-held `SIGNAL` on the subject denied it; the
    /// requester got `PERMISSION_DENIED`.
    Denied = 1,
    /// Nobody decided within the approval TTL; the requester got
    /// `PERMISSION_DENIED { "approval expired" }`.
    Expired = 2,
    /// The action was withdrawn while held, and nothing ran. Two cases: a
    /// Terminal it names was reaped, and the requester is answered
    /// `PERMISSION_DENIED { "terminal gone" }`; or the requester disconnected
    /// or its authority was revoked, and nobody is answered, because the
    /// requester is gone.
    Withdrawn = 3,
}
}

impl ApprovalOutcome {
    /// The stable lowercase name consumers print.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Withdrawn => "withdrawn",
        }
    }
}

/// A typed control-plane command carried by
/// [`FrameKind::Command`](super::FrameKind::Command) (SPEC §5.1).
///
/// Unknown tags decode as [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Command {
    /// Subscribe to one Terminal's content stream (SPEC §5.1): a fresh
    /// bootstrap generation, then `RESOURCE_OUTPUT`. Does not resize.
    /// Re-attaching replaces the generation. The verb a federation hub relays
    /// for two-hop attach (ADR-0007 §4).
    AttachResource {
        /// The Terminal whose content stream to subscribe to.
        terminal_id: ResourceId,
        /// Attach intent (ADR-0127, SPEC §8.1), one trailing byte; `None`
        /// writes nothing and means `{ PRIMARY, NEVER }`. Send `Some` only to
        /// a server advertising `ATTACH_ROLES`.
        role_policy: Option<super::RolePolicy>,
    },
    /// Drop the caller's output and event subscriptions on `terminal_id`
    /// (SPEC §5.1). Idempotent, so it never races a natural close into an
    /// error.
    DetachResource {
        /// The Terminal whose subscriptions to drop.
        terminal_id: ResourceId,
    },
    /// Terminate `terminal_id`; `RESOURCE_CLOSED` follows asynchronously.
    KillResource {
        /// The Terminal to terminate.
        terminal_id: ResourceId,
        /// Idempotency key (`docs/spec/L1.md` §5.1.1), a trailing `bytes16`
        /// read only when bytes remain; a repeat answers the first result.
        /// Send only to a server advertising
        /// [`ServerFeature::KeyedSignal`](crate::caps::ServerFeature::KeyedSignal).
        operation_id: Option<crate::ids::IdempotencyKey>,
    },
    /// Snapshot server state in `scope`; answered `OkWith(State)`.
    GetState {
        /// What to snapshot.
        scope: StateScope,
    },
    /// Read the current screen as a JSON `phux_core::ScreenState` with no
    /// side effects (ADR-0022 §5); answered `OkWith(Json)`.
    GetScreen {
        /// The Terminal whose screen to project.
        terminal_id: ResourceId,
        /// Scrollback: `None` viewport only, `Some(0)` all retained rows,
        /// `Some(n)` the latest `n`. Trailing presence byte + `u32`.
        request_scrollback: Option<u32>,
        /// Include per-cell semantic marks and styles (`cells[]`). Trailing
        /// `bool`; absent decodes as `false`.
        cells: bool,
        /// Rendering for `ScreenState.rendered`: low 7 bits select
        /// ([`GET_SCREEN_FORMAT_SELECTOR_MASK`]; `0` none, `1` HTML, `2` VT,
        /// others `INVALID_COMMAND`), high bit
        /// ([`GET_SCREEN_FORMAT_UNWRAP`]) joins soft-wrapped rows. Trailing
        /// `u8`; absent decodes as `0`. An older peer ignores it, which the
        /// client detects from an absent `rendered`.
        format: u8,
    },
    /// Deliver an input `event` to `terminal_id` without attaching or
    /// resizing (ADR-0022).
    RouteInput {
        /// The Terminal to deliver the input to.
        terminal_id: ResourceId,
        /// The structured input event (key/mouse/focus/paste).
        event: InputEvent,
    },
    /// Atomically validate, encode, write, and acknowledge an ordered input
    /// batch. Retries with the same operation id and payload are idempotent.
    ApplyInput {
        /// Non-zero client-generated operation identifier.
        operation_id: InputOperationId,
        /// The Terminal to receive the complete batch.
        terminal_id: ResourceId,
        /// Ordered structured input events.
        events: Vec<InputEvent>,
    },
    /// Terminate every Terminal in `ids` in one acquisition of the server's
    /// state lock, so no command observes a half-killed group
    /// (ADR-0019 / ADR-0027). Unknown ids are skipped.
    KillResources {
        /// The Terminals to terminate.
        ids: Vec<ResourceId>,
        /// Idempotency key of the whole batch, trailing like
        /// [`Command::KillResource::operation_id`].
        operation_id: Option<crate::ids::IdempotencyKey>,
    },
    /// Force-detach every client attached to `session`, or every attached
    /// client when `None`; answered `OkWith(Json(count))`.
    DetachClients {
        /// Target session by name, or `None` to detach every attached client.
        session: Option<String>,
    },
    /// Snapshot a Terminal's full state (grid, scrollback, shell metadata,
    /// cursor, sequence) as JSON; answered `OkWith(Json)`.
    GetTerminalState {
        /// The Terminal whose state to snapshot.
        terminal_id: ResourceId,
        /// Whether to include scrollback lines above the viewport.
        /// When `false`, only the viewport is returned.
        include_scrollback: bool,
        /// Maximum number of scrollback lines to return. Ignored if
        /// `include_scrollback` is `false`.
        max_scrollback_lines: u16,
    },
    /// Subscribe to one pane's semantic events without attaching.
    /// Re-subscribing replaces the filter; teardown is implicit on detach.
    SubscribeResourceEvents {
        /// The Terminal (pane) whose events the client subscribes to.
        terminal_id: ResourceId,
        /// Event type filter: which semantic events to forward.
        /// Empty vector = all event types.
        event_types: Vec<ResourceEventType>,
    },
    /// Graceful in-place re-exec that keeps live PTYs (ADR-0032).
    Upgrade,
    /// Stop the server with a clean exit, so a supervisor keeps it stopped
    /// (ADR-0080). **Local only**: refused off the Unix socket. A client MUST
    /// see [`ServerFeature::Shutdown`](crate::caps::ServerFeature::Shutdown)
    /// first, since an older server drops the tag silently.
    Shutdown,
    /// Take an exclusive input lease on `terminal_id` (ADR-0033): only the
    /// holder's input reaches the PTY; others are acked and dropped.
    AcquireInput {
        /// The Terminal whose input authority to seize.
        terminal_id: ResourceId,
        /// Cooperative (grant only if free) or Seize (preempt).
        mode: InputMode,
        /// Advisory lease lifetime in milliseconds (0 = server default).
        ttl_ms: u32,
    },
    /// Release the caller's input lease on `terminal_id`; a no-op when not
    /// held (ADR-0033).
    ReleaseInput {
        /// The Terminal whose lease to release.
        terminal_id: ResourceId,
    },
    /// Deliver `signal` to the process group inside `terminal_id` (ADR-0033).
    SignalTerminal {
        /// The Terminal whose process group to signal.
        terminal_id: ResourceId,
        /// The signal to deliver.
        signal: TerminalSignal,
        /// Idempotency key of this signal, trailing like
        /// [`Command::KillResource::operation_id`]: a repeat answers the
        /// first result and delivers nothing.
        operation_id: Option<crate::ids::IdempotencyKey>,
    },
    /// Write one acknowledged chunk into the host's server-owned upload
    /// sandbox (ADR-0059); the server chooses the path. Retries are
    /// idempotent, and the final chunk carries the SHA-256 digest.
    PutFile {
        /// Non-zero client-generated identifier stable across chunk retries.
        upload_id: FileUploadId,
        /// A Terminal on the host that must be able to read the completed file.
        terminal_id: ResourceId,
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
    /// Report that an agent in `terminal_id` is blocked on a question
    /// (ADR-0036); the server emits [`AgentEvent::Asked`].
    ReportAsked {
        /// The Terminal/pane that owns the blocked agent.
        terminal_id: ResourceId,
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
        terminal_id: ResourceId,
        /// Immediate lifecycle evidence.
        state: ReportedAgentState,
    },
    /// Read in-process performance telemetry as a JSON
    /// `phux_perf::PerfReport`; metric names are not a wire contract.
    GetPerf {
        /// Zero every metric after snapshotting it, so the next report
        /// covers only what happened since.
        reset: bool,
    },
    /// Transcribe a completed upload and paste the text (without submitting)
    /// into a Terminal; answered `OkWith(Json { text, pasted, duration_ms })`.
    Transcribe {
        /// The finished upload (its final `PUT_FILE` chunk was acknowledged).
        upload_id: FileUploadId,
        /// The Terminal to paste the transcript into.
        terminal_id: ResourceId,
    },
    /// Append complete codec records to a producer-fed resource's output
    /// (`docs/spec/L1.md` §5.1), at most
    /// [`MAX_APPEND_BYTES`](super::MAX_APPEND_BYTES). Refused with
    /// [`ErrorCode::WrongResourceKind`], [`ErrorCode::NotProducer`],
    /// [`ErrorCode::RecordInvalid`] (nothing appended), or
    /// [`ErrorCode::Overflow`] (retry after backoff).
    AppendResourceOutput {
        /// The producer-fed resource to append to.
        terminal_id: ResourceId,
        /// One or more complete records under the resource's codec.
        bytes: Vec<u8>,
    },
    /// Kill one resource only if every precondition holds, checked and
    /// applied atomically (`docs/spec/L1.md` §5.2.1, ADR-0109); otherwise
    /// `PRECONDITION_FAILED`. Gated on
    /// [`ServerFeature::ConditionalKill`](crate::caps::ServerFeature::ConditionalKill).
    KillResourceIf {
        /// The resource to terminate.
        terminal_id: ResourceId,
        /// What must hold for the kill to proceed.
        precondition: KillPrecondition,
        /// Idempotency key of this kill, trailing after the condition bits
        /// like [`Command::KillResource::operation_id`].
        operation_id: Option<crate::ids::IdempotencyKey>,
    },
    /// Open a listener for one remote attach (`docs/spec/L1.md` §5.6,
    /// ADR-0120) that admits only a token minted for it and closes after
    /// `linger_secs` idle. **Local only**: refused off the Unix socket.
    /// Answered `OkWith(Json)` with port, cert fingerprint, token, and
    /// linger. Gated on
    /// [`ServerFeature::OpenListener`](crate::caps::ServerFeature::OpenListener).
    OpenListener {
        /// The transport to listen on.
        transport: ListenerTransport,
        /// Inclusive port range to bind from, or `None` for any free port.
        /// Encoded as two `u16`s, with `0, 0` meaning `None`.
        port_range: Option<(u16, u16)>,
        /// Seconds the listener stays open with no connection through it;
        /// `0` asks for the server default.
        linger_secs: u32,
    },
    /// [`Self::KillResources`] that does not release keep-empty on a fully
    /// covered session (`docs/spec/L1.md` §5.2.2). Gated on
    /// [`ServerFeature::CloseTabResources`](crate::caps::ServerFeature::CloseTabResources).
    CloseTabResources {
        /// The Terminals to terminate.
        ids: Vec<ResourceId>,
    },
}

impl Command {
    /// The trailing `operation_id` of a keyed supervisory command
    /// (`docs/spec/L1.md` §5.1.1), or `None`.
    #[must_use]
    pub const fn idempotency_key(&self) -> Option<&crate::ids::IdempotencyKey> {
        match self {
            Self::KillResource { operation_id, .. }
            | Self::KillResourceIf { operation_id, .. }
            | Self::KillResources { operation_id, .. }
            | Self::SignalTerminal { operation_id, .. } => operation_id.as_ref(),
            _ => None,
        }
    }
}

/// [`Command::GetScreen::format`]'s low 7 bits: which rendering to
/// produce (D9). `0` none, `1` HTML, `2` VT; `3..=127` are undefined and
/// refused with `INVALID_COMMAND`.
pub const GET_SCREEN_FORMAT_SELECTOR_MASK: u8 = 0x7F;

/// [`Command::GetScreen::format`]'s high bit: join soft-wrapped rows.
/// Ignored when the selector is `0`.
pub const GET_SCREEN_FORMAT_UNWRAP: u8 = 0x80;

/// The transport a [`Command::OpenListener`] asks for (`u8` on the wire).
///
/// Only [`Self::Quic`] is defined. A decoder keeps any other value, so a
/// server can refuse it by name instead of the whole frame failing to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListenerTransport {
    /// QUIC over TLS 1.3, admitting a listener-scoped bearer token (`0x00`).
    Quic,
    /// A value this build does not define. A server refuses it with
    /// `INVALID_COMMAND`.
    Unknown(u8),
}

impl ListenerTransport {
    /// Classify a wire byte, keeping values this build does not define.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Quic,
            other => Self::Unknown(other),
        }
    }

    /// The wire byte.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Quic => 0,
            Self::Unknown(value) => value,
        }
    }
}

/// The preconditions a [`Command::KillResourceIf`] carries (ADR-0109).
///
/// Wire body, after the tagged `ResourceId`: an `Option` tag (`0`/`1`), the
/// 16 instance bytes when it is `1`, then the condition bits as one `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KillPrecondition {
    /// When `Some`, the server's current instance token must equal it: the
    /// id is meaningful only in the id space the caller learned it from.
    pub instance: Option<crate::ids::ServerInstance>,
    /// Further conditions, as a bitset.
    pub conditions: KillConditions,
}

impl KillPrecondition {
    /// The precondition for a late kill of a resource this client spawned
    /// and bound to `instance`: the id space must be unchanged, and no other
    /// connection may have attached or used it since.
    #[must_use]
    pub const fn spawned_and_unattached(instance: crate::ids::ServerInstance) -> Self {
        Self {
            instance: Some(instance),
            conditions: KillConditions::UNATTACHED_SINCE_SPAWN,
        }
    }
}

/// Condition bits of a [`KillPrecondition`] (`u8` on the wire).
///
/// A decoder keeps bits it does not know, and a server that receives one
/// refuses the kill with `PRECONDITION_FAILED`: an unknown condition can never
/// be ignored into an unconditional kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KillConditions(u8);

impl KillConditions {
    /// No condition beyond the instance token.
    pub const NONE: Self = Self(0);
    /// No connection other than the one that spawned the resource has
    /// attached or used it since it was spawned, and it has no child
    /// resource. Requires [`KillPrecondition::instance`]; a server refuses the
    /// bit without it (`docs/spec/L1.md` §5.2.1 defines it exactly).
    pub const UNATTACHED_SINCE_SPAWN: Self = Self(0x01);
    /// Every bit this build assigns a meaning to.
    const KNOWN: u8 = Self::UNATTACHED_SINCE_SPAWN.0;

    /// Wrap raw wire bits, keeping unknown ones.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// The raw wire bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `true` iff every bit of `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The set with `other`'s bits added.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The bits this build does not know; nonzero means the server must
    /// refuse.
    #[must_use]
    pub const fn unknown_bits(self) -> u8 {
        self.0 & !Self::KNOWN
    }
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
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CommandValue {
    /// A Terminal identifier (e.g. the result of a spawn).
    ResourceId(ResourceId),
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

/// The outcome of a [`Command`], carried by
/// [`FrameKind::CommandResult`](super::FrameKind::CommandResult) (SPEC §5).
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

/// A server-pushed agent event carried by
/// [`FrameKind::Event`](super::FrameKind::Event) (SPEC §7.5 / §10.3).
///
/// Each event is `tag: u8` + a length-prefixed body, so a decoder skips an
/// unknown tag to [`AgentEvent::Unknown`] instead of failing the frame.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentEvent {
    /// A shell command began (OSC-133 `B`/`C`).
    CommandStarted,
    /// A shell command finished (OSC-133 `D`).
    CommandFinished {
        /// Exit code from the `D` mark, when the shell reported one.
        exit_code: Option<i32>,
    },
    /// The Terminal's title changed (OSC 0 / OSC 2).
    TitleChanged {
        /// The new terminal title.
        title: String,
    },
    /// The Terminal received a BEL, delivered without an attach.
    Bell,
    /// A resource was spawned; the envelope carries its id. The TLV body
    /// writes `kind` only for a non-Terminal and `parent` only when bound, so
    /// a Terminal's body is empty.
    ResourceSpawned {
        /// What backs the new resource; `Terminal` when absent on the wire.
        kind: ResourceKind,
        /// The resource it is bound to, when it is a child (`docs/spec/L1.md`
        /// §1.2); `None` for a root.
        parent: Option<ResourceId>,
    },
    /// A resource closed; mirrors `RESOURCE_CLOSED`.
    ResourceClosed {
        /// Process exit code (`_exit(n)`), or `None` for signals / unknown.
        exit_status: Option<i32>,
    },
    /// The grid mutated since the last `Idle`; at most one per burst.
    Dirty,
    /// Output settled after a `Dirty`.
    Idle,
    /// The input lease changed hands or the process lifecycle moved
    /// (ADR-0033), with the acting client.
    TerminalControl {
        /// Current process lifecycle of the Terminal.
        lifecycle: ResourceLifecycle,
        /// Process exit status when `lifecycle == Exited`; `None` otherwise
        /// (or for signal-terminated / unknown exits).
        exit_status: Option<i32>,
        /// The client currently holding the input lease, or `None` if the
        /// Terminal is `Open` (any subscriber's input passes).
        input_holder: Option<ClientId>,
        /// What just happened (acquired / seized / released / signalled / …).
        action: ControlAction,
        /// The client that performed `action`, or `None` when server-driven.
        actor: Option<ClientId>,
    },
    /// An agent in the Terminal is waiting on a human answer. Field-tagged
    /// TLV body.
    Asked {
        /// Stable id the answer correlates against.
        id: String,
        /// The question text presented to the human.
        question: String,
        /// Suggested answers, in presentation order; may be empty.
        suggestions: Vec<String>,
        /// Seconds the agent has been waiting, or `None` when not reported.
        elapsed_seconds: Option<u64>,
    },
    /// The PTY child's working directory changed (best-effort, coalesced).
    CwdChanged {
        /// The Terminal's new working directory (absolute, lossy UTF-8).
        cwd: String,
    },
    /// The subscription missed journaled events `first_missing..=last_missing`
    /// (ADR-0123); re-read level state. Never journaled or stamped.
    JournalGap {
        /// First missing journal sequence, inclusive.
        first_missing: u64,
        /// Last missing journal sequence, inclusive.
        last_missing: u64,
    },
    /// The resource lost `dropped` events before journaling (ADR-0123).
    SourceGap {
        /// How many events were lost at the source.
        dropped: u64,
    },
    /// A `SIGNAL` action was held for approval (ADR-0128); details live in
    /// the `phux.approval/v1/<id>` record. Body: 16 id bytes.
    ApprovalRequested {
        /// The held action's approval id.
        id: crate::ids::ApprovalId,
    },
    /// A held action's approval ended (ADR-0128). Body: 16 id bytes then
    /// `outcome: u8`; an unknown outcome makes the event `Unknown`.
    ApprovalDecided {
        /// The held action's approval id.
        id: crate::ids::ApprovalId,
        /// How the approval ended.
        outcome: ApprovalOutcome,
    },
    /// An event tag this build does not know, body kept verbatim. Produced
    /// only by the decoder.
    Unknown {
        /// The unrecognised event tag.
        tag: u8,
        /// The event's opaque body bytes, preserved verbatim.
        body: Vec<u8>,
    },
}
