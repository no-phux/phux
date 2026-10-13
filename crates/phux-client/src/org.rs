//! Organization commands over one CLI or MCP connection (`docs/spec/L1.md`
//! §5.7), behind `features_ext.ORGANIZATION`.
//!
//! Every helper here refuses before sending when the server does not
//! advertise the bit ([`OrgError::Unsupported`]); callers that keep a legacy
//! path branch on [`supported`] first. Every mutating command carries a
//! fresh [`OperationId`] unless the caller supplies one, and a connection
//! that drops after the command was sent reports
//! [`OrgError::OutcomeUnknown`] with that id, so the caller can resend the
//! same id on a new connection or reconcile from [`fetch`].
//!
//! Compare-and-swap edits (moves, relaunch, Inbox resolve, arrangement
//! replace) re-read the records they name and retry when the server answers
//! `CONFLICT`, at most [`MAX_CAS_ATTEMPTS`] times. Removes, deletes,
//! renames, and lifetime changes carry no revision: one round trip each.
//!
//! These are the operations behind `phux rm`, `relaunch`, `restore`, `mv`,
//! bulk move, undo, and Inbox resolve; the CLI and MCP adapters own
//! selector parsing and rendering.

use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{
    AuthorityId, KEY_MIN_PREFIX, OperationId, ResourceKey, ResourceKind, Revision, SessionId,
    SessionKey,
};
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementValue, Command, CommandValue, Committed, ErrorCode, ExecState,
    ExitPolicy, LaunchMode, LaunchSpec, MAX_ORG_BATCH, MoveItem, OrgFilter, OrganizationPage,
    Outcome, Refusal, ResourceRecord, SessionLifetime, SessionRecord, TerminateMode,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::wire::HandleMap;
use crate::state::Degradation;

pub use phux_client_core::org_ops::{
    KillReport, OrgOutcome, SessionOutcome, TargetOutcome, is_inactive, live_target,
    refusal_sentence,
};

/// Most attempts a compare-and-swap edit makes before reporting the
/// conflict.
pub const MAX_CAS_ATTEMPTS: u32 = 4;

/// Records requested per `GET_ORGANIZATION` page.
const PAGE_SIZE: u32 = 512;

/// Most pages one [`fetch`] reads before giving up on a runaway cursor.
const MAX_PAGES: usize = 4096;

/// Why an organization request did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    /// The server does not advertise `ORGANIZATION`; nothing was sent.
    #[error("the server does not support the organization commands (it predates ORGANIZATION)")]
    Unsupported,
    /// The server refused the command and nothing changed.
    #[error("{}", refusal_sentence(.0))]
    Refused(Refusal),
    /// The server answered with a legacy `ERROR`.
    #[error("{message}")]
    Server {
        /// The wire error code.
        code: ErrorCode,
        /// The server's message.
        message: String,
    },
    /// The reply had a shape this command never produces.
    #[error("unexpected reply: {0}")]
    Unexpected(String),
    /// The connection dropped after the command was sent: it may or may not
    /// have committed. Resend `operation` on a new connection, or reconcile.
    #[error("the connection dropped before the server answered; the outcome is unknown")]
    OutcomeUnknown {
        /// The operation to resend or reconcile.
        operation: Option<OperationId>,
    },
    /// No record matches the target.
    #[error("no such record: {0}")]
    NotFound(String),
    /// A key prefix matches more than one record.
    #[error("key prefix {prefix:?} is ambiguous: {}", candidates.join(", "))]
    Ambiguous {
        /// The prefix as typed.
        prefix: String,
        /// The matching keys.
        candidates: Vec<String>,
    },
    /// Transport or decode failure before the command was sent.
    #[error(transparent)]
    Attach(AttachError),
}

impl OrgError {
    /// The refusal, when the server refused.
    #[must_use]
    pub const fn refusal(&self) -> Option<&Refusal> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            _ => None,
        }
    }

    /// A stable lowercase code for `--json` output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Refused(refusal) => refusal.as_str(),
            Self::Server { .. } => "server_error",
            Self::Unexpected(_) => "unexpected_reply",
            Self::OutcomeUnknown { .. } => "outcome_unknown",
            Self::NotFound(_) => "no_such_target",
            Self::Ambiguous { .. } => "selector_not_single",
            Self::Attach(_) => "transport",
        }
    }
}

/// Whether the server behind `conn` advertises `ORGANIZATION`.
#[must_use]
pub fn supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features_ext
            .contains(ServerFeatureExt::Organization)
    })
}

fn require(conn: &Connection) -> Result<(), OrgError> {
    if supported(conn) {
        Ok(())
    } else {
        Err(OrgError::Unsupported)
    }
}

/// A fresh random operation id.
#[must_use]
pub fn mint_operation() -> OperationId {
    loop {
        if let Some(id) = OperationId::new(*uuid::Uuid::new_v4().as_bytes()) {
            return id;
        }
    }
}

/// Send one organization command and classify its reply.
///
/// # Errors
///
/// [`OrgError::Unsupported`] with nothing sent; [`OrgError::OutcomeUnknown`]
/// when the connection drops before the reply; [`OrgError::Attach`] for any
/// other transport failure.
pub async fn request(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<(OrgOutcome, Degradation), OrgError> {
    require(conn)?;
    let operation = command.idempotency_key().copied();
    match conn.request(request_id, command).await {
        Ok(reply) => {
            let (result, interleaved) = reply.into_parts();
            Ok((
                OrgOutcome::from_result(&result),
                Degradation::from_interleaved(&interleaved),
            ))
        }
        Err(AttachError::Disconnected) => Err(OrgError::OutcomeUnknown { operation }),
        Err(err) => Err(OrgError::Attach(err)),
    }
}

/// Send a mutating command and return its receipt.
///
/// # Errors
///
/// [`request`]'s errors, plus [`OrgError::Refused`] / [`OrgError::Server`]
/// for a refusal and [`OrgError::Unexpected`] for another reply shape.
pub async fn commit(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<Committed, OrgError> {
    match request(conn, request_id, command).await?.0 {
        OrgOutcome::Committed(committed) => Ok(*committed),
        other => Err(outcome_error(other)),
    }
}

/// Send a command the server answers `ACCEPTED` (its effects follow as
/// events).
///
/// # Errors
///
/// As [`commit`].
pub async fn accept(
    conn: &mut Connection,
    request_id: u32,
    command: Command,
) -> Result<(), OrgError> {
    match request(conn, request_id, command).await?.0 {
        OrgOutcome::Accepted | OrgOutcome::Committed(_) => Ok(()),
        other => Err(outcome_error(other)),
    }
}

fn outcome_error(outcome: OrgOutcome) -> OrgError {
    match outcome {
        OrgOutcome::Refused(refusal) => OrgError::Refused(refusal),
        OrgOutcome::Error { code, message } => OrgError::Server { code, message },
        OrgOutcome::Unknown { reason } | OrgOutcome::NotSent { reason } => {
            OrgError::Unexpected(reason)
        }
        other => OrgError::Unexpected(format!("{other:?}")),
    }
}

/// Every record one `GET_ORGANIZATION` filter covers, read across all pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgInventory {
    /// The authority the records belong to.
    pub authority: AuthorityId,
    /// The designated Inbox session, when the caller observes it.
    pub inbox: Option<SessionKey>,
    /// Session records, in page order.
    pub sessions: Vec<SessionRecord>,
    /// Resource records, in page order.
    pub resources: Vec<ResourceRecord>,
    /// The organization revision of the first page's cut.
    pub snapshot_revision: Revision,
    /// The journal `seq` at the first page's cut.
    pub journal_seq: u64,
    /// Matching sessions awaiting organization.
    pub total_intake: u32,
    /// Hub notices that arrived with the pages.
    pub degradation: Degradation,
}

impl OrgInventory {
    fn from_page(page: &OrganizationPage) -> Self {
        Self {
            authority: page.authority,
            inbox: page.inbox,
            sessions: page.sessions.clone(),
            resources: page.resources.clone(),
            snapshot_revision: page.snapshot_revision,
            journal_seq: page.journal_seq,
            total_intake: page.total_intake,
            degradation: Degradation::default(),
        }
    }

    /// The session record `key` names.
    #[must_use]
    pub fn session(&self, key: &SessionKey) -> Option<&SessionRecord> {
        self.sessions.iter().find(|session| session.key == *key)
    }

    /// The session record named `name`.
    #[must_use]
    pub fn session_named(&self, name: &str) -> Option<&SessionRecord> {
        self.sessions.iter().find(|session| session.name == name)
    }

    /// The session record whose live handle is `id`.
    #[must_use]
    pub fn session_by_live(&self, id: SessionId) -> Option<&SessionRecord> {
        self.sessions
            .iter()
            .find(|session| session.live == Some(id))
    }

    /// The resource record `key` names.
    #[must_use]
    pub fn resource(&self, key: &ResourceKey) -> Option<&ResourceRecord> {
        self.resources.iter().find(|resource| resource.key == *key)
    }

    /// The resource record executing as `id` here.
    #[must_use]
    pub fn resource_by_live(&self, id: &ResourceId) -> Option<&ResourceRecord> {
        self.resources
            .iter()
            .find(|resource| resource.live.as_ref() == Some(id))
    }

    /// The members of `session`, in page order.
    pub fn members<'a>(
        &'a self,
        session: &'a SessionKey,
    ) -> impl Iterator<Item = &'a ResourceRecord> + 'a {
        self.resources
            .iter()
            .filter(move |resource| resource.session == *session)
    }

    /// The key-to-handle map of every resource that executes here.
    #[must_use]
    pub fn handle_map(&self) -> HandleMap {
        HandleMap::from_pairs(
            self.resources
                .iter()
                .map(|resource| (resource.key, resource.live.clone())),
        )
    }

    /// The one session or resource whose key starts with `prefix`
    /// (`^ab12cd`, at least six base32 digits, case-insensitive).
    ///
    /// # Errors
    ///
    /// [`OrgError::NotFound`] on no match or a too-short prefix,
    /// [`OrgError::Ambiguous`] on several.
    pub fn resolve_key_prefix(&self, prefix: &str) -> Result<KeyMatch, OrgError> {
        let prefix = prefix.strip_prefix('^').unwrap_or(prefix);
        if prefix.len() < KEY_MIN_PREFIX {
            return Err(OrgError::NotFound(format!("^{prefix}")));
        }
        let mut matches: Vec<KeyMatch> = self
            .sessions
            .iter()
            .filter(|session| session.key.has_prefix(prefix))
            .map(|session| KeyMatch::Session(session.clone()))
            .collect();
        matches.extend(
            self.resources
                .iter()
                .filter(|resource| resource.key.has_prefix(prefix))
                .map(|resource| KeyMatch::Resource(resource.clone())),
        );
        match matches.len() {
            0 => Err(OrgError::NotFound(format!("^{prefix}"))),
            1 => Ok(matches.remove(0)),
            _ => Err(OrgError::Ambiguous {
                prefix: format!("^{prefix}"),
                candidates: matches.iter().map(KeyMatch::key_text).collect(),
            }),
        }
    }

    /// Resources whose activation ended because a server stopped while it
    /// ran (outcome unobserved), optionally only in `session`: what
    /// `phux restore` relaunches.
    #[must_use]
    pub fn ended_by_restart(&self, session: Option<&SessionKey>) -> Vec<&ResourceRecord> {
        self.resources
            .iter()
            .filter(|resource| session.is_none_or(|key| resource.session == *key))
            .filter(|resource| {
                resource.activation.is_some_and(|activation| {
                    activation.state == ExecState::Completed
                        && activation.outcome == Some(Outcome::Unobserved)
                })
            })
            .collect()
    }

    /// What `phux restore` relaunches, in record order: every resource a
    /// server restart ended ([`Self::ended_by_restart`]) and every Terminal
    /// never started (a dormant record, such as an imported archive's pane),
    /// optionally only in `session`.
    #[must_use]
    pub fn restorable(&self, session: Option<&SessionKey>) -> Vec<&ResourceRecord> {
        let ended: Vec<ResourceKey> = self
            .ended_by_restart(session)
            .into_iter()
            .map(|record| record.key)
            .collect();
        self.resources
            .iter()
            .filter(|resource| {
                ended.contains(&resource.key)
                    || (resource.activation.is_none()
                        && resource.kind == ResourceKind::Terminal
                        && session.is_none_or(|key| resource.session == *key))
            })
            .collect()
    }
}

/// Display names for the keys one output lists, written `^ab12cd`.
///
/// Each is the key's shortest prefix that no other listed key shares, at
/// least six base32 digits ([`phux_core::Key16::short`]). Session and
/// resource keys share one namespace, as
/// [`OrgInventory::resolve_key_prefix`] resolves both.
#[derive(Debug, Clone, Default)]
pub struct KeyNames {
    keys: Vec<phux_core::ResourceKey>,
}

impl KeyNames {
    /// Names unique among `keys` (raw key bytes).
    #[must_use]
    pub fn new(keys: impl IntoIterator<Item = [u8; 16]>) -> Self {
        Self {
            keys: keys
                .into_iter()
                .map(phux_core::ResourceKey::from_bytes)
                .collect(),
        }
    }

    /// Names unique among every session and resource record of `inventory`.
    #[must_use]
    pub fn of_inventory(inventory: &OrgInventory) -> Self {
        Self::new(
            inventory
                .sessions
                .iter()
                .map(|session| *session.key.as_bytes())
                .chain(
                    inventory
                        .resources
                        .iter()
                        .map(|resource| *resource.key.as_bytes()),
                ),
        )
    }

    /// `^` and the shortest unique prefix of `key`'s text.
    #[must_use]
    pub fn display(&self, key: [u8; 16]) -> String {
        use phux_core::Key16 as _;
        format!(
            "^{}",
            phux_core::ResourceKey::from_bytes(key).short(self.keys.iter())
        )
    }
}

/// What a key prefix names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyMatch {
    /// A session record.
    Session(SessionRecord),
    /// A resource record.
    Resource(ResourceRecord),
}

impl KeyMatch {
    /// The full key text.
    #[must_use]
    pub fn key_text(&self) -> String {
        match self {
            Self::Session(session) => session.key.to_string(),
            Self::Resource(resource) => resource.key.to_string(),
        }
    }
}

/// Read every record `filter` covers, across all pages. A cursor the server
/// no longer holds restarts from the first page, once.
///
/// # Errors
///
/// [`request`]'s errors, a refusal, or an unexpected reply.
pub async fn fetch(
    conn: &mut Connection,
    filter: OrgFilter,
    first_request_id: u32,
) -> Result<OrgInventory, OrgError> {
    let mut restarted = false;
    let mut request_id = first_request_id;
    'restart: loop {
        let mut inventory: Option<OrgInventory> = None;
        let mut cursor = None;
        for _ in 0..MAX_PAGES {
            let (outcome, degradation) = request(
                conn,
                request_id,
                Command::GetOrganization {
                    filter,
                    page_size: PAGE_SIZE,
                    cursor: cursor.take(),
                },
            )
            .await?;
            request_id = request_id.wrapping_add(1);
            let page = match outcome {
                OrgOutcome::Value(value) => match *value {
                    CommandValue::OrganizationPage(page) => page,
                    other => return Err(OrgError::Unexpected(format!("{other:?}"))),
                },
                OrgOutcome::Refused(_) if inventory.is_some() && !restarted => {
                    restarted = true;
                    continue 'restart;
                }
                other => return Err(outcome_error(other)),
            };
            let next = page.next_cursor.clone();
            match inventory.as_mut() {
                None => {
                    let mut first = OrgInventory::from_page(&page);
                    first.degradation = degradation;
                    inventory = Some(first);
                }
                Some(inventory) => {
                    inventory.sessions.extend(page.sessions.iter().cloned());
                    inventory.resources.extend(page.resources.iter().cloned());
                }
            }
            match next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        return inventory.ok_or_else(|| OrgError::Unexpected("no page".to_owned()));
    }
}

/// Read one arrangement document and its revision.
///
/// # Errors
///
/// As [`request`], plus a refusal or an unexpected reply.
pub async fn arrangement(
    conn: &mut Connection,
    request_id: u32,
    key: phux_protocol::ids::ArrangementKey,
) -> Result<ArrangementValue, OrgError> {
    match request(
        conn,
        request_id,
        Command::ArrangementGet { arrangement: key },
    )
    .await?
    .0
    {
        OrgOutcome::Value(value) => match *value {
            CommandValue::Arrangement(arrangement) => Ok(*arrangement),
            other => Err(OrgError::Unexpected(format!("{other:?}"))),
        },
        other => Err(outcome_error(other)),
    }
}

/// First request id the helpers below allocate from; clear of the small ids
/// other verbs use on the same connection.
const FIRST_REQUEST_ID: u32 = 0x6f00;

/// Re-read one session's records (`OrgFilter::Session`).
async fn session_view(
    conn: &mut Connection,
    session: SessionKey,
) -> Result<OrgInventory, OrgError> {
    fetch(conn, OrgFilter::Session(session), FIRST_REQUEST_ID).await
}

fn resource_in(view: &OrgInventory, key: &ResourceKey) -> Result<ResourceRecord, OrgError> {
    view.resource(key)
        .cloned()
        .ok_or_else(|| OrgError::NotFound(key.to_string()))
}

fn session_in(view: &OrgInventory, key: &SessionKey) -> Result<SessionRecord, OrgError> {
    view.session(key)
        .cloned()
        .ok_or_else(|| OrgError::NotFound(key.to_string()))
}

/// Find the session a resource belongs to, then re-read that session.
async fn resource_view(
    conn: &mut Connection,
    resource: ResourceKey,
) -> Result<(OrgInventory, ResourceRecord), OrgError> {
    let all = fetch(conn, OrgFilter::All, FIRST_REQUEST_ID).await?;
    let session = resource_in(&all, &resource)?.session;
    let view = session_view(conn, session).await?;
    let record = resource_in(&view, &resource)?;
    Ok((view, record))
}

/// One compare-and-swap edit, built afresh from current records on every
/// attempt.
#[derive(Debug, Clone, Copy)]
enum Edit<'a> {
    Relaunch(ResourceKey),
    Move {
        resource: ResourceKey,
        to: SessionKey,
    },
    MoveMany {
        resources: &'a [ResourceKey],
        to: SessionKey,
    },
    InboxResolve(SessionKey),
    ReplaceArrangement(SessionKey, &'a ArrangementDoc),
}

/// The command for `edit` against freshly read records, under a fresh
/// operation id.
async fn build(conn: &mut Connection, edit: Edit<'_>) -> Result<Command, OrgError> {
    let operation = mint_operation();
    Ok(match edit {
        Edit::Relaunch(resource) => {
            let (_, record) = resource_view(conn, resource).await?;
            Command::ResourceRelaunch {
                operation,
                resource,
                expected: record.revision,
            }
        }
        Edit::InboxResolve(session) => {
            let record = session_in(&session_view(conn, session).await?, &session)?;
            Command::InboxResolve {
                operation,
                session,
                expected: record.revision,
            }
        }
        Edit::Move { resource, to } => {
            let (from_view, record) = resource_view(conn, resource).await?;
            let from = session_in(&from_view, &record.session)?;
            let to_record = session_in(&session_view(conn, to).await?, &to)?;
            Command::ResourceMove {
                operation,
                resource,
                from: from.key,
                from_expected: from.revision,
                to,
                to_expected: to_record.revision,
            }
        }
        Edit::ReplaceArrangement(session, document) => {
            replace_command(conn, operation, session, document).await?
        }
        Edit::MoveMany { resources, to } => {
            let all = fetch(conn, OrgFilter::All, FIRST_REQUEST_ID).await?;
            let to_record = session_in(&all, &to)?;
            let mut items = Vec::with_capacity(resources.len());
            for key in resources {
                let record = resource_in(&all, key)?;
                let from = session_in(&all, &record.session)?;
                items.push(MoveItem {
                    resource: *key,
                    from: from.key,
                    from_expected: from.revision,
                });
            }
            Command::ResourceMoveMany {
                operation,
                items,
                to,
                to_expected: to_record.revision,
            }
        }
    })
}

/// `ARRANGEMENT_REPLACE` of `session`'s arrangement at its current revision.
async fn replace_command(
    conn: &mut Connection,
    operation: OperationId,
    session: SessionKey,
    document: &ArrangementDoc,
) -> Result<Command, OrgError> {
    let record = session_in(&session_view(conn, session).await?, &session)?;
    let key = record
        .arrangement
        .ok_or_else(|| OrgError::NotFound(format!("arrangement of {session}")))?;
    let current = arrangement(conn, FIRST_REQUEST_ID, key).await?;
    Ok(Command::ArrangementReplace {
        operation,
        arrangement: key,
        expected: current.revision,
        document: document.clone(),
    })
}

/// Run `edit` against fresh records until it does not conflict, at most
/// [`MAX_CAS_ATTEMPTS`] times. An `ACCEPTED` reply (a relaunch whose start
/// follows as events) reads as a receipt under the command's operation.
async fn run_cas(conn: &mut Connection, edit: Edit<'_>) -> Result<Committed, OrgError> {
    let mut current = Revision::default();
    for _ in 0..MAX_CAS_ATTEMPTS {
        let command = build(conn, edit).await?;
        let operation = command.idempotency_key().copied();
        match request(conn, FIRST_REQUEST_ID, command).await?.0 {
            OrgOutcome::Committed(committed) => return Ok(*committed),
            OrgOutcome::Accepted => {
                return operation
                    .map(|operation| Committed::new(operation, Revision::default()))
                    .ok_or_else(|| {
                        OrgError::Unexpected("accepted without an operation".to_owned())
                    });
            }
            OrgOutcome::Refused(Refusal::Conflict { current: now }) => current = now,
            other => return Err(outcome_error(other)),
        }
    }
    Err(OrgError::Refused(Refusal::Conflict { current }))
}

/// `phux rm` for a resource: remove an inactive resource record.
///
/// # Errors
///
/// `EXECUTION_UNCERTAIN` for a lost activation; a refusal when it still
/// runs; transport failures.
pub async fn remove_resource(
    conn: &mut Connection,
    resource: ResourceKey,
) -> Result<Committed, OrgError> {
    let operation = mint_operation();
    let command = Command::ResourceRemove {
        operation,
        resource,
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// `phux rm` for a session: delete an empty session.
///
/// # Errors
///
/// `NOT_EMPTY` while it has members; transport failures.
pub async fn delete_session(
    conn: &mut Connection,
    session: SessionKey,
) -> Result<Committed, OrgError> {
    let operation = mint_operation();
    let command = Command::SessionDelete { operation, session };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// `phux relaunch`: start a new activation of an inactive resource.
///
/// # Errors
///
/// `EXECUTION_UNCERTAIN` for a lost activation; a refusal while it runs;
/// transport failures.
pub async fn relaunch(conn: &mut Connection, resource: ResourceKey) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::Relaunch(resource)).await
}

/// Create one resource record of `kind` in `session` (`RESOURCE_CREATE`).
///
/// It starts from `launch`, or, under [`LaunchMode::Deferred`], stays a
/// dormant record holding `launch` until [`relaunch`] or [`restore`] starts
/// it. The record is removed when its process exits.
///
/// # Errors
///
/// A refusal (an unknown session, a deferred create with no `launch`);
/// transport failures.
pub async fn create_resource(
    conn: &mut Connection,
    session: SessionKey,
    kind: ResourceKind,
    launch: Option<LaunchSpec>,
    launch_mode: LaunchMode,
) -> Result<Committed, OrgError> {
    commit(
        conn,
        FIRST_REQUEST_ID,
        Command::ResourceCreate {
            operation: mint_operation(),
            session,
            kind,
            launch: launch.map(Box::new),
            name: None,
            on_exit: ExitPolicy::Remove,
            launch_mode,
        },
    )
    .await
}

/// Replace `session`'s arrangement with `document` (key space), against
/// its current revision.
///
/// # Errors
///
/// [`OrgError::NotFound`] when the session has no arrangement; a refusal
/// for a document naming a non-member; transport failures.
pub async fn replace_session_arrangement(
    conn: &mut Connection,
    session: SessionKey,
    document: &ArrangementDoc,
) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::ReplaceArrangement(session, document)).await
}

/// `phux restore [SESSION]`: relaunch every restorable resource
/// ([`OrgInventory::restorable`]), in their sessions, one result per
/// resource in page order.
///
/// # Errors
///
/// Only a failure to read the records; each relaunch reports its own result.
pub async fn restore(
    conn: &mut Connection,
    session: Option<SessionKey>,
) -> Result<Vec<(ResourceKey, Result<Committed, OrgError>)>, OrgError> {
    let filter = session.map_or(OrgFilter::All, OrgFilter::Session);
    let view = fetch(conn, filter, FIRST_REQUEST_ID).await?;
    let targets: Vec<ResourceKey> = view
        .restorable(session.as_ref())
        .into_iter()
        .map(|record| record.key)
        .collect();
    let mut results = Vec::with_capacity(targets.len());
    for key in targets {
        let result = relaunch(conn, key).await;
        results.push((key, result));
    }
    Ok(results)
}

/// [`restore`] for the session named `name`, over a fresh connection to
/// `socket_path`.
///
/// # Errors
///
/// [`OrgError::Attach`] when the server cannot be reached,
/// [`OrgError::NotFound`] when no session has that name, or a failure to
/// read the records; each relaunch reports its own result.
pub async fn restore_session_on(
    socket_path: &std::path::Path,
    name: &str,
) -> Result<Vec<(ResourceKey, Result<Committed, OrgError>)>, OrgError> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(OrgError::Attach)?;
    let session = fetch(&mut conn, OrgFilter::All, FIRST_REQUEST_ID)
        .await?
        .session_named(name)
        .map(|record| record.key)
        .ok_or_else(|| OrgError::NotFound(name.to_owned()))?;
    let results = restore(&mut conn, Some(session)).await;
    drop(conn);
    results
}

/// `phux mv`: move one resource's membership into session `to`. The process
/// keeps running.
///
/// # Errors
///
/// A refusal (authorization on both sessions, a missing record); transport
/// failures.
pub async fn move_resource(
    conn: &mut Connection,
    resource: ResourceKey,
    to: SessionKey,
) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::Move { resource, to }).await
}

/// One chunk of a bulk move and how it ended.
#[derive(Debug)]
pub struct BatchResult {
    /// The resources in this chunk.
    pub resources: Vec<ResourceKey>,
    /// The chunk's receipt or refusal. A refused chunk moved nothing.
    pub result: Result<Committed, OrgError>,
}

/// Move many resources into `to`, chunked at the server's batch bound
/// ([`MAX_ORG_BATCH`]); each chunk is atomic and reports its own result.
///
/// # Errors
///
/// Only a failure to read the records; chunks report their own results.
pub async fn move_many(
    conn: &mut Connection,
    resources: &[ResourceKey],
    to: SessionKey,
) -> Result<Vec<BatchResult>, OrgError> {
    let mut results = Vec::new();
    for chunk in resources.chunks(MAX_ORG_BATCH) {
        let result = run_cas(
            conn,
            Edit::MoveMany {
                resources: chunk,
                to,
            },
        )
        .await;
        results.push(BatchResult {
            resources: chunk.to_vec(),
            result,
        });
    }
    Ok(results)
}

/// Undo a committed organization edit by its receipt. Checked against the
/// current state: a changed record refuses `CONFLICT`, an expired receipt
/// `NOT_FOUND`.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn undo(conn: &mut Connection, receipt: &Committed) -> Result<Committed, OrgError> {
    let command = Command::OrganizationUndo {
        operation: mint_operation(),
        undo: receipt.operation,
        expected: receipt.revisions.clone(),
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// Clear a session's Inbox intake mark; members and execution are untouched.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn inbox_resolve(
    conn: &mut Connection,
    session: SessionKey,
) -> Result<Committed, OrgError> {
    run_cas(conn, Edit::InboxResolve(session)).await
}

/// Make a session persistent or ephemeral. An empty session made ephemeral
/// is deleted.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn set_lifetime(
    conn: &mut Connection,
    session: SessionKey,
    lifetime: SessionLifetime,
) -> Result<Committed, OrgError> {
    let command = Command::SessionSetLifetime {
        operation: mint_operation(),
        session,
        lifetime,
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// Rename (or, with `None`, unname) a resource record.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn rename_resource(
    conn: &mut Connection,
    resource: ResourceKey,
    name: Option<&str>,
) -> Result<Committed, OrgError> {
    let command = Command::ResourceRename {
        operation: mint_operation(),
        resource,
        name: name.map(str::to_owned),
    };
    commit(conn, FIRST_REQUEST_ID, command).await
}

/// Terminate a resource's current activation; the outcome arrives as an
/// `ACTIVATION_CHANGED` event.
///
/// # Errors
///
/// [`OrgError::NotFound`] when nothing executes here; the refusal;
/// transport failures.
pub async fn terminate(
    conn: &mut Connection,
    record: &ResourceRecord,
    mode: TerminateMode,
) -> Result<(), OrgError> {
    let target = live_target(record).ok_or_else(|| OrgError::NotFound(record.key.to_string()))?;
    accept(
        conn,
        FIRST_REQUEST_ID,
        Command::ResourceTerminate {
            operation: mint_operation(),
            target,
            mode,
        },
    )
    .await
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_protocol::caps::ServerFeatureExtSet;

    use super::*;
    use crate::testkit::{OrgFixture, ScriptSpec, serve_one};

    fn assert_send<T: Send>(_: &T) {}

    /// The MCP server runs these on a multi-threaded runtime: every
    /// organization future must be `Send`. Compiled, never called.
    #[allow(dead_code, reason = "a compile-time check")]
    fn organization_futures_are_send(conn: &mut Connection, key: ResourceKey, to: SessionKey) {
        assert_send(&remove_resource(conn, key));
        assert_send(&move_many(conn, &[], to));
        assert_send(&restore(conn, None));
        assert_send(&crate::kill::kill_session(conn, SessionId::new(1), "work"));
        assert_send(&crate::pane_move::move_pane(
            conn,
            ResourceId::local(1),
            ResourceId::local(2),
            crate::layout::SplitDir::Horizontal,
            0.5,
            &[],
        ));
        assert_send(&crate::session::rename_checked(
            conn,
            "a",
            "b",
            &mut Vec::new(),
        ));
        assert_send(
            &crate::layout_ops::LayoutOps::new(conn, SessionId::new(1), 1).mutate(
                crate::layout_ops::LayoutMutation::Close {
                    target: ResourceId::local(1),
                },
            ),
        );
    }

    fn org_spec(fixture: OrgFixture) -> ScriptSpec {
        ScriptSpec::new()
            .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
            .organization(fixture)
    }

    async fn connect(
        spec: ScriptSpec,
    ) -> (
        Connection,
        tokio::task::JoinHandle<Vec<phux_protocol::wire::frame::FrameKind>>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = serve_one(dir.path(), spec);
        let conn = Connection::connect(&socket).await.expect("connect");
        (conn, server, dir)
    }

    #[tokio::test]
    async fn every_helper_refuses_without_the_bit_and_sends_nothing() {
        let (mut conn, server, _dir) = connect(ScriptSpec::new()).await;
        let err = fetch(&mut conn, OrgFilter::All, 1).await.unwrap_err();
        assert!(matches!(err, OrgError::Unsupported));
        drop(conn);
        let seen = server.await.unwrap();
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                phux_protocol::wire::frame::FrameKind::Command { .. }
            )),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn fetch_reads_every_page_and_resolves_key_prefixes() {
        let mut fixture = OrgFixture::new().page_size(1);
        let work = fixture.session("work", SessionLifetime::Persistent);
        let a = fixture.resource(work, Some(ExecState::Running));
        let _b = fixture.resource(work, Some(ExecState::Completed));
        let (mut conn, server, _dir) = connect(org_spec(fixture)).await;
        let view = fetch(&mut conn, OrgFilter::All, 1).await.unwrap();
        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.resources.len(), 2);
        assert_eq!(view.members(&work).count(), 2);
        let text = a.to_string();
        assert!(matches!(
            view.resolve_key_prefix(&format!("^{}", &text[..8])).unwrap(),
            KeyMatch::Resource(record) if record.key == a
        ));
        assert!(matches!(
            view.resolve_key_prefix("^ab"),
            Err(OrgError::NotFound(_))
        ));
        drop(conn);
        let seen = server.await.unwrap();
        let pages = seen
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    phux_protocol::wire::frame::FrameKind::Command {
                        command: Command::GetOrganization { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(pages, 3, "one page per record: {seen:?}");
    }

    #[tokio::test]
    async fn a_conflict_is_retried_against_fresh_revisions() {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let gone = fixture.resource(work, Some(ExecState::Completed));
        let fixture = fixture.inject_conflicts(2);
        let (mut conn, server, _dir) = connect(org_spec(fixture)).await;
        relaunch(&mut conn, gone).await.unwrap();
        drop(conn);
        let seen = server.await.unwrap();
        let removes = seen
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    phux_protocol::wire::frame::FrameKind::Command {
                        command: Command::ResourceRelaunch { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(removes, 3, "two conflicts, then the commit");
    }

    #[tokio::test]
    async fn a_deferred_create_is_dormant_until_restore_starts_it() {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let (mut conn, server, _dir) = connect(org_spec(fixture)).await;
        let committed = create_resource(
            &mut conn,
            work,
            ResourceKind::Terminal,
            Some(LaunchSpec::new(vec!["sh".to_owned()])),
            LaunchMode::Deferred,
        )
        .await
        .unwrap();
        let key = committed.resource.expect("a record was created");
        assert!(committed.target.is_none(), "nothing launched");
        let view = fetch(&mut conn, OrgFilter::All, 1).await.unwrap();
        assert!(view.resource(&key).unwrap().activation.is_none());
        let restorable: Vec<ResourceKey> = view
            .restorable(Some(&work))
            .into_iter()
            .map(|record| record.key)
            .collect();
        assert_eq!(restorable, vec![key]);
        let restored = restore(&mut conn, Some(work)).await.unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].0, key);
        assert!(restored[0].1.is_ok());
        drop(conn);
        let seen = server.await.unwrap();
        assert!(
            seen.iter().any(|f| matches!(
                f,
                phux_protocol::wire::frame::FrameKind::Command {
                    command: Command::ResourceCreate {
                        launch_mode: LaunchMode::Deferred,
                        ..
                    },
                    ..
                }
            )),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn rm_relaunch_restore_move_bulk_undo_and_inbox() {
        let mut fixture = OrgFixture::new();
        let work = fixture.session("work", SessionLifetime::Persistent);
        let play = fixture.session("play", SessionLifetime::Persistent);
        let empty = fixture.session("empty", SessionLifetime::Persistent);
        let inbox = fixture.inbox_session("inbox");
        let running = fixture.resource(work, Some(ExecState::Running));
        let restarted = fixture.ended_by_restart(work);
        let done = fixture.resource(work, Some(ExecState::Completed));
        let spare = fixture.resource(inbox, None);
        let (mut conn, server, _dir) = connect(org_spec(fixture)).await;

        assert!(matches!(
            remove_resource(&mut conn, running).await,
            Err(OrgError::Refused(Refusal::InvalidArgument { .. }))
        ));
        remove_resource(&mut conn, done).await.unwrap();
        delete_session(&mut conn, empty).await.unwrap();
        assert!(matches!(
            delete_session(&mut conn, work).await,
            Err(OrgError::Refused(Refusal::NotEmpty { .. }))
        ));
        let restored = restore(&mut conn, Some(work)).await.unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].0, restarted);
        assert!(restored[0].1.is_ok());

        let moved = move_resource(&mut conn, running, play).await.unwrap();
        let back = undo(&mut conn, &moved).await.unwrap();
        assert_ne!(back.operation, moved.operation);
        let chunks = move_many(&mut conn, &[running, restarted], play)
            .await
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].result.is_ok());
        let view = fetch(&mut conn, OrgFilter::Session(play), 1).await.unwrap();
        assert_eq!(view.members(&play).count(), 2);

        inbox_resolve(&mut conn, inbox).await.unwrap();
        let _ = spare;
        drop(conn);
        server.await.unwrap();
    }
}
