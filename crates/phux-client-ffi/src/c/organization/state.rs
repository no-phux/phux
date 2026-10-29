//! Per-client organization state: seeding, event intake, and result correlation.

use crate::projection::organization as projected;

use super::{
    BTreeMap, Client, Event, ExecState, OperationId, OrgOutcome, RecordView, Request, ResourceKey,
    SessionKey, SessionLifetime, SessionOrg,
};

// ----- client state ---------------------------------------------------------------

/// The C lane's organization state.
#[derive(Debug, Default)]
pub(crate) struct OrgState {
    /// The connection epoch whose replica this client seeded.
    pub(super) seeded_epoch: Option<u64>,
    /// Bumped whenever the replica changed under this client.
    pub(super) revision: u64,
    /// The last `OrganizationUnavailable` message.
    pub(super) message: Vec<u8>,
    /// The session list was replaced; re-enrich it from the replica.
    pub sessions_dirty: bool,
    /// Every resource record the replica holds.
    pub records: Vec<RecordView>,
    pub(super) requests: BTreeMap<u32, Request>,
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
            crate::c::arrangement::organization_unavailable(client, &message);
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
