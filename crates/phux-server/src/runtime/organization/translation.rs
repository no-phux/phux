//! Translation: extracted from the shared module.
use super::{Command, LiveTarget, Op, RecordRevision, Refusal, ResourceKind, ServerState, ow};

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
pub(super) fn terminate_op(
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
pub(super) fn record_pin(s: &ServerState, pin: &RecordRevision) -> phux_core::RevisionPin {
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

pub(super) fn validate_placed_launch(
    launch: &phux_protocol::wire::frame::LaunchSpec,
) -> Result<(), String> {
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

pub(super) const fn core_placement(
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
