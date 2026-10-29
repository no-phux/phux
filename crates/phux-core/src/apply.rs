//! [`apply()`]: the one pure function that turns a [`Command`] against an
//! [`Org`] into a [`Transition`] plus [`Effect`]s, or a [`Refusal`].
//!
//! Invariants every successful transition preserves:
//!
//! * every resource is a member of exactly one session, and every member
//!   points back at its session;
//! * no ephemeral session is empty after commit: removing or moving out its
//!   last member deletes it in the same transition;
//! * the designated Intake session is never ephemeral and never deleted;
//! * a session arrangement names only that session's members: entries for
//!   a resource that leaves are pruned in the same transition;
//! * every record touched by a transition bumps its revision exactly once.
//!
//! A refusal changes nothing. Retries are deduplicated by operation id
//! before any other check.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::activation::{
    Activation, ExecState, ExitPolicy, Outcome, ProcIdentity, SessionLifetime, Stamp,
};
use crate::actor::{Actor, PrincipalId, Verb};
use crate::arrangement::{Arrangement, ArrangementOwner, Group, first_group, validate_groups};
use crate::command::{
    Applied, Command, CreatePlacement, Created, Effect, LaunchMode, MAX_MOVE_BATCH, NewResource,
    Op, Refusal, Reply, SourcePin, TerminateMode,
};
use crate::keys::{
    ActivationId, ArrangementKey, FreshKeys, OperationId, ResourceKey, Revision, SessionKey,
};
use crate::org::{
    Delta, Event, InverseEdit, Org, Receipt, RecordRef, RevisionPin, Transition, Undo,
};
use crate::resource::{LaunchSpec, Provenance, Resource, ResourceKind};
use crate::session::{Attrs, MAX_NAME_BYTES, Session};

/// Apply `cmd`, issued by `actor` at `now`, to `org`.
///
/// Pure: `org` is not modified. On success the caller commits
/// [`Applied::transition`] durably, mirrors it with [`Org::commit`], then
/// runs [`Applied::effects`] and publishes the events. New keys come from
/// `fresh`.
///
/// # Errors
///
/// A [`Refusal`]; nothing is to be committed.
pub fn apply(
    org: &Org,
    cmd: &Command,
    actor: &Actor,
    now: Stamp,
    fresh: &mut dyn FreshKeys,
) -> Result<Applied, Refusal> {
    if let Some(receipt) = org.receipt(&cmd.operation) {
        return if receipt.op == idempotent_op(&cmd.op) {
            Ok(Applied {
                operation: cmd.operation,
                transition: Transition::default(),
                effects: Vec::new(),
                reply: receipt.reply,
                replayed: true,
            })
        } else {
            Err(Refusal::DuplicateOperation)
        };
    }
    if cmd.op.is_report() && actor.principal != PrincipalId::System {
        return Err(Refusal::Unauthorized { verb: Verb::Create });
    }
    let mut tx = Tx::new(org, actor, cmd.operation, now);
    let Out { reply, undo } = dispatch(&mut tx, &cmd.op, fresh)?;
    let (deltas, events, effects) = tx.finish();
    let receipt = (!cmd.op.is_report()).then(|| Receipt {
        operation: cmd.operation,
        actor: actor.clone(),
        op: idempotent_op(&cmd.op),
        at: now,
        reply,
        undo,
    });
    Ok(Applied {
        operation: cmd.operation,
        transition: Transition {
            deltas,
            events,
            receipt,
        },
        effects,
        reply,
        replayed: false,
    })
}

/// Receipt identity without secret environment values. The resource record
/// keeps the real values for execution; only the retained operation does not.
fn idempotent_op(op: &Op) -> Op {
    let mut op = op.clone();
    match &mut op {
        Op::SessionCreate { resource, .. } => {
            if let Some(launch) = resource.as_mut().and_then(|spec| spec.launch.as_mut()) {
                redact_launch(launch);
            }
        }
        Op::ResourceCreate { resource, .. } => {
            if let Some(launch) = resource.launch.as_mut() {
                redact_launch(launch);
            }
        }
        Op::ResourceRelaunch { launch, .. } => {
            if let Some(launch) = launch.as_mut() {
                redact_launch(launch);
            }
        }
        _ => {}
    }
    op
}

fn redact_launch(launch: &mut LaunchSpec) {
    let LaunchSpec::Terminal(terminal) = launch else {
        return;
    };
    for (_, value) in &mut terminal.env {
        *value = hash_env_value(value);
    }
}

fn hash_env_value(value: &str) -> String {
    use sha2::{Digest, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(value.as_bytes());
    let mut encoded = String::from("sha256:");
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0xf) as usize] as char);
    }
    encoded
}

struct Out {
    reply: Reply,
    undo: Option<Undo>,
}

impl Out {
    const fn ok(revision: Option<Revision>) -> Self {
        Self {
            reply: Reply::Ok {
                revision,
                created: Created {
                    session: None,
                    resource: None,
                    arrangement: None,
                    entry: None,
                },
            },
            undo: None,
        }
    }

    const fn created(revision: Option<Revision>, created: Created) -> Self {
        Self {
            reply: Reply::Ok { revision, created },
            undo: None,
        }
    }

    const fn accepted() -> Self {
        Self {
            reply: Reply::Accepted {
                created: Created {
                    session: None,
                    resource: None,
                    arrangement: None,
                    entry: None,
                },
            },
            undo: None,
        }
    }

    fn with_undo(mut self, undo: Undo) -> Self {
        self.undo = Some(undo);
        self
    }
}

fn dispatch(tx: &mut Tx<'_>, op: &Op, fresh: &mut dyn FreshKeys) -> Result<Out, Refusal> {
    match op {
        Op::SessionCreate {
            name,
            lifetime,
            attrs,
            resource,
        } => session_create(tx, fresh, name, *lifetime, attrs, resource.as_ref()),
        Op::SessionRename { session, name } => session_rename(tx, *session, name),
        Op::SessionSetProject { session, project } => {
            session_set_project(tx, *session, project.as_deref())
        }
        Op::SessionSetLifetime { session, lifetime } => {
            session_set_lifetime(tx, *session, *lifetime)
        }
        Op::SessionDelete { session } => session_delete(tx, *session),
        Op::SessionKill { session } => session_kill(tx, *session),
        Op::ResourceCreate {
            session,
            resource,
            placement,
        } => resource_create(tx, fresh, *session, resource, placement.as_ref()),
        Op::ResourceMove {
            resource,
            from,
            from_expected,
            to,
            to_expected,
        } => resource_move(tx, *resource, (*from, *from_expected), (*to, *to_expected)),
        Op::ResourceMoveMany {
            resources,
            sources,
            to,
            to_expected,
        } => resource_move_many(tx, resources, sources, *to, *to_expected),
        Op::InboxResolve { session, expected } => intake_resolve(tx, *session, *expected),
        Op::OrganizationUndo { original, expected } => organization_undo(tx, *original, expected),
        Op::ResourceRename { resource, name } => resource_rename(tx, *resource, name.as_deref()),
        Op::ResourceTerminate {
            resource,
            activation,
            mode,
        } => resource_terminate(tx, *resource, *activation, *mode),
        Op::ResourceRemove { resource } => resource_remove(tx, *resource),
        Op::ResourceRelaunch {
            resource,
            expected,
            launch,
        } => resource_relaunch(tx, *resource, *expected, launch.as_ref()),
        Op::ArrangementReplace {
            owner,
            expected,
            groups,
        } => arrangement_replace(tx, fresh, *owner, *expected, groups),
        Op::ActivationStarted {
            resource,
            activation,
            proc,
        } => activation_started(tx, *resource, *activation, *proc),
        Op::ActivationEnded {
            resource,
            activation,
            state,
            outcome,
            diagnostic,
        } => activation_ended(tx, *resource, *activation, *state, *outcome, *diagnostic),
        Op::ActivationLost {
            resource,
            activation,
        } => activation_lost(tx, *resource, *activation),
        Op::ResourceLaunchReported {
            resource,
            activation,
            launch,
        } => resource_launch_reported(tx, *resource, *activation, launch),
    }
}

mod sessions;
use sessions::{
    check_name, intake_resolve, session_create, session_delete, session_kill, session_rename,
    session_set_lifetime, session_set_project, set_intake,
};
mod placement;
use placement::{organization_undo, resource_create, resource_move, resource_move_many};
mod resources;
use resources::{
    activation_ended, activation_lost, activation_started, arrangement_replace,
    resource_launch_reported, resource_relaunch, resource_remove, resource_rename,
    resource_terminate,
};
mod transaction;
use transaction::Tx;
