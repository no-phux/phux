//! Receipts: extracted from the shared module.
use super::{
    ActivationInfo, CommandResult, CommandValue, Committed, LiveTarget, Op, RecordRevision,
    Refusal, Reply, ServerState, ow,
};

/// Recover a retained placed-create receipt without resubmitting its command.
pub(super) fn lookup_creation(
    s: &mut ServerState,
    operation: phux_protocol::ids::OperationId,
    session: phux_protocol::ids::SessionKey,
) -> CommandResult {
    let operation = ow::core_operation(&operation);
    let session = ow::core_session_key(&session);
    let Some(receipt) = s.org().receipt(&operation) else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    let Op::ResourceCreate {
        session: original,
        placement: Some(_),
        ..
    } = &receipt.op
    else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    if original.unwrap_or_else(|| s.org().authority().intake_session()) != session {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    }
    let applied = phux_core::Applied {
        operation,
        reply: receipt.reply,
        transition: phux_core::Transition::default(),
        effects: Vec::new(),
        replayed: true,
    };
    answer(s, &applied)
}

/// The answer a committed (or replayed) operation gets.
pub(super) fn answer(s: &mut ServerState, applied: &phux_core::Applied) -> CommandResult {
    let Some(operation) = ow::operation(applied.operation) else {
        return CommandResult::Refused(ow::refusal(&Refusal::InvalidArgument {
            field: "operation",
            reason: "must not be all zero".to_owned(),
        }));
    };
    let Reply::Ok { revision, created } = applied.reply else {
        return CommandResult::Accepted { operation };
    };
    let receipt = s.org().receipt(&applied.operation).cloned();
    let revisions: Vec<RecordRevision> =
        receipt.as_ref().and_then(|r| r.undo.as_ref()).map_or_else(
            || {
                applied
                    .transition
                    .deltas
                    .iter()
                    .filter_map(delta_revision)
                    .collect()
            },
            |undo| {
                undo.pins
                    .iter()
                    .map(|pin| RecordRevision {
                        key: record_key(pin.record),
                        revision: ow::revision(pin.revision),
                    })
                    .collect()
            },
        );
    let prior_awaiting = receipt.as_ref().and_then(|r| {
        r.undo.as_ref()?.inverse.iter().find_map(|edit| match edit {
            phux_core::InverseEdit::Intake {
                awaiting_organization,
                ..
            } if matches!(r.op, Op::InboxResolve { .. }) => Some(*awaiting_organization),
            _ => None,
        })
    });
    let visible = creation_still_in_origin(s, &created, receipt.as_ref());
    let target = created_target(s, &created, visible);
    let committed = Committed::new(
        operation,
        ow::revision(revision.unwrap_or(phux_core::Revision::ZERO)),
    )
    .with_session(created.session.and_then(ow::session_key))
    .with_resource(created.resource.and_then(ow::resource_key))
    .with_target(target)
    .with_revisions(revisions)
    .with_prior_awaiting(prior_awaiting)
    .with_replayed(applied.replayed)
    .with_creation(
        created
            .entry
            .and_then(|_| phux_protocol::ids::AuthorityId::new(*s.org().authority().id.as_bytes())),
        created
            .arrangement
            .and_then(ow::arrangement_key)
            .filter(|_| created.entry.is_some()),
        created.entry,
        creation_activation(s, &created, visible),
    );
    CommandResult::OkWith(CommandValue::Committed(Box::new(committed)))
}

pub(super) fn creation_still_in_origin(
    s: &ServerState,
    created: &phux_core::Created,
    receipt: Option<&phux_core::Receipt>,
) -> bool {
    if created.entry.is_none() {
        return true;
    }
    let Some(record) = created.resource.and_then(|key| s.org().resource(&key)) else {
        return false;
    };
    let Some(phux_core::Receipt {
        op: Op::ResourceCreate { session, .. },
        ..
    }) = receipt
    else {
        return false;
    };
    record.session == session.unwrap_or_else(|| s.org().authority().intake_session())
}

pub(super) fn created_target(
    s: &mut ServerState,
    created: &phux_core::Created,
    visible: bool,
) -> Option<LiveTarget> {
    if !visible {
        return None;
    }
    let key = created.resource?;
    let id = s.registry().resource_id(&key)?;
    let current = s.registry().record(id)?.activation.as_ref()?;
    if created.entry.is_some()
        && (current.id != phux_core::ActivationId::FIRST
            || !current.state.is_current()
            || s.resource_handle(id).is_none())
    {
        return None;
    }
    let activation = current.id;
    Some(LiveTarget {
        resource: s.intern_terminal_wire(id),
        instance: s.idspace.instance(),
        activation: ow::activation_id(activation),
    })
}

pub(super) fn creation_activation(
    s: &ServerState,
    created: &phux_core::Created,
    visible: bool,
) -> Option<ActivationInfo> {
    if !visible {
        return None;
    }
    created.entry?;
    let key = created.resource?;
    let activation = s.org().resource(&key)?.activation.as_ref()?;
    if activation.id != phux_core::ActivationId::FIRST {
        return None;
    }
    Some(ow::activation(
        activation,
        s.activation_persistence_pending(key),
    ))
}

pub(super) const fn record_key(record: phux_core::RecordRef) -> [u8; 16] {
    match record {
        phux_core::RecordRef::Session(k) => *k.as_bytes(),
        phux_core::RecordRef::Resource(k) => *k.as_bytes(),
        phux_core::RecordRef::Arrangement(k) => *k.as_bytes(),
    }
}

pub(super) const fn delta_revision(delta: &phux_core::Delta) -> Option<RecordRevision> {
    let (key, revision) = match delta {
        phux_core::Delta::PutSession(r) => (*r.key.as_bytes(), r.revision),
        phux_core::Delta::PutResource(r) => (*r.key.as_bytes(), r.revision),
        phux_core::Delta::PutArrangement(r) => (*r.key.as_bytes(), r.revision),
        _ => return None,
    };
    Some(RecordRevision {
        key,
        revision: ow::revision(revision),
    })
}
