//! Snapshot trailing elements: facets, the extension block, and keep-empty sessions.

use super::{
    AgentFacet, BootstrapId, BootstrapStreamProfile, BytesMut, ClientId, CommandResult,
    CommandValue, DecodeError, FrameKind, ResourceId, ResourceInfo, ResourceKind, SessionId,
    SessionSnapshot, StreamId, WindowId, assert_round_trip, framed_tlv, local_id_bytes, record,
    tlv_field,
};

#[test]
fn snapshot_without_resource_facets_is_byte_stable_and_decodes_defaults() {
    // A Terminal-only snapshot writes no trailing facet list: its bytes end
    // at `focused_resource`, exactly as before the list existed.
    let plain = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(1),
            WindowId::new(1),
            80,
            24,
        )]);
    let frame = FrameKind::Attached {
        attach_id: 1,
        snapshot: plain,
        initial_client_id: ClientId::new(1),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    // The SNAPSHOT field value ends with focused_resource = tag 0 + u32 1.
    let expected_tail = local_id_bytes(1);
    let field_end = {
        // Fields: 1 SNAPSHOT (first), 2 INITIAL_CLIENT_ID, 3 ATTACH_ID; the
        // last two are 4 bytes each plus a 3-byte TLV header.
        buf.len() - 2 * (3 + 4)
    };
    assert_eq!(
        &buf[field_end - expected_tail.len()..field_end],
        &expected_tail[..]
    );
    let (decoded, _) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
}

/// PHA-406 L10 review (L1 §7.3, §9.1): the journal head at the cut rides the
/// extension block as field 2, alone or beside `RESOURCE_STATE` entries, and
/// a snapshot without it keeps its earlier bytes.
#[test]
fn snapshot_journal_head_rides_the_extension_block_and_is_absent_by_default() {
    use phux_protocol::wire::frame::{CommandResult, CommandValue};
    use phux_protocol::wire::info::ExitFacet;

    let encode = |snapshot: &SessionSnapshot| {
        let mut buf = BytesMut::new();
        FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::OkWith(CommandValue::State(snapshot.clone())),
        }
        .encode(&mut buf);
        buf
    };
    let decode = |bytes: &[u8]| {
        let (frame, _) = FrameKind::decode(bytes).unwrap();
        let FrameKind::CommandResult {
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
            ..
        } = frame
        else {
            panic!("expected a state result, got {frame:?}");
        };
        snapshot
    };
    let plain = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(1),
            WindowId::new(1),
            80,
            24,
        )]);
    assert_eq!(plain.journal_head(), None);
    let plain_bytes = encode(&plain);

    let headed = plain.with_journal_head(Some(4242));
    let headed_bytes = encode(&headed);
    assert!(headed_bytes.len() > plain_bytes.len());
    let decoded = decode(&headed_bytes);
    assert_eq!(decoded.journal_head(), Some(4242));
    assert_eq!(decoded, headed);
    assert_eq!(
        encode(&headed.with_journal_head(None)),
        plain_bytes,
        "clearing the head restores the earlier bytes"
    );

    // Beside a retained resource's state, in the same block.
    let retained = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24)
                .with_lifecycle(phux_protocol::wire::frame::ResourceLifecycle::Exited)
                .with_exit(Some(ExitFacet::new(5, 9).with_exit_status(Some(3)))),
        ])
        .with_journal_head(Some(7));
    let decoded = decode(&encode(&retained));
    assert_eq!(decoded, retained);
    assert_eq!(decoded.journal_head(), Some(7));
    assert_eq!(decoded.resources[0].exit.unwrap().exit_status, Some(3));
}

#[test]
fn snapshot_resource_facets_join_by_id_and_ignore_unknown_rows() {
    // Hand-roll a snapshot: one pane, then a trailing facet list with a row
    // for it and a row for an id no pane carries.
    let mut pane = Vec::new();
    pane.extend_from_slice(&local_id_bytes(7));
    pane.extend_from_slice(&0u32.to_be_bytes()); // window 0 (sentinel)
    pane.extend_from_slice(&0u16.to_be_bytes()); // cols 0
    pane.extend_from_slice(&0u16.to_be_bytes()); // rows 0
    pane.push(0); // title None
    pane.push(0); // cwd None
    let mut snap = Vec::new();
    snap.extend_from_slice(&0u32.to_be_bytes()); // sessions
    snap.extend_from_slice(&0u32.to_be_bytes()); // windows
    snap.extend_from_slice(&1u32.to_be_bytes()); // panes
    snap.extend_from_slice(&record(&pane));
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_session
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_window
    snap.extend_from_slice(&local_id_bytes(7)); // focused_resource
    // Trailing facets: two row records.
    snap.extend_from_slice(&2u32.to_be_bytes());
    // Row for an id with no pane entry: ignored.
    let mut orphan = local_id_bytes(99);
    orphan.push(1); // AgentSession
    orphan.push(0); // parent None
    orphan.push(0); // agent None
    snap.extend_from_slice(&record(&orphan));
    // Row for pane 7, with bytes a newer server appended: skipped.
    let mut row = local_id_bytes(7);
    row.push(1); // AgentSession
    row.push(1); // parent Some
    row.extend_from_slice(&local_id_bytes(3));
    row.push(1); // agent Some
    row.extend_from_slice(&6u32.to_be_bytes());
    row.extend_from_slice(b"claude");
    row.push(0); // native_id None
    row.extend_from_slice(&7u32.to_be_bytes());
    row.extend_from_slice(b"blocked");
    row.extend_from_slice(&[0xEE; 3]);
    snap.extend_from_slice(&record(&row));
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap);
    tlv_field(&mut fields, 2, &1u32.to_be_bytes());
    tlv_field(&mut fields, 3, &1u32.to_be_bytes());
    let (decoded, _) = FrameKind::decode(&framed_tlv(0x81, &fields)).unwrap();
    let FrameKind::Attached { snapshot, .. } = decoded else {
        panic!("expected Attached");
    };
    assert_eq!(snapshot.resources.len(), 1);
    let info = &snapshot.resources[0];
    assert_eq!(info.kind, ResourceKind::AgentSession);
    assert_eq!(info.parent, Some(ResourceId::local(3)));
    assert_eq!(info.agent, Some(AgentFacet::new("claude", "blocked")));
    assert_eq!(info.window_id, WindowId::new(0));
    assert_eq!((info.cols, info.rows), (0, 0));
}

#[test]
fn bootstrap_begin_agent_events_jsonl_v1_round_trips_and_rejects_state_sync() {
    assert_round_trip(&FrameKind::BootstrapBegin {
        terminal_id: ResourceId::local(0x2B),
        stream_id: StreamId::new(1).unwrap(),
        bootstrap_id: BootstrapId::new(1).unwrap(),
        profile: BootstrapStreamProfile::AgentEventsJsonlV1,
        cols: 0,
        rows: 0,
        base_seq: 0,
    });
    // codec tag 3 + StateSync output mode is not a legal stream profile, and
    // neither is a gridless stream that claims a grid.
    for (cols, output_mode) in [(0u16, 1u8), (80, 0)] {
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, &local_id_bytes(0x2B));
        tlv_field(&mut fields, 2, &1u64.to_be_bytes());
        tlv_field(&mut fields, 3, &1u64.to_be_bytes());
        tlv_field(&mut fields, 4, &[3u8]);
        tlv_field(&mut fields, 5, &cols.to_be_bytes());
        tlv_field(&mut fields, 6, &0u16.to_be_bytes());
        tlv_field(&mut fields, 7, &[output_mode]);
        tlv_field(&mut fields, 8, &0u64.to_be_bytes());
        assert_eq!(
            FrameKind::decode(&framed_tlv(0x93, &fields)).unwrap_err(),
            DecodeError::InvalidBootstrapProfile
        );
    }
}

/// A snapshot with no keep-empty session is byte-identical to one encoded
/// before session facets existed: nothing trails `focused_resource`.
#[test]
fn snapshot_without_keep_empty_sessions_has_no_session_facets() {
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

    let plain = SessionSnapshot::new(SessionId::new(1), WindowId::new(0), ResourceId::local(0))
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "work")]);
    let marked = plain.clone().with_sessions(vec![
        SessionInfo::new(SessionId::new(1), "work").with_keep_empty(true),
    ]);
    let encode = |snapshot: SessionSnapshot| {
        let mut buf = BytesMut::new();
        FrameKind::Attached {
            attach_id: 1,
            snapshot,
            initial_client_id: ClientId::new(0),
        }
        .encode(&mut buf);
        buf
    };
    let plain_len = encode(plain).len();
    // A marked snapshot adds a zero-count resource-facet list (4 bytes), a
    // zero-count hosts list (4 bytes), a one-row session-facet count
    // (4 bytes) and the row record itself (4 + 5 bytes).
    assert_eq!(encode(marked).len(), plain_len + 4 + 4 + 4 + 4 + 5);
}

/// All three trailing lists at once (L1.md §9.1 order: facets, hosts,
/// keep-empty session facets) round-trip, in `ATTACHED` and in a
/// `GET_STATE` reply, each flag staying on its own row.
#[test]
fn facets_hosts_and_session_facets_round_trip_together() {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::{
        HostInventory, HostSessionInfo, ResourceInfo, SessionInfo, SessionSnapshot,
    };

    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(10), ResourceId::local(7))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_window_count(1),
            SessionInfo::new(SessionId::new(2), "parked").with_keep_empty(true),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(7), WindowId::new(10), 80, 24),
            ResourceInfo::resource(ResourceId::local(8), ResourceKind::AgentSession)
                .with_parent(Some(ResourceId::local(7))),
        ])
        .with_hosts(vec![
            HostInventory::reachable(
                SatelliteHost::new("edge"),
                vec![HostSessionInfo::new(SessionId::new(2), "build").with_window_count(1)],
            ),
            HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
        ]);
    assert_round_trip(&FrameKind::Attached {
        attach_id: 1,
        snapshot: snapshot.clone(),
        initial_client_id: ClientId::new(0),
    });
    assert_round_trip(&FrameKind::CommandResult {
        request_id: 4,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    });
}

/// Session facets with no hosts and no resource facets: both earlier lists
/// are written as explicit zero counts, and the field cut off after either
/// earlier list (what an older decoder reads) still decodes, with the mark
/// simply unseen.
#[test]
fn keep_empty_without_hosts_writes_zero_count_anchors() {
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

    let snapshot = SessionSnapshot::new(SessionId::new(2), WindowId::new(0), ResourceId::local(0))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(2), "parked").with_keep_empty(true),
        ]);
    let reply = FrameKind::CommandResult {
        request_id: 4,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    };
    assert_round_trip(&reply);

    let mut buf = BytesMut::new();
    reply.encode(&mut buf);
    // The field ends: facets count 0, hosts count 0, rows 1, then the row
    // record (length 5, id 2, flags 1).
    let session_row = record(&[0, 0, 0, 2, 1]);
    let mut tail = Vec::new();
    tail.extend_from_slice(&0u32.to_be_bytes());
    tail.extend_from_slice(&0u32.to_be_bytes());
    tail.extend_from_slice(&1u32.to_be_bytes());
    tail.extend_from_slice(&session_row);
    assert!(buf.ends_with(&tail), "zero-count facet and host anchors");

    // What an older decoder reads: the same snapshot field cut off after the
    // facets, and after the hosts. Hand-rolled like the host-inventory test:
    // one session, "parked" (id 2), then the trailing lists.
    let mut parked = Vec::new();
    parked.extend_from_slice(&2u32.to_be_bytes()); // id
    parked.extend_from_slice(&6u32.to_be_bytes());
    parked.extend_from_slice(b"parked");
    parked.push(0); // no active window
    parked.extend_from_slice(&0i64.to_be_bytes()); // created_at
    parked.extend_from_slice(&0u16.to_be_bytes()); // window_count
    parked.extend_from_slice(&0u16.to_be_bytes()); // attached clients
    let mut prefix = Vec::new();
    prefix.extend_from_slice(&1u32.to_be_bytes()); // sessions: one
    prefix.extend_from_slice(&record(&parked));
    prefix.extend_from_slice(&0u32.to_be_bytes()); // windows
    prefix.extend_from_slice(&0u32.to_be_bytes()); // resources
    prefix.extend_from_slice(&2u32.to_be_bytes()); // focused_session
    prefix.extend_from_slice(&0u32.to_be_bytes()); // focused_window
    prefix.extend_from_slice(&local_id_bytes(0)); // focused_resource
    let after_facets = [prefix.as_slice(), &0u32.to_be_bytes()].concat();
    let after_hosts = [after_facets.as_slice(), &0u32.to_be_bytes()].concat();
    let full = [after_hosts.as_slice(), &1u32.to_be_bytes(), &session_row].concat();

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
    for cut in [&prefix, &after_facets, &after_hosts] {
        let snapshot = decode(cut);
        assert_eq!(snapshot.sessions[0].name, "parked");
        assert!(
            !snapshot.sessions[0].keep_empty,
            "the mark is simply unseen"
        );
        assert!(snapshot.hosts().is_empty());
    }
    assert!(decode(&full).sessions[0].keep_empty);
}

/// The `phux.session.name/v1` value codec round-trips `current\0new` and
/// refuses empty sides, extra NULs, or non-UTF-8.
#[test]
fn session_rename_value_codec() {
    use phux_protocol::wire::frame::{decode_session_rename, encode_session_rename};

    assert_eq!(encode_session_rename("work", "notes"), b"work\0notes");
    assert_eq!(
        decode_session_rename(&encode_session_rename("work", "notes")),
        Some(("work", "notes"))
    );
    assert_eq!(decode_session_rename(b"work\0"), None);
    assert_eq!(decode_session_rename(b"\0notes"), None);
    assert_eq!(decode_session_rename(b"work"), None);
    assert_eq!(decode_session_rename(b"work\0notes\0x"), None);
    assert_eq!(decode_session_rename(&[0xFF, 0, b'n']), None);
}

/// The `phux.session.keep_empty/v1` value codec round-trips and refuses
/// anything but `name\0true` / `name\0false`.
#[test]
fn session_keep_empty_value_codec() {
    use phux_protocol::wire::frame::{decode_session_keep_empty, encode_session_keep_empty};

    assert_eq!(encode_session_keep_empty("work", true), b"work\0true");
    assert_eq!(
        decode_session_keep_empty(&encode_session_keep_empty("work", false)),
        Some(("work", false))
    );
    assert_eq!(
        decode_session_keep_empty(b"work\0true"),
        Some(("work", true))
    );
    assert_eq!(decode_session_keep_empty(b"work\0yes"), None);
    assert_eq!(decode_session_keep_empty(b"work"), None);
    assert_eq!(decode_session_keep_empty(&[0xFF, 0, b't']), None);
}

/// An unknown session-facet flag bit is ignored rather than refused, so the
/// flag byte can grow additively.
#[test]
fn unknown_session_facet_flag_bits_are_ignored() {
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(0), ResourceId::local(0))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_keep_empty(true),
        ]);
    let reply = FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    };
    let mut buf = BytesMut::new();
    reply.encode(&mut buf);
    // The session row is the frame's last five bytes: id (u32) then flags.
    let flags = buf.len() - 1;
    buf[flags] = 0xFF;
    let (decoded, _) = FrameKind::decode(&buf).unwrap();
    let FrameKind::CommandResult {
        result: CommandResult::OkWith(CommandValue::State(state)),
        ..
    } = decoded
    else {
        panic!("expected a GET_STATE reply");
    };
    assert!(state.sessions[0].keep_empty);
}
