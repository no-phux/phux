//! Making a split or tab: the terminal spawns first, then the session's
//! arrangement is written around it, one write at a time.

use std::time::{Duration, Instant};

use phux_client_core::layout::{LayoutNode, SplitDir, leaves, split_at};
use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::Client;
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::{OperationId, ResourceId, Revision, SessionKey};

use super::{Desk, NoticeKind, Placement};

/// A terminal waiting for one session's arrangement, never whichever session
/// happens to be selected when its replica or retry becomes ready.
pub(super) struct Unplaced {
    pub(super) terminal: ResourceId,
    session: SessionKey,
    place: Placement,
    since: Instant,
}

/// An arrangement write for a spawned terminal.
pub(super) struct SplitWrite {
    /// The operation, until the server answers.
    sent: Option<OperationId>,
    refused: bool,
    /// The replica's revision it was written against.
    base: Option<Revision>,
    item: Unplaced,
}

/// How long a spawned terminal may wait to be arranged.
const PATIENCE: Duration = Duration::from_secs(3);

impl Desk {
    pub(super) fn splits_waiting(&self) -> bool {
        !self.unplaced_splits.is_empty() || self.split_write.is_some()
    }

    /// Split the focused pane: spawn a terminal beside it, placed when the
    /// server answers.
    pub fn split(&mut self, dir: SplitDir) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let request = client.spawn_terminal(SpawnRequest {
            command: None,
            cwd: self.focused_cwd(),
            env: None,
            session_id: client.selected_session(),
            initial_size: None,
        });
        self.pending_splits.insert(
            request,
            (
                self.placement_session(client),
                Some((shown.resource.clone(), dir)),
            ),
        );
        self.zoomed = None;
    }

    /// Keep the destination captured when the spawn or move was requested.
    pub(super) fn place_split(
        &mut self,
        client: &Client,
        new: &ResourceId,
        place: Placement,
        session: Option<SessionKey>,
    ) {
        let Some(session) = session else {
            self.say(NoticeKind::Error, "The tab or split could not be queued: its destination session was not known. Open the terminal from the palette.");
            return;
        };
        self.unplaced_splits.push(Unplaced {
            terminal: new.clone(),
            session,
            place,
            since: Instant::now(),
        });
        self.place_pending_splits(client);
    }

    pub(super) fn placement_session(&self, client: &Client) -> Option<SessionKey> {
        let selected = client.selected_session()?;
        self.topology
            .as_ref()?
            .sessions
            .iter()
            .find(|s| s.id == selected)?
            .key
    }

    /// Selection is not an instruction to redirect an outstanding placement.
    /// Forget its correlation too: a late refusal must not retry in the new
    /// session, and a late success must not steal the person's focus back.
    fn cancel_foreign_splits(&mut self, client: &Client) {
        let selected = self.placement_session(client);
        let before = self.unplaced_splits.len();
        self.unplaced_splits
            .retain(|item| Some(item.session) == selected);
        let write = self
            .split_write
            .take_if(|write| Some(write.item.session) != selected);
        let cancelled = before != self.unplaced_splits.len() || write.is_some();
        if let Some(operation) = write.and_then(|write| write.sent) {
            self.layout_ops.remove(&operation);
        }
        if cancelled {
            self.say(NoticeKind::Error, "Pending tab/split placement cancelled because the selected session changed. The terminal remains in its original session; open it from the palette.");
        }
    }

    /// Write the next queued split once the session's arrangement is here,
    /// the keyboard staying where it is until then. One write at a time, each
    /// waiting for the replica to show the last: a write from an old document
    /// is refused, or worse, splits a target that isn't in it.
    pub(super) fn place_pending_splits(&mut self, client: &Client) {
        if self.receipts.waiting() {
            return;
        }
        self.cancel_foreign_splits(client);
        self.finish_split(client);
        while self.split_write.is_none() && !self.unplaced_splits.is_empty() {
            if self.arrangement(client).is_none() {
                self.expire_splits();
                return;
            }
            let base = self.arrangement_revision(client);
            let item = self.unplaced_splits.remove(0);
            let written = match &item.place {
                Some((target, dir)) => self.write_split(client, target, &item.terminal, *dir),
                None => self.write_tab(client, &item.terminal),
            };
            match written {
                Some(operation) => {
                    self.split_write = Some(SplitWrite {
                        sent: Some(operation),
                        refused: false,
                        base,
                        item,
                    });
                }
                // Not writable yet (the replica hasn't heard of the new
                // terminal): the next poll tries again.
                None if item.since.elapsed() < PATIENCE => {
                    self.unplaced_splits.insert(0, item);
                    return;
                }
                None => self.give_up_split(item.terminal),
            }
        }
    }

    /// The revision of the selected session's arrangement in the replica.
    fn arrangement_revision(&self, client: &Client) -> Option<Revision> {
        let session = self.placement_session(client)?;
        client.with_control(|control| {
            let document = control.organization().arrangement_document(&session);
            document.map(|(_, revision, _, _)| revision)
        })
    }

    /// Close out a write the server answered, once the replica has caught
    /// up: the new pane takes the keyboard, or a refused one goes again.
    fn finish_split(&mut self, client: &Client) {
        let Some(write) = &self.split_write else {
            return;
        };
        // An answer lost to a reconnect mustn't hold up every later split.
        let late = write.item.since.elapsed() >= PATIENCE;
        let unseen = self.arrangement_revision(client) == write.base;
        if (write.sent.is_some() || unseen) && !late {
            return;
        }
        let Some(SplitWrite { refused, item, .. }) = self.split_write.take() else {
            return;
        };
        match refused {
            false => self.want(item.terminal),
            true if late => self.give_up_split(item.terminal),
            true => self.unplaced_splits.insert(0, item),
        }
    }

    /// Splits that have waited too long show on their own.
    fn expire_splits(&mut self) {
        let waiting = std::mem::take(&mut self.unplaced_splits);
        for item in waiting {
            if item.since.elapsed() < PATIENCE {
                self.unplaced_splits.push(item);
            } else {
                self.give_up_split(item.terminal);
            }
        }
    }

    fn give_up_split(&mut self, new: ResourceId) {
        self.say(
            NoticeKind::Error,
            "The layout could not be saved; it shows on its own.",
        );
        self.want(new);
    }

    /// Resolve one layout correlation. An uncertain receipt must not become a
    /// fresh write with a new operation id, or masquerade as a saved layout.
    pub(super) fn layout_outcome(
        &mut self,
        client: &Client,
        operation: OperationId,
        outcome: OrgOutcome,
    ) -> bool {
        match outcome {
            OrgOutcome::Unknown { reason } | OrgOutcome::NotSent { reason } => {
                self.abandon_layout(operation, &reason)
            }
            other => {
                if !self.layout_ops.remove(&operation) {
                    return false;
                }
                self.layout_written(client, operation, !other.succeeded());
                true
            }
        }
    }

    /// Drop a failed/uncertain write without focusing or retrying old handles.
    pub(super) fn abandon_layout(&mut self, operation: OperationId, reason: &str) -> bool {
        if !self.layout_ops.remove(&operation) {
            return false;
        }
        if self
            .split_write
            .as_ref()
            .is_some_and(|w| w.sent == Some(operation))
        {
            self.split_write = None;
        }
        self.say(NoticeKind::Error, format!("Layout write did not settle: {reason}. Check the server’s panes in the palette before retrying."));
        true
    }

    /// An arrangement write settled. A split's waits for the replica to
    /// hear; the fetch covers a server that doesn't announce it.
    pub(super) fn layout_written(
        &mut self,
        client: &Client,
        operation: OperationId,
        refused: bool,
    ) {
        let split = self
            .split_write
            .as_mut()
            .filter(|write| write.sent == Some(operation));
        match split {
            Some(write) => {
                write.sent = None;
                write.refused = refused;
                let session = write.item.session;
                self.refetch_arrangement(client, session);
            }
            None if refused => self.say(
                NoticeKind::Error,
                "The layout changed elsewhere first; try again.",
            ),
            None => {}
        }
        self.place_pending_splits(client);
    }

    fn refetch_arrangement(&self, client: &Client, session: SessionKey) {
        let session = self
            .topology
            .as_ref()
            .and_then(|t| t.session_keyed(&session)?.arrangement);
        if let Some(arrangement) = session {
            let _ = client.fetch_arrangement(arrangement);
        }
    }

    fn write_split(
        &mut self,
        client: &Client,
        target: &ResourceId,
        new: &ResourceId,
        dir: SplitDir,
    ) -> Option<OperationId> {
        let placed = self.arrangement(client).map(|(session, mut doc)| {
            // Split where the target lives. One no group holds gets its own
            // tab, so no other tab's tree is overwritten.
            super::tabs::focus_group(&mut doc, Some(target));
            let holds = |tree: &LayoutNode| leaves(tree).contains(target);
            if !doc
                .active_group()
                .and_then(|g| g.tree.as_ref())
                .is_some_and(holds)
            {
                doc.add_group(doc.default_group_name(), target.clone());
            }
            (session, doc)
        });
        let mut written = None;
        if let Some((session, mut doc)) = placed
            && let Some(group) = doc.active_group_mut()
        {
            let tree = group
                .tree
                .clone()
                .unwrap_or_else(|| LayoutNode::Leaf(target.clone()));
            let base = if leaves(&tree).contains(target) {
                tree
            } else {
                LayoutNode::Leaf(target.clone())
            };
            if let Ok(split) = split_at(&base, target, new, dir, 0.5) {
                group.tree = Some(split);
                group.focus = Some(new.clone());
                written = self.submit_arrangement(client, session, &doc).ok();
            }
        }
        written
    }
}
