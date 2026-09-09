//! `DETACH_RESOURCE` retires any subscription source before acknowledging it.

#![allow(
    clippy::future_not_send,
    reason = "Screen owns libghostty state; every scenario runs on a LocalSet"
)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapStreamProfile, ClientCapabilities, EngineCodec,
    EngineFeatureSet, OutputMode,
};
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::input::{
    InputEvent,
    paste::{PasteEvent, PasteTrust},
};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SpawnResult, StateScope,
};
use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed,
    run_local, send_frame, spawn_server_with_seed_cmd, wait_for_raw_socket, wait_for_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

#[derive(Clone, Copy, Debug)]
enum Source {
    Session,
    Spawn,
    Explicit,
}

async fn command(
    stream: &mut UnixStream,
    request_id: u32,
    command: Command,
    quiet: bool,
) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            let (_, frame) = recv_typed(stream).await;
            if quiet {
                assert_no_content(&frame);
            }
            if let FrameKind::CommandResult {
                request_id: got,
                result,
            } = frame
                && got == request_id
            {
                return result;
            }
        }
    })
    .await
    .expect("correlated command reply")
}

fn assert_no_content(frame: &FrameKind) {
    assert!(
        !matches!(
            frame,
            FrameKind::ResourceOutput { .. }
                | FrameKind::BootstrapBegin { .. }
                | FrameKind::BootstrapChunk { .. }
                | FrameKind::BootstrapReady { .. }
                | FrameKind::BootstrapTombstone { .. }
                | FrameKind::HistoryTombstone { .. }
                | FrameKind::HistoryPage { .. }
                | FrameKind::HistoryRejected { .. }
                | FrameKind::ResourceClosed { .. }
        ),
        "unsolicited terminal frame after DETACH_RESOURCE success: {frame:?}"
    );
}

async fn attach_session(stream: &mut UnixStream) -> ResourceId {
    send_frame(stream, &attach_by_name("detach")).await;
    let mut pane = None;
    timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            match recv_typed(stream).await.1 {
                FrameKind::Attached { snapshot, .. } => pane = Some(snapshot.focused_resource),
                FrameKind::AttachReady { .. } => return pane.take().expect("attached pane"),
                _ => {}
            }
        }
    })
    .await
    .expect("session ready")
}

async fn spawn_pane(stream: &mut UnixStream, request_id: u32) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(vec!["/bin/cat".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: Some((80, 24)),
            resource: None,
        },
    )
    .await;
    let mut pane = None;
    timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            match recv_typed(stream).await.1 {
                FrameKind::ResourceSpawned {
                    result: SpawnResult::Ok(id),
                    ..
                } => pane = Some(id),
                FrameKind::BootstrapReady { terminal_id, .. }
                    if pane.as_ref() == Some(&terminal_id) =>
                {
                    return terminal_id;
                }
                FrameKind::ResourceSpawned { result, .. } => panic!("spawn failed: {result:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("spawn ready")
}

async fn explicit_attach(
    stream: &mut UnixStream,
    pane: &ResourceId,
    request_id: u32,
) -> Option<Screen> {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::AttachResource {
                terminal_id: pane.clone(),
            },
        },
    )
    .await;
    let mut screen = Some(Screen::new(80, 24).unwrap());
    let mut bytes = 0;
    let mut ready = false;
    timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            match recv_typed(stream).await.1 {
                FrameKind::BootstrapBegin {
                    profile: BootstrapStreamProfile::NativeState { .. },
                    ..
                } => screen = None,
                FrameKind::BootstrapChunk { payload, .. } => {
                    bytes += payload.len();
                    if let Some(screen) = screen.as_mut() {
                        screen.write(&payload);
                    }
                }
                FrameKind::BootstrapReady { .. } => ready = true,
                FrameKind::CommandResult {
                    request_id: got,
                    result,
                } if got == request_id => {
                    assert_eq!(result, CommandResult::Ok);
                    assert!(ready, "bootstrap must precede attach success");
                    assert!(bytes > 0, "reattach must publish a populated bootstrap");
                    return;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("terminal attach reply");
    screen
}

async fn write_and_observe(control: &mut UnixStream, pane: &ResourceId, marker: &str) {
    assert_eq!(
        command(
            control,
            800,
            Command::RouteInput {
                terminal_id: pane.clone(),
                event: InputEvent::Paste(PasteEvent {
                    data: format!("{marker}\n").into_bytes(),
                    trust: PasteTrust::Trusted,
                }),
            },
            false
        )
        .await,
        CommandResult::Ok
    );
    timeout(WIRE_RECV_TIMEOUT, async {
        loop {
            let result = command(
                control,
                801,
                Command::GetScreen {
                    terminal_id: pane.clone(),
                    request_scrollback: None,
                    cells: false,
                },
                false,
            )
            .await;
            let CommandResult::OkWith(CommandValue::Json(json)) = result else {
                panic!("terminal must survive detach: {result:?}");
            };
            if json.contains(marker) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("PTY output reached live terminal after detach");
}

async fn detach_and_check(
    stream: &mut UnixStream,
    control: &mut UnixStream,
    pane: &ResourceId,
    marker: &str,
) {
    assert_eq!(
        command(
            stream,
            900,
            Command::DetachResource {
                terminal_id: pane.clone()
            },
            false
        )
        .await,
        CommandResult::Ok
    );
    write_and_observe(control, pane, marker).await;
    command(
        stream,
        901,
        Command::GetState {
            scope: StateScope::Server,
        },
        true,
    )
    .await;
    // Output has positively reached the live terminal. Observe the client
    // mailbox too, including output that a previously scheduled pump queues
    // behind the state reply. No terminal process is killed to create silence.
    if let Ok((_, frame)) = timeout(Duration::from_millis(100), recv_typed(stream)).await {
        assert_no_content(&frame);
        panic!("unexpected unsolicited frame: {frame:?}");
    }
}

async fn connect(path: &std::path::Path, caps: ClientCapabilities) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "detach-output-fence".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: caps,
        },
    )
    .await;
    assert!(matches!(
        recv_typed(&mut stream).await.1,
        FrameKind::HelloOk { .. }
    ));
    stream
}

async fn scenario(source: Source, rounds: u32, caps: ClientCapabilities) {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("server.sock");
    let (shutdown, task) = spawn_server_with_seed_cmd(
        path.clone(),
        "detach",
        portable_pty::CommandBuilder::new("/bin/cat"),
    );
    let mut stream = connect(&path, caps).await;
    let mut control = wait_for_socket(&path, SOCKET_CONNECT_DEADLINE).await;
    let seed = match source {
        Source::Session | Source::Spawn => attach_session(&mut stream).await,
        Source::Explicit => {
            let CommandResult::OkWith(CommandValue::State(state)) = command(
                &mut control,
                1,
                Command::GetState {
                    scope: StateScope::Server,
                },
                false,
            )
            .await
            else {
                panic!("state");
            };
            explicit_attach(&mut stream, &state.focused_resource, 2).await;
            state.focused_resource
        }
    };
    if matches!(source, Source::Spawn) {
        assert_eq!(
            command(
                &mut stream,
                3,
                Command::DetachResource {
                    terminal_id: seed.clone()
                },
                false
            )
            .await,
            CommandResult::Ok
        );
    }
    for round in 0..rounds {
        let pane = match source {
            Source::Spawn => spawn_pane(&mut stream, 10 + round).await,
            _ => seed.clone(),
        };
        let marker = format!("DETACHED_{round}");
        detach_and_check(&mut stream, &mut control, &pane, &marker).await;
        if let Some(mut screen) = explicit_attach(&mut stream, &pane, 100 + round).await {
            assert!(
                screen.contains(&marker),
                "reattach must reconstruct output emitted while detached"
            );
        }
        if matches!(source, Source::Spawn) {
            detach_and_check(&mut stream, &mut control, &pane, &format!("AGAIN_{round}")).await;
        }
    }
    drop(stream);
    drop(control);
    shutdown.send(()).unwrap();
    timeout(SERVER_JOIN_DEADLINE, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[test]
fn detach_terminal_stops_initial_attach_output() {
    run_local(scenario(Source::Session, 1, ClientCapabilities::new()));
}

#[test]
fn detach_terminal_stops_spawn_output_and_churns_past_sixteen() {
    run_local(scenario(Source::Spawn, 20, ClientCapabilities::new()));
}

#[test]
fn detach_terminal_stops_explicit_attach_output_and_reattaches() {
    run_local(scenario(Source::Explicit, 20, ClientCapabilities::new()));
}

#[test]
fn detach_terminal_stops_state_sync_actor_output() {
    run_local(scenario(
        Source::Session,
        20,
        ClientCapabilities::new().with_output_mode(OutputMode::StateSync),
    ));
}

const fn native_caps() -> ClientCapabilities {
    ClientCapabilities::new().with_bootstrap(BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    ))
}

#[test]
fn detach_terminal_answers_a_late_history_request_with_cursor_status() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("server.sock");
        let (shutdown, task) = spawn_server_with_seed_cmd(
            path.clone(),
            "detach",
            portable_pty::CommandBuilder::new("/bin/cat"),
        );
        let mut stream = connect(&path, native_caps()).await;
        send_frame(&mut stream, &attach_by_name("detach")).await;
        let history = timeout(WIRE_RECV_TIMEOUT, async {
            loop {
                if let FrameKind::BootstrapReady {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    history_cursor,
                    ..
                } = recv_typed(&mut stream).await.1
                {
                    break FrameKind::HistoryRequest {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        cursor: history_cursor.expect("native cursor"),
                        max_bytes: 1024 * 1024,
                        max_rows: 512,
                    };
                }
            }
        })
        .await
        .unwrap();
        let FrameKind::HistoryRequest {
            ref terminal_id, ..
        } = history
        else {
            unreachable!()
        };
        assert_eq!(
            command(
                &mut stream,
                1,
                Command::DetachResource {
                    terminal_id: terminal_id.clone()
                },
                false
            )
            .await,
            CommandResult::Ok
        );
        // A request explicitly sent after detach still receives its legitimate
        // cursor status. Clients must stop automatic pagination while detach
        // is pending; cancellation fences producers, not future requests.
        send_frame(&mut stream, &history).await;
        assert!(matches!(
            recv_typed(&mut stream).await.1,
            FrameKind::HistoryTombstone {
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Stale,
                ..
            }
        ));
        command(
            &mut stream,
            2,
            Command::GetState {
                scope: StateScope::Server,
            },
            true,
        )
        .await;
        drop(stream);
        shutdown.send(()).unwrap();
        timeout(SERVER_JOIN_DEADLINE, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    });
}

#[test]
fn detach_terminal_stops_native_initial_attach_and_reattaches() {
    run_local(scenario(Source::Session, 20, native_caps()));
}

#[test]
fn detach_terminal_stops_native_spawn_and_churns_past_sixteen() {
    run_local(scenario(Source::Spawn, 20, native_caps()));
}
