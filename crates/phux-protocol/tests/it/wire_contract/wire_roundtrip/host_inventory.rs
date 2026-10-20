//! The host-session inventory trailing a snapshot.

use super::{
    CommandResult, CommandValue, FrameKind, ResourceId, ResourceInfo, ResourceKind, SessionId,
    SessionSnapshot, WindowId, assert_round_trip, framed_tlv, local_id_bytes, tlv_field,
};

/// The shared body of every round-trip test in this file: encoding then
/// decoding `frame` is the identity, and the decoder consumes the whole
/// buffer. (`FrameKind::decode` delegates to `Decoder::read_frame`, so both
/// entry points are one path.) Inside `proptest!` closures a plain panic is
/// caught and shrunk like a `prop_assert!` failure.
/// A hub `GET_STATE` reply carrying the host-session inventory round-trips,
/// with and without resource facets ahead of it.
#[test]
fn state_with_host_inventory_round_trips() {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::{HostInventory, HostSessionInfo};

    let hosts = vec![
        HostInventory::reachable(
            SatelliteHost::new("edge"),
            vec![
                HostSessionInfo::new(SessionId::new(4), "build")
                    .with_created_at_unix_secs(1_700_000_000)
                    .with_window_count(2)
                    .with_pane_count(3)
                    .with_attached_client_count(1)
                    .with_active_resource(Some(ResourceId::satellite(
                        SatelliteHost::new("edge"),
                        9,
                    ))),
                HostSessionInfo::new(SessionId::new(5), "logs"),
            ],
        ),
        HostInventory::reachable(SatelliteHost::new("idle"), Vec::new()),
        HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
    ];
    let plain = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(1),
            WindowId::new(1),
            80,
            24,
        )])
        .with_hosts(hosts);
    let faceted = plain.clone().with_resources(vec![
        ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
        ResourceInfo::resource(ResourceId::local(2), ResourceKind::AgentSession)
            .with_parent(Some(ResourceId::local(1))),
    ]);
    for snapshot in [plain, faceted] {
        assert_round_trip(&FrameKind::CommandResult {
            request_id: 9,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        });
    }
}

/// Hand-rolled bytes: a zero-count facet list anchors the host list after
/// it, and the same bytes cut off after the facet list decode as a snapshot
/// with no hosts, which is exactly what an older decoder sees (it stops at
/// the facets and ignores the rest of the field).
#[test]
fn host_inventory_trails_the_facet_list_and_is_optional() {
    let mut prefix = Vec::new();
    prefix.extend_from_slice(&0u32.to_be_bytes()); // sessions
    prefix.extend_from_slice(&0u32.to_be_bytes()); // windows
    prefix.extend_from_slice(&0u32.to_be_bytes()); // resources
    prefix.extend_from_slice(&1u32.to_be_bytes()); // focused_session
    prefix.extend_from_slice(&1u32.to_be_bytes()); // focused_window
    prefix.extend_from_slice(&local_id_bytes(1)); // focused_resource
    prefix.extend_from_slice(&0u32.to_be_bytes()); // facets: zero rows
    let mut full = prefix.clone();
    full.extend_from_slice(&1u32.to_be_bytes()); // hosts: one row
    full.extend_from_slice(&4u32.to_be_bytes());
    full.extend_from_slice(b"edge");
    full.push(0); // reachable
    full.extend_from_slice(&1u32.to_be_bytes()); // one session
    full.extend_from_slice(&3u32.to_be_bytes()); // satellite-local id
    full.extend_from_slice(&5u32.to_be_bytes());
    full.extend_from_slice(b"build");
    full.extend_from_slice(&0i64.to_be_bytes()); // created_at
    full.extend_from_slice(&2u16.to_be_bytes()); // windows
    full.extend_from_slice(&3u16.to_be_bytes()); // panes
    full.extend_from_slice(&0u16.to_be_bytes()); // attached
    full.push(0); // no active resource

    let decode = |snap: &[u8]| {
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, snap);
        tlv_field(&mut fields, 2, &1u32.to_be_bytes());
        tlv_field(&mut fields, 3, &1u32.to_be_bytes());
        let (decoded, _) = FrameKind::decode(&framed_tlv(0x81, &fields)).unwrap();
        let FrameKind::Attached { snapshot, .. } = decoded else {
            panic!("expected Attached");
        };
        snapshot
    };

    let with_hosts = decode(&full);
    assert_eq!(with_hosts.hosts().len(), 1);
    let row = &with_hosts.hosts()[0];
    assert_eq!(row.host.as_str(), "edge");
    assert!(row.is_reachable());
    assert_eq!(row.sessions[0].id, SessionId::new(3));
    assert_eq!(row.sessions[0].name, "build");
    assert_eq!(
        (row.sessions[0].window_count, row.sessions[0].pane_count),
        (2, 3)
    );
    assert!(decode(&prefix).hosts().is_empty());
}

/// A host list that declares more rows than the field holds errors instead
/// of pre-allocating (the decode-path allocation bound).
#[test]
fn host_inventory_overdeclared_count_is_malformed_not_allocated() {
    let mut snap = Vec::new();
    for _ in 0..3 {
        snap.extend_from_slice(&0u32.to_be_bytes());
    }
    snap.extend_from_slice(&1u32.to_be_bytes());
    snap.extend_from_slice(&1u32.to_be_bytes());
    snap.extend_from_slice(&local_id_bytes(1));
    snap.extend_from_slice(&0u32.to_be_bytes()); // facets
    snap.extend_from_slice(&u32::MAX.to_be_bytes()); // hosts: absurd count
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap);
    tlv_field(&mut fields, 2, &1u32.to_be_bytes());
    tlv_field(&mut fields, 3, &1u32.to_be_bytes());
    assert!(FrameKind::decode(&framed_tlv(0x81, &fields)).is_err());
}
