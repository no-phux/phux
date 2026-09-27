//! Every-machine session listing — the `phux ls --all --json` read shape,
//! `phux.hosts/v1`.
//!
//! One [`HostJson`] per machine phux reaches: this machine's own server, then
//! each `[[remote]]` registry entry in registry order. A host that did not
//! answer keeps its row with `reachable: false` and the reason, so a
//! consumer can draw "mini is down" instead of silently losing a machine.
//!
//! This document is also the contract a TUI sidebar hosts provider speaks
//! (ADR-0140): the built-in provider is `phux ls --all --json`, and a
//! replacement provider is any command that prints this shape. The
//! per-session rows reuse [`SessionJson`] so a session means the same thing
//! here as in `phux ls --json`.

use serde::{Deserialize, Serialize};

use crate::session_list::SessionJson;

/// Stable JSON contract version for [`HostListJson`]. Bump on any breaking
/// change (a removed, renamed, or retyped key); added keys are non-breaking
/// and consumers ignore keys they do not know.
pub const HOSTS_SCHEMA_VERSION: u32 = 1;

/// Which registry a [`HostJson`] row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// This machine's own server, dialed over its local socket.
    Local,
    /// A `[[remote]]` registry entry, dialed over QUIC or WSS.
    Remote,
}

/// One machine's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostJson {
    /// The name `phux attach NAME` reaches it by: the registry name for a
    /// [`HostKind::Remote`] row, `local` for this machine.
    pub name: String,
    /// Human label: the machine's hostname for [`HostKind::Local`], the
    /// registry name otherwise.
    pub label: String,
    /// Which registry the row came from.
    pub kind: HostKind,
    /// The dialed endpoint for a remote row; `None` for this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Whether the host answered within the listing's deadline.
    pub reachable: bool,
    /// Why an unreachable host could not be listed, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The host's sessions, name-sorted. Empty when unreachable.
    #[serde(default)]
    pub sessions: Vec<SessionJson>,
}

/// The whole `phux ls --all --json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostListJson {
    /// [`HOSTS_SCHEMA_VERSION`] at emit time.
    pub schema_version: u32,
    /// This machine first, then registered hosts in registry order.
    pub hosts: Vec<HostJson>,
}

impl HostListJson {
    /// A document at the current schema version.
    #[must_use]
    pub const fn new(hosts: Vec<HostJson>) -> Self {
        Self {
            schema_version: HOSTS_SCHEMA_VERSION,
            hosts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_row_round_trips_without_sessions_or_endpoint_noise() {
        let doc = HostListJson::new(vec![
            HostJson {
                name: "local".to_owned(),
                label: "laptop".to_owned(),
                kind: HostKind::Local,
                endpoint: None,
                reachable: true,
                error: None,
                sessions: vec![SessionJson {
                    name: "default".to_owned(),
                    windows: 1,
                    attached: false,
                    attached_clients: 0,
                    keep_empty: false,
                    empty: false,
                }],
            },
            HostJson {
                name: "mini".to_owned(),
                label: "mini".to_owned(),
                kind: HostKind::Remote,
                endpoint: Some("quic://100.64.0.2:8788".to_owned()),
                reachable: false,
                error: Some("timed out".to_owned()),
                sessions: Vec::new(),
            },
        ]);
        let text = serde_json::to_string(&doc).expect("serialize");
        assert!(!text.contains("\"endpoint\":null"));
        assert!(text.contains("\"kind\":\"remote\""));
        let back: HostListJson = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, doc);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let text = r#"{"schema_version":1,"future":true,"hosts":[
            {"name":"a","label":"a","kind":"remote","reachable":true,"extra":1}]}"#;
        let doc: HostListJson = serde_json::from_str(text).expect("parse");
        assert_eq!(doc.hosts[0].sessions, Vec::new());
    }
}
