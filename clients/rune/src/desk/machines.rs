//! Which machine this window is on, and adding another. One window shows one
//! server: switching replaces the connection, and every session stays
//! running where it is.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use phux_client_runtime::Target;
use rune::{Key, KeyEvent, TextEdit, Tx, Waker};

use super::Desk;
use crate::machines::{self, Add, Host, Peer};

enum Done {
    Listed(Result<Vec<Host>, String>, Vec<Peer>),
    Step(String),
    Added(Result<(), String>),
    Removed(String, Result<(), String>),
}

pub struct Machines {
    pub open: bool,
    /// The remote this window is on; `None` is this computer.
    pub current: Option<String>,
    /// The CLI that holds the registry.
    pub phux: Option<PathBuf>,
    pub hosts: Vec<Host>,
    pub peers: Vec<Peer>,
    pub field: TextEdit,
    /// Enrollment running, and its latest step.
    pub adding: Option<String>,
    pub error: Option<String>,
    /// How the last add ended; stays until the next one starts.
    pub outcome: Option<Result<String, String>>,
    /// The row asking whether to really remove its machine.
    pub removing: Option<String>,
    /// The add in hand, until a listing shows what it made, and the
    /// machines from before it.
    request: Option<Add>,
    before: Option<Vec<Host>>,
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
            outcome: None,
            removing: None,
            request: None,
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
        self.outcome = None;
        self.request = Some(request.clone());
        self.worker(move |send| {
            let result = machines::add(&phux, &request, &|step| send(Done::Step(step)));
            send(Done::Added(result));
        });
    }

    fn remove(&mut self, name: String) {
        let Some(phux) = self.phux.clone() else {
            return;
        };
        self.removing = None;
        self.worker(move |send| {
            let result = machines::remove(&phux, &name);
            send(Done::Removed(name, result));
        });
    }

    /// The host a finished add registered, whatever it was named. Only that
    /// refresh moves the window; any other list is just a list.
    fn take_added(&mut self) -> Option<String> {
        let before = self.before.take()?;
        let request = self.request.take()?;
        let host = machines::added(&self.hosts, &before, &request);
        host.map(|host| host.name.clone())
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
        // The tailnet authenticates its peers' addresses, so typing one's
        // full name is the same as pressing its row.
        let peer = machines::peer_for(&self.machines.peers, &target);
        let name = peer.map(|peer| peer.name.clone());
        self.machines.add(Add {
            tailnet: peer.is_some(),
            target,
            name,
        });
    }

    /// First press asks; the row's second answer removes. The window can't
    /// stay on a machine it no longer knows, so that one goes home first.
    pub fn ask_remove(&mut self, name: Option<String>) {
        self.machines.removing = name;
    }

    pub fn remove_machine(&mut self, name: String) {
        if self.machines.current.as_ref() == Some(&name) {
            self.switch_machine(None);
            self.machines.open = true;
        }
        self.machines.remove(name);
    }

    pub fn add_peer(&mut self, peer: &Peer) {
        self.machines.add(Add {
            target: peer.ssh_host.clone(),
            name: Some(peer.name.clone()),
            tailnet: true,
        });
    }

    /// Move this window to `machine` and remember it for the next launch.
    /// Rebuilding drops the old connection and everything it knew. A machine
    /// that can't be dialed leaves the window where it is.
    pub fn switch_machine(&mut self, machine: Option<String>) {
        if machine == self.machines.current {
            self.close_machines();
            return;
        }
        if let Some(Err(why)) = machine.as_ref().map(|m| Target::resolve(m, None)) {
            self.machines.error = Some(why);
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
        self.carry_into(&mut next);
        next.link.home = None;
        next.machines.current = machine;
        next.close_machines();
        *self = next;
        self.dial();
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
                        self.landed(host);
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
                        Err(error) => {
                            self.machines.request = None;
                            self.machines.outcome = Some(Err(error));
                        }
                    }
                }
                Done::Removed(name, result) => {
                    if let Err(error) = result {
                        self.machines.error = Some(format!("{name} not removed: {error}"));
                    }
                    self.machines.refresh();
                }
            }
        }
        changed
    }

    /// An add finished: go there, and leave the sheet saying so.
    fn landed(&mut self, host: String) {
        let open = self.machines.open;
        self.switch_machine(Some(host.clone()));
        self.machines.open = open;
        if self.machines.current.as_ref() == Some(&host) {
            self.machines.outcome = Some(Ok(format!("{host} is set up.")));
        }
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
        let host = |name: &str| Host {
            name: name.to_owned(),
            ssh: None,
            needs_direct: false,
        };
        let mut list = Machines::new(None, None);
        list.hosts = vec![host("a"), host("b")];
        assert_eq!(list.take_added(), None);
        list.before = Some(vec![host("a")]);
        list.request = Some(Add {
            target: "me@b".to_owned(),
            name: None,
            tailnet: false,
        });
        assert_eq!(list.take_added().as_deref(), Some("b"));
        assert_eq!(list.take_added(), None);
    }
}
