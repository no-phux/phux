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
/// `value: None` is a tombstone. Returns `false` when `key` is not
/// allowlisted or `scope` is not a `Local` terminal (including a
/// `Satellite` scope, which would chain).
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

    #[test]
    fn a_local_agent_record_is_stored_under_the_satellite_scope() {
        let state = SharedState::new();
        let bytes = br#"{"name":"reviewer"}"#.to_vec();
        let applied = state.with_mut(|s| {
            apply_mirrored(
                s,
                &host(),
                &local_scope(7),
                RESOURCE_AGENT_KEY,
                Some(bytes.clone()),
            )
        });
        assert!(applied);
        let stored = state.with(|s| s.metadata().get(&satellite_scope(7), RESOURCE_AGENT_KEY));
        assert_eq!(stored.as_deref(), Some(bytes.as_slice()));
        assert!(
            state
                .with(|s| s.metadata().get(&local_scope(7), RESOURCE_AGENT_KEY))
                .is_none(),
            "the hub must not keep the satellite's Local scope"
        );
    }

    #[test]
    fn a_tombstone_removes_the_mirrored_key() {
        let state = SharedState::new();
        state.with_mut(|s| {
            apply_mirrored(
                s,
                &host(),
                &local_scope(7),
                RESOURCE_ASKED_KEY,
                Some(ASKED_PRESENT.to_vec()),
            );
        });
        let cleared = state
            .with_mut(|s| apply_mirrored(s, &host(), &local_scope(7), RESOURCE_ASKED_KEY, None));
        assert!(cleared);
        assert!(
            state
                .with(|s| s.metadata().get(&satellite_scope(7), RESOURCE_ASKED_KEY))
                .is_none()
        );
    }

    #[test]
    fn a_non_allowlisted_key_and_a_satellite_scope_are_ignored() {
        let state = SharedState::new();
        let ignored = state.with_mut(|s| {
            let other = apply_mirrored(
                s,
                &host(),
                &local_scope(7),
                "phux.tags/v1",
                Some(b"x".to_vec()),
            );
            let chained = apply_mirrored(
                s,
                &host(),
                &satellite_scope(7),
                RESOURCE_AGENT_KEY,
                Some(br#"{"name":"nope"}"#.to_vec()),
            );
            other || chained
        });
        assert!(!ignored);
        assert!(
            state
                .with(|s| s.metadata().list(&satellite_scope(7)))
                .is_empty()
        );
    }

    #[test]
    fn the_asked_flag_is_one_byte_or_a_tombstone() {
        let state = SharedState::new();
        let wire = ResourceId::local(3);
        state.with_mut(|s| publish_asked_flag(s, &wire, true));
        assert_eq!(
            state
                .with(|s| s
                    .metadata()
                    .get(&Scope::Resource(wire.clone()), RESOURCE_ASKED_KEY))
                .as_deref(),
            Some(ASKED_PRESENT)
        );
        state.with_mut(|s| publish_asked_flag(s, &wire, false));
        assert!(
            state
                .with(|s| s.metadata().get(&Scope::Resource(wire), RESOURCE_ASKED_KEY))
                .is_none()
        );
    }

    #[test]
    fn the_link_retags_a_changed_record_and_a_get_reply() {
        use phux_protocol::caps::BootstrapLimits;
        use phux_protocol::wire::frame::FrameKind;

        use crate::hub::relay::{RelayRequest, RelaySession};

        let mut session = RelaySession::new(host(), BootstrapLimits::default());
        let state = SharedState::new();
        session.set_journal(Some(state.clone()));
        let frames = session.prepare_request(&RelayRequest::MirrorTerminal { terminal: 7 });
        assert!(
            session
                .handle_request_checked(RelayRequest::MirrorTerminal { terminal: 7 })
                .is_none()
        );
        assert_eq!(
            frames.len(),
            4,
            "subscribe and get, for each allowlisted key"
        );
        assert!(
            session
                .prepare_request(&RelayRequest::MirrorTerminal { terminal: 7 })
                .is_empty(),
            "a second mirror on the same connection sends nothing"
        );

        let changed = FrameKind::MetadataChanged {
            scope: local_scope(7),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(br#"{"name":"reviewer"}"#.to_vec()),
            actor: None,
        };
        let mut buf = bytes::BytesMut::new();
        changed.encode(&mut buf);
        session
            .handle_inbound(&buf)
            .expect("metadata changed is in-bounds");
        assert_eq!(
            state
                .with(|s| s.metadata().get(&satellite_scope(7), RESOURCE_AGENT_KEY))
                .as_deref(),
            Some(&br#"{"name":"reviewer"}"#[..])
        );

        let ignored = FrameKind::MetadataChanged {
            scope: local_scope(7),
            key: "phux.tags/v1".to_owned(),
            value: Some(b"nope".to_vec()),
            actor: None,
        };
        buf.clear();
        ignored.encode(&mut buf);
        session
            .handle_inbound(&buf)
            .expect("a non-allowlisted key must not tear the link");
        assert!(
            state
                .with(|s| s.metadata().get(&satellite_scope(7), "phux.tags/v1"))
                .is_none()
        );

        let chained = FrameKind::MetadataChanged {
            scope: satellite_scope(7),
            key: RESOURCE_ASKED_KEY.to_owned(),
            value: Some(ASKED_PRESENT.to_vec()),
            actor: None,
        };
        buf.clear();
        chained.encode(&mut buf);
        session
            .handle_inbound(&buf)
            .expect("a satellite-tagged scope must not tear the link");
        assert!(
            state
                .with(|s| s.metadata().get(&satellite_scope(7), RESOURCE_ASKED_KEY))
                .is_none(),
            "hub-and-spoke does not chain"
        );

        // The first GET on the wire is phux.agent/v1. Decode it and answer
        // that request id with the asked flag's sibling by walking frames.
        let mut asked_request = None;
        for frame in &frames {
            let (decoded, _) = FrameKind::decode(frame).expect("mirror frame decodes");
            if let FrameKind::GetMetadata {
                request_id, key, ..
            } = decoded
                && key == RESOURCE_ASKED_KEY
            {
                asked_request = Some(request_id);
            }
        }
        let request_id = asked_request.expect("the mirror reads the asked key");
        let reply = FrameKind::MetadataValue {
            request_id,
            value: Some(ASKED_PRESENT.to_vec()),
        };
        buf.clear();
        reply.encode(&mut buf);
        session
            .handle_inbound(&buf)
            .expect("metadata value is in-bounds");
        assert_eq!(
            state
                .with(|s| s.metadata().get(&satellite_scope(7), RESOURCE_ASKED_KEY))
                .as_deref(),
            Some(ASKED_PRESENT)
        );
    }
}
