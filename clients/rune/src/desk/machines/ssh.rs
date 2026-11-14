//! The machines in your ssh config, and setting several up one after
//! another: the onboarding step ticks a few, the Machines sheet presses one.
//! Each goes through `phux host add --install`, which installs phux there,
//! starts its server and registers it.

use std::collections::{HashMap, HashSet};

use super::super::{Desk, NoticeKind};
use super::Machines;
use crate::machines::{self, Add};

#[derive(Debug, Default)]
pub struct Ssh {
    /// The `Host` aliases in `~/.ssh/config`, read with each listing.
    pub hosts: Vec<String>,
    /// The ssh destinations the onboarding step has ticked.
    pub picked: HashSet<String>,
    /// Adds waiting for the one running to finish.
    queue: Vec<Add>,
    /// Why a destination couldn't be set up, until it's tried again.
    failed: HashMap<String, String>,
}

/// Where one offered machine has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offered {
    /// Not started; ticked or not.
    Pick(bool),
    Waiting,
    /// Being set up, and its latest step.
    Running(String),
    Done,
    Failed(String),
}

impl Machines {
    /// What the step offers, in order: `(ssh destination, name)` for each
    /// host in the ssh config, then each tailnet machine not already one.
    pub fn offered(&self) -> Vec<(String, String)> {
        let config = self
            .ssh
            .hosts
            .iter()
            .map(|alias| (alias.clone(), alias.clone()));
        let tailnet = self
            .peers
            .iter()
            .filter(|peer| peer.online && !self.ssh.hosts.contains(&peer.name))
            .map(|peer| (peer.ssh_host.clone(), peer.name.clone()));
        config.chain(tailnet).collect()
    }

    /// The ssh config's hosts not yet registered, nor offered from the tailnet.
    pub fn unregistered_ssh(&self) -> impl Iterator<Item = &String> {
        let peer = |alias: &String| self.peers.iter().any(|peer| &peer.name == alias);
        self.ssh
            .hosts
            .iter()
            .filter(move |alias| !self.set_up(alias) && !peer(alias))
    }

    fn set_up(&self, target: &str) -> bool {
        self.hosts
            .iter()
            .any(|host| host.name == target || host.ssh.as_deref() == Some(target))
    }

    pub fn offer_state(&self, target: &str) -> Offered {
        if self.request.as_ref().is_some_and(|r| r.target == target) {
            let step = self.adding.clone();
            return Offered::Running(step.unwrap_or_else(|| "Finishing…".to_owned()));
        }
        if self.set_up(target) {
            Offered::Done
        } else if self.ssh.queue.iter().any(|add| add.target == target) {
            Offered::Waiting
        } else if let Some(why) = self.ssh.failed.get(target) {
            Offered::Failed(why.clone())
        } else {
            Offered::Pick(self.ssh.picked.contains(target))
        }
    }

    fn enqueue(&mut self, add: Add) {
        self.ssh.failed.remove(&add.target);
        self.ssh.queue.push(add);
        self.next_queued();
    }

    /// Start the next waiting add once the last one has finished and been
    /// listed.
    pub(super) fn next_queued(&mut self) {
        if self.adding.is_none() && self.request.is_none() && !self.ssh.queue.is_empty() {
            let add = self.ssh.queue.remove(0);
            self.add(add);
        }
    }

    /// The failed add, with why kept beside its row.
    pub(super) fn take_failed(&mut self, error: &str) -> Option<Add> {
        let request = self.request.take()?;
        self.ssh
            .failed
            .insert(request.target.clone(), error.to_owned());
        Some(request)
    }
}

impl Desk {
    /// List again, reading the ssh config afresh.
    pub fn list_machines(&mut self) {
        self.machines.refresh();
    }

    pub fn pick_machine(&mut self, target: &str) {
        let picked = &mut self.machines.ssh.picked;
        if !picked.remove(target) {
            picked.insert(target.to_owned());
        }
    }

    /// Set up every ticked machine, one after another.
    pub fn set_up_picked(&mut self) {
        let list = &mut self.machines;
        for (target, name) in list.offered() {
            if list.offer_state(&target) != Offered::Pick(true) {
                continue;
            }
            let peer = machines::peer_for(&list.peers, &target).is_some();
            list.enqueue(Add {
                name: peer.then_some(name),
                tailnet: peer,
                target,
                code: None,
            });
        }
        list.ssh.picked.clear();
    }

    /// A host from the ssh config, pressed in the Machines sheet.
    pub fn add_ssh_host(&mut self, alias: &str) {
        if !matches!(
            self.machines.offer_state(alias),
            Offered::Pick(_) | Offered::Failed(_)
        ) {
            return;
        }
        self.machines.enqueue(Add {
            target: alias.to_owned(),
            name: None,
            tailnet: false,
            code: None,
        });
    }

    /// An add finished. With the sheet still open that's a wish to go there,
    /// and the sheet says so. With it closed the person has gone back to
    /// work, so the window stays and only offers, and the sidebar opens the
    /// machine to show its sessions: moving the window would send their
    /// next keys to another computer.
    pub(super) fn landed(&mut self, host: String) {
        if !self.machines.open {
            self.machines.outcome = Some(Ok(format!("{host} is set up.")));
            if self.machines.expanded.insert(Some(host.clone())) {
                self.machines.peek(Some(host.clone()), self.clock);
            }
            let offer = format!("{host} is set up. Switch to it from Machines.");
            return self.say(NoticeKind::Info, offer);
        }
        self.rebuild(Some(host.clone()), None);
        self.machines.open = true;
        if self.machines.current.as_ref() == Some(&host) {
            self.machines.outcome = Some(Ok(format!("{host} is set up.")));
        }
    }
}
