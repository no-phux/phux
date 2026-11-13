//! A move is submitted on the main window's client. Only its correlated
//! server receipt may dismiss the panel, replace its shell or raise main.

use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::{Client, control::Status};
use phux_protocol::ids::{OperationId, ResourceId, SessionId, SessionKey};
use phux_protocol::wire::frame::AttachTarget;

use super::super::{Desk, NoticeKind};

type Completion = Box<dyn FnOnce(Result<(), String>) + Send>;

pub(in crate::desk) struct Transfer {
    operation: OperationId,
    terminal: ResourceId,
    session: SessionKey,
    finish: Completion,
}

impl Desk {
    /// Keep the panel usable while main asks the server to move its terminal.
    pub fn move_to_tab(&mut self) {
        let (Some(panel), Some(shown)) = (&mut self.quick, &self.shown) else {
            return;
        };
        if panel.moving {
            return;
        }
        panel.moving = true;
        let terminal = shown.resource.clone();
        let me = panel.me.clone();
        panel.main.update(move |desk, _| {
            desk.adopt_quick(
                terminal,
                Box::new(move |result| {
                    me.update(move |desk, _| desk.quick_move_finished(result));
                }),
            );
        });
    }

    fn adopt_quick(&mut self, terminal: ResourceId, finish: Completion) {
        match self.submit_quick_move(&terminal) {
            Ok((operation, session)) => {
                self.quick_move = Some(Transfer {
                    operation,
                    terminal,
                    session,
                    finish,
                });
            }
            Err(error) => finish(Err(error)),
        }
    }

    fn submit_quick_move(
        &self,
        terminal: &ResourceId,
    ) -> Result<(OperationId, SessionKey), String> {
        if self.quick_move.is_some() {
            return Err("A quick terminal move is already waiting for the server.".to_owned());
        }
        if self.machines.current.is_some() {
            return Err(
                "This window is on another machine, so the quick terminal stays where it is."
                    .to_owned(),
            );
        }
        let client = self
            .client
            .as_ref()
            .filter(|client| client.status() == Status::Attached)
            .ok_or(
                "Connect the main window to this computer first, then move the quick terminal.",
            )?;
        let session = self.quick_destination(client).ok_or(
            "Open a session in the main window first, then move the quick terminal to it.",
        )?;
        let resource = client
            .organization()
            .resource_by_live(terminal)
            .map(|r| r.key)
            .ok_or(
                "The main window has not found the quick terminal yet. Try again once it connects.",
            )?;
        client
            .move_resource(resource, session)
            .map(|operation| (operation, session))
            .map_err(|error| format!("Couldn't move the quick terminal: {error}"))
    }

    fn quick_destination(&self, client: &Client) -> Option<SessionKey> {
        let selected = client.selected_session()?;
        let session = self
            .topology
            .as_ref()?
            .sessions
            .iter()
            .find(|s| s.id == selected)?;
        session.key.filter(|_| session.name != super::SESSION)
    }

    pub(in crate::desk) fn owns_quick_move(&self, operation: OperationId) -> bool {
        self.quick_move
            .as_ref()
            .is_some_and(|pending| pending.operation == operation)
    }

    pub(in crate::desk) fn quick_move_outcome(
        &mut self,
        client: &Client,
        operation: OperationId,
        outcome: &OrgOutcome,
    ) -> bool {
        if !self.owns_quick_move(operation) {
            return false;
        }
        let pending = self.quick_move.take().expect("matched move");
        let result = if outcome.succeeded() {
            self.place_quick_tab(client, &pending)
        } else {
            Err(format!(
                "Quick terminal move did not settle: {outcome:?}. Check the server's panes before retrying."
            ))
        };
        (pending.finish)(result);
        true
    }

    /// Use the original destination even if the person selected another
    /// session while waiting. Stable identity survives a session rename.
    fn place_quick_tab(&mut self, client: &Client, pending: &Transfer) -> Result<(), String> {
        let session = self.topology.as_ref()
            .and_then(|topology| topology.session_keyed(&pending.session))
            .ok_or("The quick terminal moved, but its destination is no longer listed. Check the server's panes.")?;
        client.attach_session(AttachTarget::ById(SessionId::new(session.id)));
        self.place_split(client, &pending.terminal, None, Some(pending.session));
        Ok(())
    }

    pub(in crate::desk) fn cancel_quick_move(&mut self, reason: &str) -> bool {
        let Some(pending) = self.quick_move.take() else {
            return false;
        };
        (pending.finish)(Err(format!(
            "Quick terminal move did not settle: {reason}. Check the server's panes before retrying."
        )));
        true
    }

    /// Failure stays in the window the person is using, without activating
    /// main (which would also dismiss this nonactivating panel).
    fn quick_move_finished(&mut self, result: Result<(), String>) {
        let Some(panel) = &mut self.quick else { return };
        panel.moving = false;
        if let Err(error) = result {
            self.say(NoticeKind::Error, error);
            return;
        }
        let main = panel.main.clone();
        panel.me.hide();
        self.refill_quick();
        main.open();
    }
}

#[cfg(test)]
mod tests;
