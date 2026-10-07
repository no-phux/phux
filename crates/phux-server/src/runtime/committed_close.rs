//! A kill's close, committed in the lock that admits it (`docs/spec/L1.md`
//! §5.2).
//!
//! The reply to `KILL_RESOURCE`, `KILL_RESOURCE_IF`, `KILL_RESOURCES`, and
//! `CLOSE_TAB_RESOURCES` follows the commit, so the commit is the whole
//! teardown an observer can see: every closed resource leaves the registry
//! (and so `GET_STATE`, `ATTACHED`, and every lookup), its `pane_closed` is
//! journaled, and an emptied window and session go with it, all in one
//! acquisition. Only the process outlives the commit: its actor was
//! cancelled and runs the hangup grace, and its exit watcher later finds the
//! resource already reaped and only fires `pane-exit` (see
//! [`ServerState::end_exit_watch`]).

use phux_core::ids::ResourceId as CoreResourceId;
use phux_core::process::ExitOutcome;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::CloseReason;
use tokio::sync::mpsc;

use crate::state::{ClientId, CloseAttribution, Outbound, ServerState, SharedState};

/// One resource a kill closed, captured under the commit lock for its
/// off-lock `RESOURCE_CLOSED`.
struct ClosedResource {
    wire_terminal_id: WireResourceId,
    targets: Vec<mpsc::Sender<Outbound>>,
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
    /// ADR-0105: clients of a session a group kill released.
    killed_clients: Vec<(ClientId, mpsc::Sender<Outbound>)>,
}

impl CommittedClose {
    /// How many resources the commit closed.
    pub(super) const fn len(&self) -> usize {
        self.closes.len()
    }
}

/// Close `targets` and every descendant under the caller's lock: mark why,
/// cancel each actor into its hangup grace, journal each `pane_closed`, and
/// reap each from the registry, cascading the emptied windows and sessions.
pub(super) fn commit_close(
    s: &mut ServerState,
    targets: &[CoreResourceId],
    reason: CloseReason,
    attribution: CloseAttribution,
) -> CommittedClose {
    let closing = s.mark_and_cancel(targets, reason, attribution);
    // Each resource follows its parent in `closing`; reversed, every child
    // is journaled and reaped before the parent that names it.
    let closes = closing
        .into_iter()
        .rev()
        .filter_map(|resource| close_one(s, resource))
        .collect();
    let killed_clients = s
        .take_killed_sessions()
        .into_iter()
        .flat_map(|session| s.attached_clients_in_session(session))
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
    let exit = s.retained_outcome(resource).unwrap_or(ExitOutcome::UNKNOWN);
    // Interned before the reap, which retires the wire id.
    let wire_terminal_id = s.intern_terminal_wire(resource);
    let parent = s
        .resource_parent(resource)
        .map(|parent| s.intern_terminal_wire(parent));
    // Every subscriber, including `ATTACH_RESOURCE`-only ones (L1 §3.1).
    let targets = s.terminal_fanout_targets(resource);
    let attribution = s.take_close_attribution(resource);
    super::client::journal_pane_closed(
        s,
        &wire_terminal_id,
        parent.as_ref(),
        exit.status,
        attribution,
    );
    s.note_closed_before_exit(resource, wire_terminal_id.clone());
    s.cancel_terminal_pumps(resource);
    let _ = s.reap_terminal(resource);
    Some(ClosedResource {
        wire_terminal_id,
        targets,
        reason,
        exit,
    })
}

/// Deliver what `committed` owes, off the lock and off the caller's reply
/// path, so a subscriber with a full mailbox cannot delay the kill's reply:
/// each `RESOURCE_CLOSED`, children first, then `DETACHED { SESSION_KILLED }`
/// to the clients of a released session (ADR-0105), after the closes so a
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
            super::client::broadcast_terminal_closed(
                &closed.wire_terminal_id,
                &closed.targets,
                closed.exit,
                closed.reason,
            )
            .await;
        }
        super::client::detach_clients_of_killed_session(&state, killed_clients);
    });
}
