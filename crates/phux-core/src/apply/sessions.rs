//! Sessions: extracted from the shared module.
use super::{
    Attrs, Created, Effect, Event, ExecState, FreshKeys, InverseEdit, MAX_NAME_BYTES, NewResource,
    Out, RecordRef, Refusal, Revision, Session, SessionKey, SessionLifetime, TerminateMode, Tx,
    Undo,
};

pub(super) fn check_name(field: &'static str, name: &str) -> Result<(), Refusal> {
    if name.trim().is_empty() {
        return Err(Refusal::invalid(field, "must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(Refusal::invalid(
            field,
            format!("longer than {MAX_NAME_BYTES} bytes"),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(Refusal::invalid(field, "contains a control character"));
    }
    Ok(())
}

// ---- sessions --------------------------------------------------------------

pub(super) fn session_create(
    tx: &mut Tx<'_>,
    fresh: &mut dyn FreshKeys,
    name: &str,
    lifetime: SessionLifetime,
    attrs: &Attrs,
    resource: Option<&NewResource>,
) -> Result<Out, Refusal> {
    check_name("name", name)?;
    if tx.name_taken(name, None) {
        return Err(Refusal::NameTaken);
    }
    if lifetime == SessionLifetime::Ephemeral && resource.is_none() {
        return Err(Refusal::invalid(
            "resource",
            "an ephemeral session needs a first resource",
        ));
    }
    let key = SessionKey::from_bytes(tx.fresh_key(fresh)?);
    tx.insert_session(Session {
        key,
        name: name.to_owned(),
        lifetime,
        awaiting_organization: false,
        members: Vec::new(),
        arrangement: None,
        attrs: attrs.clone(),
        created: tx.now,
        revision: Revision::FIRST,
        recent: None,
    });
    let resource = match resource {
        Some(spec) => Some(tx.create_resource(fresh, key, spec)?),
        None => None,
    };
    let arrangement = tx.arrange_new_session(fresh, key, resource.as_slice())?;
    Ok(Out::created(
        Some(Revision::FIRST),
        Created {
            session: Some(key),
            resource,
            arrangement: Some(arrangement),
            entry: None,
        },
    ))
}

/// Last writer wins: a label carries no revision check.
pub(super) fn session_rename(tx: &mut Tx<'_>, key: SessionKey, name: &str) -> Result<Out, Refusal> {
    let current = tx.session(key).ok_or(Refusal::NotFound)?;
    check_name("name", name)?;
    if current.name == name {
        return Ok(Out::ok(Some(current.revision)));
    }
    if tx.name_taken(name, Some(key)) {
        return Err(Refusal::NameTaken);
    }
    let s = tx.session_mut(key).ok_or(Refusal::NotFound)?;
    name.clone_into(&mut s.name);
    let revision = s.revision;
    tx.events.push(Event::SessionRenamed {
        key,
        name: name.to_owned(),
        revision,
    });
    Ok(Out::ok(Some(revision)))
}

/// A session name's rules, and no surrounding whitespace, so `" phux"`
/// cannot become a project apart from `"phux"`.
pub(super) fn check_project_link(link: &str) -> Result<(), Refusal> {
    check_name("project", link)?;
    if link.trim() != link {
        return Err(Refusal::invalid(
            "project",
            "has leading or trailing whitespace",
        ));
    }
    Ok(())
}

/// Last writer wins, as for a rename. `None` clears the link; the
/// free-text [`Attrs::project`] label is never touched.
pub(super) fn session_set_project(
    tx: &mut Tx<'_>,
    key: SessionKey,
    project: Option<&str>,
) -> Result<Out, Refusal> {
    let current = tx.session(key).ok_or(Refusal::NotFound)?;
    if let Some(link) = project {
        check_project_link(link)?;
    }
    if current.attrs.project_link.as_deref() == project {
        return Ok(Out::ok(Some(current.revision)));
    }
    let s = tx.session_mut(key).ok_or(Refusal::NotFound)?;
    s.attrs.project_link = project.map(str::to_owned);
    let revision = s.revision;
    tx.events.push(Event::SessionProjectSet {
        key,
        project: project.map(str::to_owned),
        revision,
    });
    Ok(Out::ok(Some(revision)))
}

/// Last writer wins, as for a rename.
pub(super) fn session_set_lifetime(
    tx: &mut Tx<'_>,
    key: SessionKey,
    lifetime: SessionLifetime,
) -> Result<Out, Refusal> {
    let current = tx.session(key).ok_or(Refusal::NotFound)?;
    if current.lifetime == lifetime {
        return Ok(Out::ok(Some(current.revision)));
    }
    if key == tx.org.authority().intake_session() && lifetime == SessionLifetime::Ephemeral {
        return Err(Refusal::invalid(
            "lifetime",
            "the designated intake session is persistent",
        ));
    }
    let s = tx.session_mut(key).ok_or(Refusal::NotFound)?;
    s.lifetime = lifetime;
    let (revision, empty) = (s.revision, s.members.is_empty());
    tx.events.push(Event::SessionLifetimeChanged {
        key,
        lifetime,
        revision,
    });
    if lifetime == SessionLifetime::Ephemeral && empty {
        tx.delete_session(key);
        return Ok(Out::ok(None));
    }
    Ok(Out::ok(Some(revision)))
}

/// No revision check: emptiness is the guard, so a member added after the
/// caller looked refuses [`Refusal::NotEmpty`].
pub(super) fn session_delete(tx: &mut Tx<'_>, key: SessionKey) -> Result<Out, Refusal> {
    let s = tx.session(key).ok_or(Refusal::NotFound)?;
    if key == tx.org.authority().intake_session() {
        return Err(Refusal::invalid(
            "session",
            "the designated intake session cannot be deleted",
        ));
    }
    if !s.members.is_empty() {
        return Err(Refusal::NotEmpty {
            members: u32::try_from(s.members.len()).unwrap_or(u32::MAX),
        });
    }
    tx.delete_session(key);
    Ok(Out::ok(None))
}

/// Remove every member, terminating what still executes, then delete the
/// session. Work that joined after the caller looked goes too.
pub(super) fn session_kill(tx: &mut Tx<'_>, key: SessionKey) -> Result<Out, Refusal> {
    let Some(session) = tx.session(key) else {
        return Ok(Out::ok(None));
    };
    if key == tx.org.authority().intake_session() {
        return Err(Refusal::invalid(
            "session",
            "the designated intake session cannot be deleted",
        ));
    }
    for member in session.members.clone() {
        let running = tx
            .resource(member)
            .and_then(|r| r.activation.as_ref())
            .filter(|a| a.state.is_current() || a.state == ExecState::Lost)
            .map(|a| (a.id, a.host_proc));
        if let Some((activation, proc)) = running {
            tx.effects.push(Effect::Terminate {
                resource: member,
                activation,
                mode: TerminateMode::Graceful,
                proc,
            });
        }
        tx.remove_resource(member);
    }
    tx.delete_session(key);
    Ok(Out::ok(None))
}

pub(super) fn intake_resolve(
    tx: &mut Tx<'_>,
    key: SessionKey,
    expected: Revision,
) -> Result<Out, Refusal> {
    let current = tx.checked_session(key, expected)?;
    if !current.awaiting_organization {
        return Ok(Out::ok(Some(current.revision)));
    }
    let revision = set_intake(tx, key, false)?;
    let undo = Undo {
        inverse: vec![InverseEdit::Intake {
            session: key,
            awaiting_organization: true,
        }],
        pins: tx.pins([RecordRef::Session(key)]),
    };
    Ok(Out::ok(Some(revision)).with_undo(undo))
}

pub(super) fn set_intake(
    tx: &mut Tx<'_>,
    key: SessionKey,
    awaiting: bool,
) -> Result<Revision, Refusal> {
    let s = tx.session_mut(key).ok_or(Refusal::NotFound)?;
    s.awaiting_organization = awaiting;
    let revision = s.revision;
    tx.events.push(Event::SessionIntakeChanged {
        session: key,
        awaiting_organization: awaiting,
        revision,
    });
    Ok(revision)
}
