//! Organization result values: records, pages, arrangements, commits, and delegations.

use super::{
    ActivationInfo, ArrangementOwner, ArrangementValue, AuthorityId, BytesMut, Committed,
    DecodeError, Decoder, Delegation, Encoder, Fields, MAX_ORG_CURSOR_BYTES, OrganizationPage,
    ResourceRecord, Revision, SessionKey, SessionRecord, encode_live_target, encode_outcome,
    encode_pair, encode_pin, encode_terminal_id, field, read_key, req, u32_value, u64_value,
    write_document, write_key, write_name, write_operation, write_resource, write_session,
};

pub(super) fn encode_session_record(record: &SessionRecord, enc: &mut Encoder<'_>) {
    write_session(enc, field::org::SESSION, &record.key);
    write_name(enc, &record.name);
    enc.write_u8_field(field::org::LIFETIME, record.lifetime.to_u8());
    for pair in &record.attrs {
        encode_pair(enc, field::org::ATTR, pair);
    }
    if let Some(arrangement) = record.arrangement {
        write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
    }
    enc.write_u64_field(field::org::REVISION, record.revision.get());
    if let Some(live) = record.live {
        enc.write_u32_field(field::org::LIVE_SESSION, live.get());
    }
    if record.awaiting_organization {
        enc.write_u8_field(field::org::AWAITING, 1);
    }
    enc.write_u32_field(field::org::MEMBERS, record.members);
}

pub(super) fn decode_session_record(value: &[u8]) -> Result<SessionRecord, DecodeError> {
    let f = Fields::read(value)?;
    Ok(SessionRecord {
        key: f.session()?,
        lifetime: f.reported_lifetime().unwrap_or_default(),
        name: req(f.name)?,
        awaiting_organization: f.awaiting.unwrap_or(false),
        arrangement: f.arrangement,
        attrs: f.attrs,
        members: f.members.unwrap_or(0),
        revision: req(f.revision)?,
        live: f.live_session,
    })
}

pub(super) fn encode_resource_record(record: &ResourceRecord, enc: &mut Encoder<'_>) {
    write_session(enc, field::org::SESSION, &record.session);
    write_resource(enc, &record.key);
    if let Some(name) = &record.name {
        write_name(enc, name);
    }
    enc.write_u8_field(field::org::KIND, record.kind.as_wire());
    enc.write_u8_field(field::org::ON_EXIT, record.on_exit.to_u8());
    if let Some(activation) = &record.activation {
        enc.write_u64_field(field::org::ACTIVATION, activation.id.get());
    }
    enc.write_u64_field(field::org::REVISION, record.revision.get());
    if let Some(live) = &record.live {
        enc.write_field_with(field::org::LIVE_RESOURCE, |e| encode_terminal_id(live, e));
    }
    if let Some(activation) = &record.activation {
        enc.write_u8_field(field::org::STATE, activation.state.to_u8());
        if let Some(outcome) = activation.outcome {
            enc.write_field_with(field::org::OUTCOME, |e| encode_outcome(outcome, e));
        }
        write_key(enc, field::org::INSTANCE, activation.instance.as_bytes());
        encode_evidence(activation, enc);
    }
}

fn encode_evidence(activation: &ActivationInfo, enc: &mut Encoder<'_>) {
    if let Some(diagnostic) = activation.diagnostic {
        // Only an enum and an optional i32: JSON serialization cannot fail.
        if let Ok(bytes) = serde_json::to_vec(&diagnostic) {
            enc.write_field(field::org::LAUNCH_DIAGNOSTIC, &bytes);
        }
    }
    if activation.persistence_pending {
        enc.write_u8_field(field::org::PERSISTENCE_PENDING, 1);
    }
}

pub(super) fn decode_resource_record(value: &[u8]) -> Result<ResourceRecord, DecodeError> {
    let f = Fields::read(value)?;
    let state = f.state();
    let activation = match (f.activation, f.instance, state) {
        (Some(id), Some(instance), Some(state)) => Some(ActivationInfo {
            id,
            instance,
            state,
            outcome: f.outcome,
            diagnostic: f.diagnostic,
            persistence_pending: f.persistence_pending,
        }),
        (None, _, _) => None,
        _ => return Err(DecodeError::UnexpectedEof),
    };
    Ok(ResourceRecord {
        key: f.resource()?,
        session: f.session()?,
        on_exit: f.reported_on_exit().unwrap_or_default(),
        name: f.name,
        kind: f.kind.unwrap_or_default(),
        activation,
        revision: req(f.revision)?,
        live: f.live_resource,
    })
}

/// Write an `ORGANIZATION_PAGE` value body (field-tagged, `field::org_page`).
pub(in crate::wire) fn encode_organization_page(page: &OrganizationPage, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        write_key(
            &mut e,
            field::org_page::AUTHORITY,
            page.authority.as_bytes(),
        );
        if let Some(inbox) = page.inbox {
            write_key(&mut e, field::org_page::INBOX, inbox.as_bytes());
        }
        for record in &page.sessions {
            e.write_field_with(field::org_page::SESSION, |e| {
                encode_session_record(record, e);
            });
        }
        for record in &page.resources {
            e.write_field_with(field::org_page::RESOURCE, |e| {
                encode_resource_record(record, e);
            });
        }
        e.write_u32_field(field::org_page::TOTAL_SESSIONS, page.total_sessions);
        e.write_u32_field(field::org_page::TOTAL_RESOURCES, page.total_resources);
        e.write_u32_field(field::org_page::TOTAL_INTAKE, page.total_intake);
        e.write_u64_field(
            field::org_page::SNAPSHOT_REVISION,
            page.snapshot_revision.get(),
        );
        e.write_u64_field(field::org_page::JOURNAL_SEQ, page.journal_seq);
        if let Some(cursor) = &page.next_cursor {
            e.write_field(field::org_page::NEXT_CURSOR, cursor);
        }
    }
    enc.write_bytes(&body);
}

/// Read an `ORGANIZATION_PAGE` value body.
pub(in crate::wire) fn decode_organization_page(
    dec: &mut Decoder<'_>,
) -> Result<OrganizationPage, DecodeError> {
    let mut body = Decoder::new(dec.read_bytes()?);
    let mut authority = None;
    let mut inbox = None;
    let mut sessions = Vec::new();
    let mut resources = Vec::new();
    let mut totals = (0, 0, 0);
    let mut snapshot_revision = Revision::new(0);
    let mut journal_seq = 0;
    let mut next_cursor = None;
    for_each_field!(body, value, {
        field::org_page::AUTHORITY => authority = Some(read_key(value, AuthorityId::new)?),
        field::org_page::INBOX => inbox = Some(read_key(value, SessionKey::new)?),
        field::org_page::SESSION => sessions.push(decode_session_record(value)?),
        field::org_page::RESOURCE => resources.push(decode_resource_record(value)?),
        field::org_page::TOTAL_SESSIONS => totals.0 = u32_value(value)?,
        field::org_page::TOTAL_RESOURCES => totals.1 = u32_value(value)?,
        field::org_page::TOTAL_INTAKE => totals.2 = u32_value(value)?,
        field::org_page::SNAPSHOT_REVISION => snapshot_revision = Revision::new(u64_value(value)?),
        field::org_page::JOURNAL_SEQ => journal_seq = u64_value(value)?,
        field::org_page::NEXT_CURSOR => {
            if value.len() > MAX_ORG_CURSOR_BYTES {
                return Err(DecodeError::OrganizationLimitExceeded);
            }
            next_cursor = Some(value.to_vec());
        }
    });
    Ok(
        OrganizationPage::new(req(authority)?, snapshot_revision, journal_seq)
            .with_inbox(inbox)
            .with_sessions(sessions)
            .with_resources(resources)
            .with_totals(totals.0, totals.1, totals.2)
            .with_next_cursor(next_cursor),
    )
}

/// Write an `ARRANGEMENT` value body (field-tagged, `field::org`).
pub(in crate::wire) fn encode_arrangement_value(value: &ArrangementValue, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        write_owner(&mut e, value.owner);
        write_key(&mut e, field::org::ARRANGEMENT, value.key.as_bytes());
        write_document(&mut e, &value.document);
        e.write_u64_field(field::org::REVISION, value.revision.get());
    }
    enc.write_bytes(&body);
}

/// Read an `ARRANGEMENT` value body.
pub(in crate::wire) fn decode_arrangement_value(
    dec: &mut Decoder<'_>,
) -> Result<ArrangementValue, DecodeError> {
    let f = Fields::read(dec.read_bytes()?)?;
    Ok(ArrangementValue {
        key: req(f.arrangement)?,
        owner: f.owner()?,
        revision: req(f.revision)?,
        document: req(f.document)?,
    })
}

/// Write an arrangement's owner: its `SESSION` or `WORKSPACE` field.
pub(super) fn write_owner(enc: &mut Encoder<'_>, owner: ArrangementOwner) {
    match owner {
        ArrangementOwner::Session(session) => write_session(enc, field::org::SESSION, &session),
        ArrangementOwner::Workspace(workspace) => {
            write_key(enc, field::org::WORKSPACE, workspace.as_bytes());
        }
    }
}

/// Write a `COMMITTED` value body (field-tagged, `field::org`).
pub(in crate::wire) fn encode_committed(committed: &Committed, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        write_operation(&mut e, &committed.operation);
        if let Some(session) = &committed.session {
            write_session(&mut e, field::org::SESSION, session);
        }
        if let Some(resource) = &committed.resource {
            write_resource(&mut e, resource);
        }
        if let Some(target) = &committed.target {
            e.write_field_with(field::org::TARGET, |e| encode_live_target(target, e));
        }
        for pin in &committed.revisions {
            encode_pin(pin, &mut e);
        }
        if let Some(authority) = &committed.authority {
            write_key(&mut e, field::org::AUTHORITY, authority.as_bytes());
        }
        if let Some(arrangement) = &committed.arrangement {
            write_key(&mut e, field::org::ARRANGEMENT, arrangement.as_bytes());
        }
        if let Some(entry) = &committed.entry {
            write_key(&mut e, field::org::ENTRY, entry);
        }
        if let Some(created) = committed.ensured_created {
            e.write_u8_field(field::org::ENSURED_CREATED, u8::from(created));
        }
        if committed.replayed {
            e.write_u8_field(field::org::REPLAYED, 1);
        }
        if let Some(activation) = &committed.activation {
            e.write_u64_field(field::org::ACTIVATION, activation.id.get());
            write_key(&mut e, field::org::INSTANCE, activation.instance.as_bytes());
            e.write_u8_field(field::org::STATE, activation.state.to_u8());
            encode_evidence(activation, &mut e);
            if let Some(outcome) = activation.outcome {
                e.write_field_with(field::org::OUTCOME, |e| encode_outcome(outcome, e));
            }
        }
        e.write_u64_field(field::org::REVISION, committed.revision.get());
        if let Some(prior) = committed.prior_awaiting {
            e.write_u8_field(field::org::AWAITING, u8::from(prior));
        }
        if let Some(members) = committed.members {
            e.write_u32_field(field::org::MEMBERS, members);
        }
    }
    enc.write_bytes(&body);
}

/// Read a `COMMITTED` value body.
pub(in crate::wire) fn decode_committed(dec: &mut Decoder<'_>) -> Result<Committed, DecodeError> {
    let f = Fields::read(dec.read_bytes()?)?;
    let state = f.state();
    Ok(Committed {
        operation: f.operation()?,
        revision: req(f.revision)?,
        ensured_created: f.ensured_created,
        session: f.session,
        resource: f.resource,
        target: f.target,
        revisions: f.pins,
        prior_awaiting: f.awaiting,
        members: f.members,
        arrangement: f.arrangement,
        entry: f.entry,
        replayed: f.replayed,
        authority: f.authority,
        activation: match (f.activation, f.instance, state) {
            (Some(id), Some(instance), Some(state)) => Some(ActivationInfo {
                id,
                instance,
                state,
                outcome: f.outcome,
                diagnostic: f.diagnostic,
                persistence_pending: f.persistence_pending,
            }),
            (None, None, None) => None,
            _ => return Err(DecodeError::UnexpectedEof),
        },
    })
}

/// Write a `DELEGATION` value body: `u32`-length-prefixed `field::org`
/// fields.
pub(in crate::wire) fn encode_delegation(delegation: &Delegation, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        e.write_field(field::org::CREDENTIAL, delegation.credential_id.as_bytes());
        write_session(&mut e, field::org::SESSION, &delegation.session);
        e.write_field(field::org::SOCKET_PATH, delegation.socket_path.as_bytes());
        e.write_u64_field(field::org::EXPIRES_AT, delegation.expires_at_ms);
    }
    enc.write_bytes(&body);
}

/// Read a `DELEGATION` value body.
pub(in crate::wire) fn decode_delegation(dec: &mut Decoder<'_>) -> Result<Delegation, DecodeError> {
    let f = Fields::read(dec.read_bytes()?)?;
    Ok(Delegation {
        session: f.session()?,
        credential_id: req(f.credential)?,
        socket_path: req(f.socket_path)?,
        expires_at_ms: req(f.expires_at)?,
    })
}
