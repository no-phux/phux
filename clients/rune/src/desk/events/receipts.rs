//! Correlated replies outlive a socket; resource handles do not. Keep old
//! outcomes separate until HELLO and attach establish their destination.

use phux_client_runtime::{
    Client,
    control::{Event, Status},
};
use rune::Tx;

use super::super::{Desk, NoticeKind};

#[derive(Default)]
pub(in crate::desk) struct Receipts {
    server: Option<Vec<u8>>,
    waiting: bool,
    deferred: Vec<(Option<Vec<u8>>, Event)>,
}

impl Receipts {
    pub(in crate::desk) fn waiting(&self) -> bool {
        self.waiting
    }
}

impl Desk {
    pub(super) fn fence_epoch_events(&mut self, events: &mut Vec<Event>, epoch: u64) {
        let mut server = self
            .connection
            .receipts
            .server
            .clone()
            .filter(|_| !self.connection.receipts.waiting);
        self.connection.receipts.waiting |= epoch != self.connection.epoch;
        let Some(start) = events.iter().rposition(|event| {
            matches!(event,
            Event::ConnectionOpened { connection_epoch } if *connection_epoch == epoch)
        }) else {
            return;
        };
        let mut deliveries = Vec::new();
        for event in events.drain(..start) {
            match event {
                Event::InputDelivery { .. } | Event::TerminalsClosed { error: Some(_), .. } => {
                    deliveries.push(event);
                }
                Event::TerminalSpawned { .. } | Event::OrganizationOperation { .. } => {
                    self.connection
                        .receipts
                        .deferred
                        .push((server.clone(), event));
                }
                // No observation established the identity of an intermediate
                // socket. Its successes cannot prove a current handle.
                Event::ConnectionOpened { .. } => server = None,
                _ => {}
            }
        }
        events.splice(0..0, deliveries);
    }

    pub(super) fn reconcile_receipts(&mut self, client: &Client, tx: &mut Tx) -> bool {
        let mut changed = self.settle_failed_receipts(client, tx);
        if self.connection.status.is_terminal() {
            self.connection.receipts.waiting = false;
            self.connection.receipts.deferred.clear();
            return changed | self.cancel_old_placements();
        }
        // server() still reports the old HELLO while Connecting. Attach is
        // the barrier for both the new identity and its organization replica.
        if self.connection.status != Status::Attached {
            return changed;
        }
        let Some(server) = client.server().map(|info| info.id) else {
            return changed;
        };
        if self.connection.receipts.waiting
            && self
                .connection
                .receipts
                .server
                .as_ref()
                .is_some_and(|old| *old != server)
        {
            changed |= self.cancel_old_placements();
            self.connection.receipts.deferred.clear();
        }
        self.connection.receipts.server = Some(server.clone());
        self.connection.receipts.waiting = false;
        for (origin, event) in std::mem::take(&mut self.connection.receipts.deferred) {
            if origin.as_ref() == Some(&server) {
                changed |= self.apply(client, event, tx);
            } else if self.owns_placement_receipt(&event) {
                changed |= self.cancel_old_placements();
            }
        }
        changed
    }

    fn owns_placement_receipt(&self, event: &Event) -> bool {
        match event {
            Event::TerminalSpawned { request_id, .. } => {
                self.connection.pending_spawns.contains(request_id)
                    || self.connection.pending_splits.contains_key(request_id)
            }
            Event::OrganizationOperation { operation, .. } => {
                self.connection.layout_ops.contains(operation)
                    || self.owns_quick_move(*operation)
                    || self.owns_sidebar_move(*operation)
            }
            _ => false,
        }
    }

    fn settle_failed_receipts(&mut self, client: &Client, tx: &mut Tx) -> bool {
        let mut changed = false;
        for (server, event) in std::mem::take(&mut self.connection.receipts.deferred) {
            match event {
                Event::TerminalSpawned {
                    terminal_id: None, ..
                } => {
                    changed |= self.apply(client, event, tx);
                }
                Event::OrganizationOperation { operation, outcome }
                    if !outcome.succeeded() && self.connection.status != Status::Attached =>
                {
                    // Failure can settle now, but must not retry a placement
                    // against the cached identity while HELLO is outstanding.
                    changed |= self.abandon_layout(operation, &format!("{outcome:?}"));
                    changed |= self.quick_move_outcome(client, operation, &outcome);
                    changed |= self.sidebar_move_outcome(client, operation, &outcome);
                }
                other => self.connection.receipts.deferred.push((server, other)),
            }
        }
        changed
    }

    fn cancel_old_placements(&mut self) -> bool {
        let quick = self.cancel_quick_move("the server incarnation could not be verified");
        let sidebar = self.cancel_sidebar_moves();
        let pending = !self.connection.pending_spawns.is_empty()
            || !self.connection.pending_splits.is_empty()
            || !self.connection.layout_ops.is_empty()
            || self.splits_waiting();
        self.connection.pending_spawns.clear();
        self.connection.pending_splits.clear();
        self.connection.resources.starting.clear();
        self.connection.layout_ops.clear();
        self.connection.unplaced_splits.clear();
        self.connection.split_write = None;
        if pending {
            self.say(NoticeKind::Error, "Pending pane/layout actions cancelled: the server incarnation changed or could not be verified. Check the server’s panes in the palette before retrying; a producer may already be running.");
        }
        pending | quick | sidebar
    }
}
