//! Work-first inventory across authorities. Browsing never acquires terminal views.

use std::collections::HashMap;

use gpui::{Context, Task};
use phux_client_runtime::control::Status;
use phux_protocol::ids::{AuthorityId, SessionKey};

use super::desk::Desk;
use super::notifications::Kind;
use crate::connection::Connection;

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

#[derive(Clone, Debug)]
pub(crate) struct Work {
    pub destination: Destination,
    pub id: u32,
    pub project: Option<(String, String)>,
    pub terminals: usize,
    pub needs_you: usize,
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
    pub revision: u64,
}

impl Fleet {
    pub fn clear(&mut self) {
        self.observers.clear();
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

    fn drain(&mut self, host: &Option<String>) {
        let Some(observer) = self.observers.get_mut(host) else {
            return;
        };
        let Some(connection) = observer.connection.as_mut() else {
            return;
        };
        connection.drain();
        let online = available(connection);
        observer.error = connection.error.clone();
        if online {
            observer.last = work_on(connection, host.clone());
            observer.error = None;
        } else {
            for work in &mut observer.last {
                work.online = false;
            }
            if connection.status.is_terminal() && observer.error.is_none() {
                observer.error = Some("Disconnected. Retry to refresh this host.".into());
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }
}

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
    let replica = connection.client.organization();
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
                needs_you: panes
                    .iter()
                    .filter(|pane| {
                        connection
                            .agents
                            .get(&pane.terminal_id)
                            .is_some_and(crate::connection::AgentInfo::needs_you)
                    })
                    .count(),
                intake: session.key.is_some() && session.key == topology.intake(),
                online: available(connection),
            }
        })
        .collect()
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
        for host in routes {
            if self
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
                                    desk.fleet.drain(&source);
                                    cx.notify();
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

    pub(crate) fn retry_fleet(&mut self, cx: &mut Context<Self>) {
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
