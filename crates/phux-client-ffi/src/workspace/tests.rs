use super::*;
use crate::*;
use phux_client_core::layout::{self, LayoutNode, LayoutState, WindowState};
use phux_protocol::wire::info::{SessionInfo, TerminalInfo, WindowInfo};
use phux_protocol::{SessionId, TerminalId, WindowId};

fn registry(selected: u32, extra: bool) -> SessionSnapshot {
    let mut panes = vec![
        TerminalInfo::new(TerminalId::local(1), WindowId::new(10), 80, 24),
        TerminalInfo::new(TerminalId::local(2), WindowId::new(10), 80, 24),
        TerminalInfo::new(TerminalId::local(9), WindowId::new(20), 80, 24),
    ];
    if extra {
        panes.push(TerminalInfo::new(
            TerminalId::local(3),
            WindowId::new(10),
            80,
            24,
        ));
    }
    SessionSnapshot::new(
        SessionId::new(selected),
        WindowId::new(10),
        TerminalId::local(1),
    )
    .with_sessions(vec![
        SessionInfo::new(SessionId::new(1), "one"),
        SessionInfo::new(SessionId::new(2), "two"),
    ])
    .with_windows(vec![
        WindowInfo::new(WindowId::new(10), SessionId::new(1), "registry-one"),
        WindowInfo::new(WindowId::new(20), SessionId::new(2), "registry-two"),
    ])
    .with_panes(panes)
}

fn harness() -> Box<PhuxClient> {
    let mut client = attached_harness();
    initial_read(&mut client.inner);
    finish(&mut client, registry(1, false), None);
    client.inner.outgoing.clear();
    client
}

fn attached_harness() -> Box<PhuxClient> {
    let limits = crate::client::Limits {
        bootstrap_chunk: 1024,
        history_page: 1024,
        history_page_rows: 128,
        history_cache_bytes: 4096,
        history_materialized_rows: 1024,
        history_prefetch_rows: 64,
    };
    let mut client = Box::new(PhuxClient {
        inner: Client::new(limits),
        _not_send_sync: std::marker::PhantomData,
    });
    client.inner.attached = true;
    client.inner.protocol_ready = true;
    attached(&mut client.inner, registry(1, false));
    client
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "fixture feeder consumes temporary frame values"
)]
fn feed(client: &mut PhuxClient, frame: FrameKind) {
    let mut bytes = bytes::BytesMut::new();
    frame.encode(&mut bytes);
    // SAFETY: live client and readable frame bytes are disjoint.
    assert_eq!(
        unsafe { phux_client_feed_frame(client, bytes.as_ptr(), bytes.len()) },
        PhuxClientResult::Ok
    );
}

fn refresh(
    client: &mut PhuxClient,
    request: u32,
    registry: SessionSnapshot,
    metadata: Option<Vec<u8>>,
) {
    // SAFETY: live exclusive client.
    assert_eq!(
        unsafe { phux_client_workspace_refresh(client, request) },
        PhuxClientResult::Ok
    );
    finish(client, registry, metadata);
}

fn finish(client: &mut PhuxClient, registry: SessionSnapshot, metadata: Option<Vec<u8>>) {
    let state_id = client
        .inner
        .workspace
        .pending
        .as_ref()
        .unwrap()
        .state_id
        .unwrap();
    feed(
        client,
        FrameKind::CommandResult {
            request_id: state_id,
            result: CommandResult::OkWith(CommandValue::State(registry)),
        },
    );
    let id = client.inner.workspace.pending.as_ref().unwrap().metadata_id;
    feed(
        client,
        FrameKind::MetadataValue {
            request_id: id,
            value: metadata,
        },
    );
}

fn split_workspace() -> Workspace {
    let a = TerminalId::local(1);
    let tree = layout::split_at(
        &LayoutNode::Leaf(a.clone()),
        &a,
        &TerminalId::local(2),
        layout::SplitDir::Vertical,
        0.3,
    )
    .unwrap();
    Workspace {
        windows: vec![WindowState::new(
            "editor".into(),
            LayoutState {
                tree: Some(tree),
                focus: Some(TerminalId::local(2)),
            },
        )],
        active: 0,
    }
}

#[test]
fn initial_attachment_waits_for_confirmed_metadata_before_publishing_topology() {
    for metadata in [None, Some(split_workspace().encode_cbor().unwrap())] {
        let mut client = attached_harness();
        assert_eq!(client.inner.workspace.selected, 1);
        assert_eq!(client.inner.workspace.catalog.terminals.len(), 3);
        assert_eq!(client.inner.sessions.len(), 2);
        assert_eq!(client.inner.workspace.state, 0);
        assert!(client.inner.workspace.topology.windows.is_empty());
        initial_read(&mut client.inner);
        let pending = client.inner.workspace.pending.as_ref().unwrap();
        let state_id = pending.state_id.unwrap();
        let metadata_id = pending.metadata_id;
        feed(
            &mut client,
            FrameKind::MetadataValue {
                request_id: metadata_id,
                value: metadata.clone(),
            },
        );
        assert_eq!(client.inner.workspace.state, 0);
        assert_eq!(client.inner.workspace.status, 1);
        assert!(client.inner.workspace.nodes.is_empty());
        assert!(client.inner.workspace.topology.windows.is_empty());
        feed(
            &mut client,
            FrameKind::CommandResult {
                request_id: state_id,
                result: CommandResult::OkWith(CommandValue::State(registry(1, false))),
            },
        );
        assert_eq!(
            client.inner.workspace.state,
            if metadata.is_some() { 2 } else { 1 }
        );
        assert_eq!(
            client.inner.workspace.topology.windows.len(),
            if metadata.is_some() { 1 } else { 2 }
        );
    }
}

#[test]
fn persisted_old_schema_is_refused_without_fallback_or_overwrite() {
    let mut client = harness();
    let before = client.inner.workspace.topology.clone();
    let revision = client.inner.workspace.revision;
    refresh(
        &mut client,
        1,
        registry(1, false),
        Some(b"\xa1\x67version\x02".to_vec()),
    );
    assert_eq!(client.inner.workspace.state, 3);
    assert_eq!(client.inner.workspace.status, 3);
    assert_eq!(client.inner.workspace.revision, revision);
    assert_eq!(client.inner.workspace.topology, before);
    assert!(
        String::from_utf8_lossy(&client.inner.workspace.message)
            .contains("unsupported layout envelope version: 2")
    );
    let input = edit(&client, 2, 6);
    let queued = client.inner.outgoing.len();
    // SAFETY: valid, disjoint client and input; stored schema must fence writes.
    assert_eq!(
        unsafe { phux_client_workspace_mutate(&raw mut *client, &raw const input) },
        PhuxClientResult::InvalidState
    );
    assert_eq!(client.inner.outgoing.len(), queued);
}

#[test]
fn authority_transition_advances_revision_even_when_topology_is_identical() {
    let mut client = harness();
    let bytes = client.inner.workspace.topology.encode_cbor().unwrap();
    let fallback_revision = client.inner.workspace.revision;
    refresh(&mut client, 1, registry(1, false), Some(bytes.clone()));
    let authoritative_revision = client.inner.workspace.revision;
    assert!(authoritative_revision > fallback_revision);
    assert_eq!(client.inner.workspace.state, 2);
    refresh(
        &mut client,
        2,
        registry(1, false),
        Some(b"invalid".to_vec()),
    );
    refresh(&mut client, 3, registry(1, false), Some(bytes));
    assert_eq!(client.inner.workspace.revision, authoritative_revision);
    refresh(&mut client, 4, registry(1, false), None);
    assert!(client.inner.workspace.revision > authoritative_revision);
    assert_eq!(client.inner.workspace.state, 1);
}

#[test]
fn removed_seed_can_be_placed_in_a_new_window() {
    let mut client = harness();
    refresh(
        &mut client,
        1,
        registry(1, false),
        Some(split_workspace().encode_cbor().unwrap()),
    );
    let retained = client.inner.workspace.topology.windows[0].id;
    let mut remove = edit(&client, 2, 3);
    remove.terminal_id = terminal_id_out(&TerminalId::local(1));
    mutate(&mut client, &remove);
    let mut add = edit(&client, 3, 1);
    add.terminal_id = terminal_id_out(&TerminalId::local(1));
    add.name = bytes_out(b"reused seed");
    mutate(&mut client, &add);
    assert_eq!(client.inner.workspace.topology.windows.len(), 2);
    assert_eq!(client.inner.workspace.topology.windows[0].id, retained);
    assert_ne!(client.inner.workspace.topology.windows[1].id, retained);
}

#[test]
fn external_registry_changes_preserve_attached_session_without_replicas() {
    let mut client = harness();
    assert_eq!(client.inner.workspace.topology.windows.len(), 2);
    let before = client.inner.workspace.revision;
    let mut snapshot = registry(2, true);
    snapshot.sessions[0].name = "renamed externally".into();
    snapshot.panes.retain(|p| p.id != TerminalId::local(2));
    refresh(&mut client, 1, snapshot, None);
    let ws = &client.inner.workspace;
    assert_eq!(ws.selected, 1);
    assert!(ws.catalog.sessions[0].focused);
    assert!(!ws.catalog.sessions[1].focused);
    assert_eq!(ws.topology.windows.len(), 2);
    assert_eq!(
        ws.topology.windows[1].state.focus,
        Some(TerminalId::local(3))
    );
    assert!(ws.revision > before);
    assert_eq!(client.inner.sessions[0].name, b"renamed externally");
    assert!(client.inner.render.is_empty());
    assert!(
        client
            .inner
            .session
            .published(&TerminalId::local(3))
            .is_none()
    );
}

#[test]
fn topology_ignores_remote_focus_and_client_count_noise() {
    let mut client = harness();
    let bytes = split_workspace().encode_cbor().unwrap();
    refresh(&mut client, 1, registry(2, false), Some(bytes.clone()));
    assert_eq!(
        client.inner.workspace.topology.windows[0].state.focus,
        Some(TerminalId::local(1))
    );
    let revision = client.inner.workspace.revision;
    let mut noisy = registry(2, false);
    noisy.sessions[0].attached_client_count = 10;
    refresh(&mut client, 2, noisy, Some(bytes));
    assert_eq!(client.inner.workspace.revision, revision);
    assert_eq!(client.inner.workspace.nodes.len(), 3);
    assert_eq!(client.inner.workspace.catalog.terminals.len(), 3);
}

#[test]
fn malformed_capacity_and_wrong_reply_keep_last_good_and_allow_terminal_operations() {
    let mut client = harness();
    let before = client.inner.workspace.topology.clone();
    refresh(&mut client, 1, registry(2, true), Some(b"broken".to_vec()));
    assert_eq!(client.inner.workspace.state, 3);
    assert_eq!(client.inner.workspace.status, 3);
    assert_eq!(client.inner.workspace.topology, before);
    assert!(client.inner.attached);
    let mut huge = registry(2, true);
    huge.sessions = (1..258)
        .map(|i| SessionInfo::new(SessionId::new(i), "s"))
        .collect();
    // SAFETY: exclusive client.
    assert_eq!(
        unsafe { phux_client_workspace_refresh(&raw mut *client, 2) },
        PhuxClientResult::Ok
    );
    let id = client
        .inner
        .workspace
        .pending
        .as_ref()
        .unwrap()
        .state_id
        .unwrap();
    feed(
        &mut client,
        FrameKind::CommandResult {
            request_id: id,
            result: CommandResult::OkWith(CommandValue::State(huge)),
        },
    );
    assert_eq!(client.inner.workspace.status, 3);
    assert!(client.inner.workspace.pending.is_none());
    assert_eq!(client.inner.workspace.topology, before);
    // SAFETY: exclusive client.
    assert_eq!(
        unsafe { phux_client_workspace_refresh(&raw mut *client, 3) },
        PhuxClientResult::Ok
    );
    let id = client
        .inner
        .workspace
        .pending
        .as_ref()
        .unwrap()
        .state_id
        .unwrap();
    feed(
        &mut client,
        FrameKind::CommandResult {
            request_id: id,
            result: CommandResult::OkWith(CommandValue::State(registry(1, false))),
        },
    );
    let id = client.inner.workspace.pending.as_ref().unwrap().metadata_id;
    feed(
        &mut client,
        FrameKind::MetadataValue {
            request_id: id,
            value: Some(b"malformed".to_vec()),
        },
    );
    assert_eq!(client.inner.workspace.topology, before);
    assert_eq!(client.inner.workspace.state, 3);
}

#[test]
fn operation_interleaving_and_internal_duplicates_are_fenced() {
    let mut client = harness();
    // SAFETY: client and stack options are live and disjoint.
    unsafe {
        assert_eq!(
            phux_client_workspace_refresh(&raw mut *client, 10),
            PhuxClientResult::Ok
        );
        let options = PhuxSpawnOptions {
            request_id: 10,
            ..PhuxSpawnOptions::default()
        };
        assert_eq!(
            phux_client_queue_spawn(&raw mut *client, &raw const options),
            PhuxClientResult::InvalidArgument
        );
        let options = PhuxSpawnOptions {
            request_id: 11,
            ..options
        };
        assert_eq!(
            phux_client_queue_spawn(&raw mut *client, &raw const options),
            PhuxClientResult::Ok
        );
        let options = PhuxSpawnOptions {
            request_id: INTERNAL_START,
            ..options
        };
        assert_eq!(
            phux_client_queue_spawn(&raw mut *client, &raw const options),
            PhuxClientResult::InvalidArgument
        );
    }
    let state_id = client
        .inner
        .workspace
        .pending
        .as_ref()
        .unwrap()
        .state_id
        .unwrap();
    feed(
        &mut client,
        FrameKind::TerminalSpawned {
            request_id: 11,
            result: phux_protocol::wire::frame::SpawnResult::Ok(TerminalId::local(4)),
        },
    );
    finish(&mut client, registry(2, false), None);
    let revision = client.inner.workspace.revision;
    feed(
        &mut client,
        FrameKind::CommandResult {
            request_id: state_id,
            result: CommandResult::OkWith(CommandValue::State(registry(2, true))),
        },
    );
    feed(
        &mut client,
        FrameKind::MetadataValue {
            request_id: u32::MAX,
            value: Some(vec![0]),
        },
    );
    assert_eq!(client.inner.workspace.revision, revision);
    assert_eq!(client.inner.workspace.status, 2);
    // SAFETY: read-only client.
    assert_eq!(
        unsafe { phux_client_operation_count(&raw const *client) },
        1
    );
}

fn edit(client: &PhuxClient, request: u32, kind: u32) -> PhuxWorkspaceMutation {
    PhuxWorkspaceMutation {
        request_id: request,
        expected_revision: client.inner.workspace.revision,
        session_id: 1,
        kind,
        window_id: client.inner.workspace.topology.windows[0].id,
        ..PhuxWorkspaceMutation::default()
    }
}

fn mutate(client: &mut PhuxClient, mutation: &PhuxWorkspaceMutation) {
    // SAFETY: client and mutation/spans are valid and disjoint.
    assert_eq!(
        unsafe { phux_client_workspace_mutate(client, mutation) },
        PhuxClientResult::Ok
    );
    let proposed = client
        .inner
        .workspace
        .pending
        .as_ref()
        .unwrap()
        .proposed
        .as_ref()
        .unwrap()
        .clone();
    finish(
        client,
        registry(1, true),
        Some(proposed.encode_topology_cbor().unwrap()),
    );
    assert_eq!(client.inner.workspace.status, 2);
}

#[test]
fn mutations_round_trip_stable_identity_and_preserve_local_focus() {
    let mut client = harness();
    refresh(
        &mut client,
        1,
        registry(1, true),
        Some(split_workspace().encode_cbor().unwrap()),
    );
    let original = client.inner.workspace.topology.windows[0].id;
    let mut split = edit(&client, 2, 2);
    split.terminal_id = terminal_id_out(&TerminalId::local(2));
    split.new_terminal_id = terminal_id_out(&TerminalId::local(3));
    split.direction = 2;
    split.ratio = 0.4;
    mutate(&mut client, &split);
    let mut resize = edit(&client, 3, 5);
    resize.path_len = 1;
    resize.path_bits = 1;
    resize.ratio = 0.7;
    mutate(&mut client, &resize);
    let mut rename = edit(&client, 4, 6);
    rename.name = bytes_out(b"renamed");
    mutate(&mut client, &rename);
    let mut remove = edit(&client, 5, 3);
    remove.terminal_id = terminal_id_out(&TerminalId::local(1));
    mutate(&mut client, &remove);
    assert_eq!(client.inner.workspace.topology.windows[0].id, original);
    assert_eq!(client.inner.workspace.topology.windows[0].name, "renamed");
    assert_eq!(
        client.inner.workspace.topology.windows[0].state.focus,
        Some(TerminalId::local(2))
    );
    assert_eq!(client.inner.workspace.catalog.terminals.len(), 4);
    assert!(
        client
            .inner
            .workspace
            .catalog
            .terminals
            .iter()
            .any(|t| t.id == TerminalId::local(1))
    );
}

#[test]
fn stale_and_competing_writes_never_optimistically_publish() {
    let mut client = harness();
    let mut input = edit(&client, 1, 6);
    input.expected_revision += 1;
    // SAFETY: client/input are valid and disjoint.
    assert_eq!(
        unsafe { phux_client_workspace_mutate(&raw mut *client, &raw const input) },
        PhuxClientResult::InvalidState
    );
    assert!(client.inner.outgoing.is_empty());
    input.expected_revision -= 1;
    input.name = bytes_out(b"my rename");
    let before = client.inner.workspace.topology.clone();
    // SAFETY: client/input are valid and disjoint.
    assert_eq!(
        unsafe { phux_client_workspace_mutate(&raw mut *client, &raw const input) },
        PhuxClientResult::Ok
    );
    assert_eq!(client.inner.workspace.topology, before);
    let winning = split_workspace();
    finish(
        &mut client,
        registry(1, true),
        Some(winning.encode_cbor().unwrap()),
    );
    assert_eq!(client.inner.workspace.status, 3);
    assert!(topology_equal(&client.inner.workspace.topology, &winning));
}

#[test]
fn retired_is_registry_absence_not_missing_replica_and_satellites_never_alias() {
    let mut snapshot = registry(1, false);
    snapshot.panes.push(TerminalInfo::new(
        TerminalId::satellite("peer", 1),
        WindowId::new(10),
        80,
        24,
    ));
    let catalog = Catalog::from_snapshot(snapshot, 1).unwrap();
    assert_eq!(catalog.terminals.last().unwrap().session, 0);
    let adopted = model::adopt(
        Some(&split_workspace().encode_cbor().unwrap()),
        &Workspace::new(),
        &catalog,
        1,
    )
    .unwrap();
    assert_eq!(
        layout::leaves(adopted.windows[0].state.tree.as_ref().unwrap()).len(),
        2
    );
    let mut snapshot = registry(1, false);
    snapshot.panes.retain(|t| t.id != TerminalId::local(1));
    let catalog = Catalog::from_snapshot(snapshot, 1).unwrap();
    let pruned =
        model::adopt(Some(&adopted.encode_cbor().unwrap()), &adopted, &catalog, 1).unwrap();
    assert_eq!(pruned.windows[0].id, adopted.windows[0].id);
    assert_eq!(
        pruned.windows[0].state.tree,
        Some(LayoutNode::Leaf(TerminalId::local(2)))
    );
}

#[test]
fn abi_null_version_size_and_disconnect() {
    assert_eq!(std::mem::size_of::<PhuxWorkspaceInfo>(), 72);
    assert_eq!(std::mem::offset_of!(PhuxWorkspaceInfo, revision), 16);
    assert_eq!(std::mem::offset_of!(PhuxWorkspaceInfo, message), 56);
    assert_eq!(std::mem::size_of::<PhuxWorkspaceWindow>(), 56);
    assert_eq!(std::mem::size_of::<PhuxWorkspaceNode>(), 56);
    assert_eq!(std::mem::size_of::<PhuxCatalogTerminal>(), 80);
    assert_eq!(std::mem::size_of::<PhuxWorkspaceMutation>(), 136);
    assert_eq!(std::mem::offset_of!(PhuxWorkspaceMutation, path_bits), 128);
    let mut client = harness();
    let mut info = PhuxWorkspaceInfo::default();
    // SAFETY: deliberate null/invalid headers, otherwise owned disjoint values.
    unsafe {
        assert_eq!(
            phux_client_workspace_info(std::ptr::null(), &raw mut info),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            phux_client_workspace_info(&raw const *client, std::ptr::null_mut()),
            PhuxClientResult::InvalidArgument
        );
        info.size -= 1;
        assert_eq!(
            phux_client_workspace_info(&raw const *client, &raw mut info),
            PhuxClientResult::InvalidArgument
        );
        info = PhuxWorkspaceInfo::default();
        info.version += 1;
        assert_eq!(
            phux_client_workspace_info(&raw const *client, &raw mut info),
            PhuxClientResult::InvalidArgument
        );
        info = PhuxWorkspaceInfo::default();
        assert_eq!(
            phux_client_workspace_info(&raw const *client, &raw mut info),
            PhuxClientResult::Ok
        );
        assert_eq!(info.terminal_count, 3);
        assert_eq!(
            phux_client_workspace_mutate(&raw mut *client, std::ptr::null()),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            phux_client_workspace_refresh(&raw mut *client, 1),
            PhuxClientResult::Ok
        );
        assert_eq!(
            phux_client_disconnect(&raw mut *client),
            PhuxClientResult::Ok
        );
    }
    assert_eq!(client.inner.workspace.status, 4);
    assert!(client.inner.workspace.pending.is_none());
}

#[test]
fn add_reorder_remove_last_and_fallback_split_roundtrip() {
    let mut client = harness();
    let mut split = edit(&client, 1, 2);
    split.terminal_id = terminal_id_out(&TerminalId::local(1));
    split.new_terminal_id = terminal_id_out(&TerminalId::local(2));
    split.direction = 2;
    split.ratio = 0.5;
    mutate(&mut client, &split);
    assert_eq!(client.inner.workspace.topology.windows.len(), 1);
    let original = client.inner.workspace.topology.windows[0].id;
    let mut add = edit(&client, 2, 1);
    add.terminal_id = terminal_id_out(&TerminalId::local(3));
    add.name = bytes_out(b"new window");
    mutate(&mut client, &add);
    let mut reorder = edit(&client, 3, 4);
    reorder.index = 1;
    mutate(&mut client, &reorder);
    assert_eq!(client.inner.workspace.topology.windows[1].id, original);
    assert_eq!(client.inner.workspace.topology.active, 1);
    for (request, id) in [(4, 3), (5, 1), (6, 2)] {
        let mut remove = edit(&client, request, 3);
        remove.terminal_id = terminal_id_out(&TerminalId::local(id));
        mutate(&mut client, &remove);
    }
    assert!(client.inner.workspace.topology.windows.is_empty());
    assert_eq!(client.inner.workspace.state, 2);
    refresh(
        &mut client,
        7,
        registry(1, true),
        Some(Workspace::new().encode_topology_cbor().unwrap()),
    );
    assert!(client.inner.workspace.topology.windows.is_empty());
    assert_eq!(client.inner.workspace.catalog.terminals.len(), 4);
}

#[test]
fn registry_is_read_after_metadata_and_both_replies_stage_atomically() {
    let mut client = harness();
    // SAFETY: live exclusive client.
    assert_eq!(
        unsafe { phux_client_workspace_refresh(&raw mut *client, 1) },
        PhuxClientResult::Ok
    );
    let first = FrameKind::decode(&client.inner.outgoing[0]).unwrap().0;
    let second = FrameKind::decode(&client.inner.outgoing[1]).unwrap().0;
    assert!(matches!(first, FrameKind::GetMetadata { .. }));
    assert!(matches!(
        second,
        FrameKind::Command {
            command: Command::GetState { .. },
            ..
        }
    ));
    let mut topology = split_workspace();
    topology.add_window("new external".into(), TerminalId::local(3));
    let revision = client.inner.workspace.revision;
    let pending = client.inner.workspace.pending.as_ref().unwrap();
    let (metadata_id, state_id) = (pending.metadata_id, pending.state_id.unwrap());
    feed(
        &mut client,
        FrameKind::MetadataValue {
            request_id: metadata_id,
            value: Some(topology.encode_cbor().unwrap()),
        },
    );
    assert_eq!(client.inner.workspace.revision, revision);
    // A duplicate metadata answer cannot replace the staged snapshot.
    feed(
        &mut client,
        FrameKind::MetadataValue {
            request_id: metadata_id,
            value: None,
        },
    );
    feed(
        &mut client,
        FrameKind::CommandResult {
            request_id: state_id,
            result: CommandResult::OkWith(CommandValue::State(registry(2, true))),
        },
    );
    assert_eq!(client.inner.workspace.status, 2);
    assert_eq!(client.inner.workspace.topology.windows.len(), 2);
    assert_eq!(
        client.inner.workspace.topology.windows[1].name,
        "new external"
    );
}

#[test]
fn workspace_limits_refuse_instead_of_truncating() {
    for ratio in [0.0, 1.0, f32::NAN, f32::INFINITY] {
        let mut workspace = split_workspace();
        let Some(LayoutNode::Split { ratio: value, .. }) = &mut workspace.windows[0].state.tree
        else {
            panic!("split")
        };
        *value = ratio;
        assert!(model::flatten(&workspace).is_err());
    }
    let mut many = Workspace::new();
    for i in 1..=33 {
        many.add_window(i.to_string(), TerminalId::local(i));
    }
    assert!(model::flatten(&many).is_err());
    let mut long_name = Workspace::single(TerminalId::local(1));
    long_name.windows[0].name = "é".repeat(2049);
    assert!(model::flatten(&long_name).is_err());
    let mut deep = LayoutNode::Leaf(TerminalId::local(1));
    for id in 2..=67 {
        deep = LayoutNode::Split {
            dir: layout::SplitDir::Vertical,
            ratio: 0.5,
            left: Box::new(deep),
            right: Box::new(LayoutNode::Leaf(TerminalId::local(id))),
        };
    }
    let deep = Workspace {
        windows: vec![WindowState::new(
            "deep".into(),
            LayoutState {
                tree: Some(deep),
                focus: Some(TerminalId::local(1)),
            },
        )],
        active: 0,
    };
    assert!(model::flatten(&deep).is_err());
    let mut duplicate = split_workspace();
    duplicate.add_window("duplicate".into(), TerminalId::local(1));
    assert!(model::flatten(&duplicate).is_err());
    let over_nodes = Workspace {
        windows: vec![WindowState::new(
            "wide".into(),
            LayoutState {
                tree: Some(balanced_tree(1, 257)),
                focus: Some(TerminalId::local(1)),
            },
        )],
        active: 0,
    };
    assert!(
        model::flatten(&over_nodes).is_err(),
        "513 nodes must refuse without truncation"
    );
}

fn balanced_tree(first: u32, leaves: u32) -> LayoutNode {
    if leaves == 1 {
        return LayoutNode::Leaf(TerminalId::local(first));
    }
    let half = leaves / 2;
    LayoutNode::Split {
        dir: layout::SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(balanced_tree(first, half)),
        right: Box::new(balanced_tree(first + half, leaves - half)),
    }
}

#[test]
fn atomic_window_removal_is_presentation_only_and_wrong_kind_isolated() {
    let mut client = harness();
    refresh(
        &mut client,
        1,
        registry(1, false),
        Some(split_workspace().encode_cbor().unwrap()),
    );
    let remove = edit(&client, 2, 7);
    mutate(&mut client, &remove);
    assert!(client.inner.workspace.topology.windows.is_empty());
    assert_eq!(client.inner.workspace.catalog.terminals.len(), 4);
    // SAFETY: live client, fresh request.
    assert_eq!(
        unsafe { phux_client_workspace_refresh(&raw mut *client, 3) },
        PhuxClientResult::Ok
    );
    let id = client
        .inner
        .workspace
        .pending
        .as_ref()
        .unwrap()
        .state_id
        .unwrap();
    feed(
        &mut client,
        FrameKind::TerminalSpawned {
            request_id: id,
            result: phux_protocol::wire::frame::SpawnResult::Ok(TerminalId::local(99)),
        },
    );
    assert_eq!(client.inner.workspace.status, 3);
    assert!(client.inner.attached);
    assert!(!client.inner.operations.admitted(&TerminalId::local(99)));
}

#[test]
fn mutation_bounds_and_session_fence_refuse_before_queue_or_span_read() {
    let mut client = harness();
    let mut input = edit(&client, 1, 3);
    input.session_id = 2;
    // SAFETY: valid record; session mismatch is intentional.
    assert_eq!(
        unsafe { phux_client_workspace_mutate(&raw mut *client, &raw const input) },
        PhuxClientResult::InvalidState
    );
    input.session_id = 1;
    input.terminal_id.host = PhuxBytes {
        data: std::ptr::null(),
        len: 4097,
    };
    // SAFETY: invalid oversized/null span is rejected by its length before dereference.
    assert_eq!(
        unsafe { phux_client_workspace_mutate(&raw mut *client, &raw const input) },
        PhuxClientResult::InvalidArgument
    );
    assert!(client.inner.outgoing.is_empty());
}
