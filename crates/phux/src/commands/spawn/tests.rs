use super::*;
use bytes::BytesMut;
use phux_client::layout::leaves;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{BootstrapCapabilities, ServerCapabilities, select_bootstrap_profile};
use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn state(spawned_window: Option<WindowId>) -> SessionSnapshot {
    let session_one = SessionId::new(1);
    let session_two = SessionId::new(2);
    let window_one = WindowId::new(10);
    let window_two = WindowId::new(20);
    let mut panes = vec![
        ResourceInfo::new(ResourceId::local(1), window_one, 80, 24),
        ResourceInfo::new(ResourceId::local(2), window_two, 80, 24),
    ];
    if let Some(window) = spawned_window {
        panes.push(ResourceInfo::new(ResourceId::local(3), window, 80, 24));
    }
    SessionSnapshot::new(session_two, window_two, ResourceId::local(2))
        .with_sessions(vec![
            SessionInfo::new(session_one, "one"),
            SessionInfo::new(session_two, "two"),
        ])
        .with_windows(vec![
            WindowInfo::new(window_one, session_one, "one"),
            WindowInfo::new(window_two, session_two, "two"),
        ])
        .with_resources(panes)
}

struct MockConnection(tokio::net::UnixStream);

impl MockConnection {
    async fn recv(&mut self) -> FrameKind {
        let mut header = [0_u8; 4];
        self.0.read_exact(&mut header).await.expect("frame header");
        let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
        self.0.read_exact(&mut body).await.expect("frame body");
        let mut framed = Vec::with_capacity(4 + body.len());
        framed.extend_from_slice(&header);
        framed.extend_from_slice(&body);
        let (frame, tail) = FrameKind::decode(&framed).expect("decode mock frame");
        assert!(tail.is_empty());
        frame
    }

    async fn send(&mut self, frame: &FrameKind) {
        let mut bytes = BytesMut::new();
        frame.encode(&mut bytes);
        self.0.write_all(&bytes).await.expect("write mock frame");
        self.0.flush().await.expect("flush mock frame");
    }
}

async fn accept(listener: &tokio::net::UnixListener) -> MockConnection {
    let (stream, _) = listener.accept().await.expect("accept mock client");
    let mut conn = MockConnection(stream);
    let FrameKind::Hello { client_caps, .. } = conn.recv().await else {
        panic!("expected HELLO");
    };
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new())
            .expect("fixture profiles intersect");
    conn.send(&FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile,
        bootstrap_limits,
    })
    .await;
    conn
}

async fn reply_state(conn: &mut MockConnection, snapshot: SessionSnapshot) {
    let FrameKind::Command {
        request_id,
        command: Command::GetState { .. },
    } = conn.recv().await
    else {
        panic!("expected GET_STATE");
    };
    conn.send(&FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    })
    .await;
}

fn spawn_mock(socket_path: &Path, wrong_owner: bool) -> std::thread::JoinHandle<()> {
    let std_listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind mock");
    std_listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock runtime");
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(std_listener).expect("listener");

            let mut pre = accept(&listener).await;
            reply_state(&mut pre, state(None)).await;
            drop(pre);

            let mut spawn = accept(&listener).await;
            let FrameKind::SpawnResource {
                request_id,
                command,
                owner_terminal,
                ..
            } = spawn.recv().await
            else {
                panic!("expected SPAWN_RESOURCE");
            };
            assert_eq!(owner_terminal, Some(ResourceId::local(1)));
            assert_eq!(command, Some(vec!["agent".to_owned()]));
            spawn
                .send(&FrameKind::ResourceSpawned {
                    request_id,
                    result: SpawnResult::Ok(ResourceId::local(3)),
                })
                .await;
            drop(spawn);

            let mut verify = accept(&listener).await;
            let spawned_window = if wrong_owner {
                WindowId::new(20)
            } else {
                WindowId::new(10)
            };
            reply_state(&mut verify, state(Some(spawned_window))).await;
            drop(verify);

            if wrong_owner {
                let mut cleanup = accept(&listener).await;
                let FrameKind::Command {
                    request_id,
                    command: Command::KillResource { terminal_id, .. },
                } = cleanup.recv().await
                else {
                    panic!("expected KILL_RESOURCE rollback");
                };
                assert_eq!(terminal_id, ResourceId::local(3));
                cleanup
                    .send(&FrameKind::CommandResult {
                        request_id,
                        result: CommandResult::Ok,
                    })
                    .await;
                return;
            }

            let mut layout = accept(&listener).await;
            let FrameKind::GetMetadata { request_id, .. } = layout.recv().await else {
                panic!("expected layout GET");
            };
            layout
                .send(&FrameKind::MetadataValue {
                    request_id,
                    value: None,
                })
                .await;
            let FrameKind::SetMetadata { value, .. } = layout.recv().await else {
                panic!("expected layout SET");
            };
            let workspace =
                phux_client::layout::ArrangementDoc::decode_cbor(&value).expect("placed workspace");
            assert_eq!(
                workspace.active_group().unwrap().focus,
                Some(ResourceId::local(1))
            );
            assert_eq!(
                leaves(workspace.active_group().unwrap().tree.as_ref().unwrap()),
                vec![ResourceId::local(1), ResourceId::local(3)]
            );
            let FrameKind::GetMetadata { request_id, .. } = layout.recv().await else {
                panic!("expected confirming layout GET");
            };
            layout
                .send(&FrameKind::MetadataValue {
                    request_id,
                    value: Some(value),
                })
                .await;
        });
    })
}

/// `phux spawn --json` pins `schema_version` 1 plus the two documented
/// fields (§4.11) for both a local and a satellite-routed spawn.
#[test]
fn spawned_document_pins_the_contract_shape() {
    let doc = phux_client::spawn::spawned_document(&ResourceId::local(7), false);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["terminal_id"], 7);
    assert!(doc["satellite"].is_null());
    assert_eq!(doc["replayed"], false);
    assert_eq!(doc.as_object().map(serde_json::Map::len), Some(4));

    let doc = phux_client::spawn::spawned_document(&ResourceId::satellite("edge", 3), true);
    assert_eq!(doc["terminal_id"], 3);
    assert_eq!(doc["satellite"], "edge");
    assert_eq!(doc["replayed"], true);
}

#[test]
fn durability_rides_the_spawn_only_when_asked() {
    assert!(SpawnDurability::default().resource().is_none());
    let key =
        phux_client::spawn::parse_idempotency_key("0123456789abcdef0123456789abcdef").expect("key");
    let resource = SpawnDurability {
        retain_secs: Some(0),
        idempotency_key: Some(key),
    }
    .resource()
    .expect("fields");
    assert_eq!(resource.retain_secs, Some(0));
    assert_eq!(resource.idempotency_key, Some(key));
}

#[test]
fn a_malformed_key_is_a_usage_error() {
    for bad in [
        "",
        "abc",
        "00000000000000000000000000000000",
        "zz23456789abcdef0123456789abcdef",
    ] {
        assert_eq!(
            parse_key_arg(Some(bad), true).expect_err(bad),
            ExitCode::from(EXIT_USAGE)
        );
    }
    assert_eq!(parse_key_arg(None, true), Ok(None));
}

#[test]
fn retain_and_key_flags_parse_before_the_command() {
    let cli = crate::parse_cli([
        "phux",
        "spawn",
        "--retain=30",
        "--idempotency-key",
        "0123456789abcdef0123456789abcdef",
        "--",
        "true",
    ])
    .expect("parses");
    let Some(crate::commands::Command::Spawn(crate::commands::SpawnArgs {
        retain,
        idempotency_key,
        command,
        ..
    })) = cli.command
    else {
        panic!("expected Spawn");
    };
    assert_eq!(retain, Some(30));
    assert_eq!(
        idempotency_key.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(command, vec!["true".to_owned()]);

    let bare = crate::parse_cli(["phux", "spawn", "--retain", "--", "true"]).expect("parses");
    let Some(crate::commands::Command::Spawn(crate::commands::SpawnArgs { retain, .. })) =
        bare.command
    else {
        panic!("expected Spawn");
    };
    assert_eq!(retain, Some(0), "bare --retain asks for the server default");
}

fn spawn_frame() -> FrameKind {
    FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command: Some(vec!["agent".to_owned()]),
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: None,
    }
}

#[test]
fn placed_spawn_verifies_ownership_before_publishing_metadata() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let socket = temp.path().join("mock.sock");
    let mock = spawn_mock(&socket, false);
    let result = dispatch_spawn_placed(
        &socket,
        spawn_frame(),
        1,
        "spawn",
        "@1",
        SpawnSplit::Vertical,
        0.3,
        None,
        None,
        false,
    );
    assert!(matches!(result, Ok(SpawnResult::Ok(id)) if id == ResourceId::local(3)));
    mock.join().expect("mock server");
}

#[test]
fn old_server_ignoring_owner_is_rolled_back_before_layout_write() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let socket = temp.path().join("mock.sock");
    let mock = spawn_mock(&socket, true);
    let result = dispatch_spawn_placed(
        &socket,
        spawn_frame(),
        1,
        "spawn",
        "@1",
        SpawnSplit::Horizontal,
        0.5,
        None,
        None,
        false,
    );
    assert!(result.is_err());
    mock.join().expect("mock server");
}
