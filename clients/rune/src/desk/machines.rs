//! Which machine this window is on, and adding another. One window shows one
//! server: switching replaces the connection, and every session stays
//! running where it is.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use phux_client_runtime::Target;
use rune::{Key, KeyEvent, TextEdit, Tx, Waker};

use super::{Desk, NoticeKind};
use crate::machines::{self, Add, Glance, Host, Peer};

/// How often an expanded machine's sessions are listed again, in seconds.
const GLANCE_EVERY: f64 = 30.0;

enum Done {
    /// A listing, tagged with the refresh that started it.
    Listed(u64, Result<Vec<Host>, String>, Vec<Peer>),
    Step(String),
    Added(Result<(), String>),
    Removed(String, Result<(), String>),
    Paused(String, Result<(), String>),
    Recolored(String, Result<(), String>),
    /// Another machine's sessions; `None` is this computer.
    Glanced(Option<String>, Result<Vec<Glance>, String>),
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
    /// The row whose colour swatches are out; [`machines::LOCAL`] is this one.
    pub picking: Option<String>,
    /// The other machines whose sessions the sidebar shows; `None` is this computer.
    pub expanded: HashSet<Option<String>>,
    /// The sidebar hides this window's own sessions under its machine.
    pub folded: bool,
    /// What each expanded machine last listed.
    pub glances: HashMap<Option<String>, Result<Vec<Glance>, String>>,
    /// When each machine's listing last started, on the app clock, and the
    /// ones still out.
    glanced: HashMap<Option<String>, f64>,
    peeking: HashSet<Option<String>>,
    /// The add in hand, until a listing shows what it made, and the
    /// machines from before it.
    request: Option<Add>,
    before: Option<Vec<Host>>,
    /// Refreshes started, and the newest one heard back: a slow one can
    /// finish after a later one and must not undo it.
    refreshes: u64,
    listed: u64,
    /// The refresh started once an add finished: an older one may have read
    /// the registry before that add wrote to it.
    awaited: Option<u64>,
    done: (Sender<Done>, Receiver<Done>),
    waker: Option<Waker>,
    /// The window, brought forward when a pairing link arrives.
    pub window: Option<rune::AppHandle<Desk>>,
    /// Links from the command line, opened once the window starts.
    pub pending: Vec<String>,
    /// A link waiting for the person to accept pairing with it.
    pub offer: Option<crate::connect::Offer>,
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
            picking: None,
            expanded: HashSet::new(),
            folded: false,
            glances: HashMap::new(),
            glanced: HashMap::new(),
            peeking: HashSet::new(),
            request: None,
            before: None,
            refreshes: 0,
            listed: 0,
            awaited: None,
            done: channel(),
            waker: None,
            window: None,
            pending: Vec::new(),
            offer: None,
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

    fn refresh(&mut self) -> u64 {
        self.refreshes += 1;
        let refresh = self.refreshes;
        if let Some(phux) = self.phux.clone() {
            self.worker(move |send| {
                let hosts = machines::registered(&phux);
                send(Done::Listed(refresh, hosts, machines::tailnet(&phux)));
            });
        }
        refresh
    }

    /// List `host`'s sessions, unless a listing is already out.
    fn peek(&mut self, host: Option<String>, clock: f64) {
        let Some(phux) = self.phux.clone() else {
            return;
        };
        let paused = |known: &Host| known.paused && Some(&known.name) == host.as_ref();
        if self.hosts.iter().any(paused) || !self.peeking.insert(host.clone()) {
            return;
        }
        self.glanced.insert(host.clone(), clock);
        self.worker(move |send| {
            let sessions = machines::sessions(&phux, host.as_deref());
            send(Done::Glanced(host, sessions));
        });
    }

    /// Relist expanded machines whose listing has gone stale, and wake for the next.
    pub(super) fn tick_glances(&mut self, clock: f64, tx: &mut Tx) {
        let mut next = None::<f64>;
        for host in self.expanded.clone() {
            let due = self
                .glanced
                .get(&host)
                .map_or(0.0, |at| at + GLANCE_EVERY - clock);
            if due <= 0.0 {
                self.peek(host, clock);
            }
            let wait = if due <= 0.0 { GLANCE_EVERY } else { due };
            next = Some(next.map_or(wait, |next| next.min(wait)));
        }
        if let Some(next) = next {
            tx.wake_after(Duration::from_secs_f64(next.max(0.5)));
        }
    }

    fn add(&mut self, request: Add) {
        let Some(phux) = self.phux.clone() else {
            self.error = Some("Install the phux CLI to add a machine.".to_owned());
            return;
        };
        if self.adding.is_some() {
            return;
        }
        let doing = if request.code.is_some() {
            "Pairing"
        } else {
            "Setting up"
        };
        self.adding = Some(format!("{doing} {}…", request.target));
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

    fn pause(&mut self, name: String, paused: bool) {
        let Some(phux) = self.phux.clone() else {
            return;
        };
        self.worker(move |send| {
            let result = machines::pause(&phux, &name, paused);
            send(Done::Paused(name, result));
        });
    }

    /// Recolour `name` now, and have the CLI write it: `id` is a palette id
    /// or `auto`.
    fn recolor(&mut self, name: String, id: &'static str) {
        let Some(phux) = self.phux.clone() else {
            return;
        };
        crate::host_color::pick(&name, id);
        self.worker(move |send| {
            let result = machines::recolor(&phux, &name, id);
            send(Done::Recolored(name, result));
        });
    }

    /// The host a finished add registered, whatever it was named. Only the
    /// refresh after that add answers it; any other list is just a list.
    fn take_added(&mut self, refresh: u64) -> Option<String> {
        self.awaited.take_if(|awaited| *awaited == refresh)?;
        let before = self.before.take()?;
        let request = self.request.take()?;
        let host = machines::added(&self.hosts, &before, &request);
        host.map(|host| host.name.clone())
    }
}

impl Desk {
    /// Open or close a machine in the sidebar. This window's own machine
    /// folds its sessions away; another lists its sessions to glance at.
    pub fn toggle_host(&mut self, host: Option<String>) {
        if host == self.machines.current {
            self.machines.folded = !self.machines.folded;
        } else if !self.machines.expanded.remove(&host) {
            self.machines.expanded.insert(host.clone());
            self.machines.peek(host, self.clock);
        }
    }

    /// Move this window to `machine` and open `session` there.
    pub fn open_on(&mut self, machine: Option<String>, session: String) {
        self.go(machine, Some(session));
    }

    pub fn toggle_machines(&mut self) {
        if let Some(native) = &mut self.native {
            native.connection_requested = true;
            return;
        }
        if self.machines.open {
            self.machines.open = false;
        } else {
            self.show_machines();
            self.machines.field.clear();
            self.machines.field.focused = true;
        }
    }

    fn show_machines(&mut self) {
        self.machines.open = true;
        self.close_settings();
        self.close_inbox();
        self.machines.error = None;
        self.machines.refresh();
    }

    /// A connect link the system opened this app with (Rune's
    /// `on_open_url`): bring the window forward with Machines open and offer
    /// to pair. Any web page can open a link, so nothing is contacted or
    /// registered until the person accepts. A link that can't be used says
    /// why in the sheet.
    pub fn pair_from_link(&mut self, url: &str) {
        if self.native.is_some() {
            return;
        }
        if let Some(window) = &self.machines.window {
            window.open();
        }
        self.show_machines();
        self.machines.outcome = None;
        match crate::connect::request(url) {
            Err(why) => {
                self.machines.offer = None;
                self.machines.error = Some(why);
            }
            Ok(offer) => self.machines.offer = Some(offer),
        }
    }

    /// The offered link was accepted: pair, then move the window there.
    pub fn accept_pairing(&mut self) {
        if self.machines.adding.is_some() {
            self.machines.error =
                Some("Another machine is still being set up. Pair once it finishes.".to_owned());
            return;
        }
        if let Some(offer) = self.machines.offer.take() {
            self.machines.add(offer.request);
        }
    }

    pub fn decline_pairing(&mut self) {
        self.machines.offer = None;
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
            code: None,
        });
    }

    /// First press asks; the row's second answer removes. The window can't
    /// stay on a machine it no longer knows, so that one goes home first.
    pub fn ask_remove(&mut self, name: Option<String>) {
        self.machines.removing = name;
    }

    /// Open `name`'s colour swatches, or close them.
    pub fn pick_color(&mut self, name: Option<String>) {
        self.machines.picking = name.filter(|name| self.machines.picking.as_ref() != Some(name));
    }

    pub fn recolor_machine(&mut self, name: String, id: &'static str) {
        self.machines.picking = None;
        self.machines.recolor(name, id);
    }

    /// Pause `name` or resume it. Like a removal, the window leaves a
    /// machine it is about to stop dialing.
    pub fn pause_machine(&mut self, name: String, paused: bool) {
        if paused && self.machines.current.as_ref() == Some(&name) {
            self.switch_machine(None);
            self.machines.open = true;
        }
        self.machines.pause(name, paused);
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
            code: None,
        });
    }

    /// Move this window to `machine` and remember it for the next launch.
    /// Rebuilding drops the old connection and everything it knew. A machine
    /// that can't be dialed leaves the window where it is.
    pub fn switch_machine(&mut self, machine: Option<String>) {
        self.go(machine, None);
    }

    fn go(&mut self, machine: Option<String>, session: Option<String>) {
        if machine == self.machines.current {
            self.close_machines();
            return;
        }
        self.rebuild(machine, session);
    }

    /// Replace this window's connection with one to `machine`, even the
    /// one it is on: a machine set up again has new credentials.
    fn rebuild(&mut self, machine: Option<String>, session: Option<String>) {
        if let Some(Err(why)) = machine.as_ref().map(|m| Target::resolve(m, None)) {
            self.machines.error = Some(why);
            return;
        }
        // An aimed run stays out of what the next real launch reopens.
        if !self.launch.aimed {
            let saved = machine.clone().unwrap_or_default();
            let _ = self.prefs.set("machine", &saved);
        }
        self.left_server();
        let mut launch = self.launch.clone();
        launch.session = session;
        let mut next = Desk::open(&launch, machine.clone());
        self.carry_into(&mut next);
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
                Done::Listed(refresh, _, _) if refresh < self.machines.listed => {}
                Done::Listed(refresh, hosts, peers) => {
                    self.machines.listed = refresh;
                    match hosts {
                        Ok(hosts) => self.machines.hosts = hosts,
                        Err(error) => self.machines.error = Some(error),
                    }
                    self.machines.peers = peers;
                    if let Some(host) = self.machines.take_added(refresh) {
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
                            self.machines.awaited = Some(self.machines.refresh());
                        }
                        Err(error) => {
                            let request = self.machines.request.take();
                            let paired = request.is_some_and(|request| request.code.is_some());
                            self.machines.outcome = Some(Err(if paired {
                                pairing_failure(&error)
                            } else {
                                error
                            }));
                        }
                    }
                }
                Done::Glanced(host, sessions) => {
                    self.machines.peeking.remove(&host);
                    self.machines.glances.insert(host, sessions);
                }
                Done::Paused(name, result) => {
                    if let Err(error) = result {
                        self.machines.error = Some(format!("{name} not changed: {error}"));
                    }
                    self.machines.refresh();
                }
                Done::Removed(name, result) => {
                    if let Err(error) = result {
                        self.machines.error = Some(format!("{name} not removed: {error}"));
                    }
                    self.machines.refresh();
                }
                Done::Recolored(name, result) => {
                    if let Err(error) = result {
                        self.machines.error = Some(format!("{name}'s colour not saved: {error}"));
                    }
                    self.machines.refresh();
                }
            }
        }
        changed
    }

    /// An add finished. With the sheet still open that's a wish to go there,
    /// and the sheet says so. With it closed the person has gone back to
    /// work, so the window stays and only offers: moving it would send their
    /// next keys to another computer.
    fn landed(&mut self, host: String) {
        if !self.machines.open {
            self.machines.outcome = Some(Ok(format!("{host} is set up.")));
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

/// Why pairing from a link failed, in the CLI's words after a sentence
/// saying what failed; a CLI from before `host add --code` is told to update.
fn pairing_failure(error: &str) -> String {
    if error.contains("'--code'") {
        return "Couldn't pair from that link: this phux is too old to pair from a link. \
                Update it with `phux update`, then open the link again."
            .to_owned();
    }
    format!("Couldn't pair from that link.\n{error}")
}

#[cfg(test)]
mod tests;
