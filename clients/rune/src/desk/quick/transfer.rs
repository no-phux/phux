//! A move is submitted on the main window's client. Only its correlated
//! server receipt may dismiss the panel, replace its shell or raise main.

use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::Client;
use phux_protocol::ids::{OperationId, ResourceId, SessionKey};

use super::super::{Desk, NoticeKind};

type Completion = Box<dyn FnOnce(Result<(), String>) + Send>;

pub(in crate::desk) struct Transfer {
    operation: OperationId,
    terminal: ResourceId,
    session: SessionKey,
    finish: Completion,
    intent: Option<super::super::view_intent::ViewIntent>,
    choice: u64,
}

impl Desk {
    /// Keep the panel usable while main asks the server to move its terminal.
    pub fn move_to_tab(&mut self) {
        let origin = self.connection.callback_fence();
        let server = self.connection.view_server.clone();
        let (Some(panel), Some(shown)) = (&mut self.quick, &self.connection.shown) else {
            return;
        };
        if panel.moving {
            return;
        }
        panel.moving = true;
        self.connection.view_choice += 1;
        let choice = self.connection.view_choice;
        let terminal = shown.resource.clone();
        let view = shown.view;
        let me = panel.me.clone();
        panel.main.update(move |desk, _| {
            if !origin
                .as_ref()
                .is_some_and(|origin| origin.permits_view(view))
            {
                me.update(move |desk, _| {
                    if origin
                        .as_ref()
                        .is_some_and(|origin| origin.same_namespace(&desk.connection))
                        && desk.connection.view_choice == choice
                    {
                        desk.quick_move_finished(Err(
                            "The original quick terminal view is no longer available.".to_owned(),
                        ));
                    }
                });
                return;
            }
            desk.adopt_quick(
                terminal,
                server,
                Box::new(move |result| {
                    me.update(move |desk, _| {
                        if origin
                            .as_ref()
                            .is_some_and(|origin| origin.is_current(&desk.connection))
                            && desk.connection.view_choice == choice
                        {
                            desk.quick_move_finished(result);
                        }
                    });
                }),
            );
        });
    }

    fn adopt_quick(&mut self, terminal: ResourceId, server: Option<Vec<u8>>, finish: Completion) {
        if self.connection.wanted.is_some() {
            finish(Err(
                "The main window's pending selection is kept; retry after it opens.".to_owned(),
            ));
            return;
        }
        let intent = self.connection.client.as_ref().map(|client| {
            super::super::view_intent::ViewIntent::new(
                client,
                &terminal,
                server,
                self.connection.view_epoch,
            )
        });
        if intent.as_ref().is_some_and(|intent| intent.refused()) {
            finish(Err(
                "The quick terminal's original server view is no longer available.".to_owned(),
            ));
            return;
        }
        match self.submit_quick_move(&terminal) {
            Ok((operation, session)) => {
                self.connection.quick_move = Some(Transfer {
                    operation,
                    terminal,
                    session,
                    finish,
                    intent,
                    choice: self.connection.view_choice,
                });
            }
            Err(error) => finish(Err(error)),
        }
    }

    fn submit_quick_move(
        &self,
        terminal: &ResourceId,
    ) -> Result<(OperationId, SessionKey), String> {
        if self.connection.quick_move.is_some() {
            return Err("A quick terminal move is already waiting for the server.".to_owned());
        }
        if self.machines.current.is_some() {
            return Err(
                "This window is on another machine, so the quick terminal stays where it is."
                    .to_owned(),
            );
        }
        let client = self.attached_client().ok_or(
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
        let session = self.selected_entry(client)?;
        session.key.filter(|_| session.name != super::SESSION)
    }

    pub(in crate::desk) fn owns_quick_move(&self, operation: OperationId) -> bool {
        self.connection
            .quick_move
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
        let mut pending = self.connection.quick_move.take().expect("matched move");
        let result = if matches!(outcome, OrgOutcome::Committed(_)) {
            self.place_quick_tab(client, &mut pending)
        } else {
            Err(format!(
                "Quick terminal move operation {} did not settle: {outcome:?}. Check the original operation and the server's panes before retrying.",
                phux_protocol::ids::hex16(operation.as_bytes())
            ))
        };
        (pending.finish)(result);
        true
    }

    /// Use the original destination even if the person selected another
    /// session while waiting. Stable identity survives a session rename.
    fn place_quick_tab(&mut self, client: &Client, pending: &mut Transfer) -> Result<(), String> {
        if pending.choice != self.connection.view_choice {
            return Err("The terminal moved, but the window's newer selection is kept.".to_owned());
        }
        let intent = pending
            .intent
            .take()
            .ok_or("The quick terminal has no original view scope.")?;
        if !intent.select_session(client, &pending.session) {
            return Err("The terminal moved, but its exact destination or original view scope is unavailable.".to_owned());
        }
        self.connection
            .view_intents
            .insert(pending.terminal.clone(), intent);
        self.place_split(client, &pending.terminal, None, Some(pending.session), true);
        Ok(())
    }

    pub(in crate::desk) fn cancel_quick_move(&mut self, reason: &str) -> bool {
        let Some(pending) = self.connection.quick_move.take() else {
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
