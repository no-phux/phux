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
//! Compare-and-swap edits re-read the records they name and retry when the
//! server answers `CONFLICT`, at most [`MAX_CAS_ATTEMPTS`] times.
//!
//! These are the operations behind `phux rm`, `relaunch`, `restore`, `mv`,
//! bulk move, undo, and Inbox resolve; the CLI and MCP adapters own
//! selector parsing and rendering.

use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{
    AuthorityId, KEY_MIN_PREFIX, OperationId, ResourceKey, Revision, SessionId, SessionKey,
};
use phux_protocol::wire::frame::{
    ArrangementValue, Command, CommandValue, Committed, ErrorCode, ExecState, MAX_ORG_BATCH,
    MoveItem, OrgFilter, OrganizationPage, Outcome, Refusal, ResourceRecord, SessionLifetime,
    SessionRecord, TerminateMode,
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

/// Run `attempt` against fresh records until it does not conflict.
async fn with_cas<F>(conn: &mut Connection, mut attempt: F) -> Result<Committed, OrgError>
where
    F: AsyncFnMut(&mut Connection) -> Result<Committed, OrgError>,
{
    let mut last = None;
    for _ in 0..MAX_CAS_ATTEMPTS {
        match attempt(conn).await {
            Err(OrgError::Refused(Refusal::Conflict { current })) => {
                last = Some(OrgError::Refused(Refusal::Conflict { current }));
            }
            other => return other,
        }
    }
    Err(last.unwrap_or_else(|| {
        OrgError::Refused(Refusal::Conflict {
            current: Revision::default(),
        })
    }))
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
    with_cas(conn, async |conn| {
        let (_, record) = resource_view(conn, resource).await?;
        let command = Command::ResourceRemove {
            operation: mint_operation(),
            resource,
            expected: record.revision,
        };
        commit(conn, FIRST_REQUEST_ID, command).await
    })
    .await
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
    with_cas(conn, async |conn| {
        let view = session_view(conn, session).await?;
        let record = session_in(&view, &session)?;
        let command = Command::SessionDelete {
            operation: mint_operation(),
            session,
            expected: record.revision,
        };
        commit(conn, FIRST_REQUEST_ID, command).await
    })
    .await
}

/// `phux relaunch`: start a new activation of an inactive resource.
///
/// # Errors
///
/// `EXECUTION_UNCERTAIN` for a lost activation; a refusal while it runs;
/// transport failures.
pub async fn relaunch(conn: &mut Connection, resource: ResourceKey) -> Result<Committed, OrgError> {
    with_cas(conn, async |conn| {
        let (_, record) = resource_view(conn, resource).await?;
        let command = Command::ResourceRelaunch {
            operation: mint_operation(),
            resource,
            expected: record.revision,
        };
        match request(conn, FIRST_REQUEST_ID, command).await?.0 {
            OrgOutcome::Committed(committed) => Ok(*committed),
            OrgOutcome::Accepted => Ok(Committed::new(mint_operation(), record.revision)),
            other => Err(outcome_error(other)),
        }
    })
    .await
}

/// `phux restore [SESSION]`: relaunch every resource a server restart ended,
/// in their sessions, one result per resource in page order.
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
        .ended_by_restart(session.as_ref())
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
    with_cas(conn, async |conn| {
        let (from_view, record) = resource_view(conn, resource).await?;
        let from = session_in(&from_view, &record.session)?;
        let to_view = session_view(conn, to).await?;
        let to_record = session_in(&to_view, &to)?;
        let command = Command::ResourceMove {
            operation: mint_operation(),
            resource,
            from: from.key,
            from_expected: from.revision,
            to,
            to_expected: to_record.revision,
        };
        commit(conn, FIRST_REQUEST_ID, command).await
    })
    .await
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
        let chunk_keys = chunk.to_vec();
        let result = with_cas(conn, async |conn| {
            let all = fetch(conn, OrgFilter::All, FIRST_REQUEST_ID).await?;
            let to_record = session_in(&all, &to)?;
            let mut items = Vec::with_capacity(chunk_keys.len());
            for key in &chunk_keys {
                let record = resource_in(&all, key)?;
                let from = session_in(&all, &record.session)?;
                items.push(MoveItem {
                    resource: *key,
                    from: from.key,
                    from_expected: from.revision,
                });
            }
            let command = Command::ResourceMoveMany {
                operation: mint_operation(),
                items,
                to,
                to_expected: to_record.revision,
            };
            commit(conn, FIRST_REQUEST_ID, command).await
        })
        .await;
        results.push(BatchResult {
            resources: chunk_keys,
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
    with_cas(conn, async |conn| {
        let view = session_view(conn, session).await?;
        let record = session_in(&view, &session)?;
        let command = Command::InboxResolve {
            operation: mint_operation(),
            session,
            expected: record.revision,
        };
        commit(conn, FIRST_REQUEST_ID, command).await
    })
    .await
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
    with_cas(conn, async |conn| {
        let view = session_view(conn, session).await?;
        let record = session_in(&view, &session)?;
        let command = Command::SessionSetLifetime {
            operation: mint_operation(),
            session,
            expected: record.revision,
            lifetime,
        };
        commit(conn, FIRST_REQUEST_ID, command).await
    })
    .await
}

/// Rename (or, with `None`, unname) a resource record.
///
/// # Errors
///
/// The refusal; transport failures.
pub async fn rename_resource(
    conn: &mut Connection,
    resource: ResourceKey,
    name: Option<String>,
) -> Result<Committed, OrgError> {
    with_cas(conn, async |conn| {
        let (_, record) = resource_view(conn, resource).await?;
        let command = Command::ResourceRename {
            operation: mint_operation(),
            resource,
            expected: record.revision,
            name: name.clone(),
        };
        commit(conn, FIRST_REQUEST_ID, command).await
    })
    .await
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
        remove_resource(&mut conn, gone).await.unwrap();
        drop(conn);
        let seen = server.await.unwrap();
        let removes = seen
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    phux_protocol::wire::frame::FrameKind::Command {
                        command: Command::ResourceRemove { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(removes, 3, "two conflicts, then the commit");
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
