//! The [`WireEvent`] vocabulary and its redacting `Debug`.

#![allow(
    clippy::wildcard_imports,
    reason = "wire records intentionally share the parent UniFFI enum namespace"
)]

use super::*;

/// A state change pushed by the server, drained by Swift via `take_events()`
/// on the poll cadence. Wire-faithful naming: `OutputStarted`/`OutputSettled`
/// mirror the server's coalesced `Dirty`/`Idle` burst events — Swift maps
/// them onto its own status vocabulary.
#[derive(uniffi::Enum, Clone, PartialEq, Eq)]
pub enum WireEvent {
    /// The session graph changed (a fresh ATTACHED arrived) — re-read
    /// `topology()`.
    TopologyChanged,
    /// The organization replica changed (a seed, a journal event, a
    /// refetch) — re-read `topology()` and `inbox()`.
    OrganizationChanged,
    /// The organization records could not be read.
    OrganizationUnavailable { message: String },
    /// A submitted organization operation resolved. `operation` is the id
    /// the submitting call returned (32 hex digits). `status` is
    /// `committed`, `accepted`, `refused`, `error`, `unknown`, or
    /// `not_sent`; `unknown` means reconcile before acting again.
    /// `refusal` is the refusal code (`0` unless refused). `undoable` says
    /// the committed receipt is retained for `undo`.
    /// `created_session_key` is the full base32 key from the committed
    /// receipt, absent unless this operation created a session. Correlate
    /// it by `operation`, not by a session name or a topology change.
    OrganizationOperation {
        operation: String,
        status: String,
        refusal: u32,
        refusal_word: String,
        conflict_revision: Option<u64>,
        revision: Option<u64>,
        message: String,
        undoable: bool,
        created_session_key: Option<String>,
    },
    /// A `kill_session` resolved. `workflow` is the id `kill_session`
    /// returned. `status` is `committed` when the session went away, else
    /// `refused`, `error`, `unknown`, or `not_sent`. `targets` is always
    /// empty: the server kills the session as one command.
    KillFinished {
        workflow: u64,
        status: String,
        message: String,
        targets: Vec<OrgTargetResult>,
    },
    /// A new pane appeared mid-session. The server pumps output only to the
    /// spawning client; when the spawner is someone else, the bridge has
    /// already sent `ATTACH_TERMINAL` for the pane on the live socket
    /// (phux-gt1) plus a `GET_STATE` topology refresh — no resync needed.
    /// Consumers just wait for the follow-up `TerminalAttached` /
    /// `TopologyChanged`.
    PaneSpawned { terminal_id: String },
    /// The server answered one of OUR `SPAWN_TERMINAL` requests
    /// (`TERMINAL_SPAWNED`, correlated by `request_id` — the value
    /// `spawn_terminal_with_command()` returned). Exactly one of
    /// `terminal_id` / `error` is set. This is how the hidden utility pane
    /// for image paste (ADR-0018) learns its target id; the server pumps a
    /// spawned pane's output to its spawner, so no resync is needed.
    TerminalSpawned {
        request_id: u32,
        terminal_id: Option<String>,
        error: Option<String>,
    },
    /// The server answered one of OUR `ATTACH_TERMINAL` commands
    /// (`COMMAND_RESULT`, correlated by `request_id` — the value
    /// `attach_terminal()` returned, or a bridge-internal id for the
    /// automatic foreign-pane pickup). `error` is `None` on success; by
    /// then the pane's authoritative `TERMINAL_SNAPSHOT` has already been
    /// routed into its buffer (the server sends it before the Ok).
    TerminalAttached {
        request_id: u32,
        terminal_id: String,
        error: Option<String>,
    },
    /// The server answered one of OUR `DETACH_TERMINAL` commands. The
    /// verb is idempotent server-side, so `error` is rare (transport-level
    /// failures surface elsewhere); it is carried for symmetry.
    TerminalDetached {
        request_id: u32,
        terminal_id: String,
        error: Option<String>,
    },
    /// The server answered one of OUR `close_terminals` batches
    /// (`CLOSE_TAB_RESOURCES`, correlated by the `request_id` it returned).
    /// `error` is `None` when every pane closed; the batch is all or
    /// nothing. Each pane's own `TerminalClosed` follows.
    TerminalsClosed {
        request_id: u32,
        error: Option<String>,
    },
    /// A pane closed (process exit, signal, or kill). `exit_status` is the
    /// process exit code, `None` for signals/unknown.
    PaneClosed {
        terminal_id: String,
        exit_status: Option<i32>,
    },
    /// The pane received a BEL.
    Bell { terminal_id: String },
    /// Live OSC 52 text. Apply only to the foreground focused terminal;
    /// discard otherwise, never defer or replay. Clipboard reads are unsupported.
    ClipboardWrite { terminal_id: String, text: String },
    /// The pane's title changed (OSC 0/2). The topology's pane entry is
    /// updated in place as well.
    TitleChanged { terminal_id: String, title: String },
    /// Output burst began (server `Dirty`): the pane is actively producing.
    OutputStarted { terminal_id: String },
    /// Output settled (server `Idle`): the pane went quiet.
    OutputSettled { terminal_id: String },
    /// A shell command began executing (OSC-133 C mark) — server-truth
    /// "running", strictly better than the burst heuristic for plain shells.
    CommandStarted { terminal_id: String },
    /// A shell command finished (OSC-133 D mark). `exit_code` is present when
    /// the shell integration reported one (`OSC 133 ; D ; n`).
    CommandFinished {
        terminal_id: String,
        exit_code: Option<i32>,
    },
    /// The pane's working directory changed (kernel cwd re-queried at prompt
    /// boundaries). The topology's pane entry is updated in place as well.
    CwdChanged { terminal_id: String, cwd: String },
    /// The pane's input lease changed hands or was restated (ADR-0033).
    /// `holder` is the server's client id of whoever drives it, `None`
    /// while the wheel is free; `mine` says it is this connection. Taking
    /// over is a deliberate `acquire_input(seize: true)`, never implied by
    /// focus or a reattach (`docs/protocol.md#attach-and-output`).
    InputHolderChanged {
        terminal_id: String,
        holder: Option<u32>,
        mine: bool,
    },
    /// An agent in the pane is waiting on a human answer (phux-2sl6). Projects
    /// the wire `AgentEvent::Asked` so Swift can populate its `AgentQuestion`
    /// without re-deriving the prompt from the grid: `id`/`question`/`suggestions`
    /// map one-for-one and `waiting_seconds` is the optional `elapsed_seconds`.
    ///
    /// An announcement only: `AgentAskedState` is the level that retracts it.
    AgentAsked {
        terminal_id: String,
        question_id: String,
        text: String,
        suggestions: Vec<String>,
        waiting_seconds: Option<u64>,
    },
    /// Whether a question is pending in the pane now: the server-owned
    /// `phux.agent.asked/v1` flag, read on attach, followed live, and re-read
    /// after every event gap and reconnect. `asked: false` retracts every
    /// earlier `AgentAsked` for the pane: drop it, never resurrect it.
    /// `asked: true` says some question is pending without naming it; the
    /// latest `AgentAsked` describes it, but after a gap or reconnect that
    /// description may be older than the question.
    AgentAskedState { terminal_id: String, asked: bool },
    /// An exact SDK intent's outcome. Queued is submission, not provider
    /// consumption; Unknown requires reconciliation and must not be replayed.
    NativeAgentResult {
        agent_session_id: String,
        operation_id: u64,
        /// `admitted`, `queued`, `completed`, `refused`, or `unknown`.
        disposition: String,
        detail: Option<String>,
    },
    /// The pane's `phux.agent/v1` L3 record changed, or its current value
    /// arrived on (re)attach (phux-cck / ADR-0046: the server-side detector
    /// derives `state` from the live screen and publishes edge-filtered
    /// writes; the bridge's per-pane GET replays the CURRENT record on every
    /// attach, so the level survives reconnects — phux-q7e.21). A tombstone,
    /// an absent record, or malformed bytes all project as `Unknown` with an
    /// empty `name`: "no declared agent", never a stale badge.
    /// `state_reading` and `attention_reading` say how the normalized values
    /// were read, so a present-but-unsupported word stays visible.
    AgentStateChanged {
        terminal_id: String,
        /// Human-facing agent name; empty when no record is declared.
        name: String,
        /// Open-vocabulary kind slug (e.g. "claude"), when declared.
        kind: Option<String>,
        /// Free-form association label declared by the agent record.
        session: Option<String>,
        state: AgentState,
        /// The EFFECTIVE attention: the record's declared level, or the
        /// spec's derivation from `state` when absent (docs/program-sequences.md#the-agent-record).
        attention: AgentAttention,
        /// How `state` was read: no record, undeclared, explicit `unknown`,
        /// recognized, or an unsupported word kept as bounded raw text.
        state_reading: AgentStateReading,
        /// Whether `attention` was declared, derived, or an unsupported word.
        attention_reading: AgentAttentionReading,
    },
    /// Records from an agent session (ADR-0103): the `AgentSession`
    /// resource `agent_session_id`, running in the pane
    /// `parent_terminal_id`, streams `AgentEventsJsonlV1` records, and the
    /// bridge subscribes every one whose pane this connection streams.
    /// `jsonl` holds zero or more complete records, one JSON object per
    /// line (`{"seq","ts_ms","type","data"}`; key order is not
    /// significant), including the `phux.transcript/v1` entries
    /// `provider_raw` records carry (ADR-0156). `seq` is the newest
    /// record's server-assigned sequence, 0 when there is none.
    ///
    /// `Retained` is the session's whole retained stream: replace every
    /// record held for it, even when empty. `Live` appends an incremental
    /// batch from live output or a retained resume suffix after reconnect or
    /// resync. An empty successful resume need not emit an event.
    /// `Closed` has no records: the session ended, its pane was detached,
    /// or the server restarted; nothing follows, and its records may be
    /// dropped. The id may name a fresh pane or session after a restart. `parent_terminal_id`, `provider` and
    /// `native_id` repeat the server's catalog on every event, so a
    /// consumer needs no topology join.
    AgentRecords {
        agent_session_id: String,
        parent_terminal_id: Option<String>,
        provider: Option<String>,
        native_id: Option<String>,
        kind: AgentRecordsKind,
        seq: u64,
        jsonl: String,
    },
    /// The server reported an ERROR frame.
    ServerError { message: String },
    /// The transport dropped; the runtime is reconnecting. Delivered only by
    /// `take_publication`, never by `take_events`.
    ConnectionLost { message: Option<String> },
    /// Connection `connection_epoch` opened: every later event in the batch
    /// belongs to it, every earlier one to a previous connection, even when
    /// session and pane ids are reused. Per-connection state (pending
    /// questions, attention, input fences) resets here, not at an ordinary
    /// `TopologyChanged`. Delivered only by `take_publication`.
    ConnectionOpened { connection_epoch: u64 },
}

impl core::fmt::Debug for WireEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Keep ordinary event diagnostics while never formatting clipboard
        // contents, including through a WirePublication's derived Debug.
        macro_rules! debug_events {
            ($($variant:ident $({ $($field:ident),* })?),* $(,)?) => {
                match self {
                    Self::TopologyChanged => f.write_str("TopologyChanged"),
                    Self::OrganizationChanged => f.write_str("OrganizationChanged"),
                    Self::ClipboardWrite { terminal_id, text } => f
                        .debug_struct("ClipboardWrite")
                        .field("terminal_id", terminal_id)
                        .field("bytes", &text.len())
                        .finish(),
                    $(Self::$variant $({ $($field),* })? => {
                        let mut value = f.debug_struct(stringify!($variant));
                        $($(value.field(stringify!($field), $field);)*)?
                        value.finish()
                    },)*
                }
            };
        }
        debug_events! {
            OrganizationUnavailable { message },
            OrganizationOperation {
                operation, status, refusal, refusal_word, conflict_revision,
                revision, message, undoable, created_session_key
            },
            KillFinished { workflow, status, message, targets },
            PaneSpawned { terminal_id },
            TerminalSpawned { request_id, terminal_id, error },
            TerminalAttached { request_id, terminal_id, error },
            TerminalDetached { request_id, terminal_id, error },
            TerminalsClosed { request_id, error },
            PaneClosed { terminal_id, exit_status },
            Bell { terminal_id },
            TitleChanged { terminal_id, title },
            OutputStarted { terminal_id },
            OutputSettled { terminal_id },
            CommandStarted { terminal_id },
            CommandFinished { terminal_id, exit_code },
            CwdChanged { terminal_id, cwd },
            InputHolderChanged { terminal_id, holder, mine },
            AgentAsked { terminal_id, question_id, text, suggestions, waiting_seconds },
            AgentAskedState { terminal_id, asked },
            NativeAgentResult { agent_session_id, operation_id, disposition, detail },
            AgentStateChanged {
                terminal_id, name, kind, session, state, attention,
                state_reading, attention_reading
            },
            AgentRecords {
                agent_session_id, parent_terminal_id, provider, native_id, kind, seq, jsonl
            },
            ServerError { message },
            ConnectionLost { message },
            ConnectionOpened { connection_epoch },
        }
    }
}
