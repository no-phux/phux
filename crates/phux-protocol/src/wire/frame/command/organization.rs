//! The organization (`docs/organization.md`) and delegation [`Command`]
//! variants.

#[cfg(doc)]
use super::Command;

/// Appends the organization and delegation [`Command`] variants to the list being
/// built and hands it to `$next`; see `command_enum!` in `command.rs`.
macro_rules! organization_commands {
    ($next:ident!($($rest:tt)*) $($done:tt)*) => {
        $next!($($rest)* $($done)*
    /// Read a retained placed-creation receipt without repeating any effect.
    /// Requires `API_DESCRIPTION` and inventory on the original destination session.
    GetCreation {
        /// Retry identity of the original creation.
        operation: OperationId,
        /// Original durable destination, not the resource's current session.
        session: SessionKey,
    },
    /// Read one bounded page of the organization (`docs/organization.md`):
    /// the records the caller inventories, with revisions, live handles,
    /// totals, and the journal cursor to subscribe after. Answered
    /// `OkWith(OrganizationPage)`. Gated, like every organization command,
    /// on [`ServerFeatureExt::Organization`](crate::caps::ServerFeatureExt::Organization).
    GetOrganization {
        /// Which records to list.
        filter: OrgFilter,
        /// Records per page; `0` asks for the server default.
        page_size: u32,
        /// The previous page's `next_cursor`, or `None` for the first page.
        cursor: Option<Vec<u8>>,
    },
    /// Create a session, and with `launch` its first resource and launch
    /// intent, in one transaction. An `Ephemeral` session requires `launch`.
    SessionCreate {
        /// Caller-generated operation id.
        operation: OperationId,
        /// Unique among live sessions; at most 256 bytes.
        name: String,
        /// Whether it outlives its last resource.
        lifetime: SessionLifetime,
        /// Descriptive attributes, in order.
        attrs: Vec<(String, String)>,
        /// The first resource's launch, if any.
        launch: Option<Box<LaunchSpec>>,
    },
    /// Rename a session; a label change only.
    SessionRename {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The session.
        session: SessionKey,
        /// The new name; at most 256 bytes.
        name: String,
    },
    /// Link a session into a named project, or clear the link with `None`;
    /// a label change only, last writer wins. Gated on
    /// `features_ext.SESSION_PROJECT`.
    SessionSetProject {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The session.
        session: SessionKey,
        /// The project link (session attribute `project.link`), at most 256
        /// bytes with no surrounding whitespace; `None` clears it.
        project: Option<String>,
    },
    /// Change a session's lifetime; `Ephemeral` on an empty session deletes
    /// it.
    SessionSetLifetime {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The session.
        session: SessionKey,
        /// The new lifetime.
        lifetime: SessionLifetime,
    },
    /// Delete an empty session; refused `NotEmpty` while it has members.
    SessionDelete {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The session.
        session: SessionKey,
    },
    /// End every member of a session and delete it, in one server-side
    /// step. Gated on `features_ext.SESSION_KILL`; answered `COMMITTED` with
    /// the member count it ended.
    SessionKill {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The session.
        session: SessionKey,
    },
    /// Create a resource record in a session, with its membership and launch
    /// intent, in one transaction.
    ResourceCreate {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The session it joins.
        session: SessionKey,
        /// What backs it.
        kind: ResourceKind,
        /// How it starts; `None` for a kind that needs no launch.
        launch: Option<Box<LaunchSpec>>,
        /// Display name; at most 256 bytes.
        name: Option<String>,
        /// What happens to the record when execution ends.
        on_exit: ExitPolicy,
        /// Whether the first activation starts now or the record stays
        /// dormant until relaunched.
        launch_mode: LaunchMode,
        /// Optional placement committed with membership and launch intent.
        /// Requires `features_ext.CREATE_PLACEMENT`; never silently downgrade.
        placement: Option<Box<super::CreatePlacement>>,
    },
    /// Move one resource between sessions; membership only, no restart.
    ResourceMove {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The resource.
        resource: ResourceKey,
        /// Its current session.
        from: SessionKey,
        /// The source session's expected revision.
        from_expected: Revision,
        /// The destination session.
        to: SessionKey,
        /// The destination's expected revision.
        to_expected: Revision,
    },
    /// Move a fixed set of resources into one session atomically: one
    /// refused item refuses the whole batch. At most
    /// [`MAX_ORG_BATCH`](super::MAX_ORG_BATCH) items.
    ResourceMoveMany {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The resources, each with its source session and revision.
        items: Vec<MoveItem>,
        /// The destination session.
        to: SessionKey,
        /// The destination's expected revision.
        to_expected: Revision,
    },
    /// Rename a resource; `None` clears the name.
    ResourceRename {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The resource.
        resource: ResourceKey,
        /// The new name; at most 256 bytes.
        name: Option<String>,
    },
    /// End a resource's current activation. Answered `Accepted`; the
    /// outcome arrives as `ActivationChanged`.
    ResourceTerminate {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The activation to end; a stale one is refused `Stale`.
        target: LiveTarget,
        /// Graceful or forced.
        mode: TerminateMode,
    },
    /// Remove a resource record whose execution is proven inactive.
    ResourceRemove {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The resource.
        resource: ResourceKey,
    },
    /// Start a new activation from the resource's launch; refused while one
    /// is current or lost.
    ResourceRelaunch {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The resource.
        resource: ResourceKey,
        /// Its expected revision.
        expected: Revision,
    },
    /// Clear an imported session's intake mark; members and execution are
    /// untouched. Legacy public/serialized variant name; prefer
    /// [`Command::intake_resolve`] when constructing this command.
    InboxResolve {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The imported session.
        session: SessionKey,
        /// Its expected revision.
        expected: Revision,
    },
    /// Apply a committed organization edit's inverse when every recorded
    /// resulting revision still holds. Never reverses execution.
    OrganizationUndo {
        /// Caller-generated id of this undo.
        operation: OperationId,
        /// The operation to undo.
        undo: OperationId,
        /// The resulting revisions its receipt reported; at most
        /// [`MAX_ORG_BATCH`](super::MAX_ORG_BATCH).
        expected: Vec<RecordRevision>,
    },
    /// Read an arrangement; answered `OkWith(Arrangement)`.
    ArrangementGet {
        /// The arrangement.
        arrangement: ArrangementKey,
    },
    /// Replace an arrangement's document. A session arrangement may name
    /// only that session's members.
    ArrangementReplace {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The arrangement.
        arrangement: ArrangementKey,
        /// Its expected revision.
        expected: Revision,
        /// The new document; at most
        /// [`MAX_ARRANGEMENT_BYTES`](super::MAX_ARRANGEMENT_BYTES) encoded.
        document: ArrangementDoc,
    },
    /// Mint a delegated agent credential on `session` (ADR-0163 §5):
    /// `INVENTORY`, `OBSERVE`, `INPUT`, and `BIND` on that session only, on behalf
    /// of the sender, through an owner-only socket of its own. Owner socket
    /// only; answered `OkWith(Delegation)`. A repeat of `operation` answers
    /// the credential it minted.
    DelegateAgent {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The session the agent may act on.
        session: SessionKey,
        /// Requested lifetime in seconds; `0` asks for the server default.
        /// The server clamps it to its maximum.
        ttl_secs: u32,
    },
    /// Withdraw a delegated agent credential: its socket closes and every
    /// connection acting as it is revoked. Owner socket only; answered `Ok`,
    /// also when the credential is already gone.
    RevokeDelegation {
        /// Caller-generated operation id.
        operation: OperationId,
        /// The credential, as [`Delegation::credential_id`](super::Delegation::credential_id)
        /// named it; at most 256 bytes.
        credential_id: String,
    },
        );
    };
}
