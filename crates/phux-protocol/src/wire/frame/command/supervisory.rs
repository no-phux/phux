//! The resource, input, and server-control [`Command`] variants.

#[cfg(doc)]
use super::{Command, CommandValue};

/// Appends the supervisory [`Command`] variants to the list being
/// built and hands it to `$next`; see `command_enum!` in `command.rs`.
macro_rules! supervisory_commands {
    ($next:ident!($($rest:tt)*) $($done:tt)*) => {
        $next!($($rest)* $($done)*
    /// Dedicated SDK-owned `AgentSession` broker request. The wire body is
    /// length-prefixed JSON bounded by `MAX_NATIVE_PAYLOAD_BYTES`; controller
    /// requests additionally require exclusive binding proof and generation.
    NativeAgent {
        /// Exact typed native launch, control, or controller operation.
        request: crate::native_agent::NativeRequestPayload,
    },
    /// Subscribe to one resource's content stream (SPEC §5.1): a fresh
    /// bootstrap generation, then `RESOURCE_OUTPUT`. Does not resize.
    /// Re-attaching replaces the generation. The verb a federation hub relays
    /// for two-hop attach (ADR-0007 §4). Construct with
    /// [`Self::attach_terminal`] or [`Self::attach_agent_session`]; the fields
    /// below are the unchanged wire shape.
    AttachResource {
        /// The resource whose content stream to subscribe to.
        terminal_id: ResourceId,
        /// Attach intent (ADR-0127, SPEC §8.1), one trailing byte; `None`
        /// writes nothing unless a cursor follows, in which case the canonical
        /// `{ PRIMARY, NEVER }` byte occupies the role slot. Send a non-default
        /// policy only to a server advertising `ATTACH_ROLES`.
        role_policy: Option<super::RolePolicy>,
        /// Last committed agent record. Send only when the server advertises
        /// `AgentStreamResume`; Terminal resources reject a cursor.
        agent_after_seq: Option<u64>,
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
        /// Idempotency key (`docs/protocol.md`), a trailing `bytes16`
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
    /// Read one bounded chunk of a file the serving user can read, on this
    /// server or on the named satellite. The reply is
    /// [`CommandValue::FileChunk`]; the chunk that reaches the end carries
    /// the whole-file SHA-256.
    GetFile {
        /// A satellite of the serving hub whose filesystem is read; `None`
        /// reads the serving server's own.
        host: Option<String>,
        /// Absolute UTF-8 path on that host.
        path: String,
        /// Byte offset at which this chunk begins.
        offset: u64,
        /// Most bytes to return, at most
        /// [`MAX_FILE_UPLOAD_CHUNK`](super::MAX_FILE_UPLOAD_CHUNK).
        max_bytes: u32,
    },
    /// One typed Git operation on the checkout `terminal_id` is in, resolved
    /// from the Terminal's live working directory by the server that owns
    /// it. The reply is [`CommandValue::Checkout`].
    Checkout {
        /// The Terminal whose working directory selects the checkout.
        terminal_id: ResourceId,
        /// The operation.
        request: crate::checkout::CheckoutRequest,
    },
    /// Send one HTTP request to a service on this server's host, or on the
    /// named satellite's (`docs/host-services.md`). The reply is
    /// [`CommandValue::ServiceHttpHead`], sent once the response head
    /// arrives; the body is pulled with [`Command::ServiceHttpRead`].
    ServiceHttpOpen {
        /// The request.
        request: Box<crate::service_http::ServiceHttpRequest>,
    },
    /// Read the next body bytes of an open exchange. The reply is
    /// [`CommandValue::ServiceHttpChunk`]: at least one byte, the end, or,
    /// after an idle deadline, nothing.
    ServiceHttpRead {
        /// The exchange [`CommandValue::ServiceHttpHead`] named.
        exchange: u64,
        /// Most bytes to return, at most
        /// [`MAX_SERVICE_HTTP_READ_BYTES`](crate::service_http::MAX_SERVICE_HTTP_READ_BYTES).
        max_bytes: u32,
    },
    /// End an exchange and release its upstream connection. Idempotent.
    ServiceHttpClose {
        /// The exchange [`CommandValue::ServiceHttpHead`] named.
        exchange: u64,
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
    /// Drive the server's developer telemetry: start or stop a span trace,
    /// or switch heap accounting. Answered `OkWith(Json)`; a stopped trace
    /// is written to the server's state directory and the reply names the
    /// file. Gated on
    /// [`ServerFeatureExt::PerfControl`](crate::caps::ServerFeatureExt::PerfControl).
    PerfControl {
        /// What to do.
        op: PerfOp,
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
    /// (`docs/protocol.md#commands-events-resources`), at most
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
    /// applied atomically (`docs/protocol.md`, ADR-0109); otherwise
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
    /// Open a listener for one remote attach (`docs/remote.md#attaching-over-ssh`,
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
    /// covered session (`docs/protocol.md#commands-events-resources`). Gated on
    /// [`ServerFeature::CloseTabResources`](crate::caps::ServerFeature::CloseTabResources).
    CloseTabResources {
        /// The Terminals to terminate.
        ids: Vec<ResourceId>,
    },
        );
    };
}
