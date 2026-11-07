//! The other machines, watched while the galaxy or the reef is open: one
//! read-only connection each (no attach, no views), folding its topology and
//! agent events the way this window folds its own, so their agents show as
//! stars and fish. Nothing is watched while both views are closed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use phux_client_runtime::control::{ControlOptions, Status, Topology};
use phux_client_runtime::{Client, ClientOptions, Runtime, Target};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::RolePolicy;

use super::galaxy::Star;
use super::{Desk, Wake};
use crate::activity::Activities;

/// One other machine's server, as a watch sees it.
pub struct Watch {
    client: Arc<Client>,
    topology: Option<Topology>,
    activities: Activities,
    status: Status,
}

impl Watch {
    /// Its agents, ids tagged with the machine so they never meet this
    /// window's own; `None` until its topology has arrived.
    pub fn stars(&self, machine: &Option<String>) -> Option<Vec<Star>> {
        let topology = self.topology.as_ref()?;
        let stars = super::galaxy::stars_on(topology, &self.activities, Some(&self.client));
        Some(tag(stars, machine))
    }

    /// Answering, as far as the last observation says.
    pub fn online(&self) -> bool {
        self.topology.is_some() && !self.status.is_terminal()
    }
}

/// Mark `stars` as `machine`'s, with ids that cannot meet another server's.
fn tag(mut stars: Vec<Star>, machine: &Option<String>) -> Vec<Star> {
    let name = machine
        .clone()
        .unwrap_or_else(|| crate::machines::LOCAL.to_owned());
    for star in &mut stars {
        star.machine.clone_from(machine);
        star.here = false;
        if let Some(id) = star.terminal.local_id() {
            star.id = ResourceId::satellite(name.clone(), id);
        }
    }
    stars
}

#[derive(Default)]
pub struct Fleet {
    pub watches: HashMap<Option<String>, Watch>,
    /// Machines that could not be dialed this time the views were open.
    refused: HashSet<Option<String>>,
}

impl Desk {
    /// Open, drain and close watches. True when any machine's agents changed.
    pub(super) fn tick_fleet(&mut self) -> bool {
        if !self.galaxy.open && !self.reef.open {
            self.fleet.watches.clear();
            self.fleet.refused.clear();
            return false;
        }
        let others = self.watchable();
        self.fleet.watches.retain(|host, _| others.contains(host));
        for host in others {
            if !self.fleet.watches.contains_key(&host) && !self.fleet.refused.contains(&host) {
                match self.watch_machine(host.as_deref()) {
                    Some(watch) => {
                        self.fleet.watches.insert(host, watch);
                    }
                    None => {
                        self.fleet.refused.insert(host);
                    }
                }
            }
        }
        let mut changed = false;
        for watch in self.fleet.watches.values_mut() {
            let mut observation = watch.client.take_activity();
            changed |= observation.status != watch.status;
            watch.status = observation.status;
            if let Some(topology) = observation.topology.take()
                && watch.topology.as_ref() != Some(&topology)
            {
                watch.topology = Some(topology);
                changed = true;
            }
            for event in &observation.events {
                changed |= watch.activities.apply(event, &[]);
            }
        }
        changed
    }

    /// Every machine but this window's that a window can dial: this computer
    /// when the window is on a remote, and each registered host with a direct
    /// route.
    fn watchable(&self) -> Vec<Option<String>> {
        let current = &self.machines.current;
        let local = current.is_some().then_some(None);
        let remotes = self
            .machines
            .hosts
            .iter()
            .filter(|host| !host.needs_direct && Some(&host.name) != current.as_ref())
            .map(|host| Some(host.name.clone()));
        local.into_iter().chain(remotes).collect()
    }

    fn watch_machine(&self, host: Option<&str>) -> Option<Watch> {
        let target = match host {
            Some(host) => Target::resolve(host, None).ok()?,
            None => Target::uds(self.launch.socket.clone().ok()?),
        };
        let client = Runtime::connect(
            target,
            ClientOptions {
                control: ControlOptions {
                    client_name: "phux-rune-fleet".to_owned(),
                    viewport: (0, 0),
                    attach: None,
                    attach_role: Some(RolePolicy::PRIMARY),
                    auto_attach_foreign_spawns: false,
                    subscribe_agent_sessions: false,
                    ..ControlOptions::default()
                },
                ..ClientOptions::default()
            },
        )
        .ok()?;
        let client = Arc::new(client);
        if let Some(waker) = self.waker.clone() {
            client.set_listener(Arc::new(Wake(waker)));
        }
        Some(Watch {
            client,
            topology: None,
            activities: Activities::default(),
            status: Status::Idle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Mark;

    fn star(id: u32) -> Star {
        Star {
            id: ResourceId::local(id),
            terminal: ResourceId::local(id),
            machine: None,
            here: true,
            kind: None,
            session: "work".into(),
            name: "agent".into(),
            mark: Mark::Working,
            project: None,
        }
    }

    #[test]
    fn another_machines_agents_keep_their_terminal_but_never_share_an_id() {
        let here = star(9);
        let there = tag(vec![star(9)], &Some("powerhouse-1".into())).remove(0);
        assert_ne!(
            here.id, there.id,
            "the same @9 on two servers is two agents"
        );
        assert_eq!(
            there.terminal,
            ResourceId::local(9),
            "opened by its own server's id"
        );
        assert_eq!(there.machine.as_deref(), Some("powerhouse-1"));
        assert!(!there.here);
        let local = tag(vec![star(9)], &None).remove(0);
        assert_eq!(local.machine, None, "this computer, seen from a remote");
        assert_ne!(local.id, here.id);
    }
}
