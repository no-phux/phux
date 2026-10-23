//! Resources: extracted from the shared module.
use super::{
    Activation, ActivationId, Arrangement, ArrangementKey, ArrangementOwner, Created, Effect,
    Event, ExecState, ExitPolicy, FreshKeys, Group, LaunchSpec, Out, Outcome, ProcIdentity,
    Refusal, Resource, ResourceKey, Revision, TerminateMode, Tx, check_name, validate_groups,
};

// ---- resources -------------------------------------------------------------

/// Last writer wins, as for a session rename.
pub(super) fn resource_rename(
    tx: &mut Tx<'_>,
    key: ResourceKey,
    name: Option<&str>,
) -> Result<Out, Refusal> {
    let current = tx.resource(key).ok_or(Refusal::NotFound)?;
    if let Some(n) = name {
        check_name("name", n)?;
    }
    if current.name.as_deref() == name {
        return Ok(Out::ok(Some(current.revision)));
    }
    let r = tx.resource_mut(key).ok_or(Refusal::NotFound)?;
    r.name = name.map(str::to_owned);
    let revision = r.revision;
    tx.events.push(Event::ResourceRenamed {
        key,
        name: name.map(str::to_owned),
        revision,
    });
    Ok(Out::ok(Some(revision)))
}

/// The resource's activation when its id is `activation`; otherwise
/// [`Refusal::Stale`] naming the current one.
pub(super) fn matching_activation(
    tx: &Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
) -> Result<Activation, Refusal> {
    let r = tx.resource(resource).ok_or(Refusal::NotFound)?;
    match &r.activation {
        Some(a) if a.id == activation => Ok(a.clone()),
        other => Err(Refusal::Stale {
            current: other.as_ref().map(|a| a.id),
        }),
    }
}

pub(super) fn resource_terminate(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
    mode: TerminateMode,
) -> Result<Out, Refusal> {
    let a = matching_activation(tx, resource, activation)?;
    if !(a.state.is_current() || a.state == ExecState::Lost) {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    if matches!(a.state, ExecState::Starting | ExecState::Running) {
        tx.set_activation(resource, |a| a.state = ExecState::Stopping);
    }
    tx.effects.push(Effect::Terminate {
        resource,
        activation,
        mode,
        proc: a.host_proc,
    });
    Ok(Out::accepted())
}

/// Refuse while `resource`'s activation is current or lost.
pub(super) fn require_inactive(r: &Resource) -> Result<(), Refusal> {
    match r.activation.as_ref().map(|a| a.state) {
        Some(ExecState::Lost) => Err(Refusal::ExecutionUncertain),
        Some(s) if s.is_current() => Err(Refusal::invalid(
            "resource",
            "execution is current; terminate it first",
        )),
        _ => Ok(()),
    }
}

/// No revision check: inactivity is the guard.
pub(super) fn resource_remove(tx: &mut Tx<'_>, key: ResourceKey) -> Result<Out, Refusal> {
    require_inactive(tx.resource(key).ok_or(Refusal::NotFound)?)?;
    tx.remove_resource(key);
    Ok(Out::ok(None))
}

pub(super) fn resource_relaunch(
    tx: &mut Tx<'_>,
    key: ResourceKey,
    expected: Revision,
    replacement: Option<&LaunchSpec>,
) -> Result<Out, Refusal> {
    let r = tx.checked_resource(key, expected)?;
    require_inactive(r)?;
    if replacement.is_some_and(|launch| launch.kind() != r.kind) {
        return Err(Refusal::invalid(
            "launch",
            "launch spec is for another kind",
        ));
    }
    let spec = replacement
        .or(r.launch.as_ref())
        .ok_or_else(|| Refusal::invalid("resource", "no launch spec to relaunch"))?
        .clone();
    let id = r
        .activation
        .as_ref()
        .map_or(ActivationId::FIRST, |a| a.id.next());
    let (instance, now) = (tx.org.instance(), tx.now);
    let record = tx.resource_mut(key).ok_or(Refusal::NotFound)?;
    if replacement.is_some() {
        record.launch = Some(spec.clone());
    }
    record.activation = Some(Activation::starting(id, instance, now));
    tx.events.push(Event::ActivationChanged {
        resource: key,
        activation: id,
        state: ExecState::Starting,
        outcome: None,
    });
    tx.effects.push(Effect::Launch {
        resource: key,
        activation: id,
        spec,
    });
    Ok(Out::accepted())
}

pub(super) fn arrangement_replace(
    tx: &mut Tx<'_>,
    fresh: &mut dyn FreshKeys,
    owner: ArrangementOwner,
    expected: Revision,
    groups: &[Group],
) -> Result<Out, Refusal> {
    let ArrangementOwner::Session(session) = owner else {
        return Err(Refusal::Unsupported {
            capability: "workspace".to_owned(),
        });
    };
    let s = tx.session(session).ok_or(Refusal::NotFound)?;
    let existing = s.arrangement;
    let current = existing
        .and_then(|k| tx.arrangement(k))
        .map_or(Revision::ZERO, |a| a.revision);
    if current != expected {
        return Err(Refusal::Conflict { current });
    }
    validate_groups(groups, &s.members).map_err(|e| Refusal::invalid("groups", e.to_string()))?;
    let (key, revision, created) = if let Some(key) = existing {
        let a = tx.arrangement_mut(key).ok_or(Refusal::NotFound)?;
        a.groups = groups.to_vec();
        (key, a.revision, None)
    } else {
        let key = ArrangementKey::from_bytes(tx.fresh_key(fresh)?);
        tx.arrangements.insert(
            key,
            Some(Arrangement {
                key,
                owner,
                groups: groups.to_vec(),
                revision: Revision::FIRST,
            }),
        );
        tx.session_mut(session)
            .ok_or(Refusal::NotFound)?
            .arrangement = Some(key);
        (key, Revision::FIRST, Some(key))
    };
    tx.events.push(Event::ArrangementChanged {
        key,
        owner,
        revision: Some(revision),
    });
    Ok(Out::created(
        Some(revision),
        Created {
            session: None,
            resource: None,
            arrangement: created,
            entry: None,
        },
    ))
}

// ---- executor reports ------------------------------------------------------

pub(super) fn resource_launch_reported(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
    launch: &LaunchSpec,
) -> Result<Out, Refusal> {
    let current = matching_activation(tx, resource, activation)?;
    if !current.state.is_current() {
        return Err(Refusal::Stale {
            current: Some(current.id),
        });
    }
    let record = tx.resource(resource).ok_or(Refusal::NotFound)?;
    if launch.kind() != record.kind {
        return Err(Refusal::invalid(
            "launch",
            "launch spec is for another kind",
        ));
    }
    if record.launch.as_ref() == Some(launch) {
        return Ok(Out::ok(Some(record.revision)));
    }
    let record = tx.resource_mut(resource).ok_or(Refusal::NotFound)?;
    record.launch = Some(launch.clone());
    Ok(Out::ok(Some(record.revision)))
}

pub(super) fn activation_started(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
    proc: Option<ProcIdentity>,
) -> Result<Out, Refusal> {
    let a = matching_activation(tx, resource, activation)?;
    if a.state != ExecState::Starting {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    let revision = tx.set_activation(resource, |a| {
        a.state = ExecState::Running;
        a.host_proc = proc;
    });
    Ok(Out::ok(revision))
}

/// An observed completion of a [`ExitPolicy::Remove`] resource removes the
/// record in the same transition. An unobserved one (a crash) keeps it.
pub(super) fn activation_ended(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
    state: ExecState,
    outcome: Outcome,
    diagnostic: Option<crate::LaunchDiagnostic>,
) -> Result<Out, Refusal> {
    if !matches!(state, ExecState::Completed | ExecState::Failed) {
        return Err(Refusal::invalid(
            "state",
            "an ending is completed or failed",
        ));
    }
    let a = matching_activation(tx, resource, activation)?;
    if !(a.state.is_current() || a.state == ExecState::Lost) {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    let now = tx.now;
    let revision = tx.set_activation(resource, |a| {
        a.state = state;
        a.ended = Some(now);
        a.outcome = Some(outcome);
        a.diagnostic = diagnostic;
    });
    let remove = state == ExecState::Completed
        && outcome.is_observed()
        && tx
            .resource(resource)
            .is_some_and(|r| r.on_exit == ExitPolicy::Remove);
    if remove {
        tx.remove_resource(resource);
        return Ok(Out::ok(None));
    }
    Ok(Out::ok(revision))
}

pub(super) fn activation_lost(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
) -> Result<Out, Refusal> {
    let a = matching_activation(tx, resource, activation)?;
    if !a.state.is_current() {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    let revision = tx.set_activation(resource, |a| a.state = ExecState::Lost);
    Ok(Out::ok(revision))
}
