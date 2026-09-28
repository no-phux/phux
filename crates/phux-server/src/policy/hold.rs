//! The guard's third outcome, Hold (ADR-0128, `workload-auth.md` §6.1): for
//! a request [`super::enforce`] admitted, whether a `SIGNAL` it needs is
//! reachable only through a holding clause. A held command waits for a
//! decision; a held `SIGNAL` on any other frame is refused.

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

/// Run or hold an admitted `request`; only a scoped grant can hold.
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

/// `SIGNAL` on every subject of the held command; `None` if no approval
/// matches.
pub(super) fn held_action_needs(s: &ServerState, request: Request<'_>) -> Option<Vec<Need>> {
    let pending = decided_approval(s, request)?;
    let command = Request::Command(&pending.command);
    let Classification::Allow { subject, .. } = classify(command) else {
        return None;
    };
    needs_for(s, pending.requester, subject, SIGNAL, command)
}

/// A `VIEWER` on a held subject cannot decide the action (ADR-0127).
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

/// Whether `client` is a `VIEWER` on `terminal`.
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
