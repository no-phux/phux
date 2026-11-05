//! Document pumps use the runtime's explicit resource-admission extension.
//! They are viewers, carry no viewport, and never allocate a terminal view.

use phux_client_runtime::{Client, control::Status};
use phux_protocol::{
    ResourceId,
    wire::frame::{Command, CommandResult, RolePolicy},
};

use super::{Body, Desk};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Subscription {
    Unrequested,
    Pending(u32),
    Active,
    Refused,
}

impl Desk {
    pub(in crate::desk) fn subscribe_document(&mut self, client: &Client, id: &ResourceId) {
        let Some(document) = self.resources.entries.get_mut(id) else {
            return;
        };
        if client.status() != Status::Attached
            || matches!(
                document.subscription,
                Subscription::Pending(_) | Subscription::Refused
            )
        {
            return;
        }
        let request = client.with_control(|control| {
            if control.terminal_is_admitted(id) {
                return None;
            }
            let role = Some(RolePolicy::VIEWER);
            control.admit_external_attach(id, role);
            Some(control.send_command(Command::attach_terminal(id.clone(), role)))
        });
        document.subscription = request.map_or(Subscription::Active, Subscription::Pending);
    }

    pub(in crate::desk) fn document_subscription_result(
        &mut self,
        client: &Client,
        request: u32,
        result: CommandResult,
    ) -> bool {
        let Some((id, document)) = self
            .resources
            .entries
            .iter_mut()
            .find(|(_, d)| d.subscription == Subscription::Pending(request))
        else {
            return false;
        };
        if matches!(result, CommandResult::Ok | CommandResult::OkWith(_)) {
            document.subscription = Subscription::Active;
            return true;
        }
        client.with_control(|control| control.release_terminal(id));
        document.subscription = Subscription::Refused;
        document.seq = None;
        document.body = Body::Invalid(format!(
            "Mirror subscription failed: {result:?}. Reopen this mirror from the palette to retry."
        ));
        self.generation += 1;
        true
    }

    pub(in crate::desk) fn document_detached(&mut self, id: &ResourceId) -> bool {
        let Some(document) = self.resources.entries.get_mut(id) else {
            return false;
        };
        document.subscription = Subscription::Unrequested;
        document.seq = None;
        document.body = Body::Pending;
        true
    }

    pub(in crate::desk) fn retry_document(&mut self, id: &ResourceId) {
        let Some(document) = self.resources.entries.get_mut(id) else {
            return;
        };
        if document.subscription == Subscription::Refused {
            document.subscription = Subscription::Unrequested;
            document.body = Body::Pending;
        }
    }
}
