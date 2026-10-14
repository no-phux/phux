//! Desktop organization is presentation state only. The runtime owns mutation,
//! authorization, revisions, receipts, and undo; no terminal is recreated here.

use gpui::{Context, Window};
use phux_client_core::org_ops::OrgOutcome;
use phux_protocol::ids::{OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Committed, MAX_ORG_BATCH};

use super::desk::{Desk, Modal};
use super::notifications::Kind;

#[derive(Default)]
pub(crate) struct Organization {
    pub selected: Vec<ResourceKey>,
    pub undo: Option<Box<Committed>>,
    pending: Option<(OperationId, Edit)>,
}

#[derive(Clone)]
pub(crate) enum Edit {
    Move { count: usize, destination: String },
    Resolve(String),
    Undo,
}

impl Edit {
    fn title(&self) -> String {
        match self {
            Self::Move { count, destination } => format!("Moved {count} to {destination}"),
            Self::Resolve(name) => format!("Organized {name}"),
            Self::Undo => "Organization undone".to_owned(),
        }
    }
}

impl Organization {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn toggle(&mut self, resource: ResourceKey) -> Result<(), &'static str> {
        if self.busy() {
            return Err("Wait for the current move to finish.");
        }
        if self.selected.contains(&resource) {
            self.selected.retain(|key| key != &resource);
            return Ok(());
        }
        if self.selected.len() == MAX_ORG_BATCH {
            return Err("This selection has reached the server's atomic move limit.");
        }
        self.selected.push(resource);
        Ok(())
    }

    pub fn drag_keys(&self, resource: ResourceKey) -> Vec<ResourceKey> {
        if self.selected.contains(&resource) {
            self.selected.clone()
        } else {
            vec![resource]
        }
    }

    pub fn select(&mut self, keys: Vec<ResourceKey>) -> Result<(), &'static str> {
        if self.busy() {
            return Err("Wait for the current move to finish.");
        }
        if keys.len() > MAX_ORG_BATCH {
            return Err("Select a smaller group; one atomic move holds up to 256 resources.");
        }
        self.selected = keys;
        Ok(())
    }

    pub fn begin(&mut self, operation: OperationId, edit: Edit) {
        self.pending = Some((operation, edit));
    }

    /// Returns only outcomes submitted by this window, never another client's edits.
    pub fn finish(
        &mut self,
        operation: OperationId,
        outcome: &OrgOutcome,
    ) -> Option<(bool, String)> {
        if self.pending.as_ref()?.0 != operation {
            return None;
        }
        let (_, edit) = self.pending.take()?;
        if let OrgOutcome::Committed(receipt) = outcome {
            self.selected.clear();
            self.undo = match edit {
                Edit::Undo => None,
                _ => Some(receipt.clone()),
            };
            return Some((true, edit.title()));
        }
        if matches!(edit, Edit::Undo) && !matches!(outcome, OrgOutcome::NotSent { .. }) {
            self.undo = None;
        }
        Some((false, crate::connection::org::outcome_sentence(outcome)))
    }

    /// Do not carry a selectable/undoable target into a different connection.
    pub fn connection_changed(&mut self) {
        self.selected.clear();
        self.undo = None;
    }
}

impl Desk {
    pub(crate) fn run_organization(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match id {
            "move-resources" => self.open_move(window, cx),
            "select-session-work" => self.select_current_work(cx),
            "undo-organization" => self.undo_organization(cx),
            _ => return false,
        }
        true
    }

    pub(crate) fn select_current_work(&mut self, cx: &mut Context<Self>) {
        let session = self
            .focused_pane()
            .map(|pane| pane.session_id)
            .or_else(|| self.link.as_ref()?.connection.home_session());
        if let Some(session) = session {
            self.select_session_work(session, cx);
        }
    }

    pub(crate) fn select_session_work(&mut self, session: u32, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let replica = client.organization();
        let key = self
            .link
            .as_ref()
            .and_then(|link| link.connection.topology.as_ref())
            .and_then(|topology| topology.sessions.iter().find(|row| row.id == session))
            .and_then(|row| row.key);
        let Some(key) = key else { return };
        let keys = replica
            .resources()
            .iter()
            .filter(|resource| resource.session == key)
            .map(|resource| resource.key)
            .collect();
        if let Err(reason) = self.organization.select(keys) {
            self.post(Kind::Info, "Selection unchanged", Some(reason.to_owned()));
        }
        cx.notify();
    }

    pub(crate) fn toggle_resource_selection(&mut self, key: ResourceKey, cx: &mut Context<Self>) {
        if let Err(reason) = self.organization.toggle(key) {
            self.post(Kind::Info, "Selection unchanged", Some(reason.to_owned()));
        }
        cx.notify();
    }

    pub(crate) fn open_move(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.organization.selected.is_empty() {
            let key = self.focused_pane().and_then(|pane| pane.key);
            let Some(key) = key else {
                self.post(Kind::Info, "Select work to move", None);
                return;
            };
            self.organization.selected.push(key);
        }
        if !self.organization.busy() {
            self.open_modal(Modal::MoveResources, window, cx);
        }
    }

    pub(crate) fn move_resources_to(
        &mut self,
        keys: Vec<ResourceKey>,
        destination: SessionKey,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if keys.is_empty() || self.organization.busy() {
            return;
        }
        if keys.len() > MAX_ORG_BATCH {
            self.post(
                Kind::Error,
                "Selection too large",
                Some(format!("Move up to {MAX_ORG_BATCH} resources at a time.")),
            );
            return;
        }
        let Some(client) = self.client() else { return };
        let replica = client.organization();
        let keys: Vec<_> = keys
            .into_iter()
            .filter(|key| {
                replica
                    .resource(key)
                    .is_none_or(|resource| resource.session != destination)
            })
            .collect();
        if keys.is_empty() {
            return;
        }
        match client.move_resources(&keys, destination) {
            Ok(operations) => {
                if let Some(operation) = operations.first() {
                    self.organization.begin(
                        *operation,
                        Edit::Move {
                            count: keys.len(),
                            destination: name,
                        },
                    );
                    self.close_modal(window, cx);
                    self.post(Kind::Info, "Moving work…", None);
                }
            }
            Err(error) => self.post(Kind::Error, "Could not move work", Some(error.to_string())),
        }
        self.after_command(window, cx);
    }

    pub(crate) fn resolve_intake(
        &mut self,
        session: SessionKey,
        name: String,
        cx: &mut Context<Self>,
    ) {
        if self.organization.busy() {
            return;
        }
        let Some(client) = self.client() else { return };
        match client.inbox_resolve(session) {
            Ok(operation) => self.organization.begin(operation, Edit::Resolve(name)),
            Err(error) => self.post(
                Kind::Error,
                "Could not organize session",
                Some(error.to_string()),
            ),
        }
        cx.notify();
    }

    pub(crate) fn undo_organization(&mut self, cx: &mut Context<Self>) {
        if self.organization.busy() {
            return;
        }
        let Some(receipt) = self.organization.undo.as_ref() else {
            return;
        };
        let Some(client) = self.client() else { return };
        match client.undo(receipt) {
            Ok(operation) => self.organization.begin(operation, Edit::Undo),
            Err(error) => self.post(
                Kind::Error,
                "Could not undo organization",
                Some(error.to_string()),
            ),
        }
        cx.notify();
    }

    pub(crate) fn note_organization(&mut self, operation: OperationId, outcome: &OrgOutcome) {
        if let Some((success, sentence)) = self.organization.finish(operation, outcome) {
            let kind = if success { Kind::Success } else { Kind::Error };
            self.post(
                kind,
                if success {
                    &sentence
                } else {
                    "Organization did not finish"
                },
                (!success).then_some(sentence.clone()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::Revision;
    use phux_protocol::wire::frame::Refusal;

    fn key(byte: u8) -> ResourceKey {
        ResourceKey::new([byte; 16]).unwrap()
    }
    fn operation(byte: u8) -> OperationId {
        OperationId::new([byte; 16]).unwrap()
    }
    fn move_edit() -> Edit {
        Edit::Move {
            count: 2,
            destination: "Work".to_owned(),
        }
    }

    #[test]
    fn selection_and_drag_share_the_same_fixed_resource_keys() {
        let mut state = Organization::default();
        state.toggle(key(1)).unwrap();
        state.toggle(key(2)).unwrap();
        assert_eq!(state.drag_keys(key(1)), vec![key(1), key(2)]);
        assert_eq!(state.drag_keys(key(3)), vec![key(3)]);
        state.toggle(key(1)).unwrap();
        assert_eq!(state.selected, vec![key(2)]);
    }

    #[test]
    fn moves_only_offer_undo_after_their_own_commit() {
        let mut state = Organization {
            selected: vec![key(1), key(2)],
            ..Default::default()
        };
        state.begin(operation(1), move_edit());
        let receipt = Committed::new(operation(1), Revision::new(2));
        let outcome = OrgOutcome::Committed(Box::new(receipt));
        assert_eq!(state.finish(operation(2), &outcome), None);
        assert!(state.undo.is_none());
        assert!(state.busy());
        assert!(state.toggle(key(3)).is_err());
        assert_eq!(
            state.finish(operation(1), &outcome),
            Some((true, "Moved 2 to Work".to_owned()))
        );
        assert!(state.selected.is_empty());
        assert_eq!(state.undo.as_ref().unwrap().operation, operation(1));
    }

    #[test]
    fn refused_move_keeps_selection_and_undo_does_not_offer_fake_redo() {
        let mut state = Organization {
            selected: vec![key(1)],
            ..Default::default()
        };
        state.begin(operation(1), move_edit());
        let refused = OrgOutcome::Refused(Refusal::NotFound);
        assert!(!state.finish(operation(1), &refused).unwrap().0);
        assert_eq!(state.selected, vec![key(1)]);
        state.begin(operation(2), Edit::Undo);
        let outcome =
            OrgOutcome::Committed(Box::new(Committed::new(operation(2), Revision::new(3))));
        assert!(state.finish(operation(2), &outcome).unwrap().0);
        assert!(state.undo.is_none());
    }

    #[test]
    fn connection_boundary_clears_stale_selection_and_undo() {
        let mut state = Organization {
            selected: vec![key(1)],
            undo: Some(Box::new(Committed::new(operation(1), Revision::new(2)))),
            ..Default::default()
        };
        state.connection_changed();
        assert!(state.selected.is_empty());
        assert!(state.undo.is_none());
    }

    #[test]
    fn queued_commit_before_connection_boundary_cannot_restore_stale_undo() {
        let mut state = Organization::default();
        state.begin(operation(1), move_edit());
        let outcome =
            OrgOutcome::Committed(Box::new(Committed::new(operation(1), Revision::new(2))));
        assert!(state.finish(operation(1), &outcome).unwrap().0);
        state.connection_changed();
        assert!(state.undo.is_none());
    }

    #[test]
    fn definitely_unsent_undo_keeps_the_original_receipt() {
        let mut state = Organization {
            undo: Some(Box::new(Committed::new(operation(1), Revision::new(2)))),
            ..Default::default()
        };
        state.begin(operation(2), Edit::Undo);
        let outcome = OrgOutcome::NotSent {
            reason: "connection ended before write".to_owned(),
        };
        assert!(!state.finish(operation(2), &outcome).unwrap().0);
        assert_eq!(state.undo.as_ref().unwrap().operation, operation(1));
        assert!(!state.busy());
    }

    #[test]
    fn bulk_selection_is_atomic_bounded_and_does_not_replace_selection_on_failure() {
        let mut state = Organization {
            selected: vec![key(1)],
            ..Default::default()
        };
        assert!(state.select(vec![key(2); MAX_ORG_BATCH + 1]).is_err());
        assert_eq!(state.selected, vec![key(1)]);
    }

    /// Uses only the explicitly named disposable server; never discovers a socket.
    #[test]
    #[ignore = "requires PHUX_DESKTOP_ORG_TEST_SOCKET with org-source (two live resources) and empty org-destination"]
    fn live_move_and_undo_preserve_execution_identity() {
        use crate::connection::Connection;

        let socket = std::env::var_os("PHUX_DESKTOP_ORG_TEST_SOCKET")
            .expect("explicit isolated test socket");
        let mut connection = Connection::connect(socket.into(), "org-source".to_owned()).unwrap();
        wait_for(&mut connection, |connection| {
            connection.client.organization().resources().len() >= 2
        });
        let replica = connection.client.organization();
        let source = replica
            .sessions()
            .iter()
            .find(|session| session.name == "org-source")
            .unwrap()
            .key;
        let destination = replica
            .sessions()
            .iter()
            .find(|session| session.name == "org-destination")
            .unwrap()
            .key;
        let before: Vec<_> = replica
            .resources()
            .iter()
            .filter(|resource| resource.session == source)
            .map(|resource| (resource.key, resource.live.clone(), resource.activation))
            .collect();
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|(_, live, _)| live.is_some()));
        let keys: Vec<_> = before.iter().map(|(key, _, _)| *key).collect();
        let mut state = Organization::default();
        state.select(keys.clone()).unwrap();
        let operations = connection
            .client
            .move_resources(&keys, destination)
            .unwrap();
        assert_eq!(operations.len(), 1);
        state.begin(operations[0], move_edit());
        wait_edit(&mut connection, &mut state);
        wait_for(&mut connection, |connection| {
            keys.iter().all(|key| {
                connection
                    .client
                    .organization()
                    .resource(key)
                    .is_some_and(|resource| resource.session == destination)
            })
        });
        let undo = connection
            .client
            .undo(state.undo.as_ref().unwrap())
            .unwrap();
        state.begin(undo, Edit::Undo);
        wait_edit(&mut connection, &mut state);
        wait_for(&mut connection, |connection| {
            keys.iter().all(|key| {
                connection
                    .client
                    .organization()
                    .resource(key)
                    .is_some_and(|resource| resource.session == source)
            })
        });
        let replica = connection.client.organization();
        for (key, live, activation) in before {
            let resource = replica.resource(&key).unwrap();
            assert_eq!(resource.live, live);
            assert_eq!(resource.activation, activation);
        }
        assert!(state.undo.is_none());
    }

    fn wait_edit(connection: &mut crate::connection::Connection, state: &mut Organization) {
        use phux_client_runtime::control::Event;
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.busy() {
            assert!(Instant::now() < deadline, "organization result deadline");
            for event in connection.drain() {
                if let Event::OrganizationOperation { operation, outcome } = event
                    && let Some((success, sentence)) = state.finish(operation, &outcome)
                {
                    assert!(success, "{sentence}");
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for(
        connection: &mut crate::connection::Connection,
        predicate: impl Fn(&crate::connection::Connection) -> bool,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !predicate(connection) {
            assert!(std::time::Instant::now() < deadline, "replica deadline");
            connection.drain();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
