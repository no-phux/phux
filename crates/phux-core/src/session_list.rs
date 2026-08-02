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
        }
    }

    /// Add the aggregate Terminal inventory in server snapshot order.
    #[must_use]
    pub fn with_terminals(mut self, terminals: Vec<String>) -> Self {
        self.terminals = terminals;
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
    use super::{LS_SCHEMA_VERSION, SessionJson, SessionListJson};

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
