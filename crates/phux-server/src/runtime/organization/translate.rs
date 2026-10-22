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
        } => {
            let kind = crate::resource::core_kind(kind);
            let op = kind
                .ok_or_else(|| Refusal::Unsupported {
                    capability: "this resource kind".to_owned(),
                })
                .map(|kind| Op::ResourceCreate {
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
    let target = created.resource.and_then(|key| {
        let id = s.registry().resource_id(&key)?;
        let activation = s.registry().record(id)?.activation.as_ref()?.id;
        Some(LiveTarget {
            resource: s.intern_terminal_wire(id),
            instance: s.idspace.instance(),
            activation: ow::activation_id(activation),
        })
    });
    let committed = Committed::new(
        operation,
        ow::revision(revision.unwrap_or(phux_core::Revision::ZERO)),
    )
    .with_session(created.session.and_then(ow::session_key))
    .with_resource(created.resource.and_then(ow::resource_key))
    .with_target(target)
    .with_revisions(revisions)
    .with_prior_awaiting(prior_awaiting);
    CommandResult::OkWith(CommandValue::Committed(Box::new(committed)))
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
