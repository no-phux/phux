//! Organization commands (ADR-0161, `docs/spec/L1.md` §5.7): every
//! mutating one commits through [`ServerState::commit_command_from`] and
//! answers `Ok` with a [`Committed`] receipt, `Accepted` when execution
//! effects follow as events, or `Refused` with a typed reason. Reads
//! (`GET_ORGANIZATION`, `ARRANGEMENT_GET`) are answered from the records.
//!
//! Only these commands ever receive `Refused` or `Accepted` (R08).
//!
//! Effects run after the commit, off its lock: a launch starts the
//! Terminal's engine and reports its activation; a termination ends a live
//! engine through the same teardown as a legacy kill (graceful: hangup, the
//! grace, then kill; forced: the process group is killed at once), or
//! signals a lost process group after rechecking its identity. Both report
//! the real outcome as `ActivationChanged`.

use std::collections::HashSet;
use std::time::Duration;

use phux_core::{Effect, Op, Refusal, Reply, ResourceKind};
use phux_protocol::ids::{ResourceId as WireResourceId, ServerInstance};
use phux_protocol::kinds::Verb;
use phux_protocol::wire::frame::{
    ActivationInfo, ArrangementValue, Command, CommandResult, CommandValue, Committed, LiveTarget,
    OrgFilter, OrganizationPage, RecordRevision, ResourceRecord, SessionRecord,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::org_wire as ow;
use crate::state::{ClientId, ServerState, SharedState};

/// Records per `GET_ORGANIZATION` page when the caller asks for the
/// default.
pub(super) const DEFAULT_PAGE_SIZE: u32 = 100;

/// Most records per page.
pub(super) const MAX_PAGE_SIZE: u32 = 1000;

/// Recognize only the reserved structured recipe, never a command/binary name.
/// Imported dormant records need no live wire handle to launch natively.
pub(super) fn native_relaunch_spec(
    s: &ServerState,
    resource: &phux_protocol::ids::ResourceKey,
    expected: phux_protocol::ids::Revision,
) -> Result<Option<phux_protocol::native_agent::SpawnNativeAgent>, CommandResult> {
    use phux_protocol::native_agent::{
        MAX_NATIVE_PAYLOAD_BYTES, NATIVE_HOST_SPEC_ENV, SpawnNativeAgent,
    };
    let Some(id) = s.registry().resource_id(&ow::core_resource_key(resource)) else {
        return Ok(None);
    };
    let Some(record) = s.registry().record(id) else {
        return Ok(None);
    };
    let Some(phux_core::LaunchSpec::Terminal(launch)) = &record.launch else {
        return Ok(None);
    };
    let mut markers = launch
        .env
        .iter()
        .filter(|(key, _)| key == NATIVE_HOST_SPEC_ENV);
    let Some((_, encoded)) = markers.next() else {
        return Ok(None);
    };
    let invalid = |reason: String| {
        CommandResult::Refused(ow::refusal(&Refusal::InvalidArgument {
            field: "native_recipe",
            reason,
        }))
    };
    if markers.next().is_some() || encoded.len() > MAX_NATIVE_PAYLOAD_BYTES {
        return Err(invalid(
            "duplicate or oversized native launch recipe".into(),
        ));
    }
    if record.revision != ow::core_revision(expected) {
        return Err(CommandResult::Refused(ow::refusal(&Refusal::Conflict {
            current: record.revision,
        })));
    }
    let mut spec: SpawnNativeAgent =
        serde_json::from_str(encoded).map_err(|error| invalid(error.to_string()))?;
    if spec.guardian.is_some()
        || spec.guardian_key.is_some()
        || spec.guardian_expected.is_some()
        || spec.env.keys().any(|key| {
            key.starts_with("PHUX_NATIVE_")
                || matches!(key.as_str(), "PHUX_SOCKET" | "PHUX_TERMINAL_ID")
        })
    {
        return Err(invalid(
            "saved native recipe contains private bootstrap or stale destination data".into(),
        ));
    }
    let session = s
        .registry()
        .session_of(id)
        .and_then(|session| s.registry().session(session))
        .ok_or_else(|| invalid("native guardian has no organization session".into()))?;
    spec.session.clone_from(&session.name);
    spec.guardian_key = Some(*resource);
    spec.guardian_expected = Some(expected);
    Ok(Some(spec))
}

/// How long a lost process group has to end after its termination signal
/// before it is killed.
const ORPHAN_GRACE: Duration = Duration::from_millis(500);

/// Answer one organization command.
pub(super) fn handle(
    state: &SharedState,
    client_id: ClientId,
    command: Command,
    root_token: &CancellationToken,
) -> CommandResult {
    match command {
        Command::GetOrganization {
            filter,
            page_size,
            cursor,
        } => state
            .with_mut(|s| organization_page(s, client_id, filter, page_size, cursor.as_deref())),
        Command::ArrangementGet { arrangement } => state.with(|s| arrangement_get(s, &arrangement)),
        Command::SessionKill { operation, session } => {
            session_kill(state, client_id, &operation, &session, root_token)
        }
        other => {
            let Some((operation, op)) = state.with(|s| translate(s, other)) else {
                return CommandResult::Error {
                    code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                    message: "not an organization command".to_owned(),
                };
            };
            let op = match op {
                Ok(op) => op,
                Err(refusal) => return CommandResult::Refused(ow::refusal(&refusal)),
            };
            mutate(state, client_id, operation, op, root_token)
        }
    }
}

/// Commit a mutating command, run its effects, and answer.
fn mutate(
    state: &SharedState,
    client_id: ClientId,
    operation: phux_core::OperationId,
    op: Op,
    root_token: &CancellationToken,
) -> CommandResult {
    let unsupported_kind = matches!(
        &op,
        Op::ResourceCreate { resource, .. } if resource.kind != ResourceKind::Terminal
    );
    if unsupported_kind {
        return CommandResult::Refused(ow::refusal(&Refusal::Unsupported {
            capability: "creating this kind through RESOURCE_CREATE".to_owned(),
        }));
    }
    let relaunch_held = state.with(|s| relaunch_engine_held(s, &op));
    if let Some(refusal) = relaunch_held {
        return CommandResult::Refused(ow::refusal(&refusal));
    }
    // R17: a replaced session arrangement is published under the TUI layout
    // key for older readers.
    let republish = match &op {
        Op::ArrangementReplace {
            owner: phux_core::ArrangementOwner::Session(key),
            ..
        } => Some(*key),
        _ => None,
    };
    let committed = state.with_mut(|s| {
        let actor = s.actor_for(client_id);
        let command = phux_core::Command { operation, op };
        let commit = s.commit_command_from(&command, &actor, Some(client_id))?;
        if let Some(session) = republish.and_then(|key| s.registry().session_id(&key))
            && !commit.applied.replayed
        {
            s.publish_arrangement_as_layout(session);
        }
        let detached: Vec<_> = s
            .take_killed_sessions()
            .into_iter()
            .flat_map(|session| s.attached_clients_in_session(session))
            .collect();
        let answer = answer(s, &commit.applied);
        Ok::<_, Refusal>((answer, commit.applied.effects, detached))
    });
    let (answer, effects, detached) = match committed {
        Ok(committed) => committed,
        Err(refusal) => {
            debug!(?client_id, %refusal, "organization command refused");
            return CommandResult::Refused(ow::refusal(&refusal));
        }
    };
    super::client::detach_clients_of_killed_session(state, detached);
    for effect in effects {
        run_effect(state, effect, root_token);
    }
    answer
}

/// `SESSION_KILL`: the kill is written to the organization store first. Only
/// a store that accepts it closes every member with an engine here, through
/// the legacy kill's teardown (hangup, the grace, then kill), and only then
/// does memory mirror the transition. A store refusal therefore leaves
/// engines, announcements, and records untouched (`STORE_FAILURE`: nothing
/// changed, and the same operation may be retried). One lock holds the
/// sequence, so nothing joins the session between the commit and the closes.
/// The receipt counts the members the kill found; a replay answers without
/// the count and without closing again.
#[allow(
    clippy::too_many_lines,
    reason = "persist the kill, close engines, then mirror, in one lock"
)]
fn session_kill(
    state: &SharedState,
    client_id: ClientId,
    operation: &phux_protocol::ids::OperationId,
    session: &phux_protocol::ids::SessionKey,
    root_token: &CancellationToken,
) -> CommandResult {
    let operation = ow::core_operation(operation);
    let key = ow::core_session_key(session);
    let (closed, committed) = state.with_mut(|s| {
        let mut members = None;
        let mut live = Vec::new();
        let mut member_keys = Vec::new();
        if s.org().receipt(&operation).is_none() {
            let Some(record) = s.org().session(&key) else {
                return (None, Err(Refusal::NotFound));
            };
            if key == s.org().authority().intake_session() {
                return (
                    None,
                    Err(Refusal::InvalidArgument {
                        field: "session",
                        reason: "the designated intake session cannot be deleted".to_owned(),
                    }),
                );
            }
            members = Some(u32::try_from(record.members.len()).unwrap_or(u32::MAX));
            member_keys = record.members.clone();
            live = record
                .members
                .iter()
                .filter_map(|member| s.registry().resource_id(member))
                .filter(|id| s.resource_handle(*id).is_some())
                .collect();
        }
        // Engines this kill will close. Their `pane_closed` records are
        // journaled before the kill's own events, so the stored journal
        // position counts them. A `Terminate` for one of them would signal
        // the group again after the teardown, so those effects are dropped.
        let closing = s.resources_closing_with(&live);
        let leading = u64::try_from(closing.len()).unwrap_or(u64::MAX);
        let closing_keys: HashSet<_> = closing
            .iter()
            .filter_map(|id| s.registry().resource_key(*id))
            .collect();
        let actor = s.actor_for(client_id);
        let command = phux_core::Command {
            operation,
            op: Op::SessionKill { session: key },
        };
        let persisted = match s.persist_command_from(&command, &actor, leading) {
            Ok(persisted) => persisted,
            Err(refusal) => return (None, Err(refusal)),
        };
        if !persisted.stored() {
            let commit = s.mirror_persisted(persisted, &actor, Some(client_id));
            let answer = answer(s, &commit.applied);
            return (None, Ok((answer, commit.applied.effects)));
        }
        // The store has the kill and memory still names the engines. Do not
        // let the close write the members again: that transition would put
        // back rows this kill already deleted.
        s.suppress_retirement(&member_keys);
        let mut closed = super::committed_close::commit_close(
            s,
            &live,
            phux_protocol::wire::frame::CloseReason::Killed,
            crate::state::CloseAttribution {
                actor: Some(client_id),
                operation_id: None,
            },
        );
        s.clear_retirement_suppression();
        let commit = s.mirror_persisted(persisted, &actor, Some(client_id));
        let detached: Vec<_> = s
            .take_killed_sessions()
            .into_iter()
            .flat_map(|session| s.attached_clients_in_session(session))
            .collect();
        closed.extend_killed_clients(detached);
        let answer = match (answer(s, &commit.applied), members) {
            (CommandResult::OkWith(CommandValue::Committed(receipt)), Some(_)) => {
                CommandResult::OkWith(CommandValue::Committed(Box::new(
                    receipt.with_members(members),
                )))
            }
            (answer, _) => answer,
        };
        let mut effects = commit.applied.effects;
        effects.retain(|effect| {
            !matches!(
                effect,
                Effect::Terminate { resource, .. } if closing_keys.contains(resource)
            )
        });
        (Some(closed), Ok((answer, effects)))
    });
    if let Some(closed) = closed {
        super::committed_close::announce(state, closed);
    }
    match committed {
        Ok((answer, effects)) => {
            for effect in effects {
                run_effect(state, effect, root_token);
            }
            answer
        }
        Err(refusal) => {
            debug!(?client_id, %refusal, "session kill refused");
            CommandResult::Refused(ow::refusal(&refusal))
        }
    }
}

/// A relaunch of a record whose exited engine is still retained is refused:
/// the engine must be released first (it holds the record's live handle).
fn relaunch_engine_held(s: &ServerState, op: &Op) -> Option<Refusal> {
    let Op::ResourceRelaunch { resource, .. } = op else {
        return None;
    };
    let id = s.registry().resource_id(resource)?;
    s.resource_handle(id)
        .is_some()
        .then(|| Refusal::InvalidArgument {
            field: "resource",
            reason: "its exited engine is still retained; remove the retention first".to_owned(),
        })
}

/// The wire command as a core operation, or the refusal for an argument
/// core cannot express. `None` for a read or a non-organization command.
#[allow(clippy::too_many_lines, reason = "one arm per organization command")]
fn translate(
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
fn answer(s: &mut ServerState, applied: &phux_core::Applied) -> CommandResult {
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

/// Run one effect a commit asked for.
fn run_effect(state: &SharedState, effect: Effect, root_token: &CancellationToken) {
    match effect {
        Effect::Launch { resource, spec, .. } => launch(state, resource, &spec, root_token),
        Effect::Terminate {
            resource,
            mode,
            proc,
            ..
        } => terminate(state, resource, mode, proc),
    }
}

/// Start a committed launch intent's engine. A failure ends the activation
/// as failed through the reap.
fn launch(
    state: &SharedState,
    resource: phux_core::ResourceKey,
    spec: &phux_core::LaunchSpec,
    root_token: &CancellationToken,
) {
    let Some((terminal, with_pty, scrollback, cmd)) = state.with_mut(|s| {
        let id = s.registry().resource_id(&resource)?;
        let session = s
            .registry()
            .session_of(id)
            .and_then(|sid| s.registry().session(sid))
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let with_pty = s.attach_create_seeds_pty();
        let cmd = with_pty.then(|| build_command(s, spec, &session));
        if let phux_core::LaunchSpec::Terminal(t) = spec {
            let wire = s.intern_terminal_wire(id);
            super::commands::set_agent_session(s, wire, t.restore.clone());
        }
        Some((id, with_pty, s.scrollback_limits(), cmd))
    }) else {
        return;
    };
    let cmd = cmd.map(|mut cmd| {
        state.with_mut(|s| {
            let _ = super::commands::register_pty_command(s, terminal, &mut cmd);
        });
        cmd
    });
    if let Err(error) = super::commands::launch_pane_actor(
        state,
        terminal,
        cmd,
        super::commands::DEFAULT_SPAWN_DIMS,
        scrollback,
        root_token,
        None,
        super::commands::SpawnAttribution::default(),
    ) {
        warn!(?terminal, with_pty, %error, "launch effect failed");
    }
}

/// The command a Terminal launch spec runs: its argv, else the default
/// shell, in its cwd, with its environment and the server's `TERM`.
fn build_command(
    s: &ServerState,
    spec: &phux_core::LaunchSpec,
    session: &str,
) -> portable_pty::CommandBuilder {
    let launch = match spec {
        phux_core::LaunchSpec::Terminal(t) => t.clone(),
        _ => phux_core::TerminalLaunch::default(),
    };
    let mut cmd = super::commands::argv_command((!launch.argv.is_empty()).then_some(launch.argv))
        .unwrap_or_else(|| {
            crate::terminal_actor::default_shell_command(s.shell(), s.login_shell())
        });
    let cwd = launch.cwd.as_ref().and_then(|p| p.to_str());
    crate::terminal_actor::apply_spawn_cwd(&mut cmd, cwd, session);
    for (key, value) in launch.env {
        cmd.env(key, value);
    }
    crate::terminal_actor::apply_term(&mut cmd, s.term());
    cmd
}

/// End an activation. A live engine closes through the legacy kill's
/// teardown, its record left to its exit policy; `Force` kills the process
/// group first. A lost process group (no engine here) is signalled after
/// its identity is rechecked, and its end committed once it is gone.
fn terminate(
    state: &SharedState,
    resource: phux_core::ResourceKey,
    mode: phux_core::TerminateMode,
    proc: Option<phux_core::ProcIdentity>,
) {
    let live = state.with_mut(|s| {
        let id = s.registry().resource_id(&resource)?;
        s.resource_handle(id)?;
        if mode == phux_core::TerminateMode::Force {
            kill_group(proc, nix::sys::signal::Signal::SIGKILL);
        }
        s.mark_terminating(id);
        let _ = s.mark_and_cancel(
            &[id],
            phux_protocol::wire::frame::CloseReason::Killed,
            crate::state::CloseAttribution::default(),
        );
        Some(())
    });
    if live.is_some() {
        return;
    }
    let Some(proc) = proc else {
        return;
    };
    let state = state.clone();
    tokio::task::spawn_local(async move {
        let first = match mode {
            phux_core::TerminateMode::Graceful => nix::sys::signal::Signal::SIGHUP,
            phux_core::TerminateMode::Force => nix::sys::signal::Signal::SIGKILL,
        };
        let mut sent = first;
        kill_group(Some(proc), first);
        if mode == phux_core::TerminateMode::Graceful {
            tokio::time::sleep(ORPHAN_GRACE).await;
            if still_running(proc) {
                sent = nix::sys::signal::Signal::SIGKILL;
                kill_group(Some(proc), sent);
            }
        }
        for _ in 0..40 {
            if !still_running(proc) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if still_running(proc) {
            warn!(pid = proc.pid, "lost process group survived termination");
            return;
        }
        state.with_mut(|s| {
            let Some(id) = s.registry().resource_id(&resource) else {
                return;
            };
            s.activation_ended(id, phux_core::ExitOutcome::signaled(sent as i32), false);
        });
    });
}

/// Whether the recorded process still runs as itself.
fn still_running(proc: phux_core::ProcIdentity) -> bool {
    let now = crate::reconcile::observe(proc.pid);
    now.alive && now.start_raw == Some(proc.start_raw)
}

/// Signal the recorded process group, only while its leader is still the
/// recorded process (a reused pid is never signalled).
fn kill_group(proc: Option<phux_core::ProcIdentity>, signal: nix::sys::signal::Signal) {
    let Some(proc) = proc.filter(|p| still_running(*p)) else {
        return;
    };
    if let Err(error) = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(proc.pgid), signal) {
        debug!(pgid = proc.pgid, %error, "process group signal failed");
    }
}

/// `ARRANGEMENT_GET`.
fn arrangement_get(s: &ServerState, key: &phux_protocol::ids::ArrangementKey) -> CommandResult {
    let Some(record) = s.org().arrangement(&ow::core_arrangement_key(key)) else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    let Some(owner) = ow::owner(record.owner) else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    CommandResult::OkWith(CommandValue::Arrangement(Box::new(ArrangementValue::new(
        *key,
        owner,
        ow::revision(record.revision),
        ow::document(&record.groups),
    ))))
}

#[allow(clippy::too_many_lines, reason = "select, filter, count, then page")]
/// One `GET_ORGANIZATION` page: the sessions the filter selects and the
/// caller may inventory, in session-key order, each followed by its members
/// in resource-key order. `cursor` is the key of the last record the previous
/// page emitted; this page resumes strictly after that key. Totals count
/// every matching record the caller may inventory; the Intake designation
/// needs Global.
fn organization_page(
    s: &mut ServerState,
    client: ClientId,
    filter: OrgFilter,
    page_size: u32,
    cursor: Option<&[u8]>,
) -> CommandResult {
    let intake = s.org().authority().intake_session();
    let selected: Vec<phux_core::SessionKey> = match filter {
        OrgFilter::All => s.org().sessions().map(|r| r.key).collect(),
        OrgFilter::Session(key) => vec![ow::core_session_key(&key)],
        OrgFilter::Intake => s
            .org()
            .sessions()
            .filter(|r| r.key == intake || r.awaiting_organization)
            .map(|r| r.key)
            .collect(),
        _ => {
            return CommandResult::Refused(ow::refusal(&Refusal::Unsupported {
                capability: "workspace".to_owned(),
            }));
        }
    };
    // Every selected session gets a live handle, so the page and the
    // caller's grant name it the same way.
    let ids: Vec<(phux_core::SessionKey, phux_core::ids::SessionId)> = selected
        .into_iter()
        .filter_map(|key| Some((key, s.registry().session_id(&key)?)))
        .collect();
    for (_, id) in &ids {
        let _ = s.idspace.intern_session(*id);
    }
    let visible: Vec<(phux_core::SessionKey, phux_core::ids::SessionId)> = ids
        .into_iter()
        .filter(|(_, id)| crate::policy::filter::admits_session(s, client, Verb::Inventory, *id))
        .collect();
    // A page bounds records, not sessions: each session is followed by its
    // admitted members, so a session with thousands of members spans pages.
    // The cursor names the last emitted record. The next page starts strictly
    // after that key, which stays put when a concurrent insert or delete
    // shifts the sequence.
    let size = match page_size {
        0 => DEFAULT_PAGE_SIZE,
        n => n.min(MAX_PAGE_SIZE),
    } as usize;
    let total_sessions = visible.len();
    let mut total_intake = 0;
    let mut entries = Vec::with_capacity(total_sessions);
    for (key, id) in &visible {
        if s.org()
            .session(key)
            .is_some_and(|r| r.awaiting_organization)
        {
            total_intake += 1;
        }
        entries.push(PageEntry::Session(*key, *id));
        let mut members: Vec<(phux_core::ResourceKey, phux_core::ids::ResourceId)> = s
            .registry()
            .members(*id)
            .into_iter()
            .filter(|member| {
                crate::policy::filter::admits_resource(s, client, Verb::Inventory, *member)
            })
            .filter_map(|member| {
                s.registry()
                    .record(member)
                    .map(|record| (record.key, member))
            })
            .collect();
        members.sort_by_key(|(resource, _)| *resource);
        entries.extend(
            members
                .into_iter()
                .map(|(resource, member)| PageEntry::Resource(*key, resource, member)),
        );
    }
    let total_resources = entries.len() - total_sessions;
    debug_assert!(entries.is_sorted_by_key(|entry| entry_pos(*entry)));
    let after = cursor.and_then(page_cursor);
    let start = after.map_or(0, |bound| {
        entries.partition_point(|entry| entry_pos(*entry) <= bound)
    });
    let page: Vec<PageEntry> = entries.iter().skip(start).take(size).copied().collect();
    let next_cursor = if start + page.len() < entries.len() {
        page.last().map(|entry| cursor_bytes(entry_pos(*entry)))
    } else {
        None
    };
    let mut sessions = Vec::new();
    let mut resources = Vec::new();
    for entry in page {
        let (key, id) = match entry {
            PageEntry::Session(key, id) => (key, id),
            PageEntry::Resource(_, _, member) => {
                if let Some(record) = resource_record(s, member) {
                    resources.push(record);
                }
                continue;
            }
        };
        let Some(record) = s.org().session(&key).cloned() else {
            continue;
        };
        let (Some(wire_key), live) = (ow::session_key(key), s.idspace.session_wire(id)) else {
            continue;
        };
        sessions.push(
            SessionRecord::new(wire_key, record.name.clone(), ow::revision(record.revision))
                .with_lifetime(ow::lifetime(record.lifetime))
                .with_awaiting_organization(record.awaiting_organization)
                .with_arrangement(record.arrangement.and_then(ow::arrangement_key))
                .with_attrs(ow::attrs(&record.attrs))
                .with_members(u32::try_from(record.members.len()).unwrap_or(u32::MAX))
                .with_live(live),
        );
    }
    let authority = phux_protocol::ids::AuthorityId::new(*s.org().authority().id.as_bytes());
    let Some(authority) = authority else {
        return CommandResult::Refused(ow::refusal(&Refusal::NotFound));
    };
    let head = s.journal_head();
    let page = OrganizationPage::new(authority, phux_protocol::ids::Revision::new(head), head)
        .with_intake(
            crate::policy::filter::admits_global(s, client, Verb::Inventory)
                .then(|| ow::session_key(intake))
                .flatten(),
        )
        .with_sessions(sessions)
        .with_resources(resources)
        .with_totals(
            u32::try_from(total_sessions).unwrap_or(u32::MAX),
            u32::try_from(total_resources).unwrap_or(u32::MAX),
            u32::try_from(total_intake).unwrap_or(u32::MAX),
        )
        .with_next_cursor(next_cursor);
    CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page)))
}

/// One record of a `GET_ORGANIZATION` page, in key order: each session, then
/// its members by resource key.
#[derive(Debug, Clone, Copy)]
enum PageEntry {
    Session(phux_core::SessionKey, phux_core::ids::SessionId),
    Resource(
        phux_core::SessionKey,
        phux_core::ResourceKey,
        phux_core::ids::ResourceId,
    ),
}

/// Where a page record sits in key order. A session sorts before its members.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PagePos {
    session: phux_core::SessionKey,
    resource: Option<phux_core::ResourceKey>,
}

const fn entry_pos(entry: PageEntry) -> PagePos {
    match entry {
        PageEntry::Session(session, _) => PagePos {
            session,
            resource: None,
        },
        PageEntry::Resource(session, resource, _) => PagePos {
            session,
            resource: Some(resource),
        },
    }
}

/// The opaque cursor: the session key, then the resource key when the last
/// record was a member. Any other shape is not a position.
fn page_cursor(bytes: &[u8]) -> Option<PagePos> {
    if bytes.len() != 16 && bytes.len() != 32 {
        return None;
    }
    let session = phux_core::SessionKey::from_bytes(bytes[..16].try_into().ok()?);
    let resource = if bytes.len() == 32 {
        Some(phux_core::ResourceKey::from_bytes(
            bytes[16..32].try_into().ok()?,
        ))
    } else {
        None
    };
    Some(PagePos { session, resource })
}

fn cursor_bytes(pos: PagePos) -> Vec<u8> {
    let mut bytes = pos.session.as_bytes().to_vec();
    if let Some(resource) = pos.resource {
        bytes.extend_from_slice(resource.as_bytes());
    }
    bytes
}

/// A resource record on the wire, with its live handle while its engine
/// runs here.
fn resource_record(s: &mut ServerState, id: phux_core::ids::ResourceId) -> Option<ResourceRecord> {
    let record = s.registry().record(id)?.clone();
    let live: Option<WireResourceId> = s
        .resource_handle(id)
        .is_some()
        .then(|| s.intern_terminal_wire(id));
    let activation = record.activation.as_ref().map(|a| {
        ActivationInfo::new(
            ow::activation_id(a.id),
            ServerInstance::new(*a.instance.as_bytes()),
            ow::exec_state(a.state),
        )
        .with_outcome(a.outcome.map(ow::outcome))
    });
    Some(
        ResourceRecord::new(
            ow::resource_key(record.key)?,
            ow::session_key(record.session)?,
            ow::revision(record.revision),
        )
        .with_name(record.name.clone())
        .with_kind(crate::resource::wire_kind(record.kind))
        .with_on_exit(ow::exit_policy(record.on_exit))
        .with_activation(activation)
        .with_live(live),
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use phux_protocol::ids::{OperationId, Revision};
    use phux_protocol::wire::frame::{
        ArrangementDoc, ExecState, LaunchSpec, Refusal as WireRefusal, SessionLifetime,
        TerminateMode,
    };

    use super::*;

    const CLIENT: ClientId = ClientId(3);

    fn op(n: u8) -> OperationId {
        OperationId::new([n; 16]).unwrap()
    }

    fn committed(result: CommandResult) -> Committed {
        match result {
            CommandResult::OkWith(CommandValue::Committed(c)) => *c,
            other => panic!("expected a committed receipt, got {other:?}"),
        }
    }

    fn page(state: &SharedState, filter: OrgFilter) -> OrganizationPage {
        let result = handle(
            state,
            CLIENT,
            Command::GetOrganization {
                filter,
                page_size: 0,
                cursor: None,
            },
            &CancellationToken::new(),
        );
        match result {
            CommandResult::OkWith(CommandValue::OrganizationPage(p)) => *p,
            other => panic!("expected a page, got {other:?}"),
        }
    }

    fn run<F: std::future::Future<Output = ()>>(f: F) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&rt, f);
    }

    /// `SESSION_KILL` ends a running member and removes a dormant one in
    /// one step, deletes the session, counts what it ended, and replays.
    #[test]
    fn session_kill_ends_every_member_and_deletes_the_session() {
        use phux_protocol::wire::frame::LaunchMode;
        run(async {
            let state = SharedState::new();
            let root = CancellationToken::new();
            let receipt = committed(handle(
                &state,
                CLIENT,
                Command::SessionCreate {
                    operation: op(1),
                    name: "work".to_owned(),
                    lifetime: SessionLifetime::Persistent,
                    attrs: Vec::new(),
                    launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
                },
                &root,
            ));
            let session = receipt.session.expect("created session");
            committed(handle(
                &state,
                CLIENT,
                Command::ResourceCreate {
                    operation: op(2),
                    session,
                    kind: phux_protocol::ids::ResourceKind::Terminal,
                    launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
                    name: None,
                    on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
                    launch_mode: LaunchMode::Deferred,
                },
                &root,
            ));
            let kill = Command::SessionKill {
                operation: op(3),
                session,
            };
            let killed = committed(handle(&state, CLIENT, kill.clone(), &root));
            assert_eq!(killed.members, Some(2));
            assert!(
                page(&state, OrgFilter::Session(session))
                    .sessions
                    .is_empty()
            );
            assert!(state.with(|s| s.session_by_name("work").is_none()));
            let replayed = committed(handle(&state, CLIENT, kill, &root));
            assert_eq!(replayed.members, None, "a replay answers without the count");

            let missing = Command::SessionKill {
                operation: op(4),
                session,
            };
            assert_eq!(
                handle(&state, CLIENT, missing, &root),
                CommandResult::Refused(WireRefusal::NotFound)
            );
            let inbox = state.with(|s| s.org().authority().inbox_session);
            let inbox = Command::SessionKill {
                operation: op(5),
                session: ow::session_key(inbox).expect("non-zero key"),
            };
            assert!(matches!(
                handle(&state, CLIENT, inbox, &root),
                CommandResult::Refused(WireRefusal::InvalidArgument { .. })
            ));
        });
    }

    /// A store that refuses the kill must leave the live engine and the
    /// records in place: `STORE_FAILURE` means nothing changed, and the same
    /// operation may be retried.
    #[test]
    fn session_kill_store_failure_closes_nothing() {
        use std::sync::atomic::Ordering;

        use crate::org_store::MemOrgStore;

        run(async {
            let state = SharedState::new();
            let root = CancellationToken::new();
            let receipt = committed(handle(
                &state,
                CLIENT,
                Command::SessionCreate {
                    operation: op(1),
                    name: "work".to_owned(),
                    lifetime: SessionLifetime::Persistent,
                    attrs: Vec::new(),
                    launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
                },
                &root,
            ));
            let session = receipt.session.expect("created session");
            let wire = receipt.target.expect("a launched engine").resource;
            let core = state
                .with(|s| s.terminal_from_wire(&wire))
                .expect("live core id");
            assert!(
                state.with(|s| s.resource_handle(core).is_some()),
                "the launch registered an engine"
            );
            let head = state.with(ServerState::journal_head);

            let store = MemOrgStore::default();
            let fail = store.fail_commits();
            state.with_mut(|s| s.set_org_store(Box::new(store)));
            fail.store(true, Ordering::Relaxed);

            let kill = Command::SessionKill {
                operation: op(2),
                session,
            };
            assert!(matches!(
                handle(&state, CLIENT, kill.clone(), &root),
                CommandResult::Refused(WireRefusal::StoreFailure { .. })
            ));
            assert!(
                state.with(|s| s.resource_handle(core).is_some()),
                "a refused kill does not close the engine"
            );
            assert!(state.with(|s| s.registry().record(core).is_some()));
            assert_eq!(
                page(&state, OrgFilter::Session(session)).sessions.len(),
                1,
                "the session is still listed"
            );
            assert_eq!(
                state.with(ServerState::journal_head),
                head,
                "a refused kill journals nothing"
            );

            fail.store(false, Ordering::Relaxed);
            let killed = committed(handle(&state, CLIENT, kill, &root));
            assert_eq!(killed.members, Some(1));
            assert!(
                page(&state, OrgFilter::Session(session))
                    .sessions
                    .is_empty()
            );
            assert!(
                state.with(|s| s.resource_handle(core).is_none()),
                "a committed kill closes the engine"
            );
        });
    }

    #[test]
    fn create_rename_and_page_round_trip_with_typed_refusals() {
        run(async {
            let state = SharedState::new();
            let root = CancellationToken::new();
            let create = |n, name: &str| Command::SessionCreate {
                operation: op(n),
                name: name.to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: vec![("project".to_owned(), "phux".to_owned())],
                launch: Some(Box::new(LaunchSpec::new(Vec::new()))),
            };
            let receipt = committed(handle(&state, CLIENT, create(1, "work"), &root));
            let session = receipt.session.expect("created session");
            let target = receipt.target.expect("a launched resource");
            assert_eq!(receipt.revision, Revision::new(1));

            // A retry with the same id and payload replays; another payload
            // under that id is a duplicate.
            assert_eq!(
                committed(handle(&state, CLIENT, create(1, "work"), &root)).session,
                Some(session)
            );
            assert_eq!(
                handle(&state, CLIENT, create(1, "other"), &root),
                CommandResult::Refused(WireRefusal::DuplicateOperation)
            );
            assert_eq!(
                handle(&state, CLIENT, create(2, "work"), &root),
                CommandResult::Refused(WireRefusal::NameTaken)
            );

            let listing = page(&state, OrgFilter::Session(session));
            assert_eq!(listing.sessions.len(), 1);
            let record = listing.sessions[0].clone();
            assert_eq!(record.name, "work");
            assert_eq!(record.members, 1);
            assert_eq!(
                record.attrs,
                vec![("project".to_owned(), "phux".to_owned())]
            );
            assert!(
                record.arrangement.is_some(),
                "every session has an arrangement"
            );
            assert_eq!(listing.resources.len(), 1);
            let activation = listing.resources[0].activation.expect("activation");
            assert_eq!(activation.state, ExecState::Running, "the launch ran");
            assert_eq!(listing.resources[0].live, Some(target.resource));
            assert!(
                listing.inbox.is_some(),
                "the owner sees the Inbox designation"
            );

            let rename = Command::SessionRename {
                operation: op(4),
                session,
                name: "renamed".to_owned(),
            };
            let renamed = committed(handle(&state, CLIENT, rename, &root));
            assert_eq!(renamed.revision.get(), record.revision.get() + 1);
            assert!(state.with(|s| s.session_by_name("renamed").is_some()));

            let link = |n: u8, project: Option<&str>| Command::SessionSetProject {
                operation: op(n),
                session,
                project: project.map(str::to_owned),
            };
            let linked = committed(handle(&state, CLIENT, link(5, Some("other")), &root));
            assert_eq!(linked.revision.get(), renamed.revision.get() + 1);
            let attrs = page(&state, OrgFilter::Session(session)).sessions[0]
                .attrs
                .clone();
            assert_eq!(attrs, vec![("project".to_owned(), "other".to_owned())]);
            assert!(matches!(
                handle(&state, CLIENT, link(6, Some(" ")), &root),
                CommandResult::Refused(WireRefusal::InvalidArgument { .. })
            ));
            let cleared = committed(handle(&state, CLIENT, link(7, None), &root));
            assert_eq!(cleared.revision.get(), linked.revision.get() + 1);
            assert!(
                page(&state, OrgFilter::Session(session)).sessions[0]
                    .attrs
                    .is_empty()
            );
        });
    }

    #[test]
    fn a_deferred_create_stays_dormant_until_relaunched_with_its_provenance() {
        use phux_protocol::wire::frame::{LaunchMode, RESOURCE_AGENT_SESSION_KEY, Scope};

        run(async {
            let state = SharedState::new();
            let root = CancellationToken::new();
            let (session, _) = state.with_mut(|s| s.seed_session("work"));
            let session = state
                .with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
            let provenance = br#"{"plugin_id":"p","integration_id":"i","native_id":"n"}"#;
            let create = Command::ResourceCreate {
                operation: op(1),
                session,
                kind: phux_protocol::ids::ResourceKind::Terminal,
                launch: Some(Box::new(
                    LaunchSpec::new(vec!["sh".to_owned()]).with_restore(Some(provenance.to_vec())),
                )),
                name: None,
                on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
                launch_mode: LaunchMode::Deferred,
            };
            let receipt = committed(handle(&state, CLIENT, create, &root));
            let resource = receipt.resource.expect("a record");
            assert!(receipt.target.is_none(), "nothing launched");
            let listing = page(&state, OrgFilter::Session(session));
            let record = listing
                .resources
                .iter()
                .find(|r| r.key == resource)
                .expect("listed")
                .clone();
            assert!(record.activation.is_none(), "dormant: no activation");
            assert!(record.live.is_none(), "dormant: no live handle");

            let relaunch = Command::ResourceRelaunch {
                operation: op(2),
                resource,
                expected: record.revision,
            };
            assert!(matches!(
                handle(&state, CLIENT, relaunch, &root),
                CommandResult::Accepted { .. }
            ));
            let wire = state.with_mut(|s| {
                let key = phux_core::ResourceKey::from_bytes(*resource.as_bytes());
                let id = s.registry().resource_id(&key).unwrap();
                assert!(
                    s.registry().record(id).unwrap().activation.is_some(),
                    "the relaunch started an activation"
                );
                s.intern_terminal_wire(id)
            });
            assert_eq!(
                state.with(|s| s
                    .metadata()
                    .get(&Scope::Resource(wire), RESOURCE_AGENT_SESSION_KEY)),
                Some(provenance.to_vec()),
                "the launch installs the stored agent-session provenance"
            );
        });
    }

    /// A `resource:` grant names the record, not an activation: it admits
    /// the relaunch of a dormant record and the live handle that relaunch
    /// starts, which no `terminal:` id could have named in advance.
    #[test]
    fn a_resource_key_grant_reaches_the_activation_a_relaunch_starts() {
        use phux_protocol::scope::TerminalScopeSet;
        use phux_protocol::wire::frame::LaunchMode;

        run(async {
            let state = SharedState::new();
            let root = CancellationToken::new();
            let (session, _) = state.with_mut(|s| s.seed_session("work"));
            let session = state
                .with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
            let create = Command::ResourceCreate {
                operation: op(1),
                session,
                kind: phux_protocol::ids::ResourceKind::Terminal,
                launch: Some(Box::new(LaunchSpec::new(vec!["sh".to_owned()]))),
                name: None,
                on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
                launch_mode: LaunchMode::Deferred,
            };
            let resource = committed(handle(&state, CLIENT, create, &root))
                .resource
                .expect("a record");
            let grant = crate::policy::ConnectionGrant::scoped(
                TerminalScopeSet::parse_all(&[&format!("*@resource:{resource}")]).unwrap(),
                Some("sha256:test".to_owned()),
            )
            .unwrap();
            let allowed = |s: &crate::state::ServerState, command: &Command| {
                crate::policy::enforce(s, CLIENT, &grant, crate::policy::Request::Command(command))
                    .is_ok()
            };
            let record = page(&state, OrgFilter::Session(session))
                .resources
                .into_iter()
                .find(|r| r.key == resource)
                .expect("listed");
            let relaunch = Command::ResourceRelaunch {
                operation: op(2),
                resource,
                expected: record.revision,
            };
            assert!(state.with(|s| allowed(s, &relaunch)));
            assert!(matches!(
                handle(&state, CLIENT, relaunch, &root),
                CommandResult::Accepted { .. }
            ));
            let (live, other) = state.with_mut(|s| {
                let key = phux_core::ResourceKey::from_bytes(*resource.as_bytes());
                let id = s.registry().resource_id(&key).unwrap();
                let (_, sibling) = s.seed_session("elsewhere");
                (s.intern_terminal_wire(id), s.intern_terminal_wire(sibling))
            });
            let screen = |terminal_id| Command::GetScreen {
                terminal_id,
                request_scrollback: None,
                cells: false,
                format: 0,
            };
            state.with(|s| {
                assert!(allowed(s, &screen(live)), "the new activation's handle");
                assert!(!allowed(s, &screen(other)), "another record's handle");
            });
        });
    }

    #[test]
    fn arrangements_read_and_replace_by_revision() {
        run(async {
            let state = SharedState::new();
            let root = CancellationToken::new();
            let (session, pane) = state.with_mut(|s| s.seed_session("work"));
            let (key, resource) = state.with(|s| {
                let record = s.registry().session(session).unwrap();
                (
                    ow::arrangement_key(record.arrangement.unwrap()).unwrap(),
                    ow::resource_key(s.registry().resource_key(pane).unwrap()).unwrap(),
                )
            });
            let get = || {
                handle(
                    &state,
                    CLIENT,
                    Command::ArrangementGet { arrangement: key },
                    &root,
                )
            };
            let CommandResult::OkWith(CommandValue::Arrangement(value)) = get() else {
                panic!("arrangement");
            };
            assert_eq!(value.revision, Revision::new(1));
            assert_eq!(
                value.document.groups.len(),
                1,
                "the seeded terminal is arranged at creation"
            );

            let document = ArrangementDoc {
                groups: vec![phux_protocol::wire::frame::ArrangementGroup {
                    id: [1; 16],
                    name: None,
                    tree: phux_protocol::wire::frame::ArrangementNode::Leaf(
                        phux_protocol::wire::frame::EntryRef {
                            entry: [2; 16],
                            target: phux_protocol::wire::frame::EntryTarget::Member(resource),
                        },
                    ),
                }],
            };
            let replace = |n, expected| Command::ArrangementReplace {
                operation: op(n),
                arrangement: key,
                expected: Revision::new(expected),
                document: document.clone(),
            };
            assert_eq!(
                committed(handle(&state, CLIENT, replace(1, 1), &root))
                    .revision
                    .get(),
                2
            );
            assert_eq!(
                handle(&state, CLIENT, replace(2, 1), &root),
                CommandResult::Refused(WireRefusal::Conflict {
                    current: Revision::new(2)
                })
            );
            let CommandResult::OkWith(CommandValue::Arrangement(value)) = get() else {
                panic!("arrangement");
            };
            assert_eq!(value.document, document);
        });
    }

    #[test]
    fn terminate_is_accepted_and_a_stale_target_refused() {
        run(async {
            let state = SharedState::new();
            let root = CancellationToken::new();
            let (_, pane) = state.with_mut(|s| s.seed_session("work"));
            let target = |activation| LiveTarget {
                resource: state.with_mut(|s| s.intern_terminal_wire(pane)),
                instance: state.with(|s| s.idspace.instance()),
                activation: phux_protocol::ids::ActivationId::new(activation),
            };
            let terminate = |n, activation| Command::ResourceTerminate {
                operation: op(n),
                target: target(activation),
                mode: TerminateMode::Graceful,
            };
            assert!(matches!(
                handle(&state, CLIENT, terminate(1, 7), &root),
                CommandResult::Refused(WireRefusal::Stale { .. })
            ));
            assert_eq!(
                handle(&state, CLIENT, terminate(2, 1), &root),
                CommandResult::Accepted { operation: op(2) }
            );
            let record = state.with(|s| s.registry().record(pane).cloned()).unwrap();
            assert_eq!(
                record.activation.unwrap().state,
                phux_core::ExecState::Stopping,
                "the outcome follows as ActivationChanged"
            );
        });
    }

    #[test]
    fn organization_events_are_journaled_with_their_actor() {
        run(async {
            let state = SharedState::new();
            state.with_mut(|s| s.set_client_name(CLIENT, "agent".to_owned()));
            let before = state.with(ServerState::journal_head);
            let create = Command::SessionCreate {
                operation: op(1),
                name: "work".to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: Vec::new(),
                launch: None,
            };
            let _ = committed(handle(&state, CLIENT, create, &CancellationToken::new()));
            let after = state.with(ServerState::journal_head);
            assert_eq!(
                after,
                before + 2,
                "SessionCreated and its ArrangementChanged"
            );
        });
    }

    /// Pages of one organization, with a fresh operation id per mutation.
    struct Org {
        state: SharedState,
        next_op: u8,
    }

    impl Org {
        fn new() -> Self {
            Self {
                state: SharedState::new(),
                next_op: 1,
            }
        }

        fn operation(&mut self) -> OperationId {
            let n = self.next_op;
            self.next_op = self.next_op.checked_add(1).expect("operation ids");
            op(n)
        }

        fn create_session(&mut self, name: &str) -> phux_protocol::ids::SessionKey {
            let operation = self.operation();
            committed(handle(
                &self.state,
                CLIENT,
                Command::SessionCreate {
                    operation,
                    name: name.to_owned(),
                    lifetime: SessionLifetime::Persistent,
                    attrs: Vec::new(),
                    launch: None,
                },
                &CancellationToken::new(),
            ))
            .session
            .unwrap_or_else(|| panic!("created {name}"))
        }

        fn create_resource(
            &mut self,
            session: phux_protocol::ids::SessionKey,
        ) -> phux_protocol::ids::ResourceKey {
            use phux_protocol::wire::frame::{ExitPolicy, LaunchMode};
            let operation = self.operation();
            committed(handle(
                &self.state,
                CLIENT,
                Command::ResourceCreate {
                    operation,
                    session,
                    kind: phux_protocol::ids::ResourceKind::Terminal,
                    launch: Some(Box::new(LaunchSpec::new(vec!["sh".to_owned()]))),
                    name: None,
                    on_exit: ExitPolicy::Retain,
                    launch_mode: LaunchMode::Deferred,
                },
                &CancellationToken::new(),
            ))
            .resource
            .expect("created resource")
        }

        fn delete_session(&mut self, session: &SessionRecord) {
            let operation = self.operation();
            let result = handle(
                &self.state,
                CLIENT,
                Command::SessionDelete {
                    operation,
                    session: session.key,
                },
                &CancellationToken::new(),
            );
            assert!(
                matches!(result, CommandResult::OkWith(_)),
                "delete {}: {result:?}",
                session.name
            );
        }

        fn remove_resource(&mut self, resource: &ResourceRecord) {
            let operation = self.operation();
            let result = handle(
                &self.state,
                CLIENT,
                Command::ResourceRemove {
                    operation,
                    resource: resource.key,
                },
                &CancellationToken::new(),
            );
            assert!(
                matches!(result, CommandResult::OkWith(_)),
                "remove {}: {result:?}",
                resource.key
            );
        }

        fn list(
            &self,
            filter: OrgFilter,
            page_size: u32,
            cursor: Option<Vec<u8>>,
        ) -> OrganizationPage {
            match handle(
                &self.state,
                CLIENT,
                Command::GetOrganization {
                    filter,
                    page_size,
                    cursor,
                },
                &CancellationToken::new(),
            ) {
                CommandResult::OkWith(CommandValue::OrganizationPage(page)) => *page,
                other => panic!("expected a page, got {other:?}"),
            }
        }

        fn drain(
            &self,
            filter: OrgFilter,
            page_size: u32,
            mut cursor: Option<Vec<u8>>,
        ) -> (Vec<SessionRecord>, Vec<ResourceRecord>) {
            let mut sessions = Vec::new();
            let mut resources = Vec::new();
            for _ in 0..32 {
                let Some(token) = cursor else {
                    return (sessions, resources);
                };
                let page = self.list(filter, page_size, Some(token));
                sessions.extend(page.sessions);
                resources.extend(page.resources);
                cursor = page.next_cursor;
            }
            panic!("paging did not finish");
        }
    }

    /// The greatest 16-byte key strictly below `cursor` that names nothing yet.
    fn free_key_before(state: &SharedState, cursor: &[u8; 16]) -> [u8; 16] {
        let mut bytes = *cursor;
        loop {
            let mut stepped = false;
            for byte in bytes.iter_mut().rev() {
                if *byte == 0 {
                    *byte = 0xff;
                    continue;
                }
                *byte -= 1;
                stepped = true;
                break;
            }
            assert!(stepped && bytes != [0; 16], "no free key below the cursor");
            let taken = state.with(|s| {
                let org = s.org();
                org.session(&phux_core::SessionKey::from_bytes(bytes))
                    .is_some()
                    || org
                        .resource(&phux_core::ResourceKey::from_bytes(bytes))
                        .is_some()
                    || org
                        .arrangement(&phux_core::ArrangementKey::from_bytes(bytes))
                        .is_some()
            });
            if !taken {
                return bytes;
            }
        }
    }

    fn insert_session_before(
        state: &SharedState,
        cursor: phux_protocol::ids::SessionKey,
    ) -> phux_protocol::ids::SessionKey {
        let bytes = free_key_before(state, cursor.as_bytes());
        let key = phux_core::SessionKey::from_bytes(bytes);
        let session = phux_core::Session {
            key,
            name: "inserted-before".to_owned(),
            lifetime: phux_core::SessionLifetime::Persistent,
            awaiting_organization: false,
            members: Vec::new(),
            arrangement: None,
            attrs: phux_core::Attrs::default(),
            created: phux_core::Stamp(0),
            revision: phux_core::Revision::FIRST,
            recent: None,
        };
        state.with_mut(|s| {
            s.registry_mut().commit(&phux_core::Transition {
                deltas: vec![phux_core::Delta::PutSession(session)],
                ..phux_core::Transition::default()
            });
        });
        phux_protocol::ids::SessionKey::new(bytes).expect("non-zero key")
    }

    fn insert_resource_before(
        state: &SharedState,
        session: phux_protocol::ids::SessionKey,
        cursor: phux_protocol::ids::ResourceKey,
    ) -> phux_protocol::ids::ResourceKey {
        let bytes = free_key_before(state, cursor.as_bytes());
        let key = phux_core::ResourceKey::from_bytes(bytes);
        let session_key = phux_core::SessionKey::from_bytes(*session.as_bytes());
        state.with_mut(|s| {
            let mut record = s.org().session(&session_key).expect("session").clone();
            record.members.push(key);
            let resource = phux_core::Resource {
                key,
                session: session_key,
                name: None,
                kind: phux_core::ResourceKind::Terminal,
                provenance: phux_core::Provenance {
                    created_by: phux_core::Actor::system(),
                    operation: phux_core::OperationId::from_bytes([9; 16]),
                    parent: None,
                },
                launch: None,
                on_exit: phux_core::ExitPolicy::Retain,
                activation: None,
                revision: phux_core::Revision::FIRST,
            };
            s.registry_mut().commit(&phux_core::Transition {
                deltas: vec![
                    phux_core::Delta::PutResource(resource),
                    phux_core::Delta::PutSession(record),
                ],
                ..phux_core::Transition::default()
            });
        });
        phux_protocol::ids::ResourceKey::new(bytes).expect("non-zero key")
    }

    #[test]
    fn deleting_before_the_page_cursor_does_not_skip_the_next_record() {
        run(async {
            let mut org = Org::new();
            for name in ["alpha", "bravo", "charlie", "delta"] {
                org.create_session(name);
            }
            let all = org.list(OrgFilter::All, 0, None);
            let first = org.list(OrgFilter::All, 2, None);
            assert_eq!(first.sessions.len(), 2);
            let cursor_key = first.sessions[1].key;
            let removed = first
                .sessions
                .iter()
                .rev()
                .find(|session| session.name != phux_core::org::INBOX_NAME)
                .expect("a deletable session on the first page")
                .clone();
            let cursor = first.next_cursor.expect("a later page");
            assert_eq!(cursor.as_slice(), cursor_key.as_bytes());
            org.delete_session(&removed);
            let (later, _) = org.drain(OrgFilter::All, 2, Some(cursor));
            let seen: Vec<_> = later.iter().map(|session| session.key).collect();
            let expected: Vec<_> = all
                .sessions
                .iter()
                .map(|session| session.key)
                .filter(|key| *key > cursor_key)
                .collect();
            assert_eq!(seen, expected, "a session was skipped or repeated");
            assert!(!seen.contains(&removed.key), "deleted session returned");

            let mut org = Org::new();
            let session = org.create_session("work");
            for _ in 0..4 {
                org.create_resource(session);
            }
            let full = org.list(OrgFilter::Session(session), 0, None);
            let first = org.list(OrgFilter::Session(session), 2, None);
            assert_eq!(
                first.sessions.len(),
                1,
                "the session record leads its members"
            );
            assert_eq!(first.resources.len(), 1);
            let emitted = first.resources[0].clone();
            let cursor = first.next_cursor.expect("members remain");
            assert_eq!(&cursor[..16], session.as_bytes());
            assert_eq!(&cursor[16..], emitted.key.as_bytes());
            org.remove_resource(&emitted);
            let (later_sessions, later_resources) =
                org.drain(OrgFilter::Session(session), 2, Some(cursor));
            assert!(later_sessions.is_empty(), "the session record was repeated");
            let seen: Vec<_> = later_resources
                .iter()
                .map(|resource| resource.key)
                .collect();
            let expected: Vec<_> = full
                .resources
                .iter()
                .map(|resource| resource.key)
                .filter(|key| *key > emitted.key)
                .collect();
            assert_eq!(seen, expected, "a resource was skipped or repeated");
        });
    }

    #[test]
    fn inserting_before_the_page_cursor_does_not_repeat_a_record() {
        run(async {
            let mut org = Org::new();
            for name in ["alpha", "bravo", "charlie", "delta"] {
                org.create_session(name);
            }
            let all = org.list(OrgFilter::All, 0, None);
            let first = org.list(OrgFilter::All, 2, None);
            let cursor_key = first.sessions[1].key;
            let cursor = first.next_cursor.expect("a later page");
            let inserted = insert_session_before(&org.state, cursor_key);
            assert!(inserted < cursor_key);
            let with_insert = org.list(OrgFilter::All, 0, None);
            assert!(
                with_insert
                    .sessions
                    .iter()
                    .any(|session| session.key == inserted),
                "the inserted session is in the organization"
            );
            let (later, _) = org.drain(OrgFilter::All, 2, Some(cursor));
            let seen: Vec<_> = later.iter().map(|session| session.key).collect();
            let expected: Vec<_> = all
                .sessions
                .iter()
                .map(|session| session.key)
                .filter(|key| *key > cursor_key)
                .collect();
            assert_eq!(seen, expected, "a session was skipped or repeated");
            assert!(
                !seen.contains(&inserted),
                "a session inserted into the consumed prefix was emitted"
            );

            let mut org = Org::new();
            let session = org.create_session("work");
            for _ in 0..4 {
                org.create_resource(session);
            }
            let full = org.list(OrgFilter::Session(session), 0, None);
            let first = org.list(OrgFilter::Session(session), 2, None);
            let emitted = first.resources[0].key;
            let cursor = first.next_cursor.expect("members remain");
            let inserted = insert_resource_before(&org.state, session, emitted);
            assert!(inserted < emitted);
            let with_insert = org.list(OrgFilter::Session(session), 0, None);
            assert!(
                with_insert
                    .resources
                    .iter()
                    .any(|resource| resource.key == inserted),
                "the inserted resource is in the organization"
            );
            let (later_sessions, later_resources) =
                org.drain(OrgFilter::Session(session), 2, Some(cursor));
            assert!(later_sessions.is_empty(), "session record repeated");
            let seen: Vec<_> = later_resources
                .iter()
                .map(|resource| resource.key)
                .collect();
            let expected: Vec<_> = full
                .resources
                .iter()
                .map(|resource| resource.key)
                .filter(|key| *key > emitted)
                .collect();
            assert_eq!(seen, expected, "a resource was skipped or repeated");
            assert!(
                !seen.contains(&inserted),
                "a resource inserted into the consumed prefix was emitted"
            );
        });
    }
}
