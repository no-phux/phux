//! Making a split or tab: the terminal spawns first, then the session's
//! arrangement is written around it, one write at a time.

use std::time::{Duration, Instant};

use phux_client_core::layout::{ArrangementDoc, LayoutNode, SplitDir, leaves, split_at};
use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::Client;
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::{OperationId, ResourceId, Revision, SessionKey};

use phux_design::tokens::SPACE;

use super::{Desk, NoticeKind, Placement};
use crate::ui::PAD;

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
    /// A split's written arrangement, shown until the replica has it, so the
    /// new pane appears when the write goes rather than when it is answered.
    preview: Option<(SessionKey, ArrangementDoc)>,
    item: Unplaced,
}

/// How long a spawned terminal may wait to be arranged.
const PATIENCE: Duration = Duration::from_secs(3);

impl Desk {
    pub(super) fn splits_waiting(&self) -> bool {
        !self.connection.unplaced_splits.is_empty() || self.connection.split_write.is_some()
    }

    /// Split the focused pane: spawn a terminal beside it, placed when the
    /// server answers.
    pub fn split(&mut self, dir: SplitDir) {
        let Some(shown) = &self.connection.shown else {
            return;
        };
        let target = shown.resource.clone();
        let spawn = SpawnRequest {
            command: None,
            cwd: self.focused_cwd(),
            env: None,
            session_id: None,
            initial_size: None,
        };
        self.split_with(target, dir, spawn);
    }

    /// Split `target` along `dir` with `spawn`'s command, cwd and env.
    pub(super) fn split_with(&mut self, target: ResourceId, dir: SplitDir, spawn: SpawnRequest) {
        let Some(client) = &self.connection.client else {
            return;
        };
        let request = client.spawn_terminal(SpawnRequest {
            session_id: client.selected_session(),
            initial_size: self.split_spawn_size(&target, dir),
            ..spawn
        });
        self.connection.pending_splits.insert(
            request,
            (self.placement_session(client), Some((target, dir))),
        );
        self.connection.zoomed = None;
    }

    /// The cells the new half of `target` will get once split along `dir`,
    /// so its program starts at that size rather than the server's default
    /// and then redraws for a resize, as the terminal UI's splits do. `None`
    /// while zoomed: the zoomed pane's area isn't the one it splits.
    fn split_spawn_size(&self, target: &ResourceId, dir: SplitDir) -> Option<(u16, u16)> {
        if self.connection.zoomed.is_some() {
            return None;
        }
        let [w, h] = self.connection.areas.get(target).copied()?;
        let half = |grid: f32| (grid + 2.0 * PAD - SPACE.card_gap) / 2.0 - 2.0 * PAD;
        let area = match dir {
            SplitDir::Horizontal => [half(w), h],
            SplitDir::Vertical => [w, half(h)],
        };
        self.spawn_size(area)
    }

    /// A tab's terminal fills the terminal area inside its card's padding:
    /// the split tree's, or the one pane's grid when there is no split.
    pub(super) fn tab_spawn_size(&self) -> Option<(u16, u16)> {
        let area = match &self.connection.layout {
            Some(LayoutNode::Split { .. }) => self
                .split_area()
                .map(|[w, h]| [w - 2.0 * PAD, h - 2.0 * PAD]),
            _ => self
                .connection
                .shown
                .as_ref()
                .and_then(|s| self.connection.areas.get(&s.resource).copied()),
        }?;
        self.spawn_size(area)
    }

    fn spawn_size(&self, area: [f32; 2]) -> Option<(u16, u16)> {
        (area[0] >= 1.0 && area[1] >= 1.0).then(|| self.metrics().fit(area))
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
        self.connection.unplaced_splits.push(Unplaced {
            terminal: new.clone(),
            session,
            place,
            since: Instant::now(),
        });
        self.place_pending_splits(client);
    }

    pub(super) fn placement_session(&self, client: &Client) -> Option<SessionKey> {
        let selected = client.selected_session()?;
        self.connection
            .topology
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
        let before = self.connection.unplaced_splits.len();
        self.connection
            .unplaced_splits
            .retain(|item| Some(item.session) == selected);
        let write = self
            .connection
            .split_write
            .take_if(|write| Some(write.item.session) != selected);
        let cancelled = before != self.connection.unplaced_splits.len() || write.is_some();
        if let Some(write) = write {
            // A previewed split asked for the keyboard; it stays where it is.
            if write.preview.is_some() {
                self.connection
                    .wanted
                    .take_if(|(terminal, _)| *terminal == write.item.terminal);
            }
            if let Some(operation) = write.sent {
                self.connection.layout_ops.remove(&operation);
            }
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
        if self.connection.receipts.waiting() {
            return;
        }
        self.cancel_foreign_splits(client);
        self.finish_split(client);
        while self.connection.split_write.is_none() && !self.connection.unplaced_splits.is_empty() {
            if self.arrangement(client).is_none() {
                self.expire_splits();
                return;
            }
            let base = self.arrangement_revision(client);
            let item = self.connection.unplaced_splits.remove(0);
            let written = match &item.place {
                Some((target, dir)) => self.write_split(client, target, &item.terminal, *dir),
                None => self.write_tab(client, &item.terminal).map(|op| (op, None)),
            };
            match written {
                Some((operation, preview)) => {
                    // A previewed split is on screen: the keyboard follows it
                    // now, as the terminal UI's does.
                    if preview.is_some() {
                        self.want(item.terminal.clone());
                    }
                    self.connection.split_write = Some(SplitWrite {
                        sent: Some(operation),
                        refused: false,
                        base,
                        preview,
                        item,
                    });
                }
                // Not writable yet (the replica hasn't heard of the new
                // terminal): the next poll tries again.
                None if item.since.elapsed() < PATIENCE => {
                    self.connection.unplaced_splits.insert(0, item);
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
        let Some(write) = &self.connection.split_write else {
            return;
        };
        // An answer lost to a reconnect mustn't hold up every later split.
        let late = write.item.since.elapsed() >= PATIENCE;
        let unseen = self.arrangement_revision(client) == write.base;
        if (write.sent.is_some() || unseen) && !late {
            return;
        }
        let Some(SplitWrite {
            refused,
            preview,
            item,
            ..
        }) = self.connection.split_write.take()
        else {
            return;
        };
        match refused {
            // A previewed split took the keyboard when it was written; it
            // stays wherever the person has moved it since.
            false if preview.is_some() => {}
            false => self.want(item.terminal),
            true if late => self.give_up_split(item.terminal),
            true => self.connection.unplaced_splits.insert(0, item),
        }
    }

    /// Splits that have waited too long show on their own.
    fn expire_splits(&mut self) {
        let waiting = std::mem::take(&mut self.connection.unplaced_splits);
        for item in waiting {
            if item.since.elapsed() < PATIENCE {
                self.connection.unplaced_splits.push(item);
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
                if !self.connection.layout_ops.remove(&operation) {
                    return false;
                }
                self.layout_written(client, operation, !other.succeeded());
                true
            }
        }
    }

    /// Drop a failed/uncertain write without focusing or retrying old handles.
    pub(super) fn abandon_layout(&mut self, operation: OperationId, reason: &str) -> bool {
        if !self.connection.layout_ops.remove(&operation) {
            return false;
        }
        if self
            .connection
            .split_write
            .as_ref()
            .is_some_and(|w| w.sent == Some(operation))
        {
            self.connection.split_write = None;
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
            .connection
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
            .connection
            .topology
            .as_ref()
            .and_then(|t| t.session_keyed(&session)?.arrangement);
        if let Some(arrangement) = session {
            let _ = client.fetch_arrangement(arrangement);
        }
    }

    /// The arrangement a split in flight wrote, while the replica catches up.
    /// A refused write shows the replica's again until it is retried.
    pub(super) fn split_preview(&self, client: &Client) -> Option<ArrangementDoc> {
        let write = self
            .connection
            .split_write
            .as_ref()
            .filter(|w| !w.refused)?;
        let (session, doc) = write.preview.as_ref()?;
        (self.placement_session(client) == Some(*session)).then(|| doc.clone())
    }

    fn write_split(
        &mut self,
        client: &Client,
        target: &ResourceId,
        new: &ResourceId,
        dir: SplitDir,
    ) -> Option<(OperationId, Option<(SessionKey, ArrangementDoc)>)> {
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
                written = self
                    .submit_arrangement(client, session, &doc)
                    .ok()
                    .map(|operation| (operation, Some((session, doc))));
            }
        }
        written
    }
}
