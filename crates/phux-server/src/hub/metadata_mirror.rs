//! Read-only hub mirror of two satellite agent metadata keys (ADR-0136).
//!
//! L3 stays server-local. This module is the one carve-out: a hub copies
//! `phux.agent/v1` and `phux.agent.asked/v1` from a satellite terminal's
//! `Local` scope into its own store under `Satellite { host, id }`. Every
//! other key, and any already-satellite-tagged scope, is ignored.

#![allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) module; pub would be crate-visible and unreachable_pub would reject it"
)]

use phux_protocol::ids::{ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY, Scope};

use crate::state::ServerState;

/// The allowlist. Order is the order the link subscribes and reads them.
pub(crate) const MIRROR_KEYS: [&str; 2] = [RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY];

/// Byte the owning server stores while an ask is pending.
pub(crate) const ASKED_PRESENT: &[u8] = b"1";

/// Whether `key` is one of the two mirrored agent keys.
pub(crate) fn is_mirrored_key(key: &str) -> bool {
    MIRROR_KEYS.contains(&key)
}

/// Write or tombstone the asked flag for a terminal this server owns.
///
/// `asked` is the ladder after the mutation that just landed. Equal bytes
/// suppress a broadcast, so a report that does not change the flag is quiet.
pub(crate) fn publish_asked_flag(state: &mut ServerState, wire: &ResourceId, asked: bool) {
    let scope = Scope::Resource(wire.clone());
    if asked {
        state.metadata_set(&scope, RESOURCE_ASKED_KEY, ASKED_PRESENT.to_vec());
    } else {
        state.metadata_delete(&scope, RESOURCE_ASKED_KEY);
    }
}

/// Retag a satellite-local metadata scope and store it on the hub.
///
/// `value: None` is a tombstone, and so is a value over the hub's
/// `metadata_value_bytes`, the cap a client write is held to. Returns
/// `false` when `key` is not allowlisted or `scope` is not a `Local`
/// terminal (including a `Satellite` scope, which would chain).
pub(crate) fn apply_mirrored(
    state: &mut ServerState,
    host: &SatelliteHost,
    scope: &Scope,
    key: &str,
    value: Option<Vec<u8>>,
) -> bool {
    if !is_mirrored_key(key) {
        return false;
    }
    let Some(retagged) = retag_local_scope(host, scope) else {
        return false;
    };
    let cap = usize::try_from(state.metadata_value_bytes()).unwrap_or(usize::MAX);
    let value = value.filter(|bytes| bytes.len() <= cap);
    match value {
        Some(bytes) => {
            state.metadata_set(&retagged, key, bytes);
        }
        None => {
            state.metadata_delete(&retagged, key);
        }
    }
    true
}

/// Tombstone both mirrored keys and drop subscriptions for a closed
/// satellite terminal.
pub(crate) fn forget_mirrored_terminal(state: &mut ServerState, host: &SatelliteHost, id: u32) {
    let wire = ResourceId::satellite(host.clone(), id);
    let scope = Scope::Resource(wire.clone());
    for key in MIRROR_KEYS {
        state.metadata_delete(&scope, key);
    }
    state.drop_terminal_metadata(&wire);
}

/// `Local { id }` becomes `Satellite { host, id }`. Anything else is `None`.
fn retag_local_scope(host: &SatelliteHost, scope: &Scope) -> Option<Scope> {
    match scope {
        Scope::Resource(ResourceId::Local { id }) => {
            Some(Scope::Resource(ResourceId::satellite(host.clone(), *id)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SharedState;

    fn host() -> SatelliteHost {
        SatelliteHost::new("gpubox")
    }

    fn local_scope(id: u32) -> Scope {
        Scope::Resource(ResourceId::local(id))
    }

    fn satellite_scope(id: u32) -> Scope {
        Scope::Resource(ResourceId::satellite(host(), id))
    }

    #[test]
    fn allowlist_is_the_agent_record_and_the_asked_flag() {
        assert!(is_mirrored_key(RESOURCE_AGENT_KEY));
        assert!(is_mirrored_key(RESOURCE_ASKED_KEY));
        assert!(!is_mirrored_key("phux.tags/v1"));
        assert!(!is_mirrored_key("phux.agent-session/v1"));
    }

    fn get(state: &SharedState, scope: &Scope, key: &str) -> Option<Vec<u8>> {
        state.with(|s| s.metadata().get(scope, key))
    }

    /// Mirrored keys land under the satellite scope (never the Local one) and
    /// a tombstone removes them; other keys and satellite scopes are ignored.
    #[test]
    fn a_mirrored_record_is_stored_under_the_satellite_scope_until_tombstoned() {
        let state = SharedState::new();
        let apply = |scope: &Scope, key: &str, value: Option<&[u8]>| {
            state.with_mut(|s| apply_mirrored(s, &host(), scope, key, value.map(<[u8]>::to_vec)))
        };
        let record = br#"{"name":"reviewer"}"#.as_slice();
        assert!(apply(&local_scope(7), RESOURCE_AGENT_KEY, Some(record)));
        assert_eq!(
            get(&state, &satellite_scope(7), RESOURCE_AGENT_KEY).as_deref(),
            Some(record)
        );
        assert!(get(&state, &local_scope(7), RESOURCE_AGENT_KEY).is_none());
        assert!(apply(&local_scope(7), RESOURCE_AGENT_KEY, None));
        assert!(
            state
                .with(|s| s.metadata().list(&satellite_scope(7)))
                .is_empty()
        );

        assert!(is_mirrored_key(RESOURCE_ASKED_KEY) && !is_mirrored_key("phux.agent-session/v1"));
        assert!(!apply(&local_scope(7), "phux.tags/v1", Some(b"x")));
        assert!(!apply(
            &satellite_scope(7),
            RESOURCE_AGENT_KEY,
            Some(record)
        ));
        assert!(
            state
                .with(|s| s.metadata().list(&satellite_scope(7)))
                .is_empty()
        );
    }

    #[test]
    fn the_asked_flag_is_one_byte_or_a_tombstone() {
        let state = SharedState::new();
        let scope = Scope::Resource(ResourceId::local(3));
        state.with_mut(|s| publish_asked_flag(s, &ResourceId::local(3), true));
        assert_eq!(
            get(&state, &scope, RESOURCE_ASKED_KEY).as_deref(),
            Some(ASKED_PRESENT)
        );
        state.with_mut(|s| publish_asked_flag(s, &ResourceId::local(3), false));
        assert!(get(&state, &scope, RESOURCE_ASKED_KEY).is_none());
    }

    #[test]
    fn the_link_retags_a_changed_record_and_a_get_reply() {
        use phux_protocol::caps::BootstrapLimits;
        use phux_protocol::wire::frame::FrameKind;

        use crate::hub::relay::{RelayRequest, RelaySession};

        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let state = SharedState::new();
        session.set_journal(Some(state.clone()));
        let mirror = || RelayRequest::MirrorTerminal { terminal: 7 };
        let frames = session.prepare_request(&mirror());
        assert!(session.handle_request_checked(mirror()).is_none());
        assert_eq!(
            frames.len(),
            4,
            "subscribe and get, for each allowlisted key"
        );
        assert!(
            session.prepare_request(&mirror()).is_empty(),
            "a second mirror on the same connection sends nothing"
        );
        let mut feed = |frame: FrameKind| {
            let mut buf = bytes::BytesMut::new();
            frame.encode(&mut buf);
            // Ignored frames must not tear the link either.
            session.handle_inbound(&buf).expect("in-bounds frame");
        };
        let changed = |scope, key: &str, value: &[u8]| FrameKind::MetadataChanged {
            scope,
            key: key.to_owned(),
            value: Some(value.to_vec()),
            actor: None,
        };

        let record = br#"{"name":"reviewer"}"#.as_slice();
        feed(changed(local_scope(7), RESOURCE_AGENT_KEY, record));
        assert_eq!(
            get(&state, &satellite_scope(7), RESOURCE_AGENT_KEY).as_deref(),
            Some(record)
        );
        feed(changed(local_scope(7), "phux.tags/v1", b"nope"));
        assert!(get(&state, &satellite_scope(7), "phux.tags/v1").is_none());
        // Hub-and-spoke does not chain.
        feed(changed(
            satellite_scope(7),
            RESOURCE_ASKED_KEY,
            ASKED_PRESENT,
        ));
        assert!(get(&state, &satellite_scope(7), RESOURCE_ASKED_KEY).is_none());

        // Answer the mirror's GET of the asked key.
        let request_id = frames
            .iter()
            .find_map(|frame| match FrameKind::decode(frame).expect("decodes").0 {
                FrameKind::GetMetadata {
                    request_id, key, ..
                } if key == RESOURCE_ASKED_KEY => Some(request_id),
                _ => None,
            })
            .expect("the mirror reads the asked key");
        feed(FrameKind::MetadataValue {
            request_id,
            value: Some(ASKED_PRESENT.to_vec()),
        });
        let asked = get(&state, &satellite_scope(7), RESOURCE_ASKED_KEY);
        assert_eq!(asked.as_deref(), Some(ASKED_PRESENT));
    }
}
