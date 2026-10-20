//! The retired layout slot and the L3 metadata and lifecycle frame properties.

use proptest::prelude::*;

use super::{
    CloseReason, Command, DirectoryEntry, DirectoryErrorCode, DirectoryListing,
    DirectoryListingError, DirectoryListingResult, FrameKind, GroupId, Layer, LayerSet,
    MAX_RESOURCE_NATIVE_ID_BYTES, MAX_RESOURCE_PROVIDER_BYTES, MoveError, MoveResult, ResourceId,
    ResourceKind, Scope, SpawnError, SpawnResource, SpawnResult, arb_terminal_id,
    assert_round_trip, framed_tlv, tlv_field,
};

pub(super) fn encode_split_with_ratio(ratio: f32) -> Vec<u8> {
    // Hand-roll an ATTACHED frame whose single WindowInfo carries a Split node
    // with `ratio`. Under field-tagged TLV the message body is two fields —
    // SNAPSHOT (id 1) and INITIAL_CLIENT_ID (id 2) — but the SessionSnapshot
    // value is still positional, so the inner bytes mirror
    // `info::encode_session_snapshot` exactly. Keep in sync when the snapshot
    // wire shape changes.
    let mut snap = Vec::new();
    // sessions: empty list
    snap.extend_from_slice(&0u32.to_be_bytes());
    // windows: one item
    snap.extend_from_slice(&1u32.to_be_bytes());
    // WindowInfo
    snap.extend_from_slice(&1u32.to_be_bytes()); // id
    snap.extend_from_slice(&1u32.to_be_bytes()); // session_id
    snap.extend_from_slice(&0u16.to_be_bytes()); // index
    snap.extend_from_slice(&1u32.to_be_bytes()); // name length
    snap.push(b'w'); // name bytes
    snap.push(0); // active_resource: None
    snap.push(1); // layout: Some
    snap.push(1); // LayoutNode::Split
    snap.push(0); // SplitDir::Horizontal
    snap.extend_from_slice(&ratio.to_be_bytes());
    // Left leaf: LAYOUT_TAG_LEAF=0, then ResourceId::Local { id: 1 }
    snap.push(0);
    snap.push(0); // RESOURCE_ID_TAG_LOCAL
    snap.extend_from_slice(&1u32.to_be_bytes());
    // Right leaf: LAYOUT_TAG_LEAF=0, then ResourceId::Local { id: 2 }
    snap.push(0);
    snap.push(0); // RESOURCE_ID_TAG_LOCAL
    snap.extend_from_slice(&2u32.to_be_bytes());
    // panes: empty list
    snap.extend_from_slice(&0u32.to_be_bytes());
    // focused_session, focused_window, focused_resource (tagged ResourceId)
    snap.extend_from_slice(&0u32.to_be_bytes()); // focused_session
    snap.extend_from_slice(&0u32.to_be_bytes()); // focused_window
    snap.push(0); // RESOURCE_ID_TAG_LOCAL
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_resource id

    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap); // field::attached::SNAPSHOT
    tlv_field(&mut fields, 2, &0u32.to_be_bytes()); // field::attached::INITIAL_CLIENT_ID
    tlv_field(&mut fields, 3, &1u32.to_be_bytes()); // field::attached::ATTACH_ID
    framed_tlv(0x81, &fields)
}

/// A peer that still writes a tree into the retired slot is tolerated: the
/// decoder walks past it without validating the ratio (the tree is discarded),
/// and the positional fields after it land where they belong.
#[test]
fn retired_layout_slot_is_skipped_whatever_its_ratio() {
    for ratio in [f32::NAN, f32::INFINITY, 1.5, -0.1, 0.0, 0.5, 1.0] {
        let bytes = encode_split_with_ratio(ratio);
        let (decoded, _tail) = FrameKind::decode(&bytes).unwrap();
        let FrameKind::Attached { snapshot, .. } = decoded else {
            panic!("expected Attached frame for ratio {ratio}");
        };
        assert_eq!(snapshot.windows.len(), 1, "ratio {ratio}");
        assert_eq!(snapshot.windows[0].name, "w", "ratio {ratio}");
        assert!(snapshot.resources.is_empty(), "ratio {ratio}");
        assert_eq!(
            snapshot.focused_resource,
            ResourceId::local(1),
            "ratio {ratio}"
        );
    }
}

pub(super) fn arb_scope() -> impl Strategy<Value = Scope> {
    prop_oneof![
        arb_terminal_id().prop_map(Scope::Resource),
        any::<u32>().prop_map(|id| Scope::Group(GroupId::new(id))),
        Just(Scope::Global),
    ]
}

pub(super) fn arb_metadata_value() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..512)
}

pub(super) fn arb_directory_listing_result() -> impl Strategy<Value = DirectoryListingResult> {
    let entry = (".{0,24}", any::<bool>())
        .prop_map(|(name, is_symlink)| DirectoryEntry { name, is_symlink });
    let ok = (
        ".{0,64}",
        proptest::option::of(".{0,64}"),
        proptest::collection::vec(entry, 0..8),
        any::<bool>(),
    )
        .prop_map(|(path, parent, entries, truncated)| {
            Ok(DirectoryListing {
                path,
                parent,
                entries,
                truncated,
            })
        });
    let code = prop_oneof![
        Just(DirectoryErrorCode::NotFound),
        Just(DirectoryErrorCode::PermissionDenied),
        Just(DirectoryErrorCode::NotADirectory),
        Just(DirectoryErrorCode::Other),
    ];
    let err = (".{0,64}", code, ".{0,64}").prop_map(|(path, code, message)| {
        Err(DirectoryListingError {
            path,
            code,
            message,
        })
    });
    prop_oneof![ok, err]
}

pub(super) fn arb_layer_set() -> impl Strategy<Value = LayerSet> {
    prop_oneof![
        Just(LayerSet::new()),
        Just(LayerSet::with(&[Layer::L2])),
        Just(LayerSet::with(&[Layer::L3])),
        Just(LayerSet::all()),
    ]
}

pub(super) fn arb_env_pair() -> impl Strategy<Value = (String, String)> {
    (".{0,16}", ".{0,32}")
}

pub(super) fn arb_spawn_error() -> impl Strategy<Value = SpawnError> {
    prop_oneof![
        Just(SpawnError::GroupNotFound),
        ".{0,128}".prop_map(SpawnError::SpawnFailed),
        Just(SpawnError::UnsupportedSatelliteRoute),
        ".{0,128}".prop_map(SpawnError::SatelliteUnreachable),
        Just(SpawnError::UnsupportedKind),
        Just(SpawnError::ParentNotFound),
        Just(SpawnError::ParentKindMismatch),
        Just(SpawnError::IdempotencyConflict),
    ]
}

pub(super) fn arb_close_reason() -> impl Strategy<Value = CloseReason> {
    prop_oneof![
        Just(CloseReason::Unknown),
        Just(CloseReason::Exited),
        Just(CloseReason::Killed),
        Just(CloseReason::ParentClosed),
        Just(CloseReason::ServerShutdown),
    ]
}

pub(super) fn arb_spawn_result() -> impl Strategy<Value = SpawnResult> {
    prop_oneof![
        arb_terminal_id().prop_map(SpawnResult::Ok),
        arb_spawn_error().prop_map(SpawnResult::Err),
        (arb_terminal_id(), any::<[u8; 16]>()).prop_map(|(id, bytes)| SpawnResult::OkBound {
            id,
            instance: phux_protocol::ids::ServerInstance::new(bytes),
        }),
        (arb_terminal_id(), proptest::option::of(any::<[u8; 16]>())).prop_map(|(id, bytes)| {
            SpawnResult::Replayed {
                id,
                instance: bytes.map(phux_protocol::ids::ServerInstance::new),
            }
        }),
    ]
}

pub(super) fn arb_move_result() -> impl Strategy<Value = MoveResult> {
    prop_oneof![
        arb_terminal_id().prop_map(MoveResult::Ok),
        ".{0,64}".prop_map(|msg| MoveResult::Err(MoveError::MoveFailed(msg))),
        Just(MoveResult::Err(MoveError::UnsupportedSatelliteRoute)),
    ]
}

proptest! {
    /// An `AgentSession` spawn carries fields 11-14 and none of the PTY
    /// shape; `parent` and `provider` are required, `native_id` optional,
    /// and `satellite` (7) and `agent_session` (9) stay legal for it.
    /// `prop_assume!` facet-length bounds cannot live inside `arb_frame_kind`.
    #[test]
    fn roundtrip_spawn_agent_session(
        request_id in any::<u32>(),
        group in any::<u32>(),
        satellite in proptest::option::of(".{1,16}"),
        agent_session in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..64)),
        parent in arb_terminal_id(),
        provider in ".{1,32}",
        native_id in proptest::option::of(".{1,64}"),
    ) {
        prop_assume!(provider.len() <= MAX_RESOURCE_PROVIDER_BYTES);
        prop_assume!(native_id.as_ref().is_none_or(|id| id.len() <= MAX_RESOURCE_NATIVE_ID_BYTES));
        assert_round_trip(&FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(group),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: satellite.map(phux_protocol::ids::SatelliteHost::new),
            owner_terminal: None,
            agent_session,
            initial_size: None,
            resource: Some(Box::new(
                SpawnResource::agent_session(parent, provider).with_native_id(native_id),
            )),
        });
    }

    /// A kind this build does not recognise round-trips its tag verbatim and
    /// is not validated, so the server can answer `UnsupportedKind`.
    #[test]
    fn roundtrip_spawn_unknown_kind(
        request_id in any::<u32>(),
        tag in 2_u8..=u8::MAX,
        parent in proptest::option::of(arb_terminal_id()),
        cwd in proptest::option::of(".{0,32}"),
    ) {
        assert_round_trip(&FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: None,
            cwd,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(SpawnResource {
                kind: ResourceKind::Unknown { tag },
                parent,
                provider: None,
                native_id: None,
                bind_instance: false, idempotency_key: None, retain_secs: None,
            })),
        });
    }

    /// KILL_RESOURCE_IF (ADR-0109): every instance presence and every
    /// condition byte round-trips, unknown bits included, so a server can
    /// refuse a condition it does not know instead of dropping it.
    /// Condition bits are not in `arb_frame_kind`.
    #[test]
    fn roundtrip_command_kill_resource_if(
        request_id in any::<u32>(),
        terminal_id in arb_terminal_id(),
        instance in proptest::option::of(any::<[u8; 16]>()),
        bits in any::<u8>(),
    ) {
        let precondition = phux_protocol::wire::frame::KillPrecondition {
            instance: instance.map(phux_protocol::ids::ServerInstance::new),
            conditions: phux_protocol::wire::frame::KillConditions::from_bits(bits),
        };
        assert_round_trip(&FrameKind::Command {
            request_id,
            command: Command::KillResourceIf { terminal_id, precondition, operation_id: None },
        });
    }

    /// SPAWN_RESOURCE.bind_instance (field 15) round-trips on a Terminal
    /// spawn. Unset, it leaves the canonical `resource: None` spawn.
    #[test]
    fn roundtrip_spawn_bind_instance(
        request_id in any::<u32>(),
        bind in any::<bool>(),
        satellite in proptest::option::of("[a-z]{1,8}"),
    ) {
        assert_round_trip(&FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: satellite.map(phux_protocol::ids::SatelliteHost::new),
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: bind.then(|| Box::new(SpawnResource::default().with_bind_instance(true))),
        });
    }
}
