//! A watch-to-window transfer retains the original id space until the
//! destination has captured its own exact presentation source.

use super::{Watch, WatchScope};
use crate::desk::{Desk, NoticeKind};
use phux_client_runtime::{Client, control::Status};
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::AttachTarget;

pub(in crate::desk) struct Handoff {
    watch: Watch,
    origin: WatchScope,
    terminal: ResourceId,
    choice: u64,
    installed: bool,
}

impl Handoff {
    fn current_origin(&self) -> bool {
        self.origin.is_current(&self.watch.client)
    }
}

impl Desk {
    pub(in crate::desk) fn handoff_agent(&mut self, machine: Option<String>, terminal: ResourceId) {
        let Some(mut watch) = self.fleet.watches.remove(&machine) else {
            return self.say(
                NoticeKind::Error,
                "The agent's original watch is unavailable.",
            );
        };
        let Some(origin) = watch.scope.take() else {
            return self.say(
                NoticeKind::Error,
                "The agent's original source is unavailable; no pane was selected.",
            );
        };
        if !origin.is_current(&watch.client)
            || !watch
                .topology
                .as_ref()
                .is_some_and(|topology| topology.pane(&terminal).is_some())
        {
            return self.say(
                NoticeKind::Error,
                "The agent's original source changed; no pane was selected.",
            );
        }
        let previous = self.connection.namespace();
        self.switch_machine(machine.clone());
        if self.machines.current != machine || self.connection.namespace() == previous {
            watch.scope = Some(origin);
            self.fleet.watches.insert(machine, watch);
            return;
        }
        self.connection.fleet_handoff = Some(Handoff {
            watch,
            origin,
            terminal,
            choice: self.connection.view_choice,
            installed: false,
        });
    }

    /// The original watch owns the pending interest, not a numeric id saved
    /// through runtime replacement. A fresh destination observation resolves
    /// only that same physical target in the original server's id space.
    pub(in crate::desk) fn settle_fleet_handoff(&mut self, client: &Client) -> bool {
        if !self.connection.owns_client(client) {
            return false;
        }
        let Some(mut handoff) = self.connection.fleet_handoff.take() else {
            return false;
        };
        if handoff.choice != self.connection.view_choice
            || self.connection.pending_session.is_some()
        {
            return false;
        }
        if handoff.installed
            && !self
                .connection
                .wanted
                .as_ref()
                .is_some_and(|(id, _)| id == &handoff.terminal)
            && !self
                .connection
                .shown
                .as_ref()
                .is_some_and(|shown| shown.resource == handoff.terminal)
        {
            self.connection.view_intents.remove(&handoff.terminal);
            return false;
        }
        if !handoff.current_origin() {
            self.retire_agent_handoff(&handoff, "The agent's source changed during navigation.");
            return true;
        }
        let ready = client.with_control(|control| {
            if let Some(server) = control.server()
                && server.id != handoff.origin.server
            {
                return Err(
                    "The destination reached a different server; no agent view was installed.",
                );
            }
            if control.status().is_terminal() {
                return Err(
                    "The destination connection ended before the agent view was available.",
                );
            }
            if control.status() != Status::Attached
                || self.connection.view_epoch != control.connection_epoch()
                || self.connection.view_server.as_deref() != Some(handoff.origin.server.as_slice())
            {
                return Ok(false);
            }
            let Some(session) = control
                .topology()
                .and_then(|topology| topology.pane(&handoff.terminal))
                .map(|pane| pane.session_id)
            else {
                return Err("The original agent terminal is not in the destination inventory.");
            };
            if control.selected_session() != Some(session) {
                control.attach_session(AttachTarget::ById(SessionId::new(session)));
            }
            Ok(true)
        });
        match ready {
            Err(message) => {
                self.retire_agent_handoff(&handoff, message);
                true
            }
            Ok(false) => {
                self.connection.fleet_handoff = Some(handoff);
                false
            }
            Ok(true) => {
                let changed = !handoff.installed;
                if changed {
                    self.want(handoff.terminal.clone());
                    handoff.choice = self.connection.view_choice;
                    handoff.installed = true;
                }
                let intent = self.connection.view_intents.get(&handoff.terminal);
                if intent.is_some_and(super::super::view_intent::ViewIntent::refused) {
                    self.retire_agent_handoff(
                        &handoff,
                        "The agent presentation source was retired before admission.",
                    );
                    true
                } else if intent.is_some_and(super::super::view_intent::ViewIntent::has_source) {
                    true
                } else {
                    self.connection.fleet_handoff = Some(handoff);
                    changed
                }
            }
        }
    }

    fn retire_agent_handoff(&mut self, handoff: &Handoff, message: &str) {
        if handoff.installed && handoff.choice == self.connection.view_choice {
            self.connection
                .wanted
                .take_if(|(terminal, _)| terminal == &handoff.terminal);
            self.connection.view_intents.remove(&handoff.terminal);
        }
        self.say(NoticeKind::Error, message);
    }
}

#[cfg(test)]
mod tests;
