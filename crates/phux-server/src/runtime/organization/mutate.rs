//! Committing mutating organization commands, including session kill.

use std::collections::HashSet;

use phux_core::{Effect, Op, Refusal};
use phux_protocol::wire::frame::{CommandResult, CommandValue};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::org_wire as ow;
use crate::state::{ClientId, ServerState, SharedState};

use super::{answer, effects::LiveTermination, run_effect};

/// Owner, expected revision, and group count of an arrangement replace,
/// captured before `op` moves into the command.
const fn arrangement_save_fields(op: &Op) -> Option<(phux_core::ArrangementOwner, u64, usize)> {
    match op {
        Op::ArrangementReplace {
            owner,
            expected,
            groups,
            ..
        } => Some((*owner, expected.get(), groups.len())),
        _ => None,
    }
}

/// Commit a mutating command, run its effects, and answer.
pub(super) fn mutate(
    state: &SharedState,
    client_id: ClientId,
    operation: phux_core::OperationId,
    op: Op,
    root_token: &CancellationToken,
) -> CommandResult {
    if let Some(refusal) = state.with(|s| launch_refusal(s, operation, &op)) {
        return CommandResult::Refused(ow::refusal(&refusal));
    }
    let arrangement_save = arrangement_save_fields(&op);
    if let Some((owner, expected, groups)) = arrangement_save {
        debug!(
            ?owner,
            expected,
            groups,
            path = "arrangement",
            "arrangement save attempt"
        );
    }
    // A placed create answers after its launch so the receipt observes the
    // activation the effect recorded.
    let committed = state.with_mut(|s| {
        let actor = s.actor_for(client_id);
        let command = phux_core::Command { operation, op };
        let prepared = s.prepare_commit(&command, &actor)?;
        let live_termination = LiveTermination::plan(s, prepared.effects())?;
        let persisted = s.persist_prepared(prepared, 0)?;
        let mut commit = s.mirror_persisted(persisted, &actor, Some(client_id));
        if let Some(termination) = live_termination {
            termination.start(s);
            commit.applied.effects.clear();
        }
        let detached: Vec<_> = s
            .take_killed_sessions()
            .into_iter()
            .flat_map(|session| s.clients().attached_in_session(session))
            .collect();
        Ok::<_, Refusal>((commit.applied, detached))
    });
    let (mut applied, detached) = match committed {
        Ok(committed) => {
            note_arrangement_result(arrangement_save, None);
            committed
        }
        Err(refusal) => {
            note_arrangement_result(arrangement_save, Some(&refusal));
            if arrangement_save.is_none() {
                debug!(?client_id, %refusal, "organization command refused");
            }
            return CommandResult::Refused(ow::refusal(&refusal));
        }
    };
    crate::runtime::connections::detach_clients_of_killed_session(state, detached);
    for effect in std::mem::take(&mut applied.effects) {
        run_effect(state, effect, root_token);
    }
    state.with_mut(|s| answer(s, &applied))
}

/// App support is checked only for a new operation. Receipts remain the
/// authority for retries, even after an engine has started or been retained.
fn launch_refusal(s: &ServerState, operation: phux_core::OperationId, op: &Op) -> Option<Refusal> {
    if s.org().receipt(&operation).is_some() {
        return None;
    }
    match op {
        Op::ResourceCreate { resource, .. } => {
            unsupported_launch(resource.kind, "creating this kind through RESOURCE_CREATE")
        }
        Op::SessionCreate {
            resource: Some(resource),
            ..
        } => unsupported_launch(resource.kind, "creating this kind through SESSION_CREATE"),
        Op::ResourceRelaunch {
            resource, expected, ..
        } => relaunch_engine_held(s, op)
            .or_else(|| projection_relaunch_refusal(s, resource, *expected)),
        _ => None,
    }
}

fn projection_relaunch_refusal(
    s: &ServerState,
    resource: &phux_core::ResourceKey,
    expected: phux_core::Revision,
) -> Option<Refusal> {
    let record = s.org().resource(resource)?;
    // Core keeps its established revision, uncertainty, activity and
    // missing-launch refusal order ahead of app support.
    if record.revision != expected || record.launch.is_none() {
        return None;
    }
    if record.activation.as_ref().is_some_and(|activation| {
        activation.state.is_current() || activation.state == phux_core::ExecState::Lost
    }) {
        return None;
    }
    unsupported_launch(
        record.kind,
        "relaunching this producer-bound kind; open a fresh projection through its producer",
    )
}

fn unsupported_launch(kind: phux_core::ResourceKind, capability: &str) -> Option<Refusal> {
    crate::runtime::apps::organization_launcher(kind)
        .is_none()
        .then(|| Refusal::Unsupported {
            capability: capability.to_owned(),
        })
}

fn note_arrangement_result(
    save: Option<(phux_core::ArrangementOwner, u64, usize)>,
    refusal: Option<&Refusal>,
) {
    let Some((owner, expected, groups)) = save else {
        return;
    };
    if let Some(reason) = refusal {
        info!(?owner, expected, groups, %reason, path = "arrangement", "arrangement save refused");
    } else {
        debug!(
            ?owner,
            expected,
            groups,
            path = "arrangement",
            "arrangement save committed"
        );
    }
}

/// `SESSION_KILL`: the store commit comes first; only an accepted one closes
/// members (legacy kill teardown) and then mirrors into memory, so a refusal
/// changes nothing (`STORE_FAILURE`, retryable). One lock holds the whole
/// sequence. A replay answers without the member count or closing again.
#[allow(
    clippy::too_many_lines,
    reason = "persist the kill, close engines, then mirror, in one lock"
)]
pub(super) fn session_kill(
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
                .filter(|id| s.resources().handle(*id).is_some())
                .collect();
        }
        // Engines this kill will close. Their `pane_closed` records are
        // journaled before the kill's own events, so the stored journal
        // position counts them. A `Terminate` for one of them would signal
        // the group again after the teardown, so those effects are dropped.
        let plan = s.plan_close(&live, phux_protocol::wire::frame::CloseReason::Killed);
        let leading = u64::try_from(plan.resources().len()).unwrap_or(u64::MAX);
        let closing_keys: HashSet<_> = plan
            .resources()
            .filter_map(|id| s.registry().resource_key(id))
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
        let mut closed = super::super::committed_close::commit_close(
            s,
            plan,
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
            .flat_map(|session| s.clients().attached_in_session(session))
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
        super::super::committed_close::announce(state, closed);
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
    s.resources()
        .handle(id)
        .is_some()
        .then(|| Refusal::InvalidArgument {
            field: "resource",
            reason: "its exited engine is still retained; remove the retention first".to_owned(),
        })
}
