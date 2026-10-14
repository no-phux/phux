//! The organization surface of the C ABI: records and typed operations,
//! over the runtime's organization replica and operation journal
//! (`docs/spec/L1.md` §5.7, CONTRACTS §04-§06).
//!
//! The runtime owns the replica and the journal (one `OperationId` per
//! action, resent with the same id after a reconnect inside the retry
//! horizon). This module only correlates host request ids with the
//! operations they submitted, and keeps the borrowed views the C records
//! point into. The client seeds the replica once per connection that
//! advertises `ORGANIZATION` (its control plane runs with
//! `automatic_lifecycle` off, so nothing else would).
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher and Client"
)]

use std::collections::BTreeMap;
use std::mem;

use phux_client_core::org_ops::{OrgOutcome, SubmitError};
use phux_client_runtime::control::{ControlPlane, Event, ReplicaState};
use phux_protocol::ResourceId;
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{
    Committed, ExecState, ExitPolicy, Outcome, ResourceRecord, SessionLifetime, TerminateMode,
};

use crate::c::client::Client;
use crate::c::error::{BridgeError, check_struct};
use crate::c::types::{PhuxBytes, PhuxResourceId, bytes_out};
use crate::c::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use crate::projection::organization::{self as projected, Resolution, Status, TargetResult};

/// Most resources one `phux_client_org_move_many` names.
pub const MAX_MOVE_MANY: usize = 1024;

// ----- C vocabulary ---------------------------------------------------------

/// `PhuxSessionLifetime`: unknown (a server that reports none).
pub const LIFETIME_UNKNOWN: u32 = 0;
/// `PhuxSessionLifetime`: survives empty.
pub const LIFETIME_PERSISTENT: u32 = 1;
/// `PhuxSessionLifetime`: deleted with its last member.
pub const LIFETIME_EPHEMERAL: u32 = 2;

/// `PHUX_SESSION_RECORD_FOCUSED`.
pub const PHUX_SESSION_RECORD_FOCUSED: u32 = 0x1;
/// `PHUX_SESSION_RECORD_AWAITING_ORGANIZATION`.
pub const PHUX_SESSION_RECORD_AWAITING_ORGANIZATION: u32 = 0x2;
/// The designated Intake session, with the existing ABI flag value.
pub const PHUX_SESSION_RECORD_INTAKE: u32 = 0x4;
/// Legacy ABI spelling of [`PHUX_SESSION_RECORD_INTAKE`].
pub const PHUX_SESSION_RECORD_INBOX: u32 = PHUX_SESSION_RECORD_INTAKE;
/// `PHUX_SESSION_RECORD_ORGANIZED`: the replica holds the record, so
/// `revision`, `arrangement_key`, and the state counts are its values.
pub const PHUX_SESSION_RECORD_ORGANIZED: u32 = 0x8;
/// `PHUX_SESSION_RECORD_EMPTY`.
pub const PHUX_SESSION_RECORD_EMPTY: u32 = 0x10;

/// `PhuxOrgKind` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Move = 1,
    MoveMany = 2,
    Terminate = 3,
    Remove = 4,
    Relaunch = 5,
    DeleteSession = 6,
    SetLifetime = 7,
    IntakeResolve = 8,
    Undo = 9,
    KillSession = 10,
}

impl Kind {
    /// Whether the server keeps an inverse for this edit (CONTRACTS §06).
    const fn undoable(self) -> bool {
        matches!(self, Self::Move | Self::MoveMany | Self::IntakeResolve)
    }
}

/// The C value of a lifetime.
#[must_use]
pub const fn lifetime_code(lifetime: Option<SessionLifetime>) -> u32 {
    match lifetime {
        None => LIFETIME_UNKNOWN,
        Some(SessionLifetime::Persistent) => LIFETIME_PERSISTENT,
        Some(SessionLifetime::Ephemeral) => LIFETIME_EPHEMERAL,
    }
}

/// A lifetime from its C value.
pub(crate) fn lifetime_in(code: u32) -> Result<SessionLifetime, BridgeError> {
    match code {
        LIFETIME_PERSISTENT => Ok(SessionLifetime::Persistent),
        LIFETIME_EPHEMERAL => Ok(SessionLifetime::Ephemeral),
        _ => Err(BridgeError::invalid(
            "lifetime must be PHUX_SESSION_LIFETIME_PERSISTENT or _EPHEMERAL",
        )),
    }
}

/// `PhuxExecState`: `0` when there is no activation.
#[must_use]
pub const fn exec_code(state: Option<ExecState>) -> u32 {
    match state {
        None => 0,
        Some(ExecState::Starting) => 1,
        Some(ExecState::Running) => 2,
        Some(ExecState::Stopping) => 3,
        Some(ExecState::Completed) => 4,
        Some(ExecState::Failed) => 5,
        Some(ExecState::Lost) => 6,
        Some(ExecState::Abandoned) => 7,
    }
}

/// `PhuxOutcomeKind` and its code: none, exited, signaled, unobserved,
/// other.
#[must_use]
pub const fn outcome_code(outcome: Option<Outcome>) -> (u32, i32) {
    match outcome {
        None => (0, 0),
        Some(Outcome::Exited(code)) => (1, code),
        Some(Outcome::Signaled(signal)) => (2, signal),
        Some(Outcome::Unobserved) => (3, 0),
        #[allow(
            unreachable_patterns,
            reason = "Outcome is non_exhaustive across crates"
        )]
        Some(_) => (4, 0),
    }
}

// ----- records ----------------------------------------------------------------

/// What the organization replica adds to one session of the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionOrg {
    /// The replica holds this session's record.
    pub known: bool,
    pub revision: u64,
    pub members: u32,
    pub arrangement: Option<ArrangementKey>,
    pub awaiting_organization: bool,
    pub inbox: bool,
    /// Members whose activation is starting, running, or stopping.
    pub running: u32,
    /// Members whose activation completed, failed, or was abandoned.
    pub completed: u32,
    /// Members whose activation is lost.
    pub lost: u32,
}

/// One resource record and the storage its C view borrows.
#[derive(Debug)]
pub(crate) struct RecordView {
    pub record: ResourceRecord,
    pub session_id: u32,
    key_text: Vec<u8>,
    name: Vec<u8>,
    live_host: Vec<u8>,
}

impl RecordView {
    fn new(record: ResourceRecord, session_id: u32) -> Self {
        let live_host = match &record.live {
            Some(ResourceId::Satellite { host, .. }) => host.as_str().as_bytes().to_vec(),
            _ => Vec::new(),
        };
        Self {
            key_text: record.key.to_string().into_bytes(),
            name: record.name.clone().unwrap_or_default().into_bytes(),
            record,
            session_id,
            live_host,
        }
    }

    fn live_view(&self) -> PhuxResourceId {
        match &self.record.live {
            None => PhuxResourceId::default(),
            Some(ResourceId::Local { id }) => PhuxResourceId {
                kind: 0,
                id: *id,
                host: PhuxBytes::default(),
            },
            Some(ResourceId::Satellite { id, .. }) => PhuxResourceId {
                kind: 1,
                id: *id,
                host: bytes_out(&self.live_host),
            },
        }
    }
}

// ----- requests -----------------------------------------------------------------

/// One submitted operation of a host request.
#[derive(Debug)]
struct Part {
    operation: OperationId,
    /// The records this part addresses (a chunk of a bulk move).
    keys: Vec<[u8; 16]>,
    is_session: bool,
    resolution: Resolution,
    receipt: Option<Committed>,
}

/// One host request and its retained result.
#[derive(Debug)]
struct Request {
    kind: Kind,
    parts: Vec<Part>,
    view: ResultView,
}

#[derive(Debug, Default)]
struct ResultView {
    status: u32,
    refusal: u32,
    undoable: bool,
    revision: u64,
    conflict_revision: u64,
    message: Vec<u8>,
    targets: Vec<TargetView>,
}

#[derive(Debug)]
struct TargetView {
    key: [u8; 16],
    is_session: bool,
    status: u32,
    refusal: u32,
    message: Vec<u8>,
}

impl TargetView {
    fn from_result(result: &TargetResult) -> Self {
        Self {
            key: result.key,
            is_session: result.is_session,
            status: result.status.code(),
            refusal: result.refusal,
            message: result.message.as_bytes().to_vec(),
        }
    }
}

impl Request {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            parts: Vec::new(),
            view: ResultView::default(),
        }
    }

    fn pending(&self) -> bool {
        self.parts
            .iter()
            .any(|part| part.resolution.status == Status::Pending)
    }

    /// Recompute the borrowed result view from the parts.
    fn refresh_view(&mut self) {
        let statuses: Vec<Status> = self
            .parts
            .iter()
            .map(|part| part.resolution.status)
            .collect();
        let status = aggregate(&statuses);
        let first_failure = self.parts.iter().find(|part| {
            !part.resolution.status.succeeded() && part.resolution.status != Status::Pending
        });
        let targets = self
            .parts
            .iter()
            .flat_map(|part| {
                part.keys.iter().map(move |key| {
                    let mut target = TargetResult::resource(*key, &part.resolution);
                    target.is_session = part.is_session;
                    TargetView::from_result(&target)
                })
            })
            .collect();
        self.view = ResultView {
            status: status.code(),
            refusal: first_failure.map_or(0, |part| part.resolution.refusal),
            undoable: self.kind.undoable()
                && !self.parts.is_empty()
                && self.parts.iter().all(|part| part.receipt.is_some()),
            revision: match self.parts.as_slice() {
                [only] => only.resolution.revision.unwrap_or(0),
                _ => 0,
            },
            conflict_revision: self
                .parts
                .iter()
                .find_map(|part| part.resolution.conflict_revision)
                .unwrap_or(0),
            message: first_failure
                .map(|part| part.resolution.message.as_bytes().to_vec())
                .unwrap_or_default(),
            targets,
        };
    }
}

/// The status of a request made of `parts`.
fn aggregate(parts: &[Status]) -> Status {
    if parts.is_empty() {
        return Status::Pending;
    }
    if parts.contains(&Status::Pending) {
        return Status::Pending;
    }
    let succeeded = parts.iter().filter(|status| status.succeeded()).count();
    if succeeded == parts.len() {
        return if parts.iter().all(|status| *status == Status::Accepted) {
            Status::Accepted
        } else {
            Status::Committed
        };
    }
    if succeeded > 0 {
        return Status::Partial;
    }
    parts[0]
}

// ----- client state ---------------------------------------------------------------

/// The C lane's organization state.
#[derive(Debug, Default)]
pub(crate) struct OrgState {
    /// The connection epoch whose replica this client seeded.
    seeded_epoch: Option<u64>,
    /// Bumped whenever the replica changed under this client.
    revision: u64,
    /// The last `OrganizationUnavailable` message.
    message: Vec<u8>,
    /// The session list was replaced; re-enrich it from the replica.
    pub sessions_dirty: bool,
    /// Every resource record the replica holds.
    pub records: Vec<RecordView>,
    requests: BTreeMap<u32, Request>,
}

impl OrgState {
    const fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Requests still in flight plus retained results.
    pub(crate) fn len(&self) -> usize {
        self.requests.len()
    }
}

/// Whether the connected server advertises `ORGANIZATION`.
pub(crate) fn supported(client: &Client) -> bool {
    client.protocol_ready && client.control().organization_supported()
}

/// Whether the replica is seeded and following the journal.
pub(crate) fn live(client: &Client) -> bool {
    supported(client) && client.control().organization().is_live()
}

/// Seed the replica once per connection that advertises `ORGANIZATION`.
pub(crate) fn seed(client: &mut Client) {
    if !supported(client) {
        return;
    }
    let epoch = client.control().connection_epoch();
    if client.org.seeded_epoch == Some(epoch) {
        return;
    }
    if client.control().refresh_organization().is_some() {
        client.org.seeded_epoch = Some(epoch);
    }
}

/// The connection ended: a new one seeds again.
pub(crate) const fn disconnect(client: &mut Client) {
    client.org.seeded_epoch = None;
}

/// Route one runtime event. Returns the event when it is not an
/// organization event.
pub(crate) fn on_event(client: &mut Client, event: Event) -> Option<Event> {
    match event {
        Event::OrganizationChanged => {
            changed(client);
            crate::c::arrangement::organization_changed(client);
            None
        }
        Event::OrganizationUnavailable { message } => {
            client.org.message = message.into_bytes();
            client.org.bump();
            None
        }
        Event::OrganizationOperation { operation, outcome } => {
            resolved(client, operation, &outcome);
            None
        }
        other => Some(other),
    }
}

/// Re-read records, session facts, and names from the replica.
#[allow(
    clippy::significant_drop_tightening,
    reason = "each control guard lives exactly as long as the replica borrow it lends"
)]
pub(crate) fn changed(client: &mut Client) {
    let (records, sessions, renames) = {
        let control = client.control();
        let replica = control.organization();
        if replica.authority().is_none() {
            return;
        }
        let intake = replica.intake();
        let live_of = |key: &SessionKey| -> u32 {
            replica
                .session(key)
                .and_then(|session| session.live)
                .map_or(0, phux_protocol::SessionId::get)
        };
        let records: Vec<RecordView> = replica
            .resources()
            .iter()
            .map(|record| RecordView::new(record.clone(), live_of(&record.session)))
            .collect();
        let mut sessions = Vec::with_capacity(client.sessions.len());
        let mut renames = Vec::new();
        for summary in &client.sessions {
            let record = summary
                .key
                .and_then(|key| replica.session(&key))
                .or_else(|| {
                    replica.session_by_live(phux_protocol::SessionId::new(summary.session_id))
                });
            let Some(record) = record else {
                sessions.push(None);
                continue;
            };
            let mut org = SessionOrg {
                known: true,
                revision: record.revision.get(),
                members: record.members,
                arrangement: record.arrangement,
                awaiting_organization: record.awaiting_organization,
                inbox: Some(record.key) == intake,
                ..SessionOrg::default()
            };
            for member in replica.members(&record.key) {
                match member.activation.map(|activation| activation.state) {
                    Some(ExecState::Starting | ExecState::Running | ExecState::Stopping) => {
                        org.running += 1;
                    }
                    Some(ExecState::Completed | ExecState::Failed | ExecState::Abandoned) => {
                        org.completed += 1;
                    }
                    Some(ExecState::Lost) => org.lost += 1,
                    None => {}
                }
            }
            if record.name.as_bytes() != summary.name.as_slice() {
                renames.push((summary.session_id, record.name.clone().into_bytes()));
            }
            sessions.push(Some((record.key, record.lifetime, org)));
        }
        (records, sessions, renames)
    };
    client.org.records = records;
    for (summary, update) in client.sessions.iter_mut().zip(sessions) {
        let Some((key, lifetime, org)) = update else {
            summary.org = SessionOrg::default();
            continue;
        };
        if summary.key != Some(key) {
            summary.key = Some(key);
            summary.key_text = key.to_string().into_bytes();
        }
        summary.lifetime = Some(lifetime);
        summary.keep_empty = lifetime == SessionLifetime::Persistent;
        summary.org = org;
    }
    for (session_id, name) in renames {
        if crate::c::session_rename::valid_name(&name).is_ok()
            && crate::c::arrangement::rename_sessions(client, |s| s.session_id == session_id, &name)
        {
            crate::c::session_rename::bump_sessions_revision(client);
        }
    }
    // The live resources' keys follow the replica's handle map.
    let keys: Vec<Option<ResourceKey>> = {
        let control = client.control();
        let handles = control.organization().handles();
        client
            .resources
            .iter()
            .map(|resource| handles.key(&resource.id))
            .collect()
    };
    for (resource, key) in client.resources.iter_mut().zip(keys) {
        resource.key = key.map_or([0; 16], |key| *key.as_bytes());
    }
    client.org.bump();
}

fn resolved(client: &mut Client, operation: OperationId, outcome: &OrgOutcome) {
    if crate::c::session_create::organization_resolved(client, operation, outcome) {
        return;
    }
    if crate::c::session_rename::organization_resolved(client, operation, outcome) {
        return;
    }
    for request in client.org.requests.values_mut() {
        let Some(part) = request
            .parts
            .iter_mut()
            .find(|part| part.operation == operation)
        else {
            continue;
        };
        part.resolution = projected::resolution(outcome);
        part.receipt = match outcome {
            OrgOutcome::Committed(committed) => Some((**committed).clone()),
            _ => None,
        };
        request.refresh_view();
        return;
    }
}

// ----- submission -------------------------------------------------------------------

fn ensure_live(client: &Client) -> Result<(), BridgeError> {
    if !client.protocol_ready || client.detached {
        return Err(BridgeError::state(
            "organization operations need a negotiated connection",
        ));
    }
    if !client.control().organization_supported() {
        return Err(BridgeError::state(
            "the server does not advertise ORGANIZATION",
        ));
    }
    if !client.control().organization().is_live() {
        return Err(BridgeError::state(
            "the organization records are not loaded yet; wait for PhuxOrganizationInfo.state LIVE",
        ));
    }
    Ok(())
}

fn ensure_capacity(client: &Client, request_id: u32) -> Result<(), BridgeError> {
    client.operations.check_request_id(request_id)?;
    if client.org.len() >= crate::c::operations::MAX_OPERATIONS {
        return Err(BridgeError::state(
            "organization results are full; release completed results",
        ));
    }
    Ok(())
}

fn submit_error(error: &SubmitError) -> BridgeError {
    match error {
        SubmitError::NotAnOperation => {
            BridgeError::state("the organization replica does not hold that record")
        }
        other => BridgeError::state(other.to_string()),
    }
}

/// Validate, submit through `submit`, and retain the request.
fn submit(
    client: &mut Client,
    request_id: u32,
    kind: Kind,
    keys: Vec<Vec<[u8; 16]>>,
    is_session: bool,
    submit: impl FnOnce(&mut ControlPlane) -> Result<Vec<OperationId>, SubmitError>,
) -> Result<(), BridgeError> {
    ensure_live(client)?;
    ensure_capacity(client, request_id)?;
    let operations = {
        let mut control = client.control();
        submit(&mut control).map_err(|error| submit_error(&error))?
    };
    client.operations.consume_request_id(request_id);
    let mut request = Request::new(kind);
    request.parts = operations
        .into_iter()
        .zip(keys)
        .map(|(operation, keys)| Part {
            operation,
            keys,
            is_session,
            resolution: Resolution::pending(),
            receipt: None,
        })
        .collect();
    request.refresh_view();
    client.org.requests.insert(request_id, request);
    client.process_runtime_events()?;
    Ok(())
}

unsafe fn key_in(key: *const u8) -> Result<[u8; 16], BridgeError> {
    if key.is_null() {
        return Err(BridgeError::invalid("key is null"));
    }
    // SAFETY: the caller promises 16 readable bytes at a non-null `key`.
    let bytes = unsafe { std::slice::from_raw_parts(key, 16) };
    let mut out = [0; 16];
    out.copy_from_slice(bytes);
    Ok(out)
}

unsafe fn session_key_in(key: *const u8) -> Result<SessionKey, BridgeError> {
    // SAFETY: forwards the caller's 16-byte contract.
    let bytes = unsafe { key_in(key) }?;
    SessionKey::new(bytes).ok_or_else(|| BridgeError::invalid("session key is all zero"))
}

unsafe fn resource_key_in(key: *const u8) -> Result<ResourceKey, BridgeError> {
    // SAFETY: forwards the caller's 16-byte contract.
    let bytes = unsafe { key_in(key) }?;
    ResourceKey::new(bytes).ok_or_else(|| BridgeError::invalid("resource key is all zero"))
}

// ----- exports: state and records ------------------------------------------------

/// Whether `HELLO_OK` advertised `ORGANIZATION`.
///
/// # Safety
/// Client is live and unmodified for the call; `out_supported` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_organization_supported(
    client: *const PhuxClient,
    out_supported: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies a writable output when non-null.
        let out = unsafe { out_supported.as_mut() }
            .ok_or_else(|| BridgeError::invalid("supported output is null"))?;
        *out = supported(client);
        Ok(())
    })
}

/// The replica's state, authority, Intake, and change counter.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOrganizationInfo {
    pub size: usize,
    pub version: u32,
    pub supported: bool,
    pub state: u32,
    pub authority: [u8; 16],
    pub inbox: [u8; 16],
    pub revision: u64,
    pub record_count: u32,
    pub message: PhuxBytes,
}

/// Read the organization replica's state.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. `message` borrows the client until the next mutable call.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the control guard lives exactly as long as the replica borrow it lends"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_organization_info(
    client: *const PhuxClient,
    out: *mut PhuxOrganizationInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxOrganizationInfo>(),
            out.version,
        )?;
        let supported = supported(client);
        let (state, authority, inbox) = {
            let control = client.control();
            let replica = control.organization();
            let state = if supported {
                match replica.state() {
                    ReplicaState::Empty => 0,
                    ReplicaState::Seeding => 1,
                    ReplicaState::Live => 2,
                    ReplicaState::Stale => 3,
                }
            } else {
                0
            };
            (
                state,
                replica.authority().map_or([0; 16], |key| *key.as_bytes()),
                replica.intake().map_or([0; 16], |key| *key.as_bytes()),
            )
        };
        *out = PhuxOrganizationInfo {
            size: out.size,
            version: out.version,
            supported,
            state,
            authority,
            inbox,
            revision: client.org.revision,
            record_count: u32::try_from(client.org.records.len()).unwrap_or(u32::MAX),
            message: bytes_out(&client.org.message),
        };
        Ok(())
    })
}

/// Reseed the replica from the first page.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_organization_refresh(
    client: *mut PhuxClient,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !supported(client) || client.detached {
            return Err(BridgeError::state(
                "the server does not advertise ORGANIZATION",
            ));
        }
        if client.control().refresh_organization().is_none() {
            return Err(BridgeError::state("the connection is not negotiated"));
        }
        let epoch = client.control().connection_epoch();
        client.org.seeded_epoch = Some(epoch);
        client.process_runtime_events()?;
        Ok(())
    })
}

/// One session's full record, beside the frozen `PhuxSessionInfo` at the
/// same index.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSessionRecord {
    pub size: usize,
    pub version: u32,
    pub session_id: u32,
    pub key: [u8; 16],
    pub arrangement_key: [u8; 16],
    pub revision: u64,
    pub created_at_unix_secs: i64,
    pub lifetime: u32,
    pub flags: u32,
    pub member_count: u32,
    pub running_count: u32,
    pub completed_count: u32,
    pub lost_count: u32,
    pub attached_client_count: u16,
    pub name: PhuxBytes,
    pub key_text: PhuxBytes,
}

/// Read the full record of the session `phux_client_session_get` reads at
/// the same index.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. Spans borrow the client until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_record_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxSessionRecord,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxSessionRecord>(), out.version)?;
        let session = client
            .sessions
            .get(index)
            .ok_or_else(|| BridgeError::invalid("session index out of range"))?;
        let org = &session.org;
        let members = if org.known {
            org.members
        } else {
            session.snapshot_members
        };
        let mut flags = 0;
        if session.focused {
            flags |= PHUX_SESSION_RECORD_FOCUSED;
        }
        if org.awaiting_organization {
            flags |= PHUX_SESSION_RECORD_AWAITING_ORGANIZATION;
        }
        if org.inbox {
            flags |= PHUX_SESSION_RECORD_INTAKE;
        }
        if org.known {
            flags |= PHUX_SESSION_RECORD_ORGANIZED;
        }
        if members == 0 {
            flags |= PHUX_SESSION_RECORD_EMPTY;
        }
        *out = PhuxSessionRecord {
            size: out.size,
            version: out.version,
            session_id: session.session_id,
            key: session.key.map_or([0; 16], |key| *key.as_bytes()),
            arrangement_key: org.arrangement.map_or([0; 16], |key| *key.as_bytes()),
            revision: org.revision,
            created_at_unix_secs: session.created_at_unix_secs,
            lifetime: lifetime_code(session.lifetime),
            flags,
            member_count: members,
            running_count: org.running,
            completed_count: org.completed,
            lost_count: org.lost,
            attached_client_count: session.attached_client_count,
            name: bytes_out(&session.name),
            key_text: bytes_out(&session.key_text),
        };
        Ok(())
    })
}

/// How many resource records the replica holds.
///
/// # Safety
/// `client`, when non-null, is live and unmodified for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_record_count(client: *const PhuxClient) -> usize {
    // SAFETY: forwards the caller's pointer contract.
    unsafe { client.as_ref() }.map_or(0, |client| client.inner.org.records.len())
}

/// One resource record, live or not.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxResourceRecord {
    pub size: usize,
    pub version: u32,
    pub kind: u32,
    pub key: [u8; 16],
    pub session_key: [u8; 16],
    pub revision: u64,
    pub activation_id: u64,
    pub exec_state: u32,
    pub outcome_kind: u32,
    pub outcome_code: i32,
    pub on_exit: u32,
    pub live: bool,
    pub live_id: PhuxResourceId,
    pub session_id: u32,
    pub name: PhuxBytes,
    pub key_text: PhuxBytes,
}

/// Read one resource record.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. Spans borrow the client until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_record_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxResourceRecord,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxResourceRecord>(), out.version)?;
        let view = client
            .org
            .records
            .get(index)
            .ok_or_else(|| BridgeError::invalid("record index out of range"))?;
        let record = &view.record;
        let activation = record.activation;
        let (outcome_kind, outcome_code) =
            outcome_code(activation.and_then(|activation| activation.outcome));
        *out = PhuxResourceRecord {
            size: out.size,
            version: out.version,
            kind: u32::from(record.kind.as_wire()),
            key: *record.key.as_bytes(),
            session_key: *record.session.as_bytes(),
            revision: record.revision.get(),
            activation_id: activation.map_or(0, |activation| activation.id.get()),
            exec_state: exec_code(activation.map(|activation| activation.state)),
            outcome_kind,
            outcome_code,
            on_exit: match record.on_exit {
                ExitPolicy::Remove => 0,
                ExitPolicy::Retain => 1,
            },
            live: record.live.is_some(),
            live_id: view.live_view(),
            session_id: view.session_id,
            name: bytes_out(&view.name),
            key_text: bytes_out(&view.key_text),
        };
        Ok(())
    })
}

// ----- exports: operations --------------------------------------------------------

/// Move one resource into another session (membership only; it keeps
/// running).
///
/// # Safety
/// Client is live and exclusively accessed; both keys are 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_move(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
    to_session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes for each key.
        let (resource, to) = unsafe { (resource_key_in(resource)?, session_key_in(to_session)?) };
        submit(
            client,
            request_id,
            Kind::Move,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| control.move_resource(resource, to).map(|op| vec![op]),
        )
    })
}

/// Move `count` resources into one session, one atomic batch per chunk of
/// the server's bound, each with its own per-target result.
///
/// # Safety
/// Client is live and exclusively accessed; `resources` is `count * 16`
/// readable bytes; `to_session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_move_many(
    client: *mut PhuxClient,
    request_id: u32,
    resources: *const u8,
    count: usize,
    to_session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if resources.is_null() || count == 0 || count > MAX_MOVE_MANY {
            return Err(BridgeError::invalid(
                "a bulk move names 1..1024 readable resource keys",
            ));
        }
        // SAFETY: caller supplies 16 readable bytes for the destination.
        let to = unsafe { session_key_in(to_session) }?;
        let mut keys = Vec::with_capacity(count);
        for index in 0..count {
            // SAFETY: caller supplies `count * 16` readable bytes; `index * 16`
            // stays inside them.
            let key = unsafe { resource_key_in(resources.add(index * 16)) }?;
            if keys.contains(&key) {
                return Err(BridgeError::invalid("a bulk move repeats a resource key"));
            }
            keys.push(key);
        }
        let chunks: Vec<Vec<[u8; 16]>> = keys
            .chunks(phux_protocol::wire::frame::MAX_ORG_BATCH)
            .map(|chunk| chunk.iter().map(|key| *key.as_bytes()).collect())
            .collect();
        submit(
            client,
            request_id,
            Kind::MoveMany,
            chunks,
            false,
            |control| control.move_resources(&keys, to),
        )
    })
}

/// Terminate a resource's current activation here; the result is accepted,
/// and the end arrives as a record change.
///
/// # Safety
/// Client is live and exclusively accessed; `resource` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_terminate(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
    force: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let resource = unsafe { resource_key_in(resource) }?;
        let mode = if force {
            TerminateMode::Force
        } else {
            TerminateMode::Graceful
        };
        submit(
            client,
            request_id,
            Kind::Terminate,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| {
                control
                    .terminate_resource(resource, mode)
                    .map(|op| vec![op])
            },
        )
    })
}

/// Remove an inactive resource record.
///
/// # Safety
/// Client is live and exclusively accessed; `resource` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_remove(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let resource = unsafe { resource_key_in(resource) }?;
        submit(
            client,
            request_id,
            Kind::Remove,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| control.remove_resource(resource).map(|op| vec![op]),
        )
    })
}

/// Start a new activation of an inactive resource from its launch.
///
/// # Safety
/// Client is live and exclusively accessed; `resource` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_relaunch(
    client: *mut PhuxClient,
    request_id: u32,
    resource: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let resource = unsafe { resource_key_in(resource) }?;
        submit(
            client,
            request_id,
            Kind::Relaunch,
            vec![vec![*resource.as_bytes()]],
            false,
            |control| control.relaunch_resource(resource).map(|op| vec![op]),
        )
    })
}

/// Delete an empty session; refused `NOT_EMPTY` while it has members.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_delete_session(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        submit(
            client,
            request_id,
            Kind::DeleteSession,
            vec![vec![*session.as_bytes()]],
            true,
            |control| control.delete_session(session).map(|op| vec![op]),
        )
    })
}

/// Make a session persistent or ephemeral; ephemeral on an empty session
/// deletes it.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_set_lifetime(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
    lifetime: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        let lifetime = lifetime_in(lifetime)?;
        submit(
            client,
            request_id,
            Kind::SetLifetime,
            vec![vec![*session.as_bytes()]],
            true,
            |control| {
                control
                    .set_session_lifetime(session, lifetime)
                    .map(|op| vec![op])
            },
        )
    })
}

/// Legacy ABI name for clearing an imported session's Intake mark; members stay.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_inbox_resolve(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    // SAFETY: the legacy entry point has the same caller requirements.
    unsafe { phux_client_org_intake_resolve(client, request_id, session) }
}

/// Clear an imported session's Intake mark; its members and execution stay.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_intake_resolve(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        submit(
            client,
            request_id,
            Kind::IntakeResolve,
            vec![vec![*session.as_bytes()]],
            true,
            |control| control.intake_resolve(session).map(|op| vec![op]),
        )
    })
}

/// Undo a committed, undoable earlier request by its receipts. Never
/// reverses execution.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_undo(
    client: *mut PhuxClient,
    request_id: u32,
    original_request_id: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let original = client
            .org
            .requests
            .get(&original_request_id)
            .ok_or_else(|| BridgeError::invalid("unknown organization request"))?;
        if !original.view.undoable {
            return Err(BridgeError::state(
                "that request is not a committed, undoable edit",
            ));
        }
        let (receipts, keys, is_session): (Vec<Committed>, Vec<Vec<[u8; 16]>>, bool) = (
            original
                .parts
                .iter()
                .filter_map(|part| part.receipt.clone())
                .collect(),
            original
                .parts
                .iter()
                .map(|part| part.keys.clone())
                .collect(),
            original.parts.first().is_some_and(|part| part.is_session),
        );
        submit(
            client,
            request_id,
            Kind::Undo,
            keys,
            is_session,
            |control| {
                receipts
                    .iter()
                    .map(|receipt| control.undo(receipt))
                    .collect()
            },
        )
    })
}

/// Kill a whole session with one `SESSION_KILL`: the server ends every
/// member and deletes the session. Refused `INVALID_STATE` unless the server
/// advertises `SESSION_KILL`.
///
/// # Safety
/// Client is live and exclusively accessed; `session` is 16 readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_kill_session(
    client: *mut PhuxClient,
    request_id: u32,
    session: *const u8,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies 16 readable bytes.
        let session = unsafe { session_key_in(session) }?;
        submit(
            client,
            request_id,
            Kind::KillSession,
            vec![vec![*session.as_bytes()]],
            true,
            |control| control.kill_session(session).map(|op| vec![op]),
        )
    })
}

/// Resolve operations past their retry horizon.
///
/// The connected lane's driver does this itself; an embedded host calls it
/// on a timer while an organization operation is pending. `out_next_ms` receives the
/// milliseconds until the next deadline, or `UINT64_MAX` when none is due.
///
/// # Safety
/// Client is live and exclusively accessed; `out_next_ms` is writable when
/// non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_expire_operations(
    client: *mut PhuxClient,
    out_next_ms: *mut u64,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let _ = client.control().expire_operations();
        client.process_runtime_events()?;
        let next = client.control().next_operation_deadline();
        // SAFETY: caller supplies a writable output when non-null.
        if let Some(out) = unsafe { out_next_ms.as_mut() } {
            *out = next.map_or(u64::MAX, |deadline| {
                u64::try_from(
                    deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });
        }
        Ok(())
    })
}

macro_rules! sized_defaults {
    ($($ty:ty),*) => {$(
        impl Default for $ty {
            /// A zeroed record whose `size` and `version` are initialized, as
            /// every getter requires.
            fn default() -> Self {
                // SAFETY: every field is an integer, a bool, a byte array, or
                // a nullable span or id whose all-zero value is valid.
                let mut value: Self = unsafe { std::mem::zeroed() };
                value.size = mem::size_of::<Self>();
                value.version = crate::c::ABI_VERSION;
                value
            }
        }
    )*};
}

// ----- exports: results --------------------------------------------------------

/// One request's result.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOrgResult {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub kind: u32,
    pub status: u32,
    pub refusal: u32,
    pub undoable: bool,
    pub revision: u64,
    pub conflict_revision: u64,
    pub target_count: u32,
    pub message: PhuxBytes,
}

/// One target of a request's result.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOrgTarget {
    pub size: usize,
    pub version: u32,
    pub key: [u8; 16],
    pub is_session: bool,
    pub status: u32,
    pub refusal: u32,
    pub message: PhuxBytes,
}

/// How many organization results are retained, pending ones included.
///
/// # Safety
/// `client`, when non-null, is live and unmodified for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_result_count(client: *const PhuxClient) -> usize {
    // SAFETY: forwards the caller's pointer contract.
    unsafe { client.as_ref() }.map_or(0, |client| client.inner.org.requests.len())
}

/// Read the result at `index`, in request-id order.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. `message` borrows the client until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_result_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxOrgResult,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxOrgResult>(), out.version)?;
        let (request_id, request) = client
            .org
            .requests
            .iter()
            .nth(index)
            .ok_or_else(|| BridgeError::invalid("result index out of range"))?;
        let view = &request.view;
        *out = PhuxOrgResult {
            size: out.size,
            version: out.version,
            request_id: *request_id,
            kind: request.kind as u32,
            status: view.status,
            refusal: view.refusal,
            undoable: view.undoable,
            revision: view.revision,
            conflict_revision: view.conflict_revision,
            target_count: u32::try_from(view.targets.len()).unwrap_or(u32::MAX),
            message: bytes_out(&view.message),
        };
        Ok(())
    })
}

/// Read one target of the result at `result_index`.
///
/// # Safety
/// As `phux_client_org_result_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_target_get(
    client: *const PhuxClient,
    result_index: usize,
    target_index: usize,
    out: *mut PhuxOrgTarget,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxOrgTarget>(), out.version)?;
        let request = client
            .org
            .requests
            .values()
            .nth(result_index)
            .ok_or_else(|| BridgeError::invalid("result index out of range"))?;
        let target = request
            .view
            .targets
            .get(target_index)
            .ok_or_else(|| BridgeError::invalid("target index out of range"))?;
        *out = PhuxOrgTarget {
            size: out.size,
            version: out.version,
            key: target.key,
            is_session: target.is_session,
            status: target.status,
            refusal: target.refusal,
            message: bytes_out(&target.message),
        };
        Ok(())
    })
}

/// Release a settled result. A pending one is refused: its outcome is still
/// owed to the host.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_result_release(
    client: *mut PhuxClient,
    request_id: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let request = client
            .org
            .requests
            .get(&request_id)
            .ok_or_else(|| BridgeError::invalid("unknown organization request"))?;
        if request.pending() {
            return Err(BridgeError::state("the request is still pending"));
        }
        client.org.requests.remove(&request_id);
        Ok(())
    })
}

sized_defaults!(
    PhuxOrganizationInfo,
    PhuxSessionRecord,
    PhuxResourceRecord,
    PhuxOrgResult,
    PhuxOrgTarget
);

#[cfg(test)]
mod tests;
