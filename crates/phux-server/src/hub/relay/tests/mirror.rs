//! The metadata and lease mirror, and role policy forwarding.

use super::*;

/// ADR-0136 mirror bounds: a satellite writes the hub's copy only for a
/// terminal the hub asked to mirror, and never past the hub's metadata
/// value cap.
#[test]
fn a_satellite_writes_only_requested_bounded_mirror_entries() {
    use phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
    let state = crate::state::SharedState::new();
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_journal(Some(state.clone()));
    let changed = |id: u32, value: Vec<u8>| {
        encode(&FrameKind::MetadataChanged {
            scope: Scope::Resource(ResourceId::local(id)),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(value),
            actor: None,
        })
    };
    let stored = |id: u32| {
        state.with(|s| {
            s.metadata().get(
                &Scope::Resource(ResourceId::satellite(host(), id)),
                RESOURCE_AGENT_KEY,
            )
        })
    };

    session.handle_inbound(&changed(7, b"{}".to_vec())).unwrap();
    assert!(stored(7).is_none(), "the hub never asked to mirror 7");

    let _ = session.prepare_request(&RelayRequest::MirrorTerminal { terminal: 7 });
    session.handle_inbound(&changed(7, b"{}".to_vec())).unwrap();
    assert_eq!(stored(7).as_deref(), Some(b"{}".as_slice()));

    let cap = state.with(crate::state::ServerState::metadata_value_bytes) as usize;
    session
        .handle_inbound(&changed(7, vec![b'x'; cap + 1]))
        .unwrap();
    assert!(stored(7).is_none(), "an oversized value clears the copy");
}

/// A satellite terminal's lease-mirror ordering state is dropped when
/// it closes, so terminals the satellite names come and go without
/// growing the hub.
#[test]
fn a_closed_satellite_terminal_leaves_no_lease_mirror_state() {
    use phux_protocol::wire::frame::CloseReason;
    let state = crate::state::SharedState::new();
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_journal(Some(state.clone()));
    for id in 0..64 {
        state.with_mut(|s| {
            s.mirror_satellite_lease_event(&host(), id, false, Some(1));
        });
        session
            .handle_inbound(&encode(&FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(id),
                exit_status: None,
                reason: CloseReason::Exited,
                signal: None,
            }))
            .unwrap();
    }
    assert_eq!(
        state.with(crate::state::ServerState::satellite_lease_mirror_entries),
        0
    );
}

// --- outbound command rewrite ---------------------------------------

/// ADR-0127: only a takeover crosses, and only with `ATTACH_ROLES`.
#[test]
fn hub_relays_role_policy_to_the_satellite_when_it_advertises_attach_roles() {
    use phux_protocol::wire::frame::RolePolicy;
    let attach = |role_policy| Command::attach_terminal(ResourceId::local(3), role_policy);
    let roles = ServerFeatureSet::with(&[ServerFeature::AttachRoles]);
    let takeover = Some(RolePolicy::TAKEOVER);
    assert_eq!(
        link_attach_command(attach(takeover), roles, ServerFeatureExtSet::new()),
        attach(takeover)
    );
    assert_eq!(
        link_attach_command(
            attach(takeover),
            ServerFeatureSet::new(),
            ServerFeatureExtSet::new()
        ),
        attach(None),
        "a satellite without the bit gets a plain attach; the hub seizes after it",
    );
    for held in [RolePolicy::VIEWER, RolePolicy::PRIMARY] {
        assert_eq!(
            link_attach_command(attach(Some(held)), roles, ServerFeatureExtSet::new()),
            attach(None)
        );
    }
    let detach = Command::DetachResource {
        terminal_id: ResourceId::local(3),
    };
    assert_eq!(
        link_attach_command(detach.clone(), roles, ServerFeatureExtSet::new()),
        detach
    );
}
