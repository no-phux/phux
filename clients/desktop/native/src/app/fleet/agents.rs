//! Agent inventory and exact, revalidated navigation. No terminal views while browsing.

use gpui::{Context, Window};
use phux_client_runtime::control::{Event, Status};
use phux_protocol::ResourceId;

use super::{Destination, available};
use crate::app::desk::Desk;
use crate::app::notifications::Kind;
use crate::connection::{AgentInfo, Connection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Agent {
    pub destination: Destination,
    pub terminal: ResourceId,
    /// Resource ids are scoped to this server incarnation, not just a host name.
    pub boot: Option<String>,
    pub info: AgentInfo,
    pub title: String,
}

impl Agent {
    pub fn same_target(&self, other: &Self) -> bool {
        self.destination == other.destination
            && self.terminal == other.terminal
            && self.boot == other.boot
    }

    /// Attachment topology arrives before badge metadata. Navigation needs
    /// the original server/session/terminal, not a second copy of the badge.
    fn listed_on(&self, connection: &Connection) -> bool {
        if connection.server_id() != self.boot {
            return false;
        }
        let Some(topology) = &connection.topology else {
            return false;
        };
        let session = match self.destination.key {
            Some(key) => topology.session_keyed(&key),
            None => topology.session_named(&self.destination.name),
        };
        let Some(session) = session else { return false };
        connection
            .panes()
            .iter()
            .any(|pane| pane.terminal_id == self.terminal && pane.session_id == session.id)
    }
}

fn agents_on(connection: &Connection, host: Option<String>) -> Vec<Agent> {
    if !available(connection) {
        return Vec::new();
    }
    let Some(topology) = &connection.topology else {
        return Vec::new();
    };
    let boot = connection.server_id();
    connection
        .panes()
        .iter()
        .filter_map(|pane| {
            let info = connection.agents.get(&pane.terminal_id)?;
            let session = topology
                .sessions
                .iter()
                .find(|session| session.id == pane.session_id)?;
            Some(Agent {
                destination: Destination {
                    host: host.clone(),
                    authority: topology.authority,
                    key: session.key,
                    name: session.name.clone(),
                },
                terminal: pane.terminal_id.clone(),
                boot: boot.clone(),
                info: info.clone(),
                title: pane.title.clone().unwrap_or_default(),
            })
        })
        .collect()
}

impl Desk {
    /// A later choice also withdraws this click's unplaced workspace open.
    pub(crate) fn cancel_fleet_navigation(&mut self) {
        self.fleet_view.pending = None;
        if let Some(intent) = self.fleet_view.pending_open.take()
            && let Some(link) = self.link.as_mut()
        {
            link.workspace.cancel_pending_open(intent);
        }
    }

    /// Refusal ends this click. Workspace retains the original error notice.
    pub(crate) fn note_fleet_event(&mut self, event: &Event) {
        let Some(target) = self.fleet_view.pending.as_ref() else {
            return;
        };
        if matches!(event, Event::TerminalAttached { terminal_id, error: Some(_), .. }
            if terminal_id == &target.terminal)
        {
            self.cancel_fleet_navigation();
        }
    }

    pub(crate) fn agent_catalog(&self) -> Vec<Agent> {
        let active = self
            .link
            .as_ref()
            .filter(|link| available(&link.connection));
        let mut agents: Vec<_> = self
            .fleet
            .observers
            .iter()
            .filter(|(host, _)| **host != self.remote || active.is_none())
            .filter_map(|(host, observer)| {
                observer
                    .connection
                    .as_ref()
                    .map(|connection| (host, connection))
            })
            .flat_map(|(host, connection)| agents_on(connection, host.clone()))
            .collect();
        if let Some(link) = active {
            agents.extend(agents_on(&link.connection, self.remote.clone()));
        }
        // Stable spatial positions: state changes should not shuffle tiles under a pointer.
        agents.sort_by(|a, b| {
            a.destination
                .host
                .cmp(&b.destination.host)
                .then(a.destination.name.cmp(&b.destination.name))
                .then(a.terminal.cmp(&b.terminal))
        });
        agents
    }

    pub(crate) fn open_fleet_agent(
        &mut self,
        target: Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.prefs.agent_fleet
            || !self
                .agent_catalog()
                .iter()
                .any(|agent| agent.same_target(&target))
        {
            self.post(
                Kind::Refusal,
                "Agent is no longer available",
                Some(
                    "Its host, session or terminal changed. Choose it again from the fleet.".into(),
                ),
            );
            cx.notify();
            return;
        }
        self.cancel_fleet_navigation();
        self.close_modal(window, cx);
        self.navigate_work(target.destination.clone(), cx);
        self.fleet_view.pending = Some(target);
        self.fleet_view.pending_open = None;
        self.settle_fleet_terminal();
        self.after_command(window, cx);
    }

    fn reveal_fleet_terminal(&mut self, terminal: &ResourceId) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        if let Some(pane) = link.connection.pane(terminal).cloned()
            && let Some(intent) = link.workspace.open_for_navigation(&link.connection, &pane)
        {
            self.fleet_view.pending_open = Some(intent);
        }
        // Restore can still place a default pane after the selected one.
        if self.fleet_restore_finished(terminal) {
            self.cancel_fleet_navigation();
        }
    }

    fn fleet_restore_finished(&self, terminal: &ResourceId) -> bool {
        self.link.as_ref().is_some_and(|link| {
            link.workspace.restored()
                && link.workspace.pending() == 0
                && link.workspace.views_of(terminal) > 0
        })
    }

    /// Wait for the chosen session to attach. A restarted server must never
    /// redirect this click to a recycled terminal id.
    pub(crate) fn settle_fleet_terminal(&mut self) {
        let Some(target) = self.fleet_view.pending.as_ref() else {
            return;
        };
        let Some(link) = &self.link else { return };
        if !matches!(
            link.connection.status,
            Status::Attached | Status::Failed | Status::Closed
        ) {
            return;
        }
        let valid = link.connection.status == Status::Attached
            && self.work_is_active(&target.destination)
            && target.listed_on(&link.connection);
        let terminal = target.terminal.clone();
        if valid {
            self.reveal_fleet_terminal(&terminal);
        } else {
            self.cancel_fleet_navigation();
            self.post(
                Kind::Refusal,
                "Agent destination changed",
                Some("Reopen the fleet and choose the agent again.".into()),
            );
        }
    }
}
