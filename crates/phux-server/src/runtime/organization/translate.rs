//! Translating wire commands into core ops and core replies back into results.

use phux_core::{Op, Refusal, Reply, ResourceKind};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, Committed, LiveTarget, RecordRevision,
};

use crate::org_wire as ow;
use crate::state::ServerState;

/// The wire command as a core operation, or the refusal for an argument
/// core cannot express. `None` for a read or a non-organization command.
#[allow(clippy::too_many_lines, reason = "one arm per organization command")]
pub(super) fn translate(
    s: &ServerState,
    command: Command,
) -> Option<(phux_core::OperationId, Result<Op, Refusal>)> {
    let rev = ow::core_revision;
    Some(match command {
        Command::SessionCreate {
            operation,
            name,
            lifetime,
            attrs,
            launch,
        } => (
            ow::core_operation(&operation),
            Ok(Op::SessionCreate {
                name,
                lifetime: ow::core_lifetime(lifetime),
                attrs: ow::core_attrs(&attrs),
                resource: launch.map(|launch| phux_core::NewResource {
                    kind: ResourceKind::Terminal,
                    name: None,
                    launch: ow::core_launch(ResourceKind::Terminal, &launch),
                    on_exit: phux_core::ExitPolicy::Remove,
                    parent: None,
                    launch_mode: phux_core::LaunchMode::Immediate,
                }),
            }),
        ),
        Command::SessionRename {
            operation,
            session,
            name,
        } => (
            ow::core_operation(&operation),
            Ok(Op::SessionRename {
                session: ow::core_session_key(&session),
                name,
            }),
        ),
        Command::SessionSetProject {
            operation,
            session,
            project,
        } => (
            ow::core_operation(&operation),
            Ok(Op::SessionSetProject {
                session: ow::core_session_key(&session),
                project,
            }),
        ),
        Command::SessionSetLifetime {
            operation,
            session,
            lifetime,
        } => (
            ow::core_operation(&operation),
            Ok(Op::SessionSetLifetime {
                session: ow::core_session_key(&session),
                lifetime: ow::core_lifetime(lifetime),
            }),
        ),
        Command::SessionDelete { operation, session } => (
            ow::core_operation(&operation),
            Ok(Op::SessionDelete {
                session: ow::core_session_key(&session),
            }),
        ),
        Command::ResourceCreate {
            operation,
            session,
            kind,
            launch,
            name,
            on_exit,
            launch_mode,
            placement,
        } => {
            if placement.is_some()
                && s.org().receipt(&ow::core_operation(&operation)).is_none()
                && let Some(launch) = launch.as_deref()
                && let Err(reason) = validate_placed_launch(launch)
            {
                return Some((
                    ow::core_operation(&operation),
                    Err(Refusal::InvalidArgument {
                        field: "launch",
                        reason,
                    }),
                ));
            }
            let kind = crate::resource::core_kind(kind);
            let op = kind
                .ok_or_else(|| Refusal::Unsupported {
                    capability: "this resource kind".to_owned(),
                })
                .map(|kind| Op::ResourceCreate {
                    placement: placement.as_deref().map(core_placement),
                    session: Some(ow::core_session_key(&session)),
                    resource: phux_core::NewResource {
                        kind,
                        name,
                        launch: launch
                            .as_deref()
                            .and_then(|launch| ow::core_launch(kind, launch))
                            .or_else(|| {
                                (kind == ResourceKind::Terminal).then(|| {
                                    phux_core::LaunchSpec::Terminal(
                                        phux_core::TerminalLaunch::default(),
                                    )
                                })
                            }),
                        on_exit: ow::core_exit_policy(on_exit),
                        parent: None,
                        launch_mode: ow::core_launch_mode(launch_mode),
                    },
                });
            (ow::core_operation(&operation), op)
        }
        Command::ResourceMove {
            operation,
            resource,
            from,
            from_expected,
            to,
            to_expected,
        } => (
            ow::core_operation(&operation),
            Ok(Op::ResourceMove {
                resource: ow::core_resource_key(&resource),
                from: ow::core_session_key(&from),
                from_expected: rev(from_expected),
                to: ow::core_session_key(&to),
                to_expected: rev(to_expected),
            }),
        ),
        Command::ResourceMoveMany {
            operation,
            items,
            to,
            to_expected,
        } => {
            let mut sources: Vec<phux_core::SourcePin> = Vec::new();
            for item in &items {
                let pin = phux_core::SourcePin {
                    session: ow::core_session_key(&item.from),
                    expected: rev(item.from_expected),
                };
                if !sources.contains(&pin) {
                    sources.push(pin);
                }
            }
            (
                ow::core_operation(&operation),
                Ok(Op::ResourceMoveMany {
                    resources: items
                        .iter()
                        .map(|item| ow::core_resource_key(&item.resource))
                        .collect(),
                    sources,
                    to: ow::core_session_key(&to),
                    to_expected: rev(to_expected),
                }),
            )
        }
        Command::ResourceRename {
            operation,
            resource,
            name,
        } => (
            ow::core_operation(&operation),
            Ok(Op::ResourceRename {
                resource: ow::core_resource_key(&resource),
                name,
            }),
        ),
        Command::ResourceTerminate {
            operation,
            target,
            mode,
        } => (
            ow::core_operation(&operation),
            terminate_op(s, &target, mode),
        ),
        Command::ResourceRemove {
            operation,
            resource,
        } => (
            ow::core_operation(&operation),
            Ok(Op::ResourceRemove {
                resource: ow::core_resource_key(&resource),
            }),
        ),
        Command::ResourceRelaunch {
            operation,
            resource,
            expected,
        } => (
            ow::core_operation(&operation),
            Ok(Op::ResourceRelaunch {
                resource: ow::core_resource_key(&resource),
                expected: rev(expected),
                launch: None,
            }),
        ),
        Command::InboxResolve {
            operation,
            session,
            expected,
        } => (
            ow::core_operation(&operation),
            Ok(Op::InboxResolve {
                session: ow::core_session_key(&session),
                expected: rev(expected),
            }),
        ),
        Command::OrganizationUndo {
            operation,
            undo,
            expected,
        } => (
            ow::core_operation(&operation),
            Ok(Op::OrganizationUndo {
                original: ow::core_operation(&undo),
                expected: expected.iter().map(|pin| record_pin(s, pin)).collect(),
            }),
        ),
        Command::ArrangementReplace {
            operation,
            arrangement,
            expected,
            document,
        } => {
            let key = ow::core_arrangement_key(&arrangement);
            let op = match (
                s.org().arrangement(&key).map(|a| a.owner),
                ow::core_groups(&document),
            ) {
                (None, _) => Err(Refusal::NotFound),
                (_, None) => Err(Refusal::InvalidArgument {
                    field: "document",
                    reason: "an entry targets a kind this server does not know".to_owned(),
                }),
                (Some(owner), Some(groups)) => Ok(Op::ArrangementReplace {
                    owner,
                    expected: rev(expected),
                    groups,
                }),
            };
            (ow::core_operation(&operation), op)
        }
        _ => return None,
    })
}

/// `RESOURCE_TERMINATE`'s live target as core's terminate: the handle must
/// name a local resource of this server instance.
fn terminate_op(
    s: &ServerState,
    target: &LiveTarget,
    mode: phux_protocol::wire::frame::TerminateMode,
) -> Result<Op, Refusal> {
    if target.instance != s.idspace.instance() {
        return Err(Refusal::Stale { current: None });
    }
    let id = s
        .terminal_from_wire(&target.resource)
        .ok_or(Refusal::NotFound)?;
    let key = s.registry().resource_key(id).ok_or(Refusal::NotFound)?;
    Ok(Op::ResourceTerminate {
        resource: key,
        activation: ow::core_activation_id(target.activation),
        mode: ow::core_terminate_mode(mode),
    })
}

/// An undo's expected revision as a core pin: the key names a session, a
/// resource, or an arrangement, whichever exists.
fn record_pin(s: &ServerState, pin: &RecordRevision) -> phux_core::RevisionPin {
    let record = [
        phux_core::RecordRef::Session(phux_core::SessionKey::from_bytes(pin.key)),
        phux_core::RecordRef::Resource(phux_core::ResourceKey::from_bytes(pin.key)),
        phux_core::RecordRef::Arrangement(phux_core::ArrangementKey::from_bytes(pin.key)),
    ]
    .into_iter()
    .find(|record| s.org().revision_of(*record).is_some())
    .unwrap_or_else(|| phux_core::RecordRef::Session(phux_core::SessionKey::from_bytes(pin.key)));
    phux_core::RevisionPin {
        record,
        revision: ow::core_revision(pin.revision),
    }
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

fn creation_still_in_origin(
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

fn created_target(
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

fn creation_activation(
    s: &ServerState,
    created: &phux_core::Created,
    visible: bool,
) -> Option<phux_protocol::wire::frame::ActivationInfo> {
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

fn validate_placed_launch(launch: &phux_protocol::wire::frame::LaunchSpec) -> Result<(), String> {
    phux_protocol::api::validate_terminal_launch(launch)?;
    if let Some(cwd) = &launch.cwd {
        let cwd = std::path::Path::new(cwd);
        if !cwd.is_absolute()
            || !cwd.is_dir()
            || rustix::fs::access(cwd, rustix::fs::Access::EXEC_OK).is_err()
        {
            return Err("cwd must be an absolute enterable directory on the executing host".into());
        }
    }
    Ok(())
}

const fn core_placement(
    placement: &phux_protocol::wire::frame::CreatePlacement,
) -> phux_core::CreatePlacement {
    use phux_protocol::wire::frame::{CreatePlacement, SplitDirection};
    match placement {
        CreatePlacement::NewGroup => phux_core::CreatePlacement::NewGroup,
        CreatePlacement::Beside {
            entry,
            resource,
            instance,
            activation,
            dir,
            ratio,
        } => phux_core::CreatePlacement::Beside {
            entry: *entry,
            resource: ow::core_resource_key(resource),
            instance: phux_core::InstanceId::from_bytes(*instance.as_bytes()),
            activation: ow::core_activation_id(*activation),
            dir: match dir {
                SplitDirection::Horizontal => phux_core::SplitDir::Horizontal,
                SplitDirection::Vertical => phux_core::SplitDir::Vertical,
            },
            ratio: *ratio,
        },
    }
}

const fn record_key(record: phux_core::RecordRef) -> [u8; 16] {
    match record {
        phux_core::RecordRef::Session(k) => *k.as_bytes(),
        phux_core::RecordRef::Resource(k) => *k.as_bytes(),
        phux_core::RecordRef::Arrangement(k) => *k.as_bytes(),
    }
}

const fn delta_revision(delta: &phux_core::Delta) -> Option<RecordRevision> {
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
