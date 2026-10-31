//! Which machine this window is on, and adding another. One window shows one
//! server: switching replaces the connection, and every session stays
//! running where it is.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use rune::{Key, KeyEvent, TextEdit, Tx, Waker};

use super::Desk;
use crate::machines::{self, Add, Peer};

enum Done {
    Listed(Result<Vec<String>, String>, Vec<Peer>),
    Step(String),
    Added(Result<(), String>),
}

pub struct Machines {
    pub open: bool,
    /// The remote this window is on; `None` is this computer.
    pub current: Option<String>,
    /// The CLI that holds the registry.
    pub phux: Option<PathBuf>,
    pub hosts: Vec<String>,
    pub peers: Vec<Peer>,
    pub field: TextEdit,
    /// Enrollment running, and its latest step.
    pub adding: Option<String>,
    pub error: Option<String>,
    /// The machines before a finished add, to go to the one it made.
    before: Option<Vec<String>>,
    done: (Sender<Done>, Receiver<Done>),
    waker: Option<Waker>,
}

impl Machines {
    pub fn new(current: Option<String>, phux: Option<PathBuf>) -> Self {
        Self {
            open: false,
            current,
            phux,
            hosts: Vec::new(),
            peers: Vec::new(),
            field: TextEdit::new(""),
            adding: None,
            error: None,
            before: None,
            done: channel(),
            waker: None,
        }
    }

    /// List at launch, so the sheet opens with its machines in it.
    pub fn listen(&mut self, waker: Waker) {
        self.waker = Some(waker);
        self.refresh();
    }

    pub fn label(&self) -> &str {
        self.current.as_deref().unwrap_or(machines::LOCAL)
    }

    fn worker(&self, job: impl FnOnce(&dyn Fn(Done)) + Send + 'static) {
        let sender = self.done.0.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let send = |done| {
                let _ = sender.send(done);
                if let Some(waker) = &waker {
                    waker();
                }
            };
            job(&send);
        });
    }

    fn refresh(&self) {
        let Some(phux) = self.phux.clone() else {
            return;
        };
        self.worker(move |send| {
            let hosts = machines::registered(&phux);
            send(Done::Listed(hosts, machines::tailnet(&phux)));
        });
    }

    fn add(&mut self, request: Add) {
        let Some(phux) = self.phux.clone() else {
            self.error = Some("Install the phux CLI to add a machine.".to_owned());
            return;
        };
        if self.adding.is_some() {
            return;
        }
        self.adding = Some(format!("Setting up {}…", request.target));
        self.error = None;
        self.worker(move |send| {
            let result = machines::add(&phux, &request, &|step| send(Done::Step(step)));
            send(Done::Added(result));
        });
    }

    /// The host a finished add registered, whatever it was named. Only that
    /// refresh moves the window; any other list is just a list.
    fn take_added(&mut self) -> Option<String> {
        let before = self.before.take()?;
        self.hosts
            .iter()
            .find(|host| !before.contains(host))
            .cloned()
    }
}

impl Desk {
    pub fn toggle_machines(&mut self) {
        self.machines.open = !self.machines.open;
        if self.machines.open {
            self.close_settings();
            self.close_inbox();
            self.machines.error = None;
            self.machines.field.clear();
            self.machines.field.focused = true;
            self.machines.refresh();
        }
    }

    pub fn close_machines(&mut self) {
        self.machines.open = false;
        self.machines.field.focused = false;
    }

    pub fn machines_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        if event.key != Key::Escape {
            return false;
        }
        self.close_machines();
        tx.blur();
        true
    }

    /// `me@host` from the field, set up over the ssh trust you already have.
    pub fn add_typed_machine(&mut self) {
        let target = self.machines.field.text.trim().to_owned();
        if target.is_empty() {
            self.machines.error = Some("Enter an ssh destination such as me@mini.".to_owned());
            return;
        }
        self.machines.add(Add {
            target,
            name: None,
            tailnet: false,
        });
    }

    pub fn add_peer(&mut self, peer: &Peer) {
        self.machines.add(Add {
            target: peer.ssh_host.clone(),
            name: Some(peer.name.clone()),
            tailnet: true,
        });
    }

    /// Move this window to `machine` and remember it for the next launch.
    /// Rebuilding drops the old connection and everything it knew.
    pub fn switch_machine(&mut self, machine: Option<String>) {
        if machine == self.machines.current {
            self.close_machines();
            return;
        }
        // An aimed run stays out of what the next real launch reopens.
        if !self.launch.aimed {
            let saved = machine.clone().unwrap_or_default();
            let _ = self.prefs.set("machine", &saved);
        }
        // Notifications from the machine being left would open nothing here.
        self.inbox.alerts.sync(&[], |_| true, false);
        let mut launch = self.launch.clone();
        launch.session = None;
        let mut next = Desk::open(&launch, machine.clone());
        std::mem::swap(&mut next.updater, &mut self.updater);
        std::mem::swap(&mut next.machines, &mut self.machines);
        next.machines.current = machine;
        next.generation = self.generation + 1;
        next.focused = self.focused;
        if let Some(waker) = self.waker.clone() {
            next.watch(waker);
        }
        next.close_machines();
        *self = next;
    }

    pub(super) fn pump_machines(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.machines.done.1.try_recv() {
            changed = true;
            match done {
                Done::Listed(hosts, peers) => {
                    match hosts {
                        Ok(hosts) => self.machines.hosts = hosts,
                        Err(error) => self.machines.error = Some(error),
                    }
                    self.machines.peers = peers;
                    if let Some(host) = self.machines.take_added() {
                        self.switch_machine(Some(host));
                    }
                }
                Done::Step(step) => {
                    if self.machines.adding.is_some() {
                        self.machines.adding = Some(step);
                    }
                }
                Done::Added(result) => {
                    self.machines.adding = None;
                    match result {
                        Ok(()) => {
                            self.machines.before = Some(self.machines.hosts.clone());
                            self.machines.refresh();
                        }
                        Err(error) => self.machines.error = Some(error),
                    }
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_the_local_server_by_where_it_is() {
        assert_eq!(Machines::new(None, None).label(), machines::LOCAL);
        assert_eq!(Machines::new(Some("mini".into()), None).label(), "mini");
    }

    #[test]
    fn a_listing_moves_the_window_only_after_an_add() {
        let mut list = Machines::new(None, None);
        list.hosts = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(list.take_added(), None);
        list.before = Some(vec!["a".to_owned()]);
        assert_eq!(list.take_added().as_deref(), Some("b"));
        assert_eq!(list.take_added(), None);
    }
}
