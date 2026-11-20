//! A kill's close, committed in the lock that admits it (`docs/protocol.md`).
//! The reply follows the commit, so the commit is the whole observable
//! teardown: the resource leaves the registry, its `pane_closed` is
//! journaled, and an emptied ephemeral session goes too. Only the process
//! outlives it, in its hangup grace ([`ServerState::end_exit_watch`]).

use phux_core::ids::ResourceId as CoreResourceId;
use phux_core::process::ExitOutcome;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{CloseReason, CommandResult, ErrorCode};
use tokio::sync::mpsc;

use crate::state::{ClientId, CloseAttribution, ClosePlan, Outbound, ServerState, SharedState};

/// One resource a kill closed, captured under the commit lock for its
/// off-lock `RESOURCE_CLOSED`.
struct ClosedResource {
    wire_terminal_id: WireResourceId,
    targets: Vec<mpsc::Sender<Outbound>>,
    publication_fences: Vec<(
        mpsc::Sender<Outbound>,
        Vec<tokio_util::sync::CancellationToken>,
    )>,
    reason: CloseReason,
    /// Unknown unless the resource is a retained exited pane: the process
    /// of a live one has not exited yet.
    exit: ExitOutcome,
}

/// What a committed close still owes its observers once the lock drops.
#[must_use = "a committed close must be announced"]
pub(super) struct CommittedClose {
    /// Children before their parents, the order the tree came apart in.
    closes: Vec<ClosedResource>,
    /// Clients of a session the commit deleted.
    killed_clients: Vec<(ClientId, mpsc::Sender<Outbound>)>,
}

impl CommittedClose {
    /// How many resources the commit closed.
    pub(super) const fn len(&self) -> usize {
        self.closes.len()
    }

    /// Detach these clients too, after the closes.
    pub(super) fn extend_killed_clients(
        &mut self,
        clients: impl IntoIterator<Item = (ClientId, mpsc::Sender<Outbound>)>,
    ) {
        self.killed_clients.extend(clients);
    }
}

/// Admit a user close against its complete cascade and commit that same set.
pub(super) fn commit_user_close(
    s: &mut ServerState,
    targets: &[CoreResourceId],
    attribution: CloseAttribution,
) -> Result<CommittedClose, CommandResult> {
    let plan = s.plan_close(targets, CloseReason::Killed);
    let keys: Vec<_> = plan
        .resources()
        .filter_map(|id| s.registry().resource_key(id))
        .collect();
    if let Some(tab) = phux_core::arrangement::locked_tab_closed_by(s.org(), &keys) {
        return Err(CommandResult::error(
            ErrorCode::PreconditionFailed,
            format!("{tab} is locked; unlock it first"),
        ));
    }
    Ok(commit_close(s, plan, attribution))
}

/// Commit an admitted close. Session teardown can override tab locks.
pub(super) fn commit_close(
    s: &mut ServerState,
    plan: ClosePlan,
    attribution: CloseAttribution,
) -> CommittedClose {
    s.cancel_close_plan(&plan, attribution);
    let closes = plan
        .into_resources()
        .filter_map(|resource| close_one(s, resource))
        .collect();
    let killed_clients = s
        .take_killed_sessions()
        .into_iter()
        .flat_map(|session| s.clients().attached_in_session(session))
        .collect();
    CommittedClose {
        closes,
        killed_clients,
    }
}

/// Journal and reap one marked resource; `None` when another closer already
/// reaped it.
fn close_one(s: &mut ServerState, resource: CoreResourceId) -> Option<ClosedResource> {
    let reason = s.begin_resource_close(resource)?;
    // ADR-0124: a retained pane's close reports the exit it was kept with.
    let exit = s
        .retained()
        .outcome(resource)
        .unwrap_or(ExitOutcome::UNKNOWN);
    // Interned before the reap, which retires the wire id.
    let wire_terminal_id = s.idspace.intern_resource(resource);
    let parent = s
        .resource_parent(resource)
        .map(|parent| s.idspace.intern_resource(parent));
    // Every subscriber, including `ATTACH_RESOURCE`-only ones (L1 §3.1).
    let targets = s.terminal_fanout_targets(resource);
    let publication_fences = s.attach_publication_fences(resource);
    let attribution = s.take_close_attribution(resource);
    crate::runtime::connections::journal_pane_closed(
        s,
        &wire_terminal_id,
        parent.as_ref(),
        exit.status,
        attribution,
    );
    s.note_closed_before_exit(resource, wire_terminal_id.clone());
    s.resources_mut().cancel_pumps_for_terminal(resource);
    let _ = s.reap_resource(resource);
    Some(ClosedResource {
        wire_terminal_id,
        targets,
        publication_fences,
        reason,
        exit,
    })
}

/// Deliver what `committed` owes, off the lock and off the caller's reply
/// path, so a subscriber with a full mailbox cannot delay the kill's reply:
/// each `RESOURCE_CLOSED`, children first, then `DETACHED { SESSION_KILLED }`
/// to the clients of a deleted session, after the closes so a
/// client sees its last pane go before the session that held it.
pub(super) fn announce(state: &SharedState, committed: CommittedClose) {
    let CommittedClose {
        closes,
        killed_clients,
    } = committed;
    if closes.is_empty() && killed_clients.is_empty() {
        return;
    }
    let state = state.clone();
    tokio::task::spawn_local(async move {
        for closed in &closes {
            crate::runtime::connections::pane_exit::broadcast_terminal_closed_after_drain(
                &closed.wire_terminal_id,
                &closed.targets,
                closed.exit,
                closed.reason,
                &closed.publication_fences,
            )
            .await;
        }
        crate::runtime::connections::detach_clients_of_killed_session(&state, killed_clients);
    });
}
