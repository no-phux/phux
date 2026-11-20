//! Read-only fleet projection and navigation, independent of any playground.

use phux_client_runtime::Client;
use phux_client_runtime::control::Topology;
use phux_protocol::ids::ResourceId;

use super::super::Desk;
use crate::activity::{Activities, Mark};
use crate::creature::{Stats, Work};
use crate::machines::LOCAL;

/// An agent on one machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    /// Unique across machines: this window's terminal id, or another
    /// machine's tagged with its name (`ResourceId::Satellite`).
    pub id: ResourceId,
    /// The terminal on its own server.
    pub terminal: ResourceId,
    /// Its machine when not this window's; `None` for the connected machine
    /// and for this computer seen from a remote, distinguished by `here`.
    pub machine: Option<String>,
    /// Whether it is on the machine this window is connected to.
    pub here: bool,
    pub kind: Option<String>,
    pub session: String,
    pub name: String,
    pub mark: Mark,
    /// The session's project (the desktop's rule), else the directory's.
    pub project: Option<String>,
    pub work: Option<Work>,
}

/// A machine and its agents: the connected one's from this window, another's
/// from its watch, else what its session listing says.
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    pub name: String,
    pub online: bool,
    pub agents: Vec<Agent>,
    /// Whether `agents` is known: connected or watched, even when empty.
    pub known: bool,
    pub note: Option<String>,
}

impl Desk {
    /// Every machine, those with something waiting first. Equal-priority
    /// machines keep connected/local/registered order.
    pub fn fleet_machines(&self) -> Vec<Machine> {
        let current = self.machines.current.clone();
        let here = Machine {
            name: current.clone().unwrap_or_else(|| LOCAL.to_owned()),
            online: self.connection.client.is_some(),
            agents: self.agents_here(),
            known: true,
            note: None,
        };
        let others = std::iter::once(None)
            .chain(self.machines.hosts.iter().map(|h| Some(h.name.clone())))
            .filter(|host| *host != current)
            .map(|host| {
                let watch = self.fleet.watches.get(&host);
                let agents = watch.and_then(|w| w.agents(&host, self.clock));
                let glance = self.machines.glances.get(&host);
                let note = match glance {
                    Some(Ok(sessions)) => {
                        let running: u64 = sessions.iter().map(|s| s.running).sum();
                        Some(format!("{} sessions · {running} running", sessions.len()))
                    }
                    _ => None,
                };
                let online = watch.map_or(!matches!(glance, Some(Err(_))), |w| {
                    w.online() || matches!(glance, Some(Ok(_)))
                });
                Machine {
                    name: host.unwrap_or_else(|| LOCAL.to_owned()),
                    online,
                    known: agents.is_some(),
                    note: if agents.is_some() { None } else { note },
                    agents: agents.unwrap_or_default(),
                }
            });
        let mut machines: Vec<Machine> = std::iter::once(here).chain(others).collect();
        machines.sort_by_key(|m| !m.agents.iter().any(|agent| agent.mark.needs_you()));
        machines
    }

    /// The connected machine's agents, in topology order.
    fn agents_here(&self) -> Vec<Agent> {
        let Some(topology) = &self.connection.topology else {
            return Vec::new();
        };
        let mut agents = agents_on(
            topology,
            &self.activities,
            self.connection.client.as_deref(),
            (&self.fleet.here, self.clock),
        );
        for agent in &mut agents {
            agent.here = true;
        }
        agents
    }

    /// Show an agent wherever it lives. Scene callers own closing their view.
    pub(in crate::desk) fn go_to_agent(&mut self, id: &ResourceId) {
        let Some(agent) = self
            .fleet_machines()
            .into_iter()
            .flat_map(|m| m.agents)
            .find(|agent| &agent.id == id)
        else {
            return;
        };
        if agent.here {
            self.show(agent.terminal, &agent.session);
        } else {
            self.open_on(agent.machine, agent.session);
            self.want(agent.terminal);
        }
    }
}

/// A server's declared or asking agents, with work as of `now`.
pub(super) fn agents_on(
    topology: &Topology,
    activities: &Activities,
    client: Option<&Client>,
    (stats, now): (&Stats, f64),
) -> Vec<Agent> {
    topology
        .panes
        .iter()
        .filter(|pane| {
            activities.agent_name(&pane.terminal_id).is_some()
                || crate::inbox::question(pane, activities).is_some()
        })
        .map(|pane| Agent {
            id: pane.terminal_id.clone(),
            terminal: pane.terminal_id.clone(),
            machine: None,
            here: false,
            kind: activities.agent_kind(&pane.terminal_id).map(str::to_owned),
            session: pane.session_name.clone(),
            name: agent_name(
                activities.agent_name(&pane.terminal_id),
                activities.agent_kind(&pane.terminal_id),
                &crate::palette::pane_label(pane),
            ),
            mark: activities.mark(&pane.terminal_id),
            work: stats.work(
                &pane.terminal_id,
                activities.mark(&pane.terminal_id) == Mark::Working,
                now,
            ),
            project: session_project(topology, client, pane.session_id)
                .or_else(|| pane.cwd.as_deref().and_then(project).map(str::to_owned)),
        })
        .collect()
}

/// Use an agent's own name unless it only names the harness, when the pane's
/// title says what it is doing. Drop the harness's leading status glyph.
fn agent_name(agent: Option<&str>, kind: Option<&str>, label: &str) -> String {
    let title = label
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim();
    match agent {
        Some(name) if !kind.is_some_and(|kind| name.eq_ignore_ascii_case(kind)) => name.to_owned(),
        _ if !title.is_empty() => title.to_owned(),
        Some(name) => name.to_owned(),
        None => label.to_owned(),
    }
}

/// A working directory's project: its last component. A home or root says
/// nothing about the work.
fn project(cwd: &str) -> Option<&str> {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.matches('/').count() < 3 {
        return None;
    }
    trimmed.rsplit('/').next().filter(|last| !last.is_empty())
}

/// The project its session record names, as the desktop groups sessions.
fn session_project(topology: &Topology, client: Option<&Client>, session: u32) -> Option<String> {
    let key = topology.session(session)?.key?;
    let organization = client?.organization();
    let record = organization.session(&key)?;
    let attr = |name: &str| {
        record
            .attrs
            .iter()
            .find(|(k, v)| k == name && !v.is_empty())
            .map(|(_, v)| v.clone())
    };
    attr("project.name").or_else(|| attr("project.id"))
}

#[cfg(test)]
mod tests {
    use super::super::Watch;
    use super::*;
    use phux_client_runtime::control::{Event, Status};
    use std::sync::Arc;

    fn declared(activities: &mut Activities, id: u32) {
        activities.apply(
            &Event::AgentMetadata {
                terminal_id: ResourceId::local(id),
                value: Some(br#"{"name":"claude","kind":"claude","state":"working"}"#.to_vec()),
            },
            &[],
        );
    }

    fn watch(client: Arc<Client>, activities: Activities) -> Watch {
        Watch {
            topology: client.topology(),
            client,
            activities,
            status: Status::Attached,
            stats: Stats::default(),
            asked: false,
        }
    }

    fn host(name: &str) -> crate::machines::Host {
        crate::machines::Host {
            name: name.to_owned(),
            ssh: None,
            ssh_only: false,
            paused: false,
        }
    }

    #[test]
    fn projection_keeps_declared_and_asking_agents_in_topology_order() {
        let (client, _rt) = crate::test_support::input_fixture();
        let mut topology = client.topology().unwrap();
        let pane = topology.panes[0].clone();
        topology.panes = [3, 7, 2]
            .map(|id| {
                let mut pane = pane.clone();
                pane.terminal_id = ResourceId::local(id);
                pane.title = Some("\u{2733} Clone the website".to_owned());
                pane.cwd = Some("/Users/you/src/phux/".to_owned());
                pane
            })
            .to_vec();
        let mut activities = Activities::default();
        activities.apply(
            &Event::AgentAsked {
                terminal_id: ResourceId::local(7),
                question_id: "q7".to_owned(),
                text: "Ship it?".to_owned(),
                suggestions: Vec::new(),
                waiting_seconds: None,
            },
            &[],
        );
        declared(&mut activities, 2);
        let agents = agents_on(&topology, &activities, None, (&Stats::default(), 0.0));
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.id.clone())
                .collect::<Vec<_>>(),
            [ResourceId::local(7), ResourceId::local(2)]
        );
        assert_eq!(agents[0].mark, Mark::Blocked);
        assert_eq!(agents[1].name, "Clone the website");
        assert_eq!(agents[1].kind.as_deref(), Some("claude"));
        assert_eq!(agents[1].project.as_deref(), Some("phux"));
        assert!(
            agents
                .iter()
                .all(|agent| !agent.here && agent.machine.is_none())
        );
        assert_eq!(project("/Users/you"), None);
        assert_eq!(project("/"), None);
    }

    #[test]
    fn machines_keep_stable_order_and_distinguish_unknown_from_known_empty() {
        let (client, mut rt) = crate::test_support::input_fixture();
        let desk = &mut rt.state;
        desk.machines.current = Some("beta".to_owned());
        desk.machines.hosts = vec![host("alpha"), host("beta"), host("gamma")];
        desk.machines
            .glances
            .insert(Some("alpha".to_owned()), Ok(Vec::new()));
        desk.machines
            .glances
            .insert(Some("gamma".to_owned()), Err("offline".to_owned()));
        desk.fleet
            .watches
            .insert(None, watch(client.clone(), Activities::default()));
        let machines = desk.fleet_machines();
        assert_eq!(
            machines.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["beta", LOCAL, "alpha", "gamma"]
        );
        assert!(machines[0].known && machines[1].known);
        assert!(machines[1].agents.is_empty(), "an empty watch is known");
        assert!(!machines[2].known);
        assert_eq!(machines[2].note.as_deref(), Some("0 sessions · 0 running"));
        assert!(!machines[3].online);

        let mut activities = Activities::default();
        activities.apply(
            &Event::AgentAsked {
                terminal_id: ResourceId::local(1),
                question_id: "q1".to_owned(),
                text: "Ship it?".to_owned(),
                suggestions: Vec::new(),
                waiting_seconds: None,
            },
            &[],
        );
        desk.fleet
            .watches
            .insert(Some("gamma".to_owned()), watch(client, activities));
        let machines = desk.fleet_machines();
        assert_eq!(
            machines.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["gamma", "beta", LOCAL, "alpha"]
        );
        assert!(machines[0].known && machines[0].note.is_none());
    }

    #[test]
    fn navigation_uses_the_connected_terminal_and_ignores_a_vanished_agent() {
        let (client, mut rt) = crate::test_support::input_fixture();
        let desk = &mut rt.state;
        desk.connection.topology = client.topology();
        declared(&mut desk.activities, 1);
        desk.connection.pending_session = Some("untouched".to_owned());
        desk.go_to_agent(&ResourceId::local(99));
        assert_eq!(
            desk.connection.pending_session.as_deref(),
            Some("untouched")
        );
        desk.go_to_agent(&ResourceId::local(1));
        assert!(desk.connection.pending_session.is_none());
        assert_eq!(
            desk.connection.shown.as_ref().map(|shown| &shown.resource),
            Some(&ResourceId::local(1))
        );
    }

    #[test]
    fn navigation_from_a_remote_to_this_computer_uses_its_own_terminal_id() {
        let (client, mut rt) = crate::test_support::input_fixture();
        let desk = &mut rt.state;
        // An absent, test-only socket cannot discover or ensure production.
        desk.launch = crate::launch::Resolved::embedded(None);
        desk.launch.socket = Ok(std::path::PathBuf::from(format!(
            "/private/tmp/opencode/phux-fleet-{}-absent.sock",
            std::process::id()
        )));
        desk.machines.current = Some("remote".to_owned());
        let mut activities = Activities::default();
        declared(&mut activities, 1);
        desk.fleet.watches.insert(None, watch(client, activities));
        desk.go_to_agent(&ResourceId::satellite(LOCAL.to_owned(), 1));
        assert_eq!(desk.machines.current, None);
        assert_eq!(desk.launch.session.as_deref(), Some("budget"));
        assert_eq!(
            desk.connection.wanted.as_ref().map(|(id, _)| id),
            Some(&ResourceId::local(1))
        );
    }

    #[test]
    fn an_agent_is_named_for_its_work_when_it_only_names_the_harness() {
        let title = "\u{2733} Clone the website";
        assert_eq!(
            agent_name(Some("claude"), Some("claude"), title),
            "Clone the website"
        );
        assert_eq!(
            agent_name(Some("Claude"), Some("claude"), title),
            "Clone the website"
        );
        assert_eq!(
            agent_name(Some("build-box"), Some("claude"), title),
            "build-box"
        );
        assert_eq!(agent_name(Some("claude"), Some("claude"), "  "), "claude");
        assert_eq!(agent_name(None, None, "deploy"), "deploy");
    }
}
