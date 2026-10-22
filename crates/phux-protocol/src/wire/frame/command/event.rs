//! [`AgentEvent`] and the enums its bodies carry (SPEC §7.5).

use crate::ids::{
    ActivationId, ArrangementKey, ClientId, LeaseEpoch, ResourceId, ResourceKey, ResourceKind,
    Revision, SessionKey,
};
use crate::wire::frame::{ActorRef, ArrangementOwner, ExecState, Outcome, SessionLifetime};

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
    /// Someone just sent input to the envelope's Terminal. Unjournaled:
    /// the `EVENT` stamp is absent, so the body is only the origin label
    /// and never a credential id (`docs/spec/L1.md` §7.1).
    InputActivity {
        /// Who sent the input: the client's HELLO name, or `client:<id>`
        /// when it has none.
        origin: String,
    },
    /// A session record was created (`docs/spec/L1.md` §5.7). Field-tagged
    /// body, like every organization event.
    SessionCreated {
        /// The session.
        session: SessionKey,
        /// Its name.
        name: String,
        /// Its lifetime.
        lifetime: SessionLifetime,
        /// Its revision after the change.
        revision: Revision,
    },
    /// A session was renamed.
    SessionRenamed {
        /// The session.
        session: SessionKey,
        /// The new name.
        name: String,
        /// Its revision after the change.
        revision: Revision,
    },
    /// A session's project link was set, or cleared (`project: None`).
    SessionProjectSet {
        /// The session.
        session: SessionKey,
        /// The new label.
        project: Option<String>,
        /// Its revision after the change.
        revision: Revision,
    },
    /// A session record was deleted.
    SessionDeleted {
        /// The session.
        session: SessionKey,
        /// The name it had.
        name: String,
        /// Its final revision.
        revision: Revision,
    },
    /// A session's lifetime changed.
    SessionLifetimeChanged {
        /// The session.
        session: SessionKey,
        /// The new lifetime.
        lifetime: SessionLifetime,
        /// Its revision after the change.
        revision: Revision,
    },
    /// A resource moved between sessions.
    MembershipChanged {
        /// The resource.
        resource: ResourceKey,
        /// The session it left.
        from: SessionKey,
        /// The session it joined.
        to: SessionKey,
        /// The source session's revision after the change.
        from_revision: Revision,
        /// The destination session's revision after the change.
        to_revision: Revision,
    },
    /// A resource record was created.
    ResourceCreated {
        /// The resource.
        resource: ResourceKey,
        /// Its session.
        session: SessionKey,
        /// What backs it.
        kind: ResourceKind,
    },
    /// A resource record was removed.
    ResourceRemoved {
        /// The resource.
        resource: ResourceKey,
        /// The session it belonged to.
        session: SessionKey,
        /// What backed it.
        kind: ResourceKind,
    },
    /// A resource's activation changed state.
    ActivationChanged {
        /// The resource.
        resource: ResourceKey,
        /// The activation.
        activation: ActivationId,
        /// Its new state.
        state: ExecState,
        /// How it ended, once it has.
        outcome: Option<Outcome>,
    },
    /// An arrangement's document changed, or the arrangement was deleted.
    ArrangementChanged {
        /// The arrangement.
        arrangement: ArrangementKey,
        /// The record that owns it.
        owner: ArrangementOwner,
        /// Its revision after the change; `None` when it was deleted.
        revision: Option<Revision>,
    },
    /// A resource was renamed.
    ResourceRenamed {
        /// The resource.
        resource: ResourceKey,
        /// The new name; `None` when it was cleared.
        name: Option<String>,
        /// Its revision after the change.
        revision: Revision,
    },
    /// A resource's input lease changed hands.
    LeaseChanged {
        /// The resource.
        resource: ResourceKey,
        /// The new lease epoch.
        epoch: LeaseEpoch,
        /// The new holder, or `None` when released.
        holder: Option<ActorRef>,
    },
    /// A session's intake mark changed: an `INBOX_RESOLVE`, or an undo
    /// restoring it.
    IntakeChanged {
        /// The session.
        session: SessionKey,
        /// Whether it now awaits organization.
        awaiting_organization: bool,
        /// Its revision after the change.
        revision: Revision,
    },
    /// A later native SDK operation outcome, distinct from PTY delivery.
    NativeAgentResult {
        /// Exact SDK-owned `AgentSession`.
        agent_session_id: ResourceId,
        /// Original native operation identity.
        operation_id: u64,
        /// Actual controller-reported disposition.
        disposition: crate::native_agent::NativeDisposition,
        /// Factual stage or outcome explanation.
        detail: Option<String>,
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
