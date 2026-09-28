//! Structured session-list projection: the `phux ls --json` read shape.
//!
//! A stable CLI+JSON contract (ADR-0022). Every key after `sessions` is
//! additive: consumers ignore unknown keys, `#[serde(default)]` keeps older
//! payloads readable, and only removing, renaming, or retyping a key bumps
//! [`LS_SCHEMA_VERSION`]. The wire-to-JSON mapping lives in the binary;
//! `phux-core` deliberately does not depend on `phux-protocol`.

use serde::{Deserialize, Serialize};

/// Stable JSON contract version for [`SessionListJson`], independent of
/// [`crate::screen::SCHEMA_VERSION`].
pub const LS_SCHEMA_VERSION: u32 = 3;

/// One session's entry in the [`SessionListJson`] output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionJson {
    /// Session name (what `phux attach <name>` matches against).
    pub name: String,
    /// Number of windows in the session.
    pub windows: u16,
    /// Whether at least one client is attached; always
    /// `attached_clients > 0`, kept because consumers branch on it.
    pub attached: bool,
    /// Number of clients currently attached.
    #[serde(default)]
    pub attached_clients: u16,
    /// Whether the session survives its last window (ADR-0105).
    #[serde(default)]
    pub keep_empty: bool,
    /// Whether the session holds no windows right now (`windows == 0`).
    #[serde(default)]
    pub empty: bool,
}

/// One resource's row in [`SessionListJson::resources`]: canonical selector,
/// kind, parent, and lifecycle. `terminals` keeps the bare selectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceJson {
    /// Canonical selector (`@N` / `host/@N`).
    pub id: String,
    /// Resource kind, lower-case: `terminal`, `agent_session`, or `unknown`.
    pub kind: String,
    /// Canonical selector of the parent, or `null` for a root (emitted, not
    /// omitted).
    #[serde(default)]
    pub parent: Option<String>,
    /// Process lifecycle: `running`, `frozen`, or `exited` (ADR-0124).
    /// Older payloads read as `running`.
    #[serde(default = "running_lifecycle")]
    pub lifecycle: String,
    /// How a retained resource's process ended; `null` while it runs.
    #[serde(default)]
    pub exit: Option<ResourceExitJson>,
}

fn running_lifecycle() -> String {
    "running".to_owned()
}

/// How a retained resource's process ended (ADR-0124), in
/// [`ResourceJson::exit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceExitJson {
    /// `_exit(n)` status, or `null` for a signal death or an unknown status.
    #[serde(default)]
    pub status: Option<i32>,
    /// Terminating signal, or `null`.
    #[serde(default)]
    pub signal: Option<i32>,
    /// Why it ended, in the `RESOURCE_CLOSED.reason` vocabulary, or `null`.
    /// Tolerate an unknown value.
    #[serde(default)]
    pub reason: Option<String>,
    /// When the process exited, Unix milliseconds.
    pub exited_at_ms: u64,
    /// When the server will close the resource, Unix milliseconds.
    pub retained_until_ms: u64,
}

/// One host's group in [`SessionListJson::hosts`]: this host first, then
/// each federation satellite a hub dials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostJson {
    /// The satellite's hub-local name, or `null` for this host.
    pub host: Option<String>,
    /// `true` only for this host's group.
    pub local: bool,
    /// Whether the hub listed this host. This host is always reachable.
    pub reachable: bool,
    /// The hub's diagnostic when `reachable` is `false`. Branch on
    /// `reachable`, not on this text.
    #[serde(default)]
    pub unreachable: Option<String>,
    /// The host's sessions, name-sorted. Empty for an unreachable host.
    pub sessions: Vec<HostSessionJson>,
}

/// One session in a [`HostJson`] group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSessionJson {
    /// Session name on its own host.
    pub name: String,
    /// The session id on its own host (satellite ids are satellite-local).
    pub id: u32,
    /// Number of windows in the session.
    pub windows: u16,
    /// Number of Terminal-kind resources across the session's windows.
    pub panes: u16,
    /// Whether at least one client is attached (on its own host).
    pub attached: bool,
    /// Number of clients attached (on its own host).
    pub attached_clients: u16,
    /// Canonical selector of the session's remembered focused pane.
    #[serde(default)]
    pub active_terminal: Option<String>,
}

/// The `phux ls --json` payload, sessions name-sorted like `phux ls`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListJson {
    /// Contract version; see [`LS_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// This host's sessions, sorted by name.
    pub sessions: Vec<SessionJson>,
    /// Every addressable Terminal in snapshot order, as a canonical selector.
    #[serde(default)]
    pub terminals: Vec<String>,
    /// One diagnostic per satellite a federation hub failed to reach. Emitted
    /// even when empty, so `[]` positively means "complete" (an absent key
    /// is an older `phux`). Branch on emptiness, not text.
    #[serde(default)]
    pub unreachable: Vec<String>,
    /// Every addressable resource with its kind and parent, in `terminals`
    /// order.
    #[serde(default)]
    pub resources: Vec<ResourceJson>,
    /// The inventory grouped by host, reachable or not.
    #[serde(default)]
    pub hosts: Vec<HostJson>,
    /// Whether [`Self::hosts`] names every satellite (the server advertises
    /// `HOST_SESSIONS`). `false` means the satellites are unknown.
    #[serde(default)]
    pub hosts_complete: bool,
}

impl SessionListJson {
    /// Wrap an already-sorted session list at the current
    /// [`LS_SCHEMA_VERSION`]; this does not reorder.
    #[must_use]
    pub const fn new(sessions: Vec<SessionJson>) -> Self {
        Self {
            schema_version: LS_SCHEMA_VERSION,
            sessions,
            terminals: Vec::new(),
            unreachable: Vec::new(),
            resources: Vec::new(),
            hosts: Vec::new(),
            hosts_complete: false,
        }
    }

    /// Set [`Self::hosts_complete`].
    #[must_use]
    pub const fn with_hosts_complete(mut self, complete: bool) -> Self {
        self.hosts_complete = complete;
        self
    }

    /// Set [`Self::resources`].
    #[must_use]
    pub fn with_resources(mut self, resources: Vec<ResourceJson>) -> Self {
        self.resources = resources;
        self
    }

    /// Set [`Self::terminals`].
    #[must_use]
    pub fn with_terminals(mut self, terminals: Vec<String>) -> Self {
        self.terminals = terminals;
        self
    }

    /// Set [`Self::hosts`].
    #[must_use]
    pub fn with_hosts(mut self, hosts: Vec<HostJson>) -> Self {
        self.hosts = hosts;
        self
    }

    /// Set [`Self::unreachable`].
    #[must_use]
    pub fn with_unreachable(mut self, unreachable: Vec<String>) -> Self {
        self.unreachable = unreachable;
        self
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use serde_json::json;

    use super::*;

    /// Pins the whole `phux ls --json` shape: every key, `null` rather than
    /// omitted optionals, and empty-but-present `unreachable`.
    #[test]
    fn serializes_to_the_stable_json_shape() {
        let list = SessionListJson::new(vec![SessionJson {
            name: "work".to_owned(),
            windows: 3,
            attached: true,
            attached_clients: 2,
            keep_empty: false,
            empty: false,
        }])
        .with_terminals(vec!["@7".to_owned(), "@9".to_owned()])
        .with_resources(vec![
            ResourceJson {
                id: "@7".to_owned(),
                kind: "terminal".to_owned(),
                parent: None,
                lifecycle: "exited".to_owned(),
                exit: Some(ResourceExitJson {
                    status: Some(42),
                    signal: None,
                    reason: Some("exited".to_owned()),
                    exited_at_ms: 10,
                    retained_until_ms: 20,
                }),
            },
            ResourceJson {
                id: "@9".to_owned(),
                kind: "agent_session".to_owned(),
                parent: Some("@7".to_owned()),
                lifecycle: "running".to_owned(),
                exit: None,
            },
        ])
        .with_hosts(vec![
            HostJson {
                host: None,
                local: true,
                reachable: true,
                unreachable: None,
                sessions: vec![HostSessionJson {
                    name: "work".to_owned(),
                    id: 1,
                    windows: 3,
                    panes: 4,
                    attached: true,
                    attached_clients: 2,
                    active_terminal: Some("@3".to_owned()),
                }],
            },
            HostJson {
                host: Some("down".to_owned()),
                local: false,
                reachable: false,
                unreachable: Some("satellite down is unreachable".to_owned()),
                sessions: Vec::new(),
            },
        ])
        .with_hosts_complete(true);

        assert_eq!(
            serde_json::to_value(&list).expect("serialize"),
            json!({
                "schema_version": 3,
                "sessions": [{
                    "name": "work", "windows": 3, "attached": true,
                    "attached_clients": 2, "keep_empty": false, "empty": false
                }],
                "terminals": ["@7", "@9"],
                "unreachable": [],
                "resources": [
                    {
                        "id": "@7", "kind": "terminal", "parent": null, "lifecycle": "exited",
                        "exit": {
                            "status": 42, "signal": null, "reason": "exited",
                            "exited_at_ms": 10, "retained_until_ms": 20
                        }
                    },
                    {
                        "id": "@9", "kind": "agent_session", "parent": "@7",
                        "lifecycle": "running", "exit": null
                    }
                ],
                "hosts": [
                    {
                        "host": null, "local": true, "reachable": true, "unreachable": null,
                        "sessions": [{
                            "name": "work", "id": 1, "windows": 3, "panes": 4,
                            "attached": true, "attached_clients": 2, "active_terminal": "@3"
                        }]
                    },
                    {
                        "host": "down", "local": false, "reachable": false,
                        "unreachable": "satellite down is unreachable", "sessions": []
                    }
                ],
                "hosts_complete": true
            })
        );
    }

    /// Payloads from an older `phux` without the additive keys still read,
    /// with fail-safe defaults.
    #[test]
    fn older_payloads_deserialize_with_defaults() {
        let old: SessionListJson = serde_json::from_value(json!({
            "schema_version": 1,
            "sessions": [{ "name": "work", "windows": 3, "attached": true }]
        }))
        .expect("older payloads remain deserializable");
        assert_eq!(old.sessions[0].attached_clients, 0);
        assert!(old.terminals.is_empty() && old.unreachable.is_empty());
        assert!(old.resources.is_empty() && old.hosts.is_empty());
        assert!(!old.hosts_complete);

        let resource: ResourceJson =
            serde_json::from_value(json!({ "id": "@3", "kind": "terminal" }))
                .expect("pre-lifecycle payloads remain deserializable");
        assert_eq!(resource.lifecycle, "running");
        assert!(resource.exit.is_none());
    }
}
