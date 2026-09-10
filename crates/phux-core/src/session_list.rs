//! Structured session-list projection — the `phux ls --json` read shape
//! (ADR-0022 §"stable CLI+JSON contract").
//!
//! A [`SessionListJson`] is the stable, versioned JSON the CLI emits for
//! `phux ls --json`. It is a plain-data projection of the per-session fields
//! a caller needs to enumerate sessions: name, window count, and whether any
//! client is attached. The top-level `terminals` inventory carries canonical
//! direct selectors (`@N` / `host/@N`), including satellite Terminals that
//! deliberately have no hub-local session/window join. The top-level
//! `unreachable` list is the machine channel for *incompleteness*: a
//! federation hub that could not reach a satellite still answers, so without
//! it a partial listing is byte-identical to a complete one. Richer per-session
//! detail (creation time, ids, window layout) is a future additive field,
//! not a new struct — mirroring how
//! [`crate::screen::ScreenState`] reserves `--cells`/`--scrollback` growth.
//!
//! This type lives in `phux-core` (not the binary) so the shape has a single
//! documented, testable home shared with the rest of the JSON contract. The
//! mapping *into* it from the wire `SessionInfo` happens in the binary, where
//! both the protocol type and this one are in scope — `phux-core` deliberately
//! does not depend on `phux-protocol`.

use serde::{Deserialize, Serialize};

/// Stable JSON contract version for [`SessionListJson`] (ADR-0022). Bump on
/// any breaking change to the shape so consumers can pin or branch.
///
/// Tracked independently of [`crate::screen::SCHEMA_VERSION`] because the two
/// contracts (`phux snapshot --json` vs `phux ls --json`) evolve separately.
pub const LS_SCHEMA_VERSION: u32 = 3;

/// One session's entry in the [`SessionListJson`] output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionJson {
    /// Session name (what `phux attach <name>` matches against).
    pub name: String,
    /// Number of windows in the session.
    pub windows: u16,
    /// Whether at least one client is currently attached.
    ///
    /// Kept even though [`Self::attached_clients`] subsumes it: removing (or
    /// renaming) a key is the breaking move that forces a
    /// [`LS_SCHEMA_VERSION`] bump, and existing consumers branch on this
    /// bool. `attached` is always `attached_clients > 0`.
    pub attached: bool,
    /// Number of clients currently attached to the session — the wire's
    /// `attached_client_count`, no longer collapsed to a bool.
    ///
    /// **Additive, therefore non-breaking** (ADR-0022 stance): consumers of
    /// this contract must ignore unknown keys, so gaining a key does not bump
    /// [`LS_SCHEMA_VERSION`]; only removing, renaming, or retyping one does.
    /// `#[serde(default)]` keeps payloads from a pre-`attached_clients`
    /// `phux` deserializable (the count reads as `0`).
    #[serde(default)]
    pub attached_clients: u16,
}

/// One resource's row in [`SessionListJson::resources`]: the canonical
/// selector, its kind, and its parent when the kind has one.
///
/// **Additive** to the contract (no [`LS_SCHEMA_VERSION`] bump): `terminals`
/// keeps listing every addressable resource as a bare selector, and this
/// array carries the kind facts beside it. Consumers that predate resource
/// kinds keep reading `terminals`; consumers that need to skip non-terminal
/// kinds branch on `kind` here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceJson {
    /// Canonical selector for the resource (`@N` / `host/@N`), the same
    /// string the `terminals` inventory carries.
    pub id: String,
    /// Resource kind, lower-case: `terminal`, `agent_session`, or `unknown`
    /// for a kind this binary does not know.
    pub kind: String,
    /// Canonical selector of the parent resource, or `None` for a root
    /// (every Terminal-kind resource). Emitted as `null` rather than
    /// omitted, so a consumer reads "root" positively.
    #[serde(default)]
    pub parent: Option<String>,
}

/// One host's group in [`SessionListJson::hosts`]: this host first, then
/// each federation satellite a hub dials.
///
/// **Additive** (no [`LS_SCHEMA_VERSION`] bump): `sessions` keeps listing
/// this host's sessions only, and this array carries the per-host grouping
/// beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostJson {
    /// The satellite's hub-local name, or `null` for this host (the server
    /// `phux ls` talked to). Emitted as `null` rather than omitted.
    pub host: Option<String>,
    /// `true` only for this host's group.
    pub local: bool,
    /// Whether the hub listed this host. This host is always reachable.
    pub reachable: bool,
    /// The hub's diagnostic when `reachable` is `false`; `null` otherwise.
    /// Branch on `reachable`, not on this text.
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
    /// The session id on its own host. A satellite's id is satellite-local
    /// and never comparable with this host's ids.
    pub id: u32,
    /// Number of windows in the session.
    pub windows: u16,
    /// Number of Terminal-kind resources across the session's windows.
    pub panes: u16,
    /// Whether at least one client is attached (on its own host).
    pub attached: bool,
    /// Number of clients attached (on its own host).
    pub attached_clients: u16,
    /// Canonical selector (`@N` / `host/@N`) of the session's remembered
    /// focused pane, when known. For a satellite session this is the handle
    /// every Terminal-facet verb accepts through the hub.
    #[serde(default)]
    pub active_terminal: Option<String>,
}

/// The `phux ls --json` payload: a versioned list of sessions.
///
/// Sessions are emitted in the same name-sorted order as the human
/// `phux ls` text so the two views stay consistent and the JSON is stable
/// across runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListJson {
    /// Contract version; see [`LS_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Sessions, sorted by name.
    pub sessions: Vec<SessionJson>,
    /// Every addressable Terminal in snapshot order, as a canonical selector.
    #[serde(default)]
    pub terminals: Vec<String>,
    /// Parts of the fleet this listing could not see — one diagnostic per
    /// satellite a federation hub failed to reach while merging its answer.
    ///
    /// Empty means the listing is complete, and it is emitted **even when
    /// empty**: that is the whole point of the field. A consumer asking "does
    /// `sessions` contain everything?" gets a positive answer from
    /// `unreachable == []` rather than having to infer completeness from a
    /// key's absence — which is indistinguishable from talking to an older
    /// `phux` that never had the key. The human `phux ls` says the same thing
    /// on stderr; this is the machine channel for it.
    ///
    /// The strings are the hub's prose (they name the satellite); branch on
    /// emptiness, not on their text.
    #[serde(default)]
    pub unreachable: Vec<String>,
    /// Every addressable resource with its kind and parent, in the same
    /// snapshot order as `terminals` — see [`ResourceJson`]. Additive: a
    /// payload from a pre-resource-kinds `phux` lacks the key and reads as
    /// empty.
    #[serde(default)]
    pub resources: Vec<ResourceJson>,
    /// The same inventory grouped by host: this host first, then each
    /// satellite a federation hub dials, reachable or not — see
    /// [`HostJson`]. Additive: a payload from an older `phux` lacks the key
    /// and reads as empty.
    #[serde(default)]
    pub hosts: Vec<HostJson>,
    /// Whether [`Self::hosts`] names every satellite: `true` only when the
    /// server advertises the host-session inventory (`HOST_SESSIONS`).
    /// `false` means `hosts` carries this host's group alone and the
    /// satellites, if any, are unknown — their Terminals may still appear in
    /// `terminals`. Emitted always; an older `phux` lacks the key, which
    /// reads as `false`.
    #[serde(default)]
    pub hosts_complete: bool,
}

impl SessionListJson {
    /// Wrap an already-sorted list of [`SessionJson`] entries, stamping the
    /// current [`LS_SCHEMA_VERSION`].
    ///
    /// Callers are responsible for the name-sort (the binary mirrors
    /// `print_sessions`); this constructor does not reorder.
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

    /// Record whether [`Self::hosts`] is authoritative — see
    /// [`Self::hosts_complete`].
    #[must_use]
    pub const fn with_hosts_complete(mut self, complete: bool) -> Self {
        self.hosts_complete = complete;
        self
    }

    /// Add the per-resource kind/parent rows, in snapshot order.
    #[must_use]
    pub fn with_resources(mut self, resources: Vec<ResourceJson>) -> Self {
        self.resources = resources;
        self
    }

    /// Add the aggregate Terminal inventory in server snapshot order.
    #[must_use]
    pub fn with_terminals(mut self, terminals: Vec<String>) -> Self {
        self.terminals = terminals;
        self
    }

    /// Add the per-host grouping — see [`Self::hosts`].
    #[must_use]
    pub fn with_hosts(mut self, hosts: Vec<HostJson>) -> Self {
        self.hosts = hosts;
        self
    }

    /// Record the satellites this listing could not see — see
    /// [`Self::unreachable`].
    #[must_use]
    pub fn with_unreachable(mut self, unreachable: Vec<String>) -> Self {
        self.unreachable = unreachable;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{LS_SCHEMA_VERSION, ResourceJson, SessionJson, SessionListJson};

    #[test]
    fn new_stamps_schema_version_and_keeps_order() {
        let list = SessionListJson::new(vec![
            SessionJson {
                name: "alpha".to_owned(),
                windows: 2,
                attached: true,
                attached_clients: 1,
            },
            SessionJson {
                name: "beta".to_owned(),
                windows: 1,
                attached: false,
                attached_clients: 0,
            },
        ]);

        assert_eq!(list.schema_version, LS_SCHEMA_VERSION);
        assert_eq!(list.sessions.len(), 2);
        // Order is preserved as given (caller sorts).
        assert_eq!(list.sessions[0].name, "alpha");
        assert_eq!(list.sessions[1].name, "beta");
        assert!(list.terminals.is_empty());
        assert!(list.unreachable.is_empty());
    }

    #[test]
    fn serializes_to_stable_json_shape() {
        let list = SessionListJson::new(vec![SessionJson {
            name: "work".to_owned(),
            windows: 3,
            attached: true,
            attached_clients: 2,
        }])
        .with_terminals(vec!["@7".to_owned(), "devbox/@42".to_owned()]);

        let json = serde_json::to_value(&list).expect("serialize");
        // `attached_clients` arrived without a version bump: additive keys
        // are non-breaking under this contract, so the version stays put.
        assert_eq!(json["schema_version"], 3);
        assert_eq!(json["sessions"][0]["name"], "work");
        assert_eq!(json["sessions"][0]["windows"], 3);
        assert_eq!(json["sessions"][0]["attached"], true);
        assert_eq!(json["sessions"][0]["attached_clients"], 2);
        assert_eq!(json["terminals"], serde_json::json!(["@7", "devbox/@42"]));
        // Present and empty, not absent: a consumer must be able to read
        // "this listing is complete" positively. An absent key is what an
        // older phux emits, and cannot be told apart from a degraded one.
        assert_eq!(json["unreachable"], serde_json::json!([]));

        let old_shape: SessionListJson = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "sessions": []
        }))
        .expect("v1 payloads remain deserializable for compatibility");
        assert!(old_shape.terminals.is_empty());
        assert!(old_shape.unreachable.is_empty());
        assert!(old_shape.resources.is_empty());
    }

    #[test]
    fn resources_carry_kind_and_parent_without_a_version_bump() {
        let list = SessionListJson::new(Vec::new())
            .with_terminals(vec!["@7".to_owned(), "@9".to_owned()])
            .with_resources(vec![
                ResourceJson {
                    id: "@7".to_owned(),
                    kind: "terminal".to_owned(),
                    parent: None,
                },
                ResourceJson {
                    id: "@9".to_owned(),
                    kind: "agent_session".to_owned(),
                    parent: Some("@7".to_owned()),
                },
            ]);
        let json = serde_json::to_value(&list).expect("serialize");
        assert_eq!(
            json["schema_version"], 3,
            "an added key does not move the version"
        );
        assert_eq!(json["resources"][0]["kind"], "terminal");
        assert!(
            json["resources"][0]["parent"].is_null(),
            "a root resource carries `parent: null` rather than omitting the key"
        );
        assert_eq!(json["resources"][1]["kind"], "agent_session");
        assert_eq!(json["resources"][1]["parent"], "@7");
    }

    /// The per-host grouping is additive: it rides beside `sessions`, which
    /// keeps listing this host's sessions only, and the version stays put.
    #[test]
    fn hosts_group_by_host_without_a_version_bump() {
        use super::{HostJson, HostSessionJson};

        let local = HostJson {
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
        };
        let satellite = HostJson {
            host: Some("devbox".to_owned()),
            local: false,
            reachable: true,
            unreachable: None,
            sessions: vec![HostSessionJson {
                name: "build".to_owned(),
                id: 1,
                windows: 1,
                panes: 1,
                attached: false,
                attached_clients: 0,
                active_terminal: Some("devbox/@7".to_owned()),
            }],
        };
        let down = HostJson {
            host: Some("down".to_owned()),
            local: false,
            reachable: false,
            unreachable: Some("satellite down is unreachable: link is down".to_owned()),
            sessions: Vec::new(),
        };
        let list = SessionListJson::new(vec![SessionJson {
            name: "work".to_owned(),
            windows: 3,
            attached: true,
            attached_clients: 2,
        }])
        .with_hosts(vec![local, satellite, down]);

        let json = serde_json::to_value(&list).expect("serialize");
        assert_eq!(
            json["schema_version"], 3,
            "an added key does not move the version"
        );
        assert!(
            json["hosts"][0]["host"].is_null(),
            "this host is the null-named first group"
        );
        assert_eq!(json["hosts"][0]["local"], true);
        assert_eq!(json["hosts"][0]["sessions"][0]["panes"], 4);
        assert_eq!(json["hosts"][1]["host"], "devbox");
        assert_eq!(
            json["hosts"][1]["sessions"][0]["active_terminal"],
            "devbox/@7"
        );
        assert_eq!(json["hosts"][2]["reachable"], false);
        assert!(
            json["hosts"][2]["sessions"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "an unreachable host is listed with no sessions, not dropped"
        );
        // `sessions` is unchanged: satellite sessions never join it.
        assert_eq!(json["sessions"].as_array().map(Vec::len), Some(1));

        let old_shape: SessionListJson = serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "sessions": []
        }))
        .expect("a payload without hosts stays deserializable");
        assert!(old_shape.hosts.is_empty());
    }

    /// `hosts_complete` is always emitted, and an older payload without it
    /// reads as `false` — "the satellites are unknown", never "there are
    /// none".
    #[test]
    fn hosts_complete_is_emitted_and_defaults_false() {
        let incomplete = serde_json::to_value(SessionListJson::new(Vec::new())).expect("serialize");
        assert_eq!(incomplete["hosts_complete"], false);
        let complete =
            serde_json::to_value(SessionListJson::new(Vec::new()).with_hosts_complete(true))
                .expect("serialize");
        assert_eq!(complete["hosts_complete"], true);
        assert_eq!(
            complete["schema_version"], 3,
            "an added key does not move the version"
        );
        let old: SessionListJson = serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "sessions": []
        }))
        .expect("an older payload stays deserializable");
        assert!(!old.hosts_complete);
    }

    #[test]
    fn payloads_without_attached_clients_still_deserialize() {
        // A pre-`attached_clients` phux emits sessions without the key; the
        // additive field must not make those payloads unreadable.
        let list: SessionListJson = serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "sessions": [
                { "name": "work", "windows": 3, "attached": true }
            ]
        }))
        .expect("pre-attached_clients payloads remain deserializable");
        assert_eq!(list.sessions[0].attached_clients, 0);
        assert!(list.sessions[0].attached);
    }

    #[test]
    fn a_degraded_listing_names_what_it_could_not_see() {
        let list = SessionListJson::new(vec![SessionJson {
            name: "work".to_owned(),
            windows: 1,
            attached: false,
            attached_clients: 0,
        }])
        .with_unreachable(vec![
            "satellite build-box is unreachable: link is down".to_owned(),
        ]);

        let json = serde_json::to_value(&list).expect("serialize");
        assert_eq!(
            json["unreachable"],
            serde_json::json!(["satellite build-box is unreachable: link is down"])
        );
        // The sessions still come back: an unreachable satellite degrades the
        // listing, it does not fail it (`handle_get_state_federated`).
        assert_eq!(json["sessions"][0]["name"], "work");
    }
}
