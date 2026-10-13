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
/// caller may inventory, in key order, from `cursor`, with their members.
/// Totals count every matching record the caller may inventory; the Inbox
/// designation needs Global.
fn organization_page(
    s: &mut ServerState,
    client: ClientId,
    filter: OrgFilter,
    page_size: u32,
    cursor: Option<&[u8]>,
) -> CommandResult {
    let inbox = s.org().authority().inbox_session;
    let selected: Vec<phux_core::SessionKey> = match filter {
        OrgFilter::All => s.org().sessions().map(|r| r.key).collect(),
        OrgFilter::Session(key) => vec![ow::core_session_key(&key)],
        OrgFilter::Intake => s
            .org()
            .sessions()
            .filter(|r| r.key == inbox || r.awaiting_organization)
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
    // admitted members, and the cursor is an offset into that sequence, so
    // a session with thousands of members spans pages.
    let start = cursor
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map_or(0, |b| u32::from_be_bytes(b) as usize);
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
        entries.extend(
            s.registry()
                .members(*id)
                .into_iter()
                .filter(|r| crate::policy::filter::admits_resource(s, client, Verb::Inventory, *r))
                .map(PageEntry::Resource),
        );
    }
    let total_resources = entries.len() - total_sessions;
    let page: Vec<PageEntry> = entries.iter().skip(start).take(size).copied().collect();
    let next = start + page.len();
    let next_cursor = (next < entries.len()).then(|| {
        u32::try_from(next)
            .unwrap_or(u32::MAX)
            .to_be_bytes()
            .to_vec()
    });
    let mut sessions = Vec::new();
    let mut resources = Vec::new();
    for entry in page {
        let (key, id) = match entry {
            PageEntry::Session(key, id) => (key, id),
            PageEntry::Resource(member) => {
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
        .with_inbox(
            crate::policy::filter::admits_global(s, client, Verb::Inventory)
                .then(|| ow::session_key(inbox))
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

/// One record of a `GET_ORGANIZATION` page, in page order.
#[derive(Debug, Clone, Copy)]
enum PageEntry {
    Session(phux_core::SessionKey, phux_core::ids::SessionId),
    Resource(phux_core::ids::ResourceId),
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
}
