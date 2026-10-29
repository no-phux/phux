//! Native discovery and launch on the connected authority, never a local CLI proxy.
use gpui::{Context, Window};
use phux_client_runtime::control::{Event, PaneDescriptor, SpawnRequest, Status, Topology};
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::SessionKey;
use phux_protocol::native_agent::{
    NativeIntegration, NativeIntegrationCatalog, NativeRequest, NativeRequestPayload,
    SpawnedNativeAgent,
};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};
use std::sync::{Arc, Weak};

use super::desk::{Desk, Modal};
use super::notifications::Kind;
use crate::connection::Connection;

mod onboarding;
mod view;
pub(crate) use view::panel;
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct State {
    integrations: Vec<NativeIntegration>,
    discovery: Option<Flight>,
    launch: Option<Launch>,
    login: Option<Flight>,
    destination: Option<Destination>,
    error: Option<String>,
    pending: Option<Pending>,
    _deadline: Option<gpui::Task<()>>,
    setup_open: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Destination {
    session: SessionKey,
    session_id: u32,
    name: String,
    cwd: String,
}

#[derive(Debug)]
struct Flight {
    id: u32,
    owner: Weak<phux_client_runtime::Client>,
    epoch: u64,
}

#[derive(Debug)]
struct Launch {
    flight: Flight,
    integration: String,
    destination: Destination,
}

struct Pending {
    owner: Weak<phux_client_runtime::Client>,
    epoch: u64,
    session: String,
    session_key: Option<SessionKey>,
    terminal: ResourceId,
    agent: ResourceId,
    activation: String,
    revealed: bool,
}

fn owned_by(
    owner: &Weak<phux_client_runtime::Client>,
    client: &Arc<phux_client_runtime::Client>,
) -> bool {
    std::ptr::eq(owner.as_ptr(), Arc::as_ptr(client))
}

const LAUNCHED: &str = "The headless agent runs on the selected host and continues when the window closes. Opening its native conversation; creation does not confirm provider readiness or sign-in.";
const UNKNOWN: &str = "The connection changed before the launch receipt. The agent may already exist. Check the original host's panes before starting another; nothing is retried automatically.";

impl State {
    fn busy(&self) -> bool {
        self.discovery.is_some()
            || self.launch.is_some()
            || self.login.is_some()
            || self.pending.is_some()
    }

    pub(super) fn owns_terminal(
        &self,
        client: &Arc<phux_client_runtime::Client>,
        terminal: &ResourceId,
    ) -> bool {
        self.pending.as_ref().is_some_and(|pending| {
            &pending.terminal == terminal && owned_by(&pending.owner, client)
        })
    }
}

impl Desk {
    pub(crate) fn reset_native_launch(&mut self) {
        if self.native_launch.launch.is_some() || self.native_launch.login.is_some() {
            self.post(
                Kind::Error,
                "Previous host's agent setup is unconfirmed",
                Some(UNKNOWN.into()),
            );
        }
        self.native_launch = State::default();
    }

    pub(crate) fn resume_native_conversation(
        &mut self,
        terminal: ResourceId,
        agent: ResourceId,
        activation: String,
    ) {
        if self.native_launch.pending.is_some() || self.native_launch.launch.is_some() {
            return;
        }
        let Some(link) = self.link.as_ref() else {
            return;
        };
        self.native_launch.pending = Some(Pending {
            owner: Arc::downgrade(&link.connection.client),
            epoch: link.connection.connection_epoch(),
            session: self.session.clone(),
            session_key: link.connection.home_key(),
            terminal,
            agent,
            activation,
            revealed: false,
        });
    }

    pub(crate) fn open_native_launch(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.modal = Modal::NativeLaunch;
        self.refresh_native_integrations(cx);
    }

    fn refresh_native_integrations(&mut self, cx: &mut Context<Self>) {
        if self.native_launch.busy() {
            return;
        }
        self.native_launch.destination = self.native_destination().ok();
        self.native_launch.integrations.clear();
        match self.queue_native_request(NativeRequest::ListIntegrations {}) {
            Ok(flight) => {
                self.native_launch.error = None;
                self.native_launch.discovery = Some(flight);
                self.arm_native_deadline(cx);
            }
            Err(error) => self.native_launch.error = Some(error),
        }
        cx.notify();
    }

    fn launch_native_integration(
        &mut self,
        integration: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.native_launch.busy() {
            return;
        }
        if !self
            .native_launch
            .integrations
            .iter()
            .any(|item| item.integration == integration && item.native_available)
        {
            return;
        }
        match self.queue_native_launch(integration) {
            Ok(launch) => {
                self.native_launch.launch = Some(launch);
                self.native_launch.error = None;
                self.close_modal(window, cx);
                self.arm_native_deadline(cx);
            }
            Err(error) => self.native_launch.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn reveal_pending_native(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self
            .native_launch
            .pending
            .as_ref()
            .filter(|pending| !pending.revealed && pending_guardian_ready(self, pending))
        else {
            return;
        };
        let terminal = pending.terminal.clone();
        if let Some(pending) = self.native_launch.pending.as_mut() {
            pending.revealed = true;
        }
        self.reveal_terminal(&terminal);
        cx.notify();
    }

    pub(crate) fn prepare_pending_native(
        &mut self,
        placement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.native_launch.pending.as_ref() else {
            return;
        };
        if !pending.revealed
            || !pending_matches(self, pending)
            || self
                .placement(placement)
                .and_then(|item| item.terminal_id.as_ref())
                != Some(&pending.terminal)
        {
            return;
        }
        self.prepare_conversation(placement, window, cx);
        if let Some(face) = self.conversation_faces.get_mut(placement) {
            face.visible = true;
            self.native_launch.pending = None;
            self.native_launch.error = None;
        }
        if self.focused_placement().map(|item| item.id.as_str()) == Some(placement) {
            self.focus_conversation(placement, window, cx);
        }
    }

    /// Provider setup can finish after the reader has switched tabs. Prepare
    /// that exact conversation without taking focus or holding the launch lock.
    pub(crate) fn settle_pending_native(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self
            .native_launch
            .pending
            .as_ref()
            .filter(|pending| pending.revealed && pending_matches(self, pending))
        else {
            return;
        };
        let placement = self
            .link
            .iter()
            .flat_map(|link| &link.workspace.tabs)
            .flat_map(|tab| crate::workspace::layout::placements(&tab.root))
            .find(|placement| placement.terminal_id.as_ref() == Some(&pending.terminal))
            .map(|placement| placement.id.clone());
        let Some(placement) = placement else {
            return;
        };
        let desk = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = desk.update_in(cx, |desk, window, cx| {
                desk.prepare_pending_native(&placement, window, cx)
            });
        });
    }
}

fn pending_connection<'a>(desk: &'a Desk, pending: &Pending) -> Option<&'a Connection> {
    let connection = &desk.link.as_ref()?.connection;
    let session_exists =
        connection
            .topology
            .as_ref()
            .is_some_and(|topology| match pending.session_key {
                Some(key) => topology.session_keyed(&key).is_some(),
                None => topology.session_named(&pending.session).is_some(),
            });
    (session_exists && owned_by(&pending.owner, &connection.client)).then_some(connection)
}

fn pending_guardian_ready(desk: &Desk, pending: &Pending) -> bool {
    pending_connection(desk, pending).is_some_and(|connection| {
        // Revealing the catalogued guardian establishes its agent subscription.
        // Waiting for the unsubscribed conversation first deadlocks new launches.
        connection.status == Status::Attached
            && connection.pane(&pending.terminal).is_some()
            && connection.topology.as_ref().is_some_and(|topology| {
                topology.agent_sessions.iter().any(|session| {
                    session.id == pending.agent
                        && session.parent.as_ref() == Some(&pending.terminal)
                })
            })
    })
}

fn pending_matches(desk: &Desk, pending: &Pending) -> bool {
    pending_connection(desk, pending)
        .and_then(|connection| connection.conversation(&pending.terminal))
        .is_some_and(|conversation| {
            conversation.resource == pending.agent
                && conversation
                    .native
                    .as_ref()
                    .is_some_and(|view| view.activation == pending.activation)
        })
}
