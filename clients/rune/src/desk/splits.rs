//! Making a split or tab: the terminal spawns first, then the session's
//! arrangement is written around it, one write at a time.

use std::time::{Duration, Instant};

use phux_client_core::layout::{LayoutNode, SplitDir, leaves, split_at};
use phux_client_runtime::Client;
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::{OperationId, ResourceId, Revision};

use super::{Desk, NoticeKind, Placement};

/// A spawned terminal waiting for the arrangement: where it goes, and since
/// when.
pub(super) type Unplaced = (ResourceId, Placement, Instant);

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
        self.pending_splits
            .insert(request, Some((shown.terminal.clone(), dir)));
        self.zoomed = None;
    }

    /// Queue a spawned terminal for placement.
    pub(super) fn place_split(&mut self, client: &Client, new: &ResourceId, place: Placement) {
        self.unplaced_splits
            .push((new.clone(), place, Instant::now()));
        self.place_pending_splits(client);
    }

    /// Write the next queued split once the session's arrangement is here,
    /// the keyboard staying where it is until then. One write at a time, each
    /// waiting for the replica to show the last: a write from an old document
    /// is refused, or worse, splits a target that isn't in it.
    pub(super) fn place_pending_splits(&mut self, client: &Client) {
        self.finish_split(client);
        while self.split_write.is_none() && !self.unplaced_splits.is_empty() {
            if self.arrangement(client).is_none() {
                self.expire_splits();
                return;
            }
            let base = self.arrangement_revision(client);
            let (new, place, since) = self.unplaced_splits.remove(0);
            let written = match &place {
                Some((target, dir)) => self.write_split(client, target, &new, *dir),
                None => self.write_tab(client, &new),
            };
            match written {
                Some(operation) => {
                    self.split_write = Some(SplitWrite {
                        sent: Some(operation),
                        refused: false,
                        base,
                        item: (new, place, since),
                    });
                }
                // Not writable yet (the replica hasn't heard of the new
                // terminal): the next poll tries again.
                None if since.elapsed() < PATIENCE => {
                    self.unplaced_splits.insert(0, (new, place, since));
                    return;
                }
                None => self.give_up_split(new),
            }
        }
    }

    /// The revision of the selected session's arrangement in the replica.
    fn arrangement_revision(&self, client: &Client) -> Option<Revision> {
        let selected = client.selected_session()?;
        let topology = self.topology.as_ref()?;
        let session = topology.sessions.iter().find(|s| s.id == selected)?.key?;
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
        let late = write.item.2.elapsed() >= PATIENCE;
        let unseen = self.arrangement_revision(client) == write.base;
        if (write.sent.is_some() || unseen) && !late {
            return;
        }
        let Some(SplitWrite { refused, item, .. }) = self.split_write.take() else {
            return;
        };
        match refused {
            false => self.want(item.0),
            true if late => self.give_up_split(item.0),
            true => self.unplaced_splits.insert(0, item),
        }
    }

    /// Splits that have waited too long show on their own.
    fn expire_splits(&mut self) {
        let waiting = std::mem::take(&mut self.unplaced_splits);
        for item in waiting {
            if item.2.elapsed() < PATIENCE {
                self.unplaced_splits.push(item);
            } else {
                self.give_up_split(item.0);
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
                self.refetch_arrangement(client);
            }
            None if refused => self.say(
                NoticeKind::Error,
                "The layout changed elsewhere first; try again.",
            ),
            None => {}
        }
        self.place_pending_splits(client);
    }

    fn refetch_arrangement(&self, client: &Client) {
        let selected = client.selected_session();
        let session = self.topology.as_ref().and_then(|t| {
            let session = t.sessions.iter().find(|s| Some(s.id) == selected)?;
            session.arrangement
        });
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
