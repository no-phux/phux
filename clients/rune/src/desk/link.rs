//! The window's connection: finding the local server off the UI thread,
//! dialing, saying where things stand, and getting back after a failure.
//!
//! The runtime rides out a dropped socket itself, so a stale window usually
//! has a live client and only needs the server back (`phux server --ensure`).
//! A client that ended (a refusal, a first connection that never landed)
//! can't reopen, so this replaces it and keeps the rest of the window.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use phux_client_runtime::control::Status;
use phux_client_runtime::reconnect::is_fatal_refusal_detail;
use phux_client_runtime::{Client, Target};
use phux_protocol::ids::SessionKey;
use rune::Tx;

use super::{Desk, Notice, NoticeKind, sessions};
use crate::launch::Resolved;
use crate::machines::LOCAL;

/// The longest wait between automatic attempts on an ended connection.
const RETRY_CEILING: Duration = Duration::from_secs(30);
/// How long a connection may be down before anyone is told: a resync or a
/// server's own upgrade is over by then.
const BANNER_AFTER: Duration = Duration::from_secs(1);
/// How long a local server may stay down before this starts it again.
const ENSURE_AFTER: Duration = Duration::from_secs(3);
/// How long between those starts.
const ENSURE_EVERY: Duration = Duration::from_secs(30);

/// How the runtime renders a refusal of this client's protocol major.
const INCOMPATIBLE: &str = "VersionIncompatible";

/// A search's attempt number and what it found.
type Found = (u64, Resolved);

pub struct Link {
    /// Bumped by each search for the server: only the latest answer counts.
    attempt: u64,
    found: (Sender<Found>, Receiver<Found>),
    /// A search for the server is running.
    finding: bool,
    /// Automatic attempts since the last good connection.
    tries: u32,
    retry_at: Option<Instant>,
    /// Since when the connection has been down.
    down_since: Option<Instant>,
    /// The banner has had its redraw.
    warned: bool,
    last_ensure: Option<Instant>,
    updater_started: bool,
    /// What the server said when it ended the attach.
    pub ended: Option<String>,
    /// The server refused this client's protocol, in its own words.
    pub incompatible: Option<String>,
    /// The session to return to, across a reconnect.
    pub home: Option<(String, Option<SessionKey>)>,
    /// The connection on which the organization records can't be read.
    pub org_unavailable: Option<u64>,
    /// `new_session` lent the runtime its automatic attach, which carries a
    /// create across the redial.
    pub creating: bool,
    /// Why this window isn't on the machine it remembered, for after its
    /// first terminal opens.
    pub moved: Option<String>,
}

impl Default for Link {
    fn default() -> Self {
        Self {
            attempt: 0,
            found: channel(),
            finding: false,
            tries: 0,
            retry_at: None,
            down_since: None,
            warned: false,
            last_ensure: None,
            updater_started: false,
            ended: None,
            incompatible: None,
            home: None,
            org_unavailable: None,
            creating: false,
            moved: None,
        }
    }
}

impl Link {
    fn is_down_for(&self, wait: Duration) -> bool {
        self.down_since.is_some_and(|since| since.elapsed() >= wait)
    }

    fn up(&mut self) {
        self.warned = false;
        self.tries = 0;
        self.retry_at = None;
        self.down_since = None;
        self.last_ensure = None;
        self.ended = None;
        self.incompatible = None;
    }
}

/// The wait before automatic attempt `tries`: 2 s doubling to the ceiling.
fn backoff(tries: u32) -> Duration {
    Duration::from_secs(2u64.saturating_mul(1u64 << tries.min(8))).min(RETRY_CEILING)
}

/// A failure in words, without the transport's.
pub fn plainly(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if is_fatal_refusal_detail(error) {
        "This machine no longer accepts this computer. Set it up again from Machines.".to_owned()
    } else if lower.contains("refused") || lower.contains("no such file") {
        "The server isn't answering.".to_owned()
    } else if lower.contains("no answer") || lower.contains("timed out") {
        "No answer from the machine.".to_owned()
    } else {
        error.to_owned()
    }
}

/// The server's words from a protocol refusal, when `error` is one.
fn incompatible(error: &str) -> Option<String> {
    let (_, words) = error.split_once(INCOMPATIBLE)?;
    Some(words.trim_start_matches([':', ' ']).to_owned())
}

/// Where the window stands when there is no terminal to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// `phux server --ensure` is running.
    Starting,
    /// No CLI to start a server with.
    NoCli,
    /// The server speaks another protocol major.
    Incompatible(String),
    /// Nothing answered, and this keeps trying unless retrying can't help.
    Unreachable(String),
    /// The runtime is dialing, with the last thing it heard.
    Connecting(String),
    /// The connection ended on purpose.
    Closed(String),
    /// A server with no sessions at all.
    NoSessions,
    /// The selected session holds no terminals.
    NoTerminals,
    /// Nothing is wrong; a terminal is on its way.
    Opening,
}

impl Desk {
    /// Route a new attempt: a remote dials at once, a named socket too, and
    /// this computer's server is found on a worker first.
    pub(super) fn dial(&mut self) {
        self.error = None;
        let Some(machine) = self.machines.current.clone() else {
            return self.dial_local();
        };
        match Target::resolve(&machine, None) {
            Ok(target) => self.connect(target),
            Err(why) => self.lost_remembered(&machine, why),
        }
    }

    fn dial_local(&mut self) {
        match (self.launch.can_ensure(), self.launch.socket.clone()) {
            (false, Ok(socket)) => self.connect(Target::uds(socket)),
            _ => self.find_server(),
        }
    }

    /// The machine this window remembered can't be dialed: this computer
    /// stands in, and a host that's gone is forgotten for the next launch.
    fn lost_remembered(&mut self, machine: &str, why: String) {
        if why.contains("not a registered host") {
            let _ = self.prefs.set("machine", &String::new());
        }
        self.machines.current = None;
        self.link.moved = Some(format!("{machine} isn't available, so this is {LOCAL}."));
        self.dial_local();
    }

    fn connect(&mut self, target: Target) {
        match sessions::connect(target) {
            Ok(client) => {
                self.client = Some(std::sync::Arc::new(client));
                if let Some(waker) = self.waker.clone() {
                    self.watch(waker);
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// Ask the CLI for the local server on a worker. The answer is never
    /// waited for: a stale one is dropped by `pump_link`.
    pub(super) fn find_server(&mut self) {
        self.link.attempt += 1;
        self.link.finding = true;
        self.link.last_ensure = Some(Instant::now());
        let attempt = self.link.attempt;
        let launch = self.launch.clone();
        let sender = self.link.found.0.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let _ = sender.send((attempt, launch.ensured()));
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    /// Start the updater once it knows which CLI and server it manages.
    pub(super) fn start_updater(&mut self) {
        if std::mem::replace(&mut self.link.updater_started, true) {
            return;
        }
        if let Some(waker) = self.waker.clone() {
            self.updater.start(waker);
        }
    }

    pub(super) fn pump_link(&mut self) -> bool {
        let mut changed = false;
        while let Ok((attempt, found)) = self.link.found.1.try_recv() {
            if attempt == self.link.attempt {
                self.link.finding = false;
                self.adopt(found);
                changed = true;
            }
        }
        changed
    }

    /// The worker answered. A window on a remote only learns what to update.
    fn adopt(&mut self, found: Resolved) {
        let moved = found.socket != self.launch.socket;
        self.machines.phux = found.phux.clone();
        let server = found.socket.clone().ok();
        self.updater.target = found.phux.clone().zip(server);
        self.updater
            .target
            .take_if(|_| self.updater.install.is_err() || found.aimed);
        self.start_updater();
        self.launch.phux = found.phux;
        self.launch.socket = found.socket;
        if self.machines.current.is_some() {
            return;
        }
        match self.launch.socket.clone() {
            Err(why) => self.error = Some(why),
            Ok(socket) => {
                self.error = None;
                self.reach(socket, moved);
            }
        }
    }

    /// The local server is up at `socket`: use it, by the client that's
    /// still dialing when that client can get there.
    fn reach(&mut self, socket: std::path::PathBuf, moved: bool) {
        match &self.client {
            Some(client) if !moved && !client.status().is_terminal() => client.nudge(),
            Some(_) => {
                self.renew();
                self.connect(Target::uds(socket));
            }
            None => self.connect(Target::uds(socket)),
        }
    }

    /// Retry from the placeholder, the banner or the machine row.
    pub fn reconnect(&mut self) {
        self.link.tries = 0;
        self.link.retry_at = None;
        self.link.incompatible = None;
        self.attempt();
    }

    fn attempt(&mut self) {
        let alive = self
            .client
            .as_ref()
            .filter(|client| !client.status().is_terminal());
        if let Some(client) = alive {
            client.nudge();
            if self.machines.current.is_none() && self.launch.can_ensure() {
                self.find_server();
            }
            return;
        }
        self.renew();
        self.dial();
    }

    /// Take down what the old connection knew; what the window itself holds
    /// stays, so a background retry doesn't close the sheet you're in.
    fn renew(&mut self) {
        let mut next = Desk::open(&self.launch, self.machines.current.clone());
        self.carry_into(&mut next);
        std::mem::swap(&mut next.inbox, &mut self.inbox);
        std::mem::swap(&mut next.activities, &mut self.activities);
        std::mem::swap(&mut next.palette_query, &mut self.palette_query);
        next.palette_open = self.palette_open;
        next.palette_picked = self.palette_picked;
        next.settings_open = self.settings_open;
        next.sidebar_open = self.sidebar_open;
        next.wanted = self
            .shown
            .as_ref()
            .map(|shown| (shown.terminal.clone(), Instant::now()));
        *self = next;
    }

    /// What survives any replacement of this window's connection.
    pub(super) fn carry_into(&mut self, next: &mut Desk) {
        std::mem::swap(&mut next.updater, &mut self.updater);
        std::mem::swap(&mut next.machines, &mut self.machines);
        std::mem::swap(&mut next.link, &mut self.link);
        next.generation = self.generation + 1;
        next.focused = self.focused;
        next.waker = self.waker.clone();
    }

    /// Whether the connection is working: anything else waits on this module.
    fn link_is_up(&self) -> bool {
        self.client.is_some() && self.status == Status::Attached && self.link.ended.is_none()
    }

    /// Retrying can't change this answer: someone has to act first.
    fn stuck(&self) -> bool {
        self.link.incompatible.is_some()
            || self.status == Status::Closed
            || self.error.as_deref().is_some_and(is_fatal_refusal_detail)
    }

    /// Note a terminal failure the runtime reported.
    pub(super) fn failed_with(&mut self, error: &str) {
        self.link.incompatible = incompatible(error);
    }

    /// Once per poll: keep trying while the connection is down. True when
    /// the window should redraw.
    pub(super) fn tend_link(&mut self, tx: &mut Tx) -> bool {
        if self.link_is_up() {
            let was_down = self.link.down_since.is_some();
            self.link.up();
            return was_down;
        }
        let now = Instant::now();
        if self.link.down_since.is_none() {
            self.link.down_since = Some(now);
            tx.wake_after(BANNER_AFTER);
        }
        // The banner has nothing else to redraw it.
        let due = self.link.is_down_for(BANNER_AFTER);
        let warn = due && !std::mem::replace(&mut self.link.warned, due);
        let dead = self
            .client
            .as_ref()
            .is_none_or(|c| c.status().is_terminal());
        if self.link.finding {
            warn
        } else if dead {
            warn | self.tend_dead(now, tx)
        } else {
            self.tend_dialing();
            warn
        }
    }

    /// An ended connection: a new one on a widening schedule.
    fn tend_dead(&mut self, now: Instant, tx: &mut Tx) -> bool {
        if self.stuck() {
            return false;
        }
        let Some(at) = self.link.retry_at else {
            let wait = backoff(self.link.tries);
            self.link.retry_at = Some(now + wait);
            tx.wake_after(wait);
            return false;
        };
        if now < at {
            return false;
        }
        self.link.retry_at = None;
        self.link.tries += 1;
        self.attempt();
        true
    }

    /// A live client walking its ladder, whose every step wakes this: a
    /// local server may need starting.
    fn tend_dialing(&mut self) {
        let local = self.machines.current.is_none() && self.launch.can_ensure();
        let since = self.link.last_ensure.map(|at| at.elapsed());
        let due = since.is_none_or(|since| since >= ENSURE_EVERY);
        if local && due && self.link.is_down_for(ENSURE_AFTER) {
            self.find_server();
        }
    }

    /// Back at the window: try now, whatever the schedule said.
    pub(super) fn hurry_link(&mut self) {
        if let Some(client) = &self.client {
            client.nudge();
        }
        if self.link.retry_at.is_some() {
            self.link.retry_at = Some(Instant::now());
        }
    }

    pub fn standing(&self) -> Standing {
        if let Some(words) = &self.link.incompatible {
            return Standing::Incompatible(words.clone());
        }
        let why = || {
            let last = self.client.as_ref().and_then(|client| client.last_error());
            let error = self.error.clone().or(last).unwrap_or_default();
            plainly(&error)
        };
        match (&self.client, self.status) {
            (None, _) if self.link.finding => Standing::Starting,
            (None, _) if self.machines.current.is_none() && self.launch.missing_cli() => {
                Standing::NoCli
            }
            (None, _) | (Some(_), Status::Failed) => Standing::Unreachable(why()),
            (Some(_), Status::Closed) => {
                Standing::Closed(self.link.ended.clone().unwrap_or_default())
            }
            (Some(_), Status::Attached) if self.link.ended.is_none() => self.inventory(),
            (Some(_), _) => Standing::Connecting(why()),
        }
    }

    /// Connected, with nothing on screen: is there anything to show?
    fn inventory(&self) -> Standing {
        let selected = self.client.as_ref().and_then(|c| c.selected_session());
        let Some(topology) = &self.topology else {
            return Standing::Opening;
        };
        let session = topology.sessions.iter().find(|s| Some(s.id) == selected);
        let waiting = self.pending_session.is_some() || !self.pending_spawns.is_empty();
        match session {
            _ if waiting => Standing::Opening,
            None if topology.sessions.is_empty() => Standing::NoSessions,
            Some(s) if !topology.panes.iter().any(|p| p.session_id == s.id) => {
                Standing::NoTerminals
            }
            _ => Standing::Opening,
        }
    }

    /// `(what happened, what it means)` over panes that have stopped
    /// updating. Silent for the first moment of any drop.
    pub fn banner(&self) -> Option<(String, String)> {
        if self.shown.is_none() || self.link_is_up() || !self.link.is_down_for(BANNER_AFTER) {
            return None;
        }
        let name = self.machines.label();
        let last = self.client.as_ref().and_then(|client| client.last_error());
        let why = self.link.ended.clone().or(last).map(|why| plainly(&why));
        Some((
            format!("Lost {name}. Reconnecting…"),
            why.unwrap_or_else(|| "Your sessions keep running on the server.".to_owned()),
        ))
    }

    /// A word for the current machine's row.
    pub fn link_word(&self) -> &'static str {
        match self.status {
            _ if self.link_is_up() => "connected",
            Status::Failed | Status::Closed => "not connected",
            _ if self.client.is_none() && !self.link.finding => "not connected",
            _ => "connecting",
        }
    }

    pub fn link_notice(&mut self) -> Option<Notice> {
        let text = self.link.moved.take()?;
        Some(Notice {
            kind: NoticeKind::Error,
            text,
        })
    }

    /// Whether the server lists `terminal`.
    pub(super) fn lists(&self, terminal: &phux_protocol::ids::ResourceId) -> bool {
        self.topology
            .as_ref()
            .is_some_and(|t| t.panes.iter().any(|p| &p.terminal_id == terminal))
    }
}

/// Whether a client's control plane has the records a home choice reads.
pub(super) fn seeded(client: &Client, unavailable: Option<u64>) -> bool {
    client.with_control(|control| {
        !control.organization_supported()
            || unavailable == Some(control.connection_epoch())
            || control.organization().is_live()
    })
}

#[cfg(test)]
mod tests;
