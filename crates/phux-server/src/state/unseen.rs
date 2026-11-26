//! `phux.unseen/v1`: what happened in a pane since anyone last looked
//! (`docs/scripting.md#unseen-marks`).
//!
//! Output, a bell and an agent finishing set it, unless some attached view
//! reports showing the pane (`phux.in-view/v1`). Coming into view, or a
//! client's `DELETE_METADATA`, sets it to `seen` for everyone.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{
    AgentEvent, PaneUnseen, RESOURCE_IN_VIEW_KEY, RESOURCE_UNSEEN_KEY, Scope,
};

use super::{ClientId, ServerState};
use crate::upgrade::blob::{MetadataBlob, StateBlob};

/// Why a client's write of `value` at `(scope, key)` is refused: only the
/// server sets a mark, and a view reports per Terminal.
pub(crate) fn refusal(scope: &Scope, key: &str, value: &[u8]) -> Option<&'static str> {
    match key {
        RESOURCE_UNSEEN_KEY => Some("server-set; delete it to mark the pane seen"),
        RESOURCE_IN_VIEW_KEY
            if !matches!(scope, Scope::Resource(_)) || !matches!(value, b"true" | b"false") =>
        {
            Some("want a Terminal scope and true or false")
        }
        _ => None,
    }
}

impl ServerState {
    /// Mark the pane an engine event is about.
    ///
    /// ponytail: `dirty` opens a burst and `idle` closes it, so a pane left
    /// mid-burst is marked only when it settles, and one left within a tick
    /// of its last output is marked by that output's `idle`. An output
    /// counter per view would be exact.
    pub(crate) fn mark_unseen_event(&mut self, pane: &WireResourceId, event: &AgentEvent) {
        match event {
            AgentEvent::Bell => self.mark_unseen(pane, PaneUnseen::Bell),
            AgentEvent::Dirty | AgentEvent::Idle => self.mark_unseen(pane, PaneUnseen::Output),
            _ => {}
        }
    }

    /// Mark a pane whose agent record went from under way to `done`.
    pub(super) fn mark_unseen_finish(
        &mut self,
        pane: &WireResourceId,
        before: Option<&[u8]>,
        after: &[u8],
    ) {
        use phux_agent_session::state::stored_state;
        if matches!(stored_state(before).as_deref(), Some("working" | "blocked"))
            && stored_state(Some(after)).as_deref() == Some("done")
        {
            self.mark_unseen(pane, PaneUnseen::Finished);
        }
    }

    fn mark_unseen(&mut self, pane: &WireResourceId, mark: PaneUnseen) {
        // A satellite's marks are its own server's.
        if !pane.is_local() || self.in_view(pane) {
            return;
        }
        let scope = Scope::Resource(pane.clone());
        let held = self.metadata().get(&scope, RESOURCE_UNSEEN_KEY);
        if held.as_deref().and_then(PaneUnseen::decode) < Some(mark) {
            let _ = self.metadata_set(
                &scope,
                RESOURCE_UNSEEN_KEY,
                mark.as_str().as_bytes().to_vec(),
            );
        }
    }

    fn in_view(&self, pane: &WireResourceId) -> bool {
        self.attached().values().any(|c| c.in_view.contains(pane))
    }

    /// `client` reports `pane` came into (`shown`) or left its view. A client
    /// with no attachment has no view to hold, so its report is one look.
    pub(crate) fn set_in_view(&mut self, client: ClientId, pane: &WireResourceId, shown: bool) {
        let idspace = &self.idspace;
        if let Some(attached) = self.clients.attached.get_mut(&client) {
            // A closed pane is in no view.
            (attached.in_view).retain(|pane| idspace.resource_from_wire(pane).is_some());
            if shown {
                attached.in_view.insert(pane.clone());
            } else {
                attached.in_view.remove(pane);
            }
        }
        if shown {
            let _ = self.note_seen(client, pane);
        }
    }

    /// `client` looked at `pane`, or says someone did. Returns the clients
    /// told.
    pub(crate) fn note_seen(&mut self, client: ClientId, pane: &WireResourceId) -> Vec<ClientId> {
        let seen = PaneUnseen::Seen.as_str().as_bytes().to_vec();
        let scope = Scope::Resource(pane.clone());
        self.metadata_set_by(&scope, RESOURCE_UNSEEN_KEY, seen, Some(client))
    }

    /// The marks an upgrade carries: pane metadata does not cross by itself.
    pub(super) fn unseen_blobs(&self) -> impl Iterator<Item = MetadataBlob> + '_ {
        self.metadata
            .local_entries(RESOURCE_UNSEEN_KEY)
            .map(|(pane, value)| MetadataBlob {
                group: None,
                pane: Some(pane),
                key: RESOURCE_UNSEEN_KEY.to_owned(),
                value: value.to_vec(),
            })
    }

    /// Put back a carried mark, if its pane crossed.
    pub(super) fn restore_unseen(&mut self, blob: &StateBlob, pane: u32, value: &[u8]) {
        if blob.panes.iter().any(|p| p.wire_id == pane) && PaneUnseen::decode(value).is_some() {
            let scope = Scope::Resource(WireResourceId::local(pane));
            let _ = self
                .metadata
                .set(&scope, RESOURCE_UNSEEN_KEY, value.to_vec());
        }
    }
}

#[cfg(test)]
mod tests;
