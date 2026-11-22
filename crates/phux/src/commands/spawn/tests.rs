use super::*;
use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::{
    ActivationId, ArrangementKey, AuthorityId, ResourceKey, Revision, ServerInstance, SessionId,
    SessionKey, WindowId,
};
use phux_protocol::wire::frame::{
    ActivationInfo, ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementOwner,
    ArrangementValue, Command, CommandResult, CommandValue, Committed, CreatePlacement, EntryRef,
    EntryTarget, ExecState, LiveTarget, OrganizationPage, Refusal, ResourceRecord, SessionRecord,
    SplitDirection,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn state() -> SessionSnapshot {
    let session_one = SessionId::new(1);
    let session_two = SessionId::new(2);
    let window_one = WindowId::new(10);
    let window_two = WindowId::new(20);
    let panes = vec![
        ResourceInfo::new(ResourceId::local(1), window_one, 80, 24),
        ResourceInfo::new(ResourceId::local(2), window_two, 80, 24),
    ];
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

async fn accept(listener: &tokio::net::UnixListener, placed: bool) -> MockConnection {
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
        server_caps: ServerCapabilities::new().with_features_ext(ServerFeatureExtSet::with(
            if placed {
                &[
                    ServerFeatureExt::Organization,
                    ServerFeatureExt::CreatePlacement,
                ]
            } else {
                &[]
            },
        )),
        server_id: Vec::new(),
        selected_profile: Some(selected_profile),
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

#[derive(Clone, Copy)]
enum Reply {
    Committed,
    Stale,
    Unknown,
    Disconnected,
    Legacy,
}

#[allow(clippy::too_many_lines, reason = "one ordered mock wire exchange")]
fn spawn_mock(socket_path: &Path, reply: Reply) -> std::thread::JoinHandle<()> {
    let std_listener = std::os::unix::net::UnixListener::bind(socket_path).expect("bind mock");
    std_listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mock runtime");
        runtime
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(10), async move {
                    let listener =
                        tokio::net::UnixListener::from_std(std_listener).expect("listener");

                    let mut conn = accept(&listener, !matches!(reply, Reply::Legacy)).await;
                    reply_state(&mut conn, state()).await;
                    if matches!(reply, Reply::Legacy) {
                        assert_no_writes(conn, listener).await;
                        return;
                    }
                    let authority = AuthorityId::new([0x55; 16]).unwrap();
                    let session = SessionKey::new([0x66; 16]).unwrap();
                    let resource = ResourceKey::new([0x77; 16]).unwrap();
                    let arrangement = ArrangementKey::new([0x22; 16]).unwrap();
                    let entry = [0x33; 16];
                    let instance = ServerInstance::new([0x44; 16]);
                    let activation = ActivationId::new(12);
                    let rev = Revision::new(5);
                    let FrameKind::Command {
                        request_id,
                        command: Command::GetOrganization { .. },
                    } = conn.recv().await
                    else {
                        panic!("expected GET_ORGANIZATION");
                    };
                    conn.send(&FrameKind::CommandResult {
                        request_id,
                        result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                            OrganizationPage::new(authority, rev, 0)
                                .with_sessions(vec![
                                    SessionRecord::new(session, "one", rev)
                                        .with_arrangement(Some(arrangement)),
                                ])
                                .with_resources(vec![
                                    ResourceRecord::new(resource, session, rev)
                                        .with_live(Some(ResourceId::local(1)))
                                        .with_activation(Some(ActivationInfo::new(
                                            activation,
                                            instance,
                                            ExecState::Running,
                                        ))),
                                ]),
                        ))),
                    })
                    .await;
                    let FrameKind::Command {
                        request_id,
                        command: Command::ArrangementGet { arrangement: key },
                    } = conn.recv().await
                    else {
                        panic!("expected ARRANGEMENT_GET");
                    };
                    assert_eq!(key, arrangement);
                    conn.send(&FrameKind::CommandResult {
                        request_id,
                        result: CommandResult::OkWith(CommandValue::Arrangement(Box::new(
                            ArrangementValue::new(
                                arrangement,
                                ArrangementOwner::Session(session),
                                rev,
                                ArrangementDoc {
                                    groups: vec![ArrangementGroup::new(
                                        [1; 16],
                                        None,
                                        ArrangementNode::Leaf(EntryRef::new(
                                            entry,
                                            EntryTarget::Member(resource),
                                        )),
                                    )],
                                },
                            ),
                        ))),
                    })
                    .await;
                    let FrameKind::Command {
                        request_id,
                        command:
                            Command::ResourceCreate {
                                operation,
                                session: destination,
                                placement: Some(placement),
                                launch: Some(launch),
                                on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
                                launch_mode: phux_protocol::wire::frame::LaunchMode::Immediate,
                                ..
                            },
                    } = conn.recv().await
                    else {
                        panic!("expected only RESOURCE_CREATE");
                    };
                    assert_eq!(destination, session);
                    assert_eq!(
                        *placement,
                        CreatePlacement::Beside {
                            entry,
                            resource,
                            instance,
                            activation,
                            dir: SplitDirection::Vertical,
                            ratio: 0.3,
                        }
                    );
                    assert_eq!(launch.command, vec!["agent".to_owned()]);
                    if matches!(reply, Reply::Disconnected) {
                        drop(conn);
                        assert!(
                            tokio::time::timeout(
                                std::time::Duration::from_millis(100),
                                listener.accept()
                            )
                            .await
                            .is_err(),
                            "a lost acknowledgement must not retry or clean up"
                        );
                        return;
                    }
                    let result = match reply {
                        Reply::Stale => CommandResult::Refused(Refusal::Stale {
                            current: Some(ActivationId::new(13)),
                        }),
                        Reply::Committed => {
                            CommandResult::OkWith(CommandValue::Committed(Box::new(
                                Committed::new(operation, rev)
                                    .with_resource(ResourceKey::new([5; 16]))
                                    .with_creation(
                                        Some(authority),
                                        Some(arrangement),
                                        Some([4; 16]),
                                        None,
                                    )
                                    .with_target(Some(LiveTarget {
                                        resource: ResourceId::local(3),
                                        instance,
                                        activation: ActivationId::new(1),
                                    })),
                            )))
                        }
                        // A nonreceipt ack leaves the outcome unknown, not failed.
                        Reply::Unknown => CommandResult::Ok,
                        Reply::Legacy | Reply::Disconnected => unreachable!(),
                    };
                    conn.send(&FrameKind::CommandResult { request_id, result })
                        .await;
                    assert_no_writes(conn, listener).await;
                })
                .await
            })
            .expect("placement exchange completed");
    })
}

async fn assert_no_writes(mut conn: MockConnection, listener: tokio::net::UnixListener) {
    let mut byte = [0];
    tokio::select! {
        read = conn.0.read(&mut byte) => assert_eq!(read.expect("socket read"), 0, "unexpected trailing frame"),
        _ = listener.accept() => panic!("unexpected second connection"),
    }
}

/// `phux spawn --json` pins `schema_version` 1 plus the two documented
/// fields for both a local and a satellite-routed spawn.
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
    let cli = crate::parse_cli(
        "phux spawn --retain=30 --idempotency-key 0123456789abcdef0123456789abcdef -- true"
            .split_whitespace(),
    )
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

#[test]
fn spawn_app_takes_a_name_an_optional_browser_url_and_headless() {
    let app = |line: &str| {
        let cli = crate::parse_cli(line.split_whitespace()).expect("parses");
        let Some(crate::commands::Command::Spawn(crate::commands::SpawnArgs {
            app,
            browser,
            headless,
            ..
        })) = cli.command
        else {
            panic!("expected Spawn");
        };
        (app, browser, headless)
    };
    assert_eq!(
        app("phux spawn --app TextEdit"),
        (Some("TextEdit".into()), None, false)
    );
    assert_eq!(
        app("phux spawn --app chrome --browser --headless"),
        (Some("chrome".into()), Some("about:blank".into()), true)
    );
    assert_eq!(
        app("phux spawn --app chrome --browser=https://phux.sh"),
        (Some("chrome".into()), Some("https://phux.sh".into()), false)
    );
    assert!(crate::parse_cli(["phux", "spawn", "--browser"]).is_err());
    assert!(crate::parse_cli(["phux", "spawn", "--app", "x", "--satellite", "h"]).is_err());
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
fn spawn_split_names_the_divider() {
    assert_eq!(
        split_dir(SpawnSplit::Horizontal),
        Direction::Horizontal.wire()
    );
    assert_eq!(split_dir(SpawnSplit::Horizontal), SplitDir::Vertical);
    assert_eq!(split_dir(SpawnSplit::Vertical), Direction::Vertical.wire());
    assert_eq!(split_dir(SpawnSplit::Vertical), SplitDir::Horizontal);
}

#[test]
fn placed_spawn_uses_a_single_canonical_create() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let socket = temp.path().join("mock.sock");
    let mock = spawn_mock(&socket, Reply::Committed);
    let result = dispatch_spawn_placed(
        &socket,
        spawn_frame(),
        1,
        "spawn",
        "@1",
        // User-facing horizontal stacks, so the wire axis stays Vertical.
        SpawnSplit::Horizontal,
        0.3,
        None,
        None,
        false,
    );
    assert!(matches!(result, Ok(SpawnResult::Ok(id)) if id == ResourceId::local(3)));
    mock.join().expect("mock server");
}

#[test]
fn placed_spawn_refusals_and_unknown_outcomes_never_trigger_cleanup() {
    for (reply, code) in [
        (Reply::Stale, "stale"),
        (Reply::Unknown, codes::SPAWN_OUTCOME_UNKNOWN),
        (Reply::Disconnected, codes::SPAWN_OUTCOME_UNKNOWN),
        (Reply::Legacy, codes::UNSUPPORTED_SERVER),
    ] {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let path = temp.path().join("mock.sock");
        let mock = spawn_mock(&path, reply);
        let err = cli_runtime()
            .unwrap()
            .block_on(
                PlacedSpawn::from_spawn_frame(spawn_frame(), SplitDir::Vertical, 0.3, None, None)
                    .unwrap()
                    .run(&path, &parse_target(Some("@1")).unwrap(), 1),
            )
            .unwrap_err();
        let (refusal, _) = err.refusal("@1");
        assert_eq!(refusal.code, code);
        if matches!(reply, Reply::Unknown | Reply::Disconnected) {
            let document = phux_client::contract::error_document(&refusal, 1);
            let recovery = &document["error"]["recovery"];
            assert_eq!(recovery["effect_certainty"], "unknown");
            assert_eq!(recovery["operation_id"].as_str().unwrap().len(), 32);
            assert_eq!(
                recovery["scope"]["id"],
                AuthorityId::new([0x55; 16]).unwrap().to_string()
            );
        }
        mock.join().expect("mock server");
    }
}
