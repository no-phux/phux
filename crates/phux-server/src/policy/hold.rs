//! The guard's third outcome, Hold (ADR-0128, `docs/spec/workload-auth.md`
//! §6.1), and the subject a decision is judged on.
//!
//! Kept apart from [`super::enforce`], which answers "does the grant reach
//! this at all?" and admits a held verb like any other. This module asks the
//! next question for a request `enforce` already admitted: can it run now,
//! or does a `SIGNAL` it needs reach its subject only through a clause that
//! holds it? A held command waits for a decision; a held `SIGNAL` on any
//! other frame is refused, because there is no result to defer.

use phux_protocol::ids::{ApprovalId, ResourceId as WireResourceId};
use phux_protocol::kinds::{Classification, Verb, Verbs};
use phux_protocol::wire::frame::FrameKind;

use super::enforce::{Denial, Need, Request, classify, covers_all, needs_for, unwrap_command};
use super::{Authority, ConnectionGrant};
use crate::state::{ClientId, PendingApproval, ServerState};

const SIGNAL: Verbs = Verbs::of(&[Verb::Signal]);

/// A held `SIGNAL` on a request with no result to defer.
const HELD: Denial = Denial {
    verbs: SIGNAL,
    subject: "held",
};

/// A decision by a connection that subscribed a held subject as a `VIEWER`.
const VIEWER_DECISION: Denial = Denial {
    verbs: SIGNAL,
    subject: "viewer",
};

/// What the guard does with a request the grant admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Run it now.
    Run,
    /// Hold the command for a decision; its `COMMAND_RESULT` is deferred.
    Hold,
}

/// Run or hold `request`, which [`super::enforce`] already admitted.
///
/// Only a scoped grant holding some verb can hold anything: the owner's
/// grant, and so every `local` and transitional connection, always runs.
///
/// # Errors
///
/// A held `SIGNAL` on anything but a nested command.
pub(super) fn admission(
    s: &ServerState,
    client: ClientId,
    grant: &ConnectionGrant,
    request: Request<'_>,
) -> Result<Admission, Denial> {
    let Authority::Scoped { effective, .. } = &grant.authority else {
        return Ok(Admission::Run);
    };
    if !effective.holds_any() {
        return Ok(Admission::Run);
    }
    let holdable = matches!(request, Request::Command(_));
    let request = unwrap_command(request);
    let Classification::Allow { verbs, subject } = classify(request) else {
        return Ok(Admission::Run);
    };
    if !verbs.contains(Verb::Signal) {
        return Ok(Admission::Run);
    }
    let needs = needs_for(s, client, subject, verbs, request).unwrap_or_default();
    if covers_all(&effective.without_holds(), &needs) {
        return Ok(Admission::Run);
    }
    if holdable {
        Ok(Admission::Hold)
    } else {
        Err(HELD)
    }
}

/// The needs of a decision: `SIGNAL` on every subject the held command
/// needs, resolved under the current topology. `None` when the key names no
/// pending approval, which the guard refuses like an absent target.
pub(super) fn held_action_needs(s: &ServerState, request: Request<'_>) -> Option<Vec<Need>> {
    let pending = decided_approval(s, request)?;
    let command = Request::Command(&pending.command);
    let Classification::Allow { subject, .. } = classify(command) else {
        return None;
    };
    needs_for(s, pending.requester, subject, SIGNAL, command)
}

/// A connection subscribed to a held subject as a `VIEWER` declared itself
/// observe-only there (ADR-0127), so it cannot decide the action, whatever
/// its grant.
///
/// # Errors
///
/// A denial naming the viewer, never the subject.
pub(super) fn refuse_viewer_decision(
    s: &ServerState,
    client: ClientId,
    request: Request<'_>,
) -> Result<(), Denial> {
    let Some(pending) = decided_approval(s, unwrap_command(request)) else {
        return Ok(());
    };
    if super::held_terminals(&pending.command)
        .any(|terminal| decider_is_viewer_of(s, client, terminal))
    {
        return Err(VIEWER_DECISION);
    }
    Ok(())
}

/// Whether `client` is a `VIEWER` on `terminal`: the one question a
/// decision asks about attach roles. It means whatever
/// [`ServerState::is_viewer`] means.
fn decider_is_viewer_of(s: &ServerState, client: ClientId, terminal: &WireResourceId) -> bool {
    s.is_viewer(client, terminal)
}

/// The pending approval a `phux.approval.decide/v1/<id>` write names.
fn decided_approval<'s>(s: &'s ServerState, request: Request<'_>) -> Option<&'s PendingApproval> {
    let Request::Frame(FrameKind::SetMetadata { key, .. }) = request else {
        return None;
    };
    s.pending_approval(ApprovalId::from_decide_key(key)?)
}
