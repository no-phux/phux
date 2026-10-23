//! Placement: extracted from the shared module.
use super::{
    ActivationId, ArrangementKey, ArrangementOwner, CreatePlacement, Created, Event, ExitPolicy,
    FreshKeys, Group, HashSet, InverseEdit, MAX_MOVE_BATCH, NewResource, OperationId, Out,
    RecordRef, Refusal, ResourceKey, Revision, RevisionPin, SessionKey, SourcePin, Tx, Undo,
    arrangement_replace, set_intake,
};

// ---- membership ------------------------------------------------------------

pub(super) fn resource_create(
    tx: &mut Tx<'_>,
    fresh: &mut dyn FreshKeys,
    session: Option<SessionKey>,
    spec: &NewResource,
    placement: Option<&CreatePlacement>,
) -> Result<Out, Refusal> {
    let session = session.unwrap_or_else(|| tx.org.authority().intake_session());
    if tx.session(session).is_none() {
        return Err(Refusal::NotFound);
    }
    let key = tx.create_resource(fresh, session, spec)?;
    if placement.is_some() && spec.on_exit == ExitPolicy::Retain {
        tx.resource_mut(key)
            .ok_or(Refusal::NotFound)?
            .durable_result = true;
    }
    let placed = placement
        .map(|placement| place_created_resource(tx, fresh, session, key, placement))
        .transpose()?;
    Ok(Out::created(
        Some(Revision::FIRST),
        Created {
            session: None,
            resource: Some(key),
            arrangement: placed.map(|(arrangement, _)| arrangement),
            entry: placed.map(|(_, entry)| entry),
        },
    ))
}

/// Commit a placement in the same transition as its resource and launch intent.
pub(super) fn place_created_resource(
    tx: &mut Tx<'_>,
    fresh: &mut dyn FreshKeys,
    session: SessionKey,
    resource: ResourceKey,
    placement: &CreatePlacement,
) -> Result<(ArrangementKey, [u8; 16]), Refusal> {
    use crate::arrangement::{EntryRef, Group, LayoutNode};
    let key = tx
        .session(session)
        .and_then(|s| s.arrangement)
        .ok_or(Refusal::NotFound)?;
    let arrangement = tx.arrangement(key).ok_or(Refusal::NotFound)?;
    let revision = arrangement.revision;
    let mut groups = arrangement.groups.clone();
    let entry = EntryRef::member(tx.fresh_key(fresh)?, resource);
    match placement {
        CreatePlacement::NewGroup => groups.push(Group {
            id: tx.fresh_key(fresh)?,
            name: None,
            tree: LayoutNode::Leaf(entry.clone()),
        }),
        CreatePlacement::Beside {
            entry: anchor,
            resource: target,
            instance,
            activation,
            dir,
            ratio,
        } => {
            validate_placement_target(tx, session, *target, *instance, *activation)?;
            split_created_entry(&mut groups, *anchor, *target, entry.clone(), *dir, *ratio)?;
        }
    }
    arrangement_replace(
        tx,
        fresh,
        ArrangementOwner::Session(session),
        revision,
        &groups,
    )?;
    Ok((key, entry.entry))
}

pub(super) fn split_created_entry(
    groups: &mut [Group],
    anchor: [u8; 16],
    target: ResourceKey,
    entry: crate::arrangement::EntryRef,
    dir: crate::arrangement::SplitDir,
    ratio: f32,
) -> Result<(), Refusal> {
    let group = groups
        .iter_mut()
        .find(|group| group.tree.leaves().iter().any(|leaf| leaf.entry == anchor))
        .ok_or(Refusal::NotFound)?;
    let anchor = group
        .tree
        .leaves()
        .into_iter()
        .find(|leaf| leaf.entry == anchor)
        .cloned()
        .ok_or(Refusal::NotFound)?;
    if anchor.resource() != Some(target) {
        return Err(Refusal::invalid(
            "placement",
            "entry no longer shows the observed resource",
        ));
    }
    group
        .tree
        .split(&anchor, entry, dir, ratio)
        .map_err(|error| Refusal::invalid("placement", error.to_string()))
}

pub(super) fn validate_placement_target(
    tx: &Tx<'_>,
    session: SessionKey,
    resource: ResourceKey,
    instance: crate::keys::InstanceId,
    activation: ActivationId,
) -> Result<(), Refusal> {
    let record = tx.resource(resource).ok_or(Refusal::NotFound)?;
    if record.session != session {
        return Err(Refusal::invalid(
            "placement",
            "anchor is not a member of the destination session",
        ));
    }
    let current = record
        .activation
        .as_ref()
        .ok_or(Refusal::Stale { current: None })?;
    if current.instance != instance || current.id != activation || !current.state.is_current() {
        return Err(Refusal::Stale {
            current: Some(current.id),
        });
    }
    Ok(())
}

/// Move `moves` (resource, destination) and build the undo that puts each
/// back where it was, including the source arrangement a move prunes.
pub(super) fn move_with_undo(
    tx: &mut Tx<'_>,
    moves: &[(ResourceKey, SessionKey)],
) -> Result<Option<Undo>, Refusal> {
    let mut inverse = Vec::new();
    let mut layouts = Vec::new();
    let mut touched = Vec::new();
    for &(resource, to) in moves {
        let from = tx.resource(resource).ok_or(Refusal::NotFound)?.session;
        if let Some(a) = tx
            .session(from)
            .and_then(|s| s.arrangement)
            .and_then(|key| tx.arrangement(key))
            .filter(|a| a.resources().contains(&resource))
            && !layouts
                .iter()
                .any(|e| matches!(e, InverseEdit::Arrangement { key, .. } if *key == a.key))
        {
            touched.push(RecordRef::Arrangement(a.key));
            layouts.push(InverseEdit::Arrangement {
                key: a.key,
                groups: a.groups.clone(),
            });
        }
        if tx.move_resource(resource, to)? != to {
            inverse.push(InverseEdit::Membership { resource, to: from });
            touched.extend([
                RecordRef::Resource(resource),
                RecordRef::Session(from),
                RecordRef::Session(to),
            ]);
        }
    }
    inverse.extend(layouts);
    Ok((!inverse.is_empty()).then(|| Undo {
        inverse,
        pins: tx.pins(touched),
    }))
}

pub(super) fn resource_move(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    (from, from_expected): (SessionKey, Revision),
    (to, to_expected): (SessionKey, Revision),
) -> Result<Out, Refusal> {
    let current = tx.resource(resource).ok_or(Refusal::NotFound)?;
    if current.session != from {
        return Err(Refusal::Conflict {
            current: current.revision,
        });
    }
    tx.checked_session(from, from_expected)?;
    tx.checked_session(to, to_expected)?;
    let undo = move_with_undo(tx, &[(resource, to)])?;
    let revision = tx.resource(resource).map(|r| r.revision);
    let out = Out::ok(revision);
    Ok(match undo {
        Some(u) => out.with_undo(u),
        None => out,
    })
}

pub(super) fn resource_move_many(
    tx: &mut Tx<'_>,
    resources: &[ResourceKey],
    sources: &[SourcePin],
    to: SessionKey,
    to_expected: Revision,
) -> Result<Out, Refusal> {
    if resources.is_empty() {
        return Err(Refusal::invalid("resources", "empty selection"));
    }
    if resources.len() > MAX_MOVE_BATCH {
        return Err(Refusal::invalid(
            "resources",
            format!("more than {MAX_MOVE_BATCH} resources"),
        ));
    }
    if resources.iter().collect::<HashSet<_>>().len() != resources.len() {
        return Err(Refusal::invalid("resources", "duplicate resource"));
    }
    tx.checked_session(to, to_expected)?;
    let mut pinned = HashSet::new();
    for pin in sources {
        tx.checked_session(pin.session, pin.expected)?;
        pinned.insert(pin.session);
    }
    for key in resources {
        let r = tx.resource(*key).ok_or(Refusal::NotFound)?;
        if r.session != to && !pinned.contains(&r.session) {
            return Err(Refusal::invalid(
                "sources",
                format!("source session {} of {key} is not pinned", r.session),
            ));
        }
    }
    let moves: Vec<(ResourceKey, SessionKey)> = resources.iter().map(|r| (*r, to)).collect();
    let undo = move_with_undo(tx, &moves)?;
    let revision = tx.session(to).map(|s| s.revision);
    let out = Out::ok(revision);
    Ok(match undo {
        Some(u) => out.with_undo(u),
        None => out,
    })
}

pub(super) fn organization_undo(
    tx: &mut Tx<'_>,
    original: OperationId,
    expected: &[RevisionPin],
) -> Result<Out, Refusal> {
    let receipt = tx.org.receipt(&original).ok_or(Refusal::NotFound)?;
    let undo = receipt.undo.clone().ok_or_else(|| Refusal::Unsupported {
        capability: "undo".to_owned(),
    })?;
    for pin in undo.pins.iter().chain(expected) {
        let current = tx.revision_of(pin.record);
        if current != Some(pin.revision) {
            return Err(Refusal::Conflict {
                current: current.unwrap_or(Revision::ZERO),
            });
        }
    }
    let mut moves = Vec::new();
    let mut intake = Vec::new();
    let mut layouts = Vec::new();
    for edit in &undo.inverse {
        match edit {
            InverseEdit::Membership { resource, to } => {
                if tx.session(*to).is_none() {
                    return Err(Refusal::invalid(
                        "undo",
                        "the original session no longer exists",
                    ));
                }
                moves.push((*resource, *to));
            }
            InverseEdit::Intake {
                session,
                awaiting_organization,
            } => intake.push((*session, *awaiting_organization)),
            InverseEdit::Arrangement { key, groups } => layouts.push((*key, groups)),
        }
    }
    let mut redo = move_with_undo(tx, &moves)?.unwrap_or(Undo {
        inverse: Vec::new(),
        pins: Vec::new(),
    });
    let mut touched = Vec::new();
    for (session, awaiting) in intake {
        let prior = tx
            .session(session)
            .ok_or(Refusal::NotFound)?
            .awaiting_organization;
        set_intake(tx, session, awaiting)?;
        redo.inverse.push(InverseEdit::Intake {
            session,
            awaiting_organization: prior,
        });
        touched.push(RecordRef::Session(session));
    }
    for (key, groups) in layouts {
        let a = tx.arrangement_mut(key).ok_or(Refusal::NotFound)?;
        a.groups.clone_from(groups);
        let (owner, revision) = (a.owner, a.revision);
        tx.events.push(Event::ArrangementChanged {
            key,
            owner,
            revision: Some(revision),
        });
        touched.push(RecordRef::Arrangement(key));
    }
    let records: Vec<RecordRef> = redo.pins.iter().map(|p| p.record).chain(touched).collect();
    redo.pins = tx.pins(records);
    Ok(Out::ok(None).with_undo(redo))
}
