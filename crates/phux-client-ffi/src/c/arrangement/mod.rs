//! The attached session's shared arrangement, over `GET_STATE` for the
//! catalog and one of two documents: the session's stored arrangement
//! (`ARRANGEMENT_GET` and compare-and-swap `ARRANGEMENT_REPLACE`) when the
//! server advertises `ORGANIZATION`, else the legacy
//! `phux.tui.layout/v1/<session-id>` metadata value (last-write-wins `SET`
//! and a confirming `GET`) of an older server.
//!
//! An `ORGANIZATION` server gives every session an arrangement (SPEC L2
//! section 2.2), so the legacy value is never read or written there: a read
//! that starts before the replica names the session's arrangement waits for
//! it, and fails if the replica becomes unavailable instead.
//!
//! An arrangement is presentation only: an empty one means the session's
//! members are not arranged, never that the session is empty, and no
//! arrangement is ever synthesized from the catalog.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher"
)]

mod exports;
mod model;
mod mutation;
pub(crate) mod resources;
mod types;
pub use exports::*;
pub use types::*;

use crate::c::{client::Client, error::BridgeError};
use model::{Catalog, Node};
use phux_client_core::layout::wire::{EntryBinding, from_protocol, to_protocol};
use phux_client_core::layout::{ArrangementDoc, LAYOUT_METADATA_GROUP, MAX_LAYOUT_METADATA_BYTES};
use phux_client_core::org_ops::OrgOutcome;
use phux_protocol::ids::{ArrangementKey, SessionKey};
use phux_protocol::wire::frame::{
    ArrangementValue, Command, CommandResult, CommandValue, FrameKind, Refusal, Scope, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;

pub(crate) const INTERNAL_START: u32 = 0x8000_0000;

/// `PhuxArrangementInfo.state`: nothing published yet, or the connection
/// ended.
pub const PHUX_ARRANGEMENT_STATE_UNAVAILABLE: u32 = 0;
/// The session has no stored arrangement: its members are not arranged.
pub const PHUX_ARRANGEMENT_STATE_UNARRANGED: u32 = 1;
/// A stored arrangement, confirmed by the server.
pub const PHUX_ARRANGEMENT_STATE_AUTHORITATIVE: u32 = 2;
/// The last good publication, after a read or edit failed.
pub const PHUX_ARRANGEMENT_STATE_LAST_GOOD_WITH_ERROR: u32 = 3;

/// `PhuxArrangementInfo.backend`: nothing published yet.
pub const PHUX_ARRANGEMENT_BACKEND_NONE: u32 = 0;
/// `PhuxArrangementInfo.backend`: the legacy layout metadata value.
pub const PHUX_ARRANGEMENT_BACKEND_LEGACY: u32 = 1;
/// The session's stored arrangement, edited by compare-and-swap.
pub const PHUX_ARRANGEMENT_BACKEND_ORGANIZATION: u32 = 2;

/// Which document a transaction reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Legacy,
    Organization(ArrangementKey),
    /// An `ORGANIZATION` server whose replica does not name the session's
    /// arrangement yet: nothing is queued until it does.
    Awaiting,
}

struct Pending {
    request: u32,
    state_id: Option<u32>,
    /// The document read: `GET_METADATA` or `ARRANGEMENT_GET`.
    metadata_id: u32,
    /// The write: `SET_METADATA` (no reply) or `ARRANGEMENT_REPLACE`.
    set_id: Option<u32>,
    catalog: Option<Catalog>,
    metadata: Option<DocReply>,
    /// `ARRANGEMENT_REPLACE`'s answer.
    replace: Option<OrgOutcome>,
    proposed: Option<ArrangementDoc>,
    backend: Backend,
}

impl Pending {
    const fn complete(&self) -> bool {
        self.catalog.is_some()
            && self.metadata.is_some()
            && (!matches!(self.backend, Backend::Organization(_))
                || self.set_id.is_none()
                || self.replace.is_some())
    }
}

enum DocReply {
    Absent,
    Legacy(Vec<u8>),
    Stored(Box<ArrangementValue>),
}

pub(crate) struct SharedArrangement {
    selected: u32,
    pub(super) revision: u64,
    pub(super) state: u32,
    authoritative: bool,
    pub(super) request: u32,
    pub(super) status: u32,
    pub(super) message: Vec<u8>,
    catalog: Catalog,
    pub(crate) topology: ArrangementDoc,
    nodes: Vec<Node>,
    roots: Vec<u32>,
    pending: Option<Pending>,
    pub(crate) subscriptions: resources::Subscriptions,
    next_internal: u32,
    /// The document the publication came from.
    backend: Backend,
    /// The stored arrangement's revision: what a replace expects.
    server_revision: u64,
    /// What writing the stored arrangement back needs.
    binding: EntryBinding,
    /// The arrangement the runtime replica was asked to follow.
    followed: Option<ArrangementKey>,
}

impl Default for SharedArrangement {
    fn default() -> Self {
        Self {
            selected: 0,
            revision: 0,
            state: PHUX_ARRANGEMENT_STATE_UNAVAILABLE,
            authoritative: false,
            request: 0,
            status: 0,
            message: Vec::new(),
            catalog: Catalog::default(),
            topology: ArrangementDoc::new(),
            nodes: Vec::new(),
            roots: Vec::new(),
            pending: None,
            subscriptions: resources::Subscriptions::default(),
            next_internal: INTERNAL_START,
            backend: Backend::Legacy,
            server_revision: 0,
            binding: EntryBinding::default(),
            followed: None,
        }
    }
}

impl SharedArrangement {
    /// The next bridge-internal request ID. Shared by every bridge-owned
    /// request (arrangement reads, a rename's confirmation read), so none of
    /// them can collide with each other or with the host's request space.
    pub(crate) fn reserve_internal(&mut self) -> Result<u32, BridgeError> {
        let id = self.next_internal;
        self.next_internal = id
            .checked_add(1)
            .ok_or_else(|| BridgeError::state("arrangement request space exhausted"))?;
        Ok(id)
    }

    pub(crate) fn disconnect(&mut self) {
        self.subscriptions.clear();
        self.followed = None;
        // Retain the last publication's identity and arrays for frozen display.
        // Revoking its authority must not require consumers to allocate a new
        // copy just to observe an interrupted transaction's unknown outcome.
        self.state = PHUX_ARRANGEMENT_STATE_UNAVAILABLE;
        self.authoritative = false;
        if self.pending.take().is_some() {
            self.status = 4;
            self.message = b"connection ended; arrangement transaction outcome unknown".to_vec();
        }
    }

    fn fail(&mut self, error: &BridgeError) {
        self.pending = None;
        self.state = PHUX_ARRANGEMENT_STATE_LAST_GOOD_WITH_ERROR;
        self.status = 3;
        self.message = bounded(&error.message);
    }

    fn begin(&mut self, pending: Pending) {
        self.request = pending.request;
        self.status = 1;
        self.message.clear();
        self.pending = Some(pending);
    }

    fn busy(&self) -> Result<(), BridgeError> {
        if self.pending.is_some() {
            return Err(BridgeError::state(
                "arrangement transaction already pending",
            ));
        }
        if self.selected == 0 {
            return Err(BridgeError::state("no attached arrangement session"));
        }
        Ok(())
    }

    /// The stored arrangement's key, when the publication came from one.
    const fn stored_key(&self) -> Option<ArrangementKey> {
        match self.backend {
            Backend::Legacy | Backend::Awaiting => None,
            Backend::Organization(key) => Some(key),
        }
    }

    /// `PhuxArrangementInfo.arrangement_key`: all zero for the legacy value.
    pub(super) fn stored_key_bytes(&self) -> [u8; 16] {
        self.stored_key().map_or([0; 16], |key| *key.as_bytes())
    }

    /// `PhuxArrangementInfo.backend`: `0` before any publication.
    pub(super) const fn backend_code(&self) -> u32 {
        if self.state == PHUX_ARRANGEMENT_STATE_UNAVAILABLE && self.revision == 0 {
            return PHUX_ARRANGEMENT_BACKEND_NONE;
        }
        match self.backend {
            Backend::Legacy => PHUX_ARRANGEMENT_BACKEND_LEGACY,
            Backend::Organization(_) | Backend::Awaiting => PHUX_ARRANGEMENT_BACKEND_ORGANIZATION,
        }
    }

    /// The stored arrangement's revision, `0` for the legacy value.
    pub(super) const fn server_revision(&self) -> u64 {
        self.server_revision
    }

    /// Terminals of the selected session that no group shows.
    pub(super) fn unarranged(&self) -> usize {
        self.catalog.unarranged(&self.topology, self.selected)
    }

    /// Stored entries whose resource has no live handle here.
    pub(super) fn unplaced(&self) -> usize {
        self.binding.unplaced_count()
    }
}

fn bounded(message: &str) -> Vec<u8> {
    let mut end = message.len().min(model::MAX_TEXT);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.as_bytes()[..end].to_vec()
}

pub(crate) fn attached(client: &mut Client, snapshot: SessionSnapshot) {
    let selected = snapshot.focused_session.get();
    client.arrangement.selected = selected;
    // ATTACHED carries registry data, not evidence that shared metadata is absent.
    // Leave topology unavailable until the initial document/state pair completes.
    let result = Catalog::from_snapshot(snapshot, selected)
        .and_then(|catalog| publish_catalog(client, catalog));
    if let Err(error) = result {
        client.arrangement.fail(&error);
    }
}

pub(crate) fn initial_read(client: &mut Client) {
    let result = start_read(client, 0, None);
    if let Err(error) = result {
        client.arrangement.fail(&error);
    }
}

/// The selected session's key, from the snapshot or the replica.
fn selected_key(client: &Client) -> Option<SessionKey> {
    let selected = client.arrangement.selected;
    client
        .sessions
        .iter()
        .find(|session| session.session_id == selected)
        .and_then(|session| session.key)
        .or_else(|| {
            client
                .control()
                .organization()
                .session_by_live(phux_protocol::SessionId::new(selected))
                .map(|record| record.key)
        })
}

/// The selected session's stored arrangement, when the replica is live and
/// names one.
fn stored_arrangement(client: &Client) -> Option<ArrangementKey> {
    if !crate::c::organization::live(client) {
        return None;
    }
    let key = selected_key(client)?;
    client.control().organization().session(&key)?.arrangement
}

/// The document a read goes to: the stored arrangement on an
/// `ORGANIZATION` server (awaited until the replica names it), else the
/// legacy value.
fn read_backend(client: &Client) -> Backend {
    if !crate::c::organization::supported(client) {
        return Backend::Legacy;
    }
    stored_arrangement(client).map_or(Backend::Awaiting, Backend::Organization)
}

/// Queue the reads of a transaction: the document first, then the registry
/// (pruning against an older registry could falsely retire newly created
/// leaves). An awaited document queues nothing yet.
fn queue_reads(
    client: &mut Client,
    backend: Backend,
    metadata_id: u32,
    state_id: u32,
) -> Result<(), BridgeError> {
    match backend {
        Backend::Awaiting => return Ok(()),
        Backend::Legacy => {
            client.queue_frame(&metadata_frame(metadata_id, client.arrangement.selected))?;
        }
        Backend::Organization(arrangement) => client.queue_frame(&FrameKind::Command {
            request_id: metadata_id,
            command: Command::ArrangementGet { arrangement },
        })?,
    }
    client.queue_frame(&state_frame(state_id))
}

/// A read waiting for the replica to name the session's arrangement: start
/// it once the replica does.
fn resume_awaited_read(client: &mut Client) -> Result<(), BridgeError> {
    let Some(pending) = client.arrangement.pending.as_ref() else {
        return Ok(());
    };
    if pending.backend != Backend::Awaiting {
        return Ok(());
    }
    let (metadata_id, state_id) = (pending.metadata_id, pending.state_id);
    let Some(key) = stored_arrangement(client) else {
        return Ok(());
    };
    let state_id = state_id.ok_or_else(|| BridgeError::state("missing arrangement registry"))?;
    let backend = Backend::Organization(key);
    queue_reads(client, backend, metadata_id, state_id)?;
    if let Some(pending) = client.arrangement.pending.as_mut() {
        pending.backend = backend;
    }
    Ok(())
}

/// The replica could not be read: a read still waiting for it to name the
/// session's arrangement fails rather than falling back to the legacy value.
pub(crate) fn organization_unavailable(client: &mut Client, message: &str) {
    let awaiting = client
        .arrangement
        .pending
        .as_ref()
        .is_some_and(|pending| pending.backend == Backend::Awaiting);
    if awaiting && !crate::c::organization::live(client) {
        client.arrangement.fail(&BridgeError::state(format!(
            "the session's arrangement is unavailable: {message}"
        )));
    }
}

/// The replica changed: follow the selected session's stored arrangement,
/// start a read that was waiting for it, and read it again when it moved
/// past the publication, appeared, or went away.
pub(crate) fn organization_changed(client: &mut Client) {
    if let Err(error) = resume_awaited_read(client) {
        client.arrangement.fail(&error);
    }
    let ws = &client.arrangement;
    if ws.selected == 0 || ws.state == PHUX_ARRANGEMENT_STATE_UNAVAILABLE {
        return;
    }
    let current = stored_arrangement(client);
    let held_revision = current.and_then(|key| {
        client
            .control()
            .organization()
            .arrangement(&key)
            .map(|value| value.revision.get())
    });
    if let Some(key) = current
        && held_revision.is_none()
        && client.arrangement.followed != Some(key)
        && client.control().fetch_arrangement(key).is_some()
    {
        client.arrangement.followed = Some(key);
    }
    let ws = &client.arrangement;
    if ws.pending.is_some() {
        return;
    }
    let stale = match (ws.stored_key(), current) {
        (None, None) => false,
        (Some(published), Some(current)) if published == current => {
            held_revision.is_some_and(|revision| revision > ws.server_revision)
        }
        _ => true,
    };
    if stale && let Err(error) = start_read(client, 0, None) {
        client.arrangement.fail(&error);
    }
}

const fn state_frame(id: u32) -> FrameKind {
    FrameKind::Command {
        request_id: id,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    }
}

fn metadata_frame(id: u32, selected: u32) -> FrameKind {
    FrameKind::GetMetadata {
        request_id: id,
        scope: Scope::Group(LAYOUT_METADATA_GROUP),
        key: model::key(selected),
    }
}

/// A proposed edit, already encoded for the backend the publication came
/// from.
pub(super) enum Proposal {
    Legacy(ArrangementDoc, Vec<u8>),
    Stored(
        ArrangementDoc,
        Box<phux_protocol::wire::frame::ArrangementDoc>,
    ),
}

/// Encode `next` for the publication's backend. An `ORGANIZATION` server's
/// arrangement is only ever written by compare-and-swap, so an edit before
/// it was read is refused.
pub(super) fn propose(client: &Client, next: ArrangementDoc) -> Result<Proposal, BridgeError> {
    match client.arrangement.backend {
        Backend::Legacy | Backend::Awaiting if crate::c::organization::supported(client) => Err(
            BridgeError::state("the session's arrangement is not loaded yet"),
        ),
        Backend::Legacy | Backend::Awaiting => {
            let bytes = next
                .encode_topology_cbor()
                .map_err(|e| BridgeError::invalid(e.to_string()))?;
            if bytes.len() > MAX_LAYOUT_METADATA_BYTES {
                return Err(BridgeError::state("layout metadata exceeds 256 KiB"));
            }
            Ok(Proposal::Legacy(next, bytes))
        }
        Backend::Organization(_) => {
            let wire = {
                let control = client.control();
                to_protocol(
                    &next,
                    control.organization().handles(),
                    &client.arrangement.binding,
                )
                .map_err(|error| BridgeError::state(error.to_string()))?
            };
            Ok(Proposal::Stored(next, Box::new(wire)))
        }
    }
}

pub(super) fn start_read(
    client: &mut Client,
    request: u32,
    proposed: Option<Proposal>,
) -> Result<(), BridgeError> {
    client.arrangement.busy()?;
    if client.outgoing.len() > 125 {
        return Err(BridgeError::state("outgoing queue full"));
    }
    // A write goes to the document the publication came from; a read goes to
    // the stored arrangement on an `ORGANIZATION` server.
    let backend = match &proposed {
        Some(Proposal::Legacy(..)) => Backend::Legacy,
        Some(Proposal::Stored(..)) => client
            .arrangement
            .stored_key()
            .map_or(Backend::Awaiting, Backend::Organization),
        None => read_backend(client),
    };
    let state_id = client.arrangement.reserve_internal()?;
    let metadata_id = client.arrangement.reserve_internal()?;
    let mut set_id = None;
    let proposed_doc = match proposed {
        None => None,
        Some(Proposal::Legacy(doc, bytes)) => {
            let id = client.arrangement.reserve_internal()?;
            client.queue_frame(&FrameKind::SetMetadata {
                request_id: id,
                scope: Scope::Group(LAYOUT_METADATA_GROUP),
                key: model::key(client.arrangement.selected),
                value: bytes,
            })?;
            set_id = Some(id);
            Some(doc)
        }
        Some(Proposal::Stored(doc, wire)) => {
            let Backend::Organization(arrangement) = backend else {
                return Err(BridgeError::state("the stored arrangement is not loaded"));
            };
            let id = client.arrangement.reserve_internal()?;
            client.queue_frame(&FrameKind::Command {
                request_id: id,
                command: Command::ArrangementReplace {
                    operation: phux_client_runtime::control::mint_operation(),
                    arrangement,
                    expected: phux_protocol::ids::Revision::new(client.arrangement.server_revision),
                    document: *wire,
                },
            })?;
            set_id = Some(id);
            Some(doc)
        }
    };
    // A write is queued first, so the read sees it.
    queue_reads(client, backend, metadata_id, state_id)?;
    client.arrangement.begin(Pending {
        request,
        state_id: Some(state_id),
        metadata_id,
        set_id,
        catalog: None,
        metadata: None,
        replace: None,
        proposed: proposed_doc,
        backend,
    });
    Ok(())
}

pub(crate) fn dispatch(client: &mut Client, frame: FrameKind) -> Option<FrameKind> {
    match frame {
        FrameKind::CommandResult { request_id, result } if request_id >= INTERNAL_START => {
            if let Err(error) = receive_result(client, request_id, result) {
                client.arrangement.fail(&error);
            }
        }
        FrameKind::MetadataValue { request_id, value } if request_id >= INTERNAL_START => {
            if let Err(error) = receive_metadata(client, request_id, value.as_deref()) {
                client.arrangement.fail(&error);
            }
        }
        FrameKind::Error {
            request_id: Some(request_id),
            message,
            ..
        } if request_id >= INTERNAL_START => {
            if let Err(error) = receive_error(client, request_id, message) {
                client.arrangement.fail(&error);
            }
        }
        FrameKind::ResourceSpawned { request_id, .. }
        | FrameKind::ResourceMoved { request_id, .. }
            if request_id >= INTERNAL_START =>
        {
            if pending_id(&client.arrangement, request_id) {
                client.arrangement.fail(&BridgeError::invalid(
                    "unexpected reply kind for arrangement transaction",
                ));
            }
        }
        other => return Some(other),
    }
    None
}

fn pending_id(arrangement: &SharedArrangement, id: u32) -> bool {
    arrangement.pending.as_ref().is_some_and(|p| {
        p.state_id == Some(id)
            || (p.metadata_id == id && p.metadata.is_none())
            || p.set_id == Some(id)
    })
}

fn reject_owned_reply(arrangement: &SharedArrangement, id: u32) -> Result<(), BridgeError> {
    if pending_id(arrangement, id) {
        return Err(BridgeError::invalid(
            "unexpected reply kind for arrangement transaction",
        ));
    }
    Ok(())
}

fn receive_result(client: &mut Client, id: u32, result: CommandResult) -> Result<(), BridgeError> {
    let Some(pending) = client.arrangement.pending.as_ref() else {
        return Ok(());
    };
    let organization = matches!(pending.backend, Backend::Organization(_));
    if organization && pending.metadata_id == id && pending.metadata.is_none() {
        return receive_stored(client, result);
    }
    if organization && pending.set_id == Some(id) && pending.replace.is_none() {
        if let Some(pending) = client.arrangement.pending.as_mut() {
            pending.replace = Some(OrgOutcome::from_result(&result));
        }
        return finish_read(client);
    }
    if pending.state_id != Some(id) {
        return reject_owned_reply(&client.arrangement, id);
    }
    let snapshot = match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        CommandResult::Error { message, .. } => return Err(BridgeError::state(message)),
        _ => {
            return Err(BridgeError::invalid(
                "unexpected arrangement GET_STATE reply",
            ));
        }
    };
    let catalog = Catalog::from_snapshot(snapshot.clone(), client.arrangement.selected)?;
    resources::reconcile(client, &snapshot, &catalog)?;
    if let Some(pending) = client.arrangement.pending.as_mut() {
        pending.state_id = None;
        pending.catalog = Some(catalog);
    }
    finish_read(client)
}

/// `ARRANGEMENT_GET`'s answer. A refusal that the arrangement does not
/// exist reads as no arrangement; any other refusal fails the read.
fn receive_stored(client: &mut Client, result: CommandResult) -> Result<(), BridgeError> {
    let reply = match result {
        CommandResult::OkWith(CommandValue::Arrangement(value)) => DocReply::Stored(value),
        CommandResult::Refused(Refusal::NotFound) => DocReply::Absent,
        CommandResult::Refused(refusal) => {
            return Err(BridgeError::state(
                phux_client_core::org_ops::refusal_sentence(&refusal),
            ));
        }
        CommandResult::Error { message, .. } => return Err(BridgeError::state(message)),
        _ => {
            return Err(BridgeError::invalid("unexpected ARRANGEMENT_GET reply"));
        }
    };
    if let Some(pending) = client.arrangement.pending.as_mut() {
        pending.metadata = Some(reply);
    }
    finish_read(client)
}

fn receive_error(client: &Client, id: u32, message: String) -> Result<(), BridgeError> {
    if pending_id(&client.arrangement, id) {
        return Err(BridgeError::state(message));
    }
    Ok(())
}

fn receive_metadata(client: &mut Client, id: u32, bytes: Option<&[u8]>) -> Result<(), BridgeError> {
    let Some(pending) = client.arrangement.pending.as_ref() else {
        return Ok(());
    };
    if pending.metadata_id != id || pending.backend != Backend::Legacy {
        return reject_owned_reply(&client.arrangement, id);
    }
    if pending.metadata.is_some() {
        return Ok(());
    }
    if bytes.is_some_and(|bytes| bytes.len() > MAX_LAYOUT_METADATA_BYTES) {
        return Err(BridgeError::state("layout metadata exceeds 256 KiB"));
    }
    if let Some(pending) = client.arrangement.pending.as_mut() {
        pending.metadata =
            Some(bytes.map_or(DocReply::Absent, |bytes| DocReply::Legacy(bytes.to_vec())));
    }
    finish_read(client)
}

fn finish_read(client: &mut Client) -> Result<(), BridgeError> {
    if !client
        .arrangement
        .pending
        .as_ref()
        .is_some_and(Pending::complete)
    {
        return Ok(());
    }
    let pending = client
        .arrangement
        .pending
        .take()
        .ok_or_else(|| BridgeError::state("missing arrangement transaction"))?;
    complete_snapshot(client, pending)
}

/// The read document in handle space, with what a write back needs.
struct Read {
    doc: ArrangementDoc,
    stored: bool,
    revision: u64,
    binding: EntryBinding,
    key: Option<ArrangementKey>,
}

fn read_document(client: &Client, reply: DocReply) -> Result<Read, BridgeError> {
    Ok(match reply {
        DocReply::Absent => Read {
            doc: ArrangementDoc::new(),
            stored: false,
            revision: 0,
            binding: EntryBinding::default(),
            key: None,
        },
        DocReply::Legacy(bytes) => Read {
            doc: model::decode_legacy(Some(&bytes))?,
            stored: true,
            revision: 0,
            binding: EntryBinding::default(),
            key: None,
        },
        DocReply::Stored(value) => {
            let (doc, binding) = {
                let control = client.control();
                from_protocol(&value.document, control.organization().handles())
            };
            Read {
                doc,
                stored: true,
                revision: value.revision.get(),
                binding,
                key: Some(value.key),
            }
        }
    })
}

fn complete_snapshot(client: &mut Client, pending: Pending) -> Result<(), BridgeError> {
    let catalog = pending
        .catalog
        .ok_or_else(|| BridgeError::state("missing arrangement registry"))?;
    let reply = pending
        .metadata
        .ok_or_else(|| BridgeError::state("missing arrangement document"))?;
    let read = match read_document(client, reply).and_then(|read| {
        let doc = model::adopt(
            read.doc.clone(),
            &client.arrangement.topology,
            &catalog,
            client.arrangement.selected,
        )?;
        Ok(Read { doc, ..read })
    }) {
        Ok(read) => read,
        Err(error) => {
            publish_catalog(client, catalog)?;
            return Err(error);
        }
    };
    let (status, message) = match (&pending.proposed, pending.replace) {
        (None, _) | (Some(_), Some(OrgOutcome::Committed(_))) => (2, Vec::new()),
        (Some(_), Some(outcome)) => {
            let resolution = crate::projection::organization::resolution(&outcome);
            let message = match resolution.conflict_revision {
                Some(current) => format!(
                    "the arrangement changed meanwhile (now at revision {current}); adopted it, redo the edit against it"
                ),
                None => resolution.message,
            };
            (3, message.into_bytes())
        }
        (Some(proposed), None) if topology_equal(proposed, &read.doc) => (2, Vec::new()),
        (Some(_), None) => (
            3,
            b"concurrent layout write won confirmation; adopted current shared topology".to_vec(),
        ),
    };
    let backend = match (pending.backend, read.key) {
        (Backend::Legacy, _) => Backend::Legacy,
        (Backend::Organization(_) | Backend::Awaiting, Some(key)) => Backend::Organization(key),
        // The stored arrangement went away: wait for the replica to name one.
        (Backend::Organization(_) | Backend::Awaiting, None) => Backend::Awaiting,
    };
    publish(client, catalog, read.doc, read.stored)?;
    let ws = &mut client.arrangement;
    ws.backend = backend;
    ws.server_revision = read.revision;
    ws.binding = read.binding;
    ws.status = status;
    ws.message = message;
    Ok(())
}

fn topology_equal(a: &ArrangementDoc, b: &ArrangementDoc) -> bool {
    a.groups.len() == b.groups.len()
        && a.groups
            .iter()
            .zip(&b.groups)
            .all(|(a, b)| a.id == b.id && a.name == b.name && a.state.tree == b.state.tree)
}

/// Whether two catalogs publish the same thing. Attached-client counts are
/// noise here: they change with every viewer and move no arrangement.
fn catalog_equal(a: &Catalog, b: &Catalog) -> bool {
    a.terminals == b.terminals
        && a.sessions.len() == b.sessions.len()
        && a.sessions.iter().zip(&b.sessions).all(|(a, b)| {
            a.session_id == b.session_id
                && a.name == b.name
                && a.created_at_unix_secs == b.created_at_unix_secs
                && a.window_count == b.window_count
                && a.keep_empty == b.keep_empty
                && a.key == b.key
                && a.lifetime == b.lifetime
                && a.snapshot_members == b.snapshot_members
        })
}

fn publish_catalog(client: &mut Client, catalog: Catalog) -> Result<(), BridgeError> {
    let ws = &mut client.arrangement;
    if !catalog_equal(&ws.catalog, &catalog) || ws.revision == 0 {
        ws.revision = ws
            .revision
            .checked_add(1)
            .ok_or_else(|| BridgeError::state("arrangement revision exhausted"))?;
    }
    client.sessions.clone_from(&catalog.sessions);
    client.org.sessions_dirty = true;
    ws.catalog = catalog;
    Ok(())
}

/// Give every session `matches` selects the name `new_name`, in the list
/// `phux_client_session_get` reads and in the arrangement catalog it mirrors,
/// so the next arrangement read does not see a stale name as a change. True
/// when any entry changed. Used for a rename the server applied
/// (`crate::c::session_rename`); `new_name` is already validated.
pub(crate) fn rename_sessions(
    client: &mut Client,
    matches: impl Fn(&crate::c::client::SessionSummary) -> bool,
    new_name: &[u8],
) -> bool {
    let mut changed = false;
    for session in client
        .sessions
        .iter_mut()
        .chain(client.arrangement.catalog.sessions.iter_mut())
    {
        if matches(session) && session.name != new_name {
            session.name = new_name.to_vec();
            changed = true;
        }
    }
    changed
}

/// The session summaries a `GET_STATE` snapshot carries, validated and
/// bounded exactly as `ATTACHED`'s catalog is (see `crate::c::session_query`).
pub(crate) fn session_summaries(
    snapshot: SessionSnapshot,
) -> Result<Vec<crate::c::client::SessionSummary>, BridgeError> {
    let selected = snapshot.focused_session.get();
    Ok(Catalog::from_snapshot(snapshot, selected)?.sessions)
}

fn publish(
    client: &mut Client,
    catalog: Catalog,
    topology: ArrangementDoc,
    authoritative: bool,
) -> Result<(), BridgeError> {
    let (nodes, roots) = model::flatten(&topology)?;
    let ws = &mut client.arrangement;
    if !topology_equal(&ws.topology, &topology)
        || !catalog_equal(&ws.catalog, &catalog)
        || ws.authoritative != authoritative
        || ws.state == PHUX_ARRANGEMENT_STATE_UNAVAILABLE
        || ws.revision == 0
    {
        ws.revision = ws
            .revision
            .checked_add(1)
            .ok_or_else(|| BridgeError::state("arrangement revision exhausted"))?;
    }
    client.sessions.clone_from(&catalog.sessions);
    client.org.sessions_dirty = true;
    ws.catalog = catalog;
    ws.topology = topology;
    ws.nodes = nodes;
    ws.roots = roots;
    ws.authoritative = authoritative;
    ws.state = if authoritative {
        PHUX_ARRANGEMENT_STATE_AUTHORITATIVE
    } else {
        PHUX_ARRANGEMENT_STATE_UNARRANGED
    };
    Ok(())
}

#[cfg(test)]
mod tests;
