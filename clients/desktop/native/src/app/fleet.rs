//! Work-first inventory across authorities. Browsing never acquires terminal views.

use std::collections::{HashMap, HashSet};

use gpui::{Context, Task};
use phux_client_runtime::control::Status;
use phux_protocol::ids::{AuthorityId, SessionKey};

use super::desk::Desk;
use super::notifications::Kind;
use crate::connection::{AgentState, Connection};

/// A route chooses a connection; persistent identity chooses work on that authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Destination {
    pub host: Option<String>,
    pub authority: Option<AuthorityId>,
    pub key: Option<SessionKey>,
    pub name: String,
}

impl Destination {
    pub fn host_label(&self) -> &str {
        self.host.as_deref().unwrap_or("Local")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Work {
    pub destination: Destination,
    pub id: u32,
    pub project: Option<(String, String)>,
    pub terminals: usize,
    /// The most pressing state among its agents that need a person.
    pub attention: Option<AgentState>,
    pub intake: bool,
    pub online: bool,
}

struct Observer {
    connection: Option<Connection>,
    last: Vec<Work>,
    error: Option<String>,
    _wake: Option<Task<()>>,
}

impl Drop for Observer {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_mut() {
            connection.close();
        }
    }
}

#[derive(Default)]
pub(crate) struct Fleet {
    observers: HashMap<Option<String>, Observer>,
    /// Routes the user disconnected; not redialed until a retry.
    held: HashSet<Option<String>>,
    pub revision: u64,
}

impl Fleet {
    pub fn clear(&mut self) {
        self.observers.clear();
        self.held.clear();
    }

    pub fn failures(&self) -> Vec<(Option<String>, String)> {
        let mut failures: Vec<_> = self
            .observers
            .iter()
            .filter_map(|(host, observer)| {
                observer
                    .error
                    .as_ref()
                    .map(|error| (host.clone(), error.clone()))
            })
            .collect();
        failures.sort_by(|a, b| a.0.cmp(&b.0));
        failures
    }

    /// Fold one host's activity into its work. True when the work or the
    /// host's error changed.
    fn drain(&mut self, host: &Option<String>) -> bool {
        let Some(observer) = self.observers.get_mut(host) else {
            return false;
        };
        let Some(connection) = observer.connection.as_mut() else {
            return false;
        };
        connection.drain();
        let (mut last, mut error) = (Vec::new(), connection.error.clone());
        if available(connection) {
            last = work_on(connection, host.clone());
            error = None;
        } else {
            last.clone_from(&observer.last);
            for work in &mut last {
                work.online = false;
            }
            if connection.status.is_terminal() && error.is_none() {
                error = Some("Disconnected. Retry to refresh this host.".into());
            }
        }
        if observer.last == last && observer.error == error {
            return false;
        }
        observer.last = last;
        observer.error = error;
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

/// What a machine row says about its route right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Negotiated; its sessions are listed in Work.
    Connected,
    /// Dialing, or not observed yet.
    Connecting,
    /// The last dial failed or the route closed, with the reason.
    Unavailable(String),
}

impl Reach {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Connecting => "Connecting…",
            Self::Unavailable(_) => "Unavailable",
        }
    }
}

/// Why a machine the user disconnected is not listed live.
pub(crate) const DISCONNECTED: &str = "Disconnected. Retry to reconnect.";

fn available(connection: &Connection) -> bool {
    matches!(connection.status, Status::Negotiated | Status::Attached)
        && connection.topology.is_some()
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, value)| key == name && !value.is_empty())
        .map(|(_, value)| value.as_str())
}

fn work_on(connection: &Connection, host: Option<String>) -> Vec<Work> {
    let Some(topology) = connection.topology.as_ref() else {
        return Vec::new();
    };
    connection.with_organization(|replica| {
        topology
            .sessions
            .iter()
            .map(|session| {
                let record = session.key.and_then(|key| replica.session(&key));
                let project = record
                    .and_then(|record| {
                        let id = attr(&record.attrs, "project.id")?;
                        let name = attr(&record.attrs, "project.name").unwrap_or(id);
                        Some((id.to_owned(), name.to_owned()))
                    })
                    .or_else(|| {
                        connection
                            .projects
                            .get(&session.id)
                            .map(|label| (format!("label:{label}"), label.clone()))
                    });
                let panes: Vec<_> = connection
                    .panes()
                    .iter()
                    .filter(|pane| pane.session_id == session.id)
                    .collect();
                Work {
                    destination: Destination {
                        host: host.clone(),
                        authority: topology.authority,
                        key: session.key,
                        name: session.name.clone(),
                    },
                    id: session.id,
                    project,
                    terminals: panes.len(),
                    attention: super::ui::status::strongest(
                        panes
                            .iter()
                            .filter_map(|pane| connection.agents.get(&pane.terminal_id))
                            .filter(|info| info.needs_you())
                            .map(|info| info.state),
                    ),
                    intake: session.key.is_some() && session.key == topology.intake(),
                    online: available(connection),
                }
            })
            .collect()
    })
}

#[cfg(test)]
mod tests;

impl Desk {
    /// Every registered authority remains discoverable while the active projection changes.
    pub(crate) fn sync_fleet(&mut self, cx: &mut Context<Self>) {
        let mut routes: Vec<_> = self
            .hosts
            .iter()
            .map(|host| Some(host.name.clone()))
            .collect();
        if self.remote.is_some() || !self.hosts.is_empty() {
            routes.push(None);
        }
        self.fleet.observers.retain(|host, _| routes.contains(host));
        self.fleet.held.retain(|host| routes.contains(host));
        for host in routes {
            let held = self.fleet.held.contains(&host);
            if held
                || self
                    .fleet
                    .observers
                    .get(&host)
                    .is_some_and(|observer| observer.connection.is_some())
            {
                continue;
            }
            let last = self
                .fleet
                .observers
                .remove(&host)
                .map(|observer| observer.last.clone())
                .unwrap_or_default();
            let mut observer = Observer {
                connection: None,
                last,
                error: None,
                _wake: None,
            };
            match Connection::browse(&self.socket, host.as_deref()) {
                Ok(connection) => {
                    let rx = connection.activity();
                    let source = host.clone();
                    observer._wake = Some(cx.spawn(async move |desk, cx| {
                        while rx.recv().await.is_ok() {
                            if desk
                                .update(cx, |desk, cx| {
                                    if desk.fleet.drain(&source) {
                                        cx.notify();
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }));
                    observer.connection = Some(connection);
                }
                Err(error) => observer.error = Some(error.to_string()),
            }
            self.fleet.observers.insert(host, observer);
        }
        self.fleet.revision = self.fleet.revision.wrapping_add(1);
    }

    pub(crate) fn work_catalog(&self) -> Vec<Work> {
        let mut work: Vec<_> = self
            .fleet
            .observers
            .iter()
            .filter(|(host, _)| {
                **host != self.remote
                    || self
                        .link
                        .as_ref()
                        .is_none_or(|link| !available(&link.connection))
            })
            .flat_map(|(_, observer)| observer.last.clone())
            .collect();
        if let Some(link) = &self.link
            && available(&link.connection)
        {
            work.extend(work_on(&link.connection, self.remote.clone()));
        }
        work.sort_by(|a, b| {
            a.project
                .cmp(&b.project)
                .then(a.destination.name.cmp(&b.destination.name))
                .then(a.destination.host.cmp(&b.destination.host))
        });
        work
    }

    pub(crate) fn work_is_active(&self, destination: &Destination) -> bool {
        if self.remote != destination.host {
            return false;
        }
        let authority = self
            .link
            .as_ref()
            .and_then(|link| link.connection.topology.as_ref())
            .and_then(|topology| topology.authority);
        if authority != destination.authority {
            return false;
        }
        match destination.key {
            Some(key) => {
                self.link
                    .as_ref()
                    .and_then(|link| link.connection.home_key())
                    == Some(key)
            }
            None => self.session == destination.name,
        }
    }

    /// Revalidate the catalog destination before replacing the active projection.
    pub(crate) fn navigate_work(&mut self, destination: Destination, cx: &mut Context<Self>) {
        if self.work_is_active(&destination) {
            return;
        }
        let listed = self
            .work_catalog()
            .into_iter()
            .find(|work| work.destination == destination);
        if !listed.is_some_and(|work| work.online) {
            self.post(
                Kind::Refusal,
                "Work is unavailable",
                Some(
                    "The host is offline or the session changed. Refresh and choose it again."
                        .into(),
                ),
            );
            cx.notify();
            return;
        }
        self.adopt_work(
            destination.host,
            destination.name,
            destination.key,
            destination.authority,
            cx,
        );
    }

    /// One machine's state: this window's own link first, then its observer.
    pub(crate) fn machine_reach(&self, host: Option<&str>) -> Reach {
        if host == self.remote.as_deref() {
            match &self.link {
                Some(link) if available(&link.connection) => return Reach::Connected,
                Some(link) if link.connection.status.is_terminal() => {
                    return Reach::Unavailable(
                        link.connection
                            .error
                            .clone()
                            .unwrap_or_else(|| "Disconnected. Reconnect this window.".into()),
                    );
                }
                None => {
                    if let Some(error) = &self.connect_error {
                        return Reach::Unavailable(error.clone());
                    }
                }
                Some(_) => {}
            }
        }
        let Some(observer) = self.fleet.observers.get(&host.map(str::to_owned)) else {
            return Reach::Connecting;
        };
        if let Some(error) = &observer.error {
            return Reach::Unavailable(error.clone());
        }
        if observer.connection.as_ref().is_some_and(available) {
            Reach::Connected
        } else {
            Reach::Connecting
        }
    }

    /// Open a machine from its row: its intake, else its first live session.
    /// A machine that cannot be opened says why; an unavailable one is retried,
    /// or reconnected when it is this window's own route.
    pub(crate) fn open_machine(&mut self, host: Option<String>, cx: &mut Context<Self>) {
        let target = self
            .work_catalog()
            .into_iter()
            .filter(|work| work.destination.host == host && work.online)
            .min_by_key(|work| !work.intake)
            .map(|work| work.destination);
        if let Some(destination) = target {
            self.navigate_work(destination, cx);
            return;
        }
        let name = host.as_deref().unwrap_or("Local");
        match self.machine_reach(host.as_deref()) {
            Reach::Unavailable(_) if host == self.remote => {
                self.post(Kind::Info, &format!("Reconnecting {name}"), None);
                self.reconnect(cx);
            }
            Reach::Unavailable(_) => {
                self.post(Kind::Info, &format!("Retrying {name}"), None);
                self.retry_fleet(cx);
            }
            Reach::Connecting => {
                self.post(Kind::Info, &format!("{name} is still connecting"), None);
            }
            Reach::Connected => self.post(
                Kind::Refusal,
                &format!("{name} has no sessions"),
                Some("Create one there with `phux new -s NAME`, then refresh.".into()),
            ),
        }
        cx.notify();
    }

    /// Stop observing another machine: close its inventory connection, keep
    /// its work listed offline, and leave it down until a retry. This
    /// window's own route reconnects instead; it is never disconnected here.
    pub(crate) fn disconnect_machine(&mut self, host: Option<String>, cx: &mut Context<Self>) {
        if host == self.remote {
            return;
        }
        if let Some(observer) = self.fleet.observers.get_mut(&host) {
            if let Some(mut connection) = observer.connection.take() {
                connection.close();
            }
            observer._wake = None;
            for work in &mut observer.last {
                work.online = false;
            }
            observer.error = Some(DISCONNECTED.into());
        }
        self.fleet.held.insert(host);
        self.fleet.revision = self.fleet.revision.wrapping_add(1);
        cx.notify();
    }

    pub(crate) fn retry_fleet(&mut self, cx: &mut Context<Self>) {
        self.fleet.held.clear();
        for observer in self.fleet.observers.values_mut() {
            if observer
                .connection
                .as_ref()
                .is_some_and(|connection| connection.client.status().is_terminal())
            {
                observer.connection = None;
                observer._wake = None;
                for work in &mut observer.last {
                    work.online = false;
                }
            } else if let Some(connection) = &observer.connection {
                connection.client.resync();
            }
        }
        self.refresh_hosts(cx);
        self.sync_fleet(cx);
        cx.notify();
    }
}
