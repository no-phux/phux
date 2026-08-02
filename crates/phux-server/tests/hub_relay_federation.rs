//! Loopback federation harness for the hub frame relay (phux-v45.4,
//! ADR-0007 §4).
//!
//! Two real `ServerRuntime`s run in one process: a *satellite* listening
//! on a loopback WebSocket, and a *hub* whose `[[satellites]]` registry
//! points at it. A consumer speaks the ordinary wire protocol to the hub
//! over UDS and addresses the satellite's terminal as
//! `TerminalId::Satellite { host: "sat", id }`:
//!
//! * `command_round_trip_and_stream_retagging` — a satellite-tagged
//!   `GET_SCREEN` round-trips through the hub (outbound id rewrite +
//!   response correlation); a satellite-scoped `SUBSCRIBE_EVENTS` plus a
//!   relayed `REPORT_ASKED` proves the return leg re-tags
//!   `Local -> Satellite { host, id }`; killing the satellite then
//!   delivers the typed `ERROR { SatelliteUnreachable }` teardown
//!   notification and subsequent commands fail fast with the same code.
//! * `down_satellite_fails_fast_with_typed_error` — a registry entry
//!   pointing at a dead port yields `SatelliteUnreachable`, promptly.
//! * `unknown_satellite_host_is_unsupported_route` — a host absent from
//!   the hub table (and any satellite id on a non-hub server) yields
//!   `UnsupportedSatelliteRoute`.
//! * `two_hop_attach_snapshot_output_input_ack_and_detach` (phux-v45.7) —
//!   interactive attach to a satellite terminal through the hub:
//!   `ATTACH_TERMINAL` relays, the authoritative `TERMINAL_SNAPSHOT`
//!   arrives (re-tagged, before any output delta), `INPUT_KEY` echoes
//!   back as re-tagged `TERMINAL_OUTPUT` from the satellite's PTY,
//!   `FRAME_ACK` relays without stalling the stream, and
//!   `DETACH_TERMINAL` + re-attach cycle cleanly.
//! * `satellite_input_lease_excludes_other_hub_consumers` (phux-v45.7) —
//!   the hub-side lease ledger: consumer A's cooperative `ACQUIRE_INPUT`
//!   over a satellite terminal excludes consumer B's `ACQUIRE_INPUT` /
//!   `ROUTE_INPUT`, and B's `RELEASE_INPUT` cannot release A's lease —
//!   even though both share the link's identity on the satellite.
//! * `satellite_seize_takeover_notifies_evicted_hub_consumer`
//!   (phux-v45.13) — a SEIZE takeover over a satellite terminal preempts
//!   the prior hub holder AND notifies it (a re-tagged
//!   `TerminalControl(Seized)` event), then the evicted holder's input is
//!   lease-gated; the satellite cannot notify (shared link identity).
//! * `acquire_and_release_input_off_hub_are_unsupported_routes`
//!   (phux-v45.11 finding 5) — satellite-tagged supervisory verbs on a
//!   non-hub server resolve through the shared routing path, not dead
//!   per-handler guards.
//! * `ssh_stub_link_relays_commands_end_to_end` — the same relay over
//!   the SSH-stdio dial path (phux-v45.9): the hub's registry entry is
//!   `ssh://`, `$PHUX_SSH` points at a stub program (the documented
//!   operator seam), and the harness plays the remote bridge by
//!   splicing the stub child's stdio onto the satellite's real UDS —
//!   the exact splice `phux stdio-bridge` performs, proven against the
//!   real binary in `crates/phux/tests/stdio_bridge_e2e.rs`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{encode_frame, recv_typed, send_frame, wait_for_raw_socket, wait_for_socket};
use futures_util::{SinkExt, StreamExt};
use phux_config::SatelliteConfigEntry;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::{GroupId, SatelliteHost, TerminalId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind, SpawnError, SpawnResult,
};
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// Generous per-step deadline, mirroring `common::WIRE_RECV_TIMEOUT`'s
/// rationale (the hub link dials with backoff under full-parallel nextest).
const STEP_DEADLINE: Duration = Duration::from_secs(15);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn satellite_entry(name: &str, port: u16) -> SatelliteConfigEntry {
    SatelliteConfigEntry {
        name: name.to_owned(),
        endpoint: format!("ws://127.0.0.1:{port}"),
        enabled: true,
        token_file: None,
        cert_fingerprint: None,
    }
}

/// Spawn the satellite: a plain server with one seeded (no-PTY) session,
/// listening on loopback WebSocket in addition to its own UDS.
fn spawn_satellite(
    socket_path: PathBuf,
    ws_port: u16,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some("sat-session".to_owned()),
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .listen_ws(format!("127.0.0.1:{ws_port}").parse().unwrap())
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Spawn a satellite whose seeded pane is backed by a real PTY running
/// `/bin/cat` — the deterministic echo fixture the two-hop attach test
/// drives input through (mirrors `input_dispatch.rs`).
fn spawn_satellite_with_cat(
    socket_path: PathBuf,
    ws_port: u16,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some("sat-session".to_owned()),
        seed_with_pty: true,
        seed_command: Some(portable_pty::CommandBuilder::new("/bin/cat")),
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .listen_ws(format!("127.0.0.1:{ws_port}").parse().unwrap())
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Spawn the hub: UDS-only, no seeded session, dialing `satellites`.
fn spawn_hub(
    socket_path: PathBuf,
    satellites: Vec<SatelliteConfigEntry>,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    spawn_hub_with_session(socket_path, satellites, None)
}

/// Like [`spawn_hub`] but optionally pre-seeds a local (no-PTY) session,
/// so aggregation tests can see local AND satellite terminals.
fn spawn_hub_with_session(
    socket_path: PathBuf,
    satellites: Vec<SatelliteConfigEntry>,
    pre_seeded_session: Option<&str>,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: pre_seeded_session.map(str::to_owned),
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .hub(satellites)
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Learn the satellite's seeded pane id over a direct WebSocket client:
/// `GET_STATE` returns the snapshot whose focused pane is the seed.
async fn discover_satellite_pane(ws_port: u16) -> u32 {
    let addr = format!("127.0.0.1:{ws_port}");
    let url = format!("ws://{addr}/");
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut ws = loop {
        assert!(
            Instant::now() < deadline,
            "satellite WebSocket never became connectable"
        );
        if let Ok(tcp) = TcpStream::connect(&addr).await
            && let Ok((ws, _)) = tokio_tungstenite::client_async(&url, tcp).await
        {
            break ws;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    ws.send(Message::Binary(
        encode_frame(&FrameKind::Hello {
            client_name: "hub-relay-federation-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::default(),
        })
        .to_vec(),
    ))
    .await
    .unwrap();
    let get_state = FrameKind::Command {
        request_id: 900,
        command: Command::GetState {
            scope: phux_protocol::wire::frame::StateScope::Server,
        },
    };
    ws.send(Message::Binary(encode_frame(&get_state).to_vec()))
        .await
        .unwrap();
    let deadline = Instant::now() + STEP_DEADLINE;
    loop {
        assert!(Instant::now() < deadline, "GET_STATE reply never arrived");
        let Some(Ok(Message::Binary(data))) = ws.next().await else {
            continue;
        };
        let (frame, _) = FrameKind::decode(&data).expect("decode satellite frame");
        if let FrameKind::CommandResult {
            request_id: 900,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        } = frame
        {
            return snapshot
                .focused_pane
                .local_id()
                .expect("seeded pane is local on the satellite");
        }
    }
}

/// Issue one satellite-tagged `GET_SCREEN` through the hub and return the
/// result. `request_id` correlates the reply against interleaved frames.
async fn get_screen_via_hub(
    hub: &mut UnixStream,
    request_id: u32,
    terminal_id: TerminalId,
) -> CommandResult {
    send_frame(
        hub,
        &FrameKind::Command {
            request_id,
            command: Command::GetScreen {
                terminal_id,
                request_scrollback: None,
                cells: false,
            },
        },
    )
    .await;
    loop {
        let (_, frame) = recv_typed(hub).await;
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
}

/// Retry `GET_SCREEN` through the hub until the link connects and the
/// command succeeds (the dialer backs off while the satellite boots).
async fn get_screen_until_ok(hub: &mut UnixStream, sat_pane: u32) -> CommandResult {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut request_id = 1000;
    loop {
        let result =
            get_screen_via_hub(hub, request_id, TerminalId::satellite("sat", sat_pane)).await;
        match result {
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            } if Instant::now() < deadline => {
                request_id += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => return other,
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linear federation scenario: boot both servers, relay a command, prove re-tagging, then observe teardown; splitting would re-boot the topology per step"
)]
fn command_round_trip_and_stream_retagging() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );

        // Learn the satellite's seeded pane id directly (LIST aggregation
        // through the hub is phux-v45.5; this test scopes to relay).
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = TerminalId::satellite("sat", sat_pane);

        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        // 1. Outbound leg + response correlation: a satellite-tagged
        //    GET_SCREEN round-trips once the link is up.
        let result = get_screen_until_ok(&mut hub, sat_pane).await;
        let CommandResult::OkWith(CommandValue::Json(json)) = result else {
            panic!("GET_SCREEN through the hub must succeed, got {result:?}");
        };
        assert!(
            json.contains("\"cols\""),
            "screen JSON came from the satellite pane: {json}"
        );

        // A satellite-tagged ROUTE_INPUT (the attach-free input path)
        // relays and acks Ok end-to-end.
        send_frame(
            &mut hub,
            &FrameKind::Command {
                request_id: 2000,
                command: Command::RouteInput {
                    terminal_id: sat_id.clone(),
                    event: phux_protocol::input::InputEvent::Focus(
                        phux_protocol::input::focus::FocusEvent::Gained,
                    ),
                },
            },
        )
        .await;
        loop {
            let (_, frame) = recv_typed(&mut hub).await;
            if let FrameKind::CommandResult {
                request_id: 2000,
                result,
            } = frame
            {
                assert_eq!(result, CommandResult::Ok, "ROUTE_INPUT relays Ok");
                break;
            }
        }

        // 2. Return leg re-tagging: subscribe to the satellite pane's
        //    events through the hub, trigger one via a relayed
        //    REPORT_ASKED, and require the EVENT to come back tagged
        //    Satellite { "sat", id }.
        send_frame(
            &mut hub,
            &FrameKind::SubscribeEvents {
                terminal: Some(sat_id.clone()),
            },
        )
        .await;
        send_frame(
            &mut hub,
            &FrameKind::Command {
                request_id: 2001,
                command: Command::ReportAsked {
                    terminal_id: sat_id.clone(),
                    id: "q-1".to_owned(),
                    question: "proceed?".to_owned(),
                    suggestions: vec!["yes".to_owned(), "no".to_owned()],
                    elapsed_seconds: Some(3),
                },
            },
        )
        .await;
        let mut saw_ack = false;
        let mut saw_event = false;
        while !(saw_ack && saw_event) {
            let (_, frame) = recv_typed(&mut hub).await;
            match frame {
                FrameKind::CommandResult {
                    request_id: 2001,
                    result,
                } => {
                    assert_eq!(result, CommandResult::Ok, "REPORT_ASKED relays Ok");
                    saw_ack = true;
                }
                FrameKind::Event { terminal, event } => {
                    assert_eq!(
                        terminal.as_ref(),
                        Some(&sat_id),
                        "return-leg events must be re-tagged Satellite {{ host, id }}"
                    );
                    assert!(
                        matches!(event, AgentEvent::Asked { ref id, .. } if id == "q-1"),
                        "expected the relayed Asked event, got {event:?}"
                    );
                    saw_event = true;
                }
                _ => {}
            }
        }

        // 3. Satellite disconnect: subscribed consumers get the typed
        //    teardown notification, not silence.
        drop(sat_shutdown);
        let deadline = Instant::now() + STEP_DEADLINE;
        loop {
            assert!(
                Instant::now() < deadline,
                "no SatelliteUnreachable teardown notification arrived"
            );
            let (_, frame) = recv_typed(&mut hub).await;
            if let FrameKind::Error {
                request_id: None,
                code: ErrorCode::SatelliteUnreachable,
                message,
            } = frame
            {
                assert!(
                    message.contains("sat"),
                    "teardown notification names the satellite: {message}"
                );
                break;
            }
        }
        sat_task.await.unwrap().unwrap();

        // 4. Commands to the now-dead satellite fail fast with the same
        //    typed code (bounded by recv_typed's own timeout — no hang).
        let result = get_screen_via_hub(&mut hub, 3000, sat_id).await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    ..
                }
            ),
            "command to a dead satellite must fail fast, got {result:?}"
        );

        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
    });
}

/// Issue one `GET_STATE { SERVER }` through the hub; return the correlated
/// result plus every un-correlated `ERROR` frame observed before it (the
/// per-satellite degradation indication, L1 §9.1 "Aggregated LIST").
async fn get_state_via_hub(
    hub: &mut UnixStream,
    request_id: u32,
) -> (CommandResult, Vec<(ErrorCode, String)>) {
    send_frame(
        hub,
        &FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: phux_protocol::wire::frame::StateScope::Server,
            },
        },
    )
    .await;
    let mut errors = Vec::new();
    loop {
        let (_, frame) = recv_typed(hub).await;
        match frame {
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => return (result, errors),
            FrameKind::Error {
                request_id: None,
                code,
                message,
            } => errors.push((code, message)),
            _ => {}
        }
    }
}

/// Issue one `SPAWN_TERMINAL` (optionally satellite-addressed) and return
/// the correlated `TERMINAL_SPAWNED.result`.
async fn spawn_via_stream(
    stream: &mut UnixStream,
    request_id: u32,
    satellite: Option<&str>,
    command: Option<Vec<String>>,
) -> SpawnResult {
    send_frame(
        stream,
        &FrameKind::SpawnTerminal {
            request_id,
            group: GroupId::new(1),
            command,
            cwd: None,
            env: None,
            term: None,
            satellite: satellite.map(SatelliteHost::new),
            owner_terminal: None,
            agent_session: None,
        },
    )
    .await;
    loop {
        let (_, frame) = recv_typed(stream).await;
        if let FrameKind::TerminalSpawned {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linear spawn-routing scenario: boot the topology once, spawn on the satellite, prove the returned id routes, then the typed refusals"
)]
fn satellite_targeted_spawn_round_trips_and_routes() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        // Wait for the satellite to be up (also proves it seeded its
        // session, which will host the relayed spawn).
        let _seed_pane = discover_satellite_pane(ws_port).await;
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        // 1. Spawn on the satellite through the hub, retrying while the
        //    dialer backs off. /bin/cat: a quiet, portable PTY child.
        let deadline = Instant::now() + STEP_DEADLINE;
        let mut request_id = 5000;
        let spawned_id = loop {
            let result = spawn_via_stream(
                &mut hub,
                request_id,
                Some("sat"),
                Some(vec!["/bin/cat".to_owned()]),
            )
            .await;
            match result {
                SpawnResult::Ok(id) => break id,
                SpawnResult::Err(SpawnError::SatelliteUnreachable(_))
                    if Instant::now() < deadline =>
                {
                    request_id += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => panic!("satellite spawn must succeed once the link is up: {other:?}"),
            }
        };
        // The returned id is re-tagged with the registry host.
        let TerminalId::Satellite { ref host, id } = spawned_id else {
            panic!("satellite spawn must return a Satellite-tagged id, got {spawned_id:?}");
        };
        assert_eq!(host.as_str(), "sat");

        // 2. The returned id routes through the hub immediately:
        //    GET_SCREEN on it round-trips to the satellite's new pane.
        let result = get_screen_via_hub(&mut hub, 6000, spawned_id.clone()).await;
        assert!(
            matches!(
                result,
                CommandResult::OkWith(CommandValue::Json(ref json)) if json.contains("\"cols\"")
            ),
            "GET_SCREEN on the spawned satellite pane must succeed, got {result:?}"
        );

        // 3. The aggregated LIST (phux-v45.5) now shows it too.
        let (result, _) = get_state_via_hub(&mut hub, 6001).await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
            panic!("GET_STATE must succeed, got {result:?}");
        };
        assert!(
            snapshot
                .panes
                .iter()
                .any(|p| p.id == TerminalId::satellite("sat", id)),
            "spawned satellite pane must appear in the aggregate: {snapshot:?}"
        );

        // 4. Unknown host on a hub: the typed configuration refusal.
        let result = spawn_via_stream(&mut hub, 6002, Some("nowhere"), None).await;
        assert!(
            matches!(
                result,
                SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute)
            ),
            "unknown satellite host must refuse with UnsupportedSatelliteRoute, got {result:?}"
        );

        drop(sat_shutdown);
        sat_task.await.unwrap().unwrap();

        // 5. Dead satellite: the typed fail-fast error, not a hang.
        let deadline = Instant::now() + STEP_DEADLINE;
        loop {
            let result = spawn_via_stream(&mut hub, 6003, Some("sat"), None).await;
            match result {
                SpawnResult::Err(SpawnError::SatelliteUnreachable(message)) => {
                    assert!(
                        message.contains("sat"),
                        "diagnostic names the host: {message}"
                    );
                    break;
                }
                // The link may not have observed the disconnect yet and
                // still relay into the closing connection; retry briefly.
                other => {
                    assert!(
                        Instant::now() < deadline,
                        "spawn to a dead satellite must fail with SatelliteUnreachable, got {other:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
    });
}

#[test]
fn non_hub_server_refuses_satellite_spawn() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (shutdown, task) = common::spawn_server(tmp.path().join("plain.sock"), Some("s"));
        let mut plain = wait_for_socket(&tmp.path().join("plain.sock"), STEP_DEADLINE).await;
        let result = spawn_via_stream(&mut plain, 1, Some("sat"), None).await;
        assert!(
            matches!(
                result,
                SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute)
            ),
            "non-hub server must refuse satellite spawns, got {result:?}"
        );
        drop(shutdown);
        task.await.unwrap().unwrap();
    });
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linear aggregation scenario over a three-server topology; splitting re-boots it per assertion"
)]
fn aggregated_list_merges_local_and_satellite_terminals_and_degrades() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        // Two registry entries: "sat" is live, "down" points at a port
        // nothing listens on — the degradation half of phux-v45.5.
        let dead_port = free_port();
        let (hub_shutdown, hub_task) = spawn_hub_with_session(
            tmp.path().join("hub.sock"),
            vec![
                satellite_entry("sat", ws_port),
                satellite_entry("down", dead_port),
            ],
            Some("hub-session"),
        );

        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = TerminalId::satellite("sat", sat_pane);
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        // Retry until the live link is up and its pane appears in the
        // aggregate (the dialer backs off while the satellite boots). The
        // dead satellite must never fail the LIST — every attempt returns
        // Ok, with "down" degrading to an un-correlated typed ERROR.
        let deadline = Instant::now() + STEP_DEADLINE;
        let mut request_id = 4000;
        let (snapshot, errors) = loop {
            let (result, errors) = get_state_via_hub(&mut hub, request_id).await;
            let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                panic!("aggregated GET_STATE must never fail, got {result:?}");
            };
            if snapshot.panes.iter().any(|p| p.id == sat_id) {
                break (snapshot, errors);
            }
            assert!(
                Instant::now() < deadline,
                "satellite pane never appeared in the aggregate: {snapshot:?}"
            );
            request_id += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        // Local terminals are present and stay Local-tagged.
        assert!(
            snapshot
                .panes
                .iter()
                .any(|p| matches!(p.id, TerminalId::Local { .. })),
            "hub's own seeded pane must stay in the aggregate: {snapshot:?}"
        );
        // The satellite pane is re-tagged and its shape relayed verbatim
        // (the satellite's seeded no-PTY pane is 80x24).
        let sat_info = snapshot
            .panes
            .iter()
            .find(|p| p.id == sat_id)
            .expect("satellite pane in aggregate");
        assert_eq!(
            (sat_info.cols, sat_info.rows),
            (80, 24),
            "satellite pane dims relayed verbatim"
        );
        // The dead satellite contributes nothing...
        assert!(
            !snapshot
                .panes
                .iter()
                .any(|p| p.id.host().is_some_and(|h| h.as_str() == "down")),
            "dead satellite must contribute no panes: {snapshot:?}"
        );
        // ...but degrades observably: one un-correlated typed ERROR naming
        // the host arrived before the COMMAND_RESULT.
        assert!(
            errors
                .iter()
                .any(|(code, message)| *code == ErrorCode::SatelliteUnreachable
                    && message.contains("down")),
            "expected a SatelliteUnreachable degradation notice for 'down', got {errors:?}"
        );
        // Satellite sessions/windows are NOT merged (their ids are not
        // federation-routable): the only session is the hub's own.
        assert_eq!(
            snapshot
                .sessions
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["hub-session"],
            "satellite sessions must not merge into the hub snapshot"
        );

        drop(sat_shutdown);
        sat_task.await.unwrap().unwrap();
        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
    });
}

/// Create a FIFO at `path` (unix-only, like the UDS transport itself).
fn mkfifo(path: &std::path::Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo {} failed", path.display());
}

/// Write the `$PHUX_SSH` stub: a shell-free-argv program the hub spawns
/// in place of the system `ssh`. It pipes the child's stdin into the
/// `c2s` FIFO and the `s2c` FIFO out its stdout, so the harness can play
/// the remote end of the transport in-process. The argv the hub built
/// (`-o BatchMode=yes ... -- HOST phux stdio-bridge`) is ignored, exactly
/// as an operator wrapper is free to do; the argv itself is asserted in
/// `phux-server::hub::link`'s stub tests.
fn write_ssh_stub(dir: &std::path::Path, c2s: &std::path::Path, s2c: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-ssh");
    // `exec 3<&0` first: a background job's stdin is reassigned to
    // /dev/null before its own redirections run (POSIX sh), so the
    // child's real stdin must be duplicated onto another fd for the
    // background cat to read it.
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nexec 3<&0\ncat <&3 > {c2s} &\nexec cat {s2c}\n",
            c2s = c2s.display(),
            s2c = s2c.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Play the remote bridge: splice the stub ssh child's stdio (via the
/// FIFO pair) onto the satellite server's real Unix socket, byte for
/// byte — the job `phux stdio-bridge` does on a real satellite host.
/// Ends when either side closes (satellite shutdown, or the hub tearing
/// the child down).
async fn run_stub_bridge(c2s: PathBuf, s2c: PathBuf, sat_sock: PathBuf) {
    use tokio::net::unix::pipe;
    // Read end first: opening a FIFO receiver never blocks. The phantom
    // sender keeps the receiver from seeing EOF before (or between) stub
    // children have the write end open.
    let mut from_hub = pipe::OpenOptions::new().open_receiver(&c2s).unwrap();
    let _hold_open = pipe::OpenOptions::new().open_sender(&c2s).unwrap();
    // A FIFO sender cannot open until the stub's `cat` holds the read
    // end; retry until the child the hub spawned is up.
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut to_hub = loop {
        match pipe::OpenOptions::new().open_sender(&s2c) {
            Ok(tx) => break tx,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(err) => panic!("ssh stub never opened its stdout FIFO: {err}"),
        }
    };
    let satellite = wait_for_raw_socket(&sat_sock, STEP_DEADLINE).await;
    let (mut sat_rd, mut sat_wr) = satellite.into_split();
    // One finished direction ends the bridge, mirroring `phux
    // stdio-bridge`: the transport is gone either way and the hub's
    // redial loop owns recovery.
    tokio::select! {
        _ = tokio::io::copy(&mut from_hub, &mut sat_wr) => {}
        _ = tokio::io::copy(&mut sat_rd, &mut to_hub) => {}
    }
}

#[test]
fn ssh_stub_link_relays_commands_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let c2s = tmp.path().join("c2s.fifo");
    let s2c = tmp.path().join("s2c.fifo");
    mkfifo(&c2s);
    mkfifo(&s2c);
    let stub = write_ssh_stub(tmp.path(), &c2s, &s2c);
    // SAFETY: nextest runs each test in its own process and no other
    // thread exists yet — the runtime is built inside run_local below.
    unsafe { std::env::set_var("PHUX_SSH", &stub) };

    common::run_local(async move {
        let ws_port = free_port();
        let sat_sock = tmp.path().join("sat.sock");
        let (sat_shutdown, sat_task) = spawn_satellite(sat_sock.clone(), ws_port);
        let bridge = tokio::task::spawn_local(run_stub_bridge(c2s, s2c, sat_sock));

        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![SatelliteConfigEntry {
                name: "sat".to_owned(),
                // Validated ssh authority; the stub ignores the host, an
                // operator wrapper's prerogative.
                endpoint: "ssh://sat-host".to_owned(),
                enabled: true,
                token_file: None,
                cert_fingerprint: None,
            }],
        );

        // Discovery goes over the satellite's own WS listener — only the
        // hub-to-satellite leg under test rides the bridged ssh stdio.
        let sat_pane = discover_satellite_pane(ws_port).await;
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        // A satellite-tagged command round-trips through the hub over
        // the ssh child's pipes: spawn, length-prefixed framing on
        // stdin/stdout, relay id rewrite, response correlation.
        let result = get_screen_until_ok(&mut hub, sat_pane).await;
        let CommandResult::OkWith(CommandValue::Json(json)) = result else {
            panic!("GET_SCREEN over the ssh link must succeed, got {result:?}");
        };
        assert!(
            json.contains("\"cols\""),
            "screen JSON came from the satellite pane: {json}"
        );

        // Remove the stub before killing the satellite so every redial
        // fails at spawn — post-teardown commands then always fail fast
        // (backoff/connecting drains) instead of racing a fresh child
        // that has no bridge behind it into the relay's 30s deadline.
        std::fs::remove_file(&stub).unwrap();

        // Satellite death closes the UDS, the bridge ends, the stub's
        // cat sees EOF, the child exits: the hub must treat the exited
        // ssh child as an ordinary dropped link and type the failure.
        drop(sat_shutdown);
        sat_task.await.unwrap().unwrap();
        let deadline = Instant::now() + STEP_DEADLINE;
        let mut request_id = 5000;
        loop {
            assert!(
                Instant::now() < deadline,
                "commands to the dead ssh satellite never failed with SatelliteUnreachable"
            );
            let result =
                get_screen_via_hub(&mut hub, request_id, TerminalId::satellite("sat", sat_pane))
                    .await;
            if matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    ..
                }
            ) {
                break;
            }
            request_id += 1;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
        // The bridge ended with the satellite; reap the task.
        let _ = bridge.await;
    });
}

#[test]
fn down_satellite_fails_fast_with_typed_error() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        // A registry entry pointing at a port nothing listens on: the
        // link supervisor dials and backs off forever.
        let dead_port = free_port();
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", dead_port)],
        );
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        let started = Instant::now();
        let result = get_screen_via_hub(&mut hub, 1, TerminalId::satellite("sat", 1)).await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    ..
                }
            ),
            "unreachable satellite must produce a typed error, got {result:?}"
        );
        assert!(
            started.elapsed() < STEP_DEADLINE,
            "the error must be fail-fast, not a hang"
        );

        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
    });
}

/// Send `command` through `hub` and await the correlated result,
/// collecting every satellite-tagged `TERMINAL_SNAPSHOT` /
/// `TERMINAL_OUTPUT` frame that interleaves before it (SPEC §5 allows
/// command-triggered stream frames to precede `COMMAND_RESULT`).
async fn command_via_hub(
    hub: &mut UnixStream,
    request_id: u32,
    command: Command,
) -> (CommandResult, Vec<FrameKind>) {
    send_frame(
        hub,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    let mut interleaved = Vec::new();
    loop {
        let (_, frame) = recv_typed(hub).await;
        match frame {
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => return (result, interleaved),
            other => interleaved.push(other),
        }
    }
}

/// Keep issuing `ATTACH_TERMINAL` for the satellite pane until the link
/// is up and the command succeeds; returns the frames that interleaved
/// before the successful `Ok`.
async fn attach_terminal_until_ok(hub: &mut UnixStream, sat_id: &TerminalId) -> Vec<FrameKind> {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut request_id = 5000;
    loop {
        let (result, frames) = command_via_hub(
            hub,
            request_id,
            Command::AttachTerminal {
                terminal_id: sat_id.clone(),
            },
        )
        .await;
        match result {
            CommandResult::Ok => return frames,
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            } if Instant::now() < deadline => {
                request_id += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => panic!("ATTACH_TERMINAL through the hub must succeed, got {other:?}"),
        }
    }
}

/// Send one ASCII key + Enter to `sat_id` over `hub` (cooked-mode PTYs
/// are line-buffered, so the Enter flushes `cat`'s echo).
async fn send_key_and_enter(hub: &mut UnixStream, sat_id: &TerminalId, c: char) {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
    let key = match c {
        'a' => PhysicalKey::A,
        'b' => PhysicalKey::B,
        _ => panic!("extend the fixture map for {c:?}"),
    };
    send_frame(
        hub,
        &FrameKind::InputKey {
            terminal_id: sat_id.clone(),
            event: KeyEvent {
                action: KeyAction::Press,
                key,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: Some(c.to_string()),
                unshifted_codepoint: Some(c as u32),
            },
        },
    )
    .await;
    send_frame(
        hub,
        &FrameKind::InputKey {
            terminal_id: sat_id.clone(),
            event: KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::Enter,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: None,
                unshifted_codepoint: None,
            },
        },
    )
    .await;
}

/// Drain re-tagged `TERMINAL_OUTPUT` frames for `sat_id` until `needle`
/// appears, returning the stream/generation identity and last `seq`.
async fn await_satellite_echo(
    hub: &mut UnixStream,
    sat_id: &TerminalId,
    needle: u8,
) -> (
    phux_protocol::ids::StreamId,
    phux_protocol::ids::BootstrapId,
    u64,
) {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = Instant::now() + STEP_DEADLINE;
    while Instant::now() < deadline {
        let (_, frame) = recv_typed(hub).await;
        if let FrameKind::TerminalOutput {
            terminal_id,
            seq,
            stream_id,
            bootstrap_id,
            bytes,
        } = frame
        {
            assert_eq!(
                &terminal_id, sat_id,
                "two-hop output must be re-tagged Satellite {{ host, id }}"
            );
            acc.extend_from_slice(&bytes);
            if acc.contains(&needle) {
                return (stream_id, bootstrap_id, seq);
            }
        }
    }
    panic!(
        "echo byte {:?} never arrived through the hub; got {:?}",
        needle as char,
        String::from_utf8_lossy(&acc)
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linear two-hop attach scenario: boot the topology once, then walk attach -> snapshot -> input echo -> ack -> detach -> re-attach in order; splitting re-boots both servers per step"
)]
fn two_hop_attach_snapshot_output_input_ack_and_detach() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) =
            spawn_satellite_with_cat(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = TerminalId::satellite("sat", sat_pane);
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        // 1. ATTACH_TERMINAL relays through the hub, and the ADR-0007 §4
        //    snapshot-on-attach invariant holds across both hops: an
        //    authoritative TERMINAL_SNAPSHOT — re-tagged to the id the
        //    consumer used, with the satellite as the byte authority —
        //    arrives, and no TERMINAL_OUTPUT delta precedes it.
        let frames = attach_terminal_until_ok(&mut hub, &sat_id).await;
        let snapshot_pos = frames
            .iter()
            .position(|f| matches!(f, FrameKind::BootstrapBegin { .. }))
            .expect("ATTACH_TERMINAL must deliver a TERMINAL_SNAPSHOT");
        if let FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            ..
        } = &frames[snapshot_pos]
        {
            assert_eq!(
                terminal_id, &sat_id,
                "snapshot must be re-tagged Satellite {{ host, id }}"
            );
            assert!(*cols > 0 && *rows > 0, "snapshot carries real dims");
        }
        assert!(
            !frames[..snapshot_pos]
                .iter()
                .any(|f| matches!(f, FrameKind::TerminalOutput { .. })),
            "no output delta may precede the attach snapshot"
        );

        // 2. Interactive input over two hops: INPUT_KEY frames relayed
        //    over the link pass the satellite's subscription gate (the
        //    link consumer holds an ATTACH_TERMINAL subscription) and
        //    `cat` echoes back as re-tagged TERMINAL_OUTPUT.
        send_key_and_enter(&mut hub, &sat_id, 'a').await;
        let (stream_id, bootstrap_id, seq) = await_satellite_echo(&mut hub, &sat_id, b'a').await;

        // 3. FRAME_ACK relays without deadlocking or killing the stream
        //    (ADR-0018 flow control across the extra hop): ack what we
        //    saw, then prove the stream still flows.
        send_frame(
            &mut hub,
            &FrameKind::FrameAck {
                terminal_id: sat_id.clone(),
                stream_id,
                bootstrap_id,
                seq,
            },
        )
        .await;
        send_key_and_enter(&mut hub, &sat_id, 'b').await;
        let _ = await_satellite_echo(&mut hub, &sat_id, b'b').await;

        // 4. DETACH_TERMINAL resolves hub-side (idempotent Ok), and a
        //    re-attach delivers a fresh snapshot — the lifecycle cycles.
        let (result, _) = command_via_hub(
            &mut hub,
            7000,
            Command::DetachTerminal {
                terminal_id: sat_id.clone(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok, "DETACH_TERMINAL acks Ok");
        let frames = attach_terminal_until_ok(&mut hub, &sat_id).await;
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, FrameKind::BootstrapBegin { .. })),
            "re-attach after detach must deliver a fresh snapshot"
        );

        drop(sat_shutdown);
        drop(hub_shutdown);
        let _ = sat_task.await.unwrap();
        hub_task.await.unwrap().unwrap();
    });
}

#[test]
fn satellite_input_lease_excludes_other_hub_consumers() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = TerminalId::satellite("sat", sat_pane);
        let mut a = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let mut b = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        // Wait for the link (any relayed command succeeding proves it).
        let _ = get_screen_until_ok(&mut a, sat_pane).await;

        let acquire = |terminal_id: TerminalId| Command::AcquireInput {
            terminal_id,
            mode: phux_protocol::wire::frame::InputMode::Cooperative,
            ttl_ms: 0,
        };

        // A takes the wheel over the satellite pane, through the hub.
        let (result, _) = command_via_hub(&mut a, 100, acquire(sat_id.clone())).await;
        assert_eq!(result, CommandResult::Ok, "A's cooperative acquire wins");

        // B — a different hub consumer sharing the same link identity on
        // the satellite — must be excluded by the hub's lease ledger:
        // cooperative acquire and ROUTE_INPUT both refuse with
        // InputLeaseHeld (phux-v45.7, L1 §9.1).
        let (result, _) = command_via_hub(&mut b, 200, acquire(sat_id.clone())).await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::InputLeaseHeld,
                    ..
                }
            ),
            "B's cooperative acquire must lose to A, got {result:?}"
        );
        let (result, _) = command_via_hub(
            &mut b,
            201,
            Command::RouteInput {
                terminal_id: sat_id.clone(),
                event: phux_protocol::input::InputEvent::Focus(
                    phux_protocol::input::focus::FocusEvent::Gained,
                ),
            },
        )
        .await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::InputLeaseHeld,
                    ..
                }
            ),
            "B's ROUTE_INPUT must be lease-gated, got {result:?}"
        );

        // B's RELEASE_INPUT is the idempotent no-op Ok — and must NOT
        // release A's lease (the aliasing bug this ledger fixes).
        let (result, _) = command_via_hub(
            &mut b,
            202,
            Command::ReleaseInput {
                terminal_id: sat_id.clone(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok, "non-holder release is a no-op");
        let (result, _) = command_via_hub(&mut b, 203, acquire(sat_id.clone())).await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::InputLeaseHeld,
                    ..
                }
            ),
            "A's lease must survive B's release, got {result:?}"
        );

        // A releases; now B acquires.
        let (result, _) = command_via_hub(
            &mut a,
            101,
            Command::ReleaseInput {
                terminal_id: sat_id.clone(),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok, "holder release succeeds");
        let (result, _) = command_via_hub(&mut b, 204, acquire(sat_id)).await;
        assert_eq!(result, CommandResult::Ok, "freed lease grants to B");

        drop(sat_shutdown);
        drop(hub_shutdown);
        let _ = sat_task.await.unwrap();
        hub_task.await.unwrap().unwrap();
    });
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linear takeover scenario: boot the topology once, acquire, seize, observe the eviction notification and the lost wheel; splitting re-boots both servers"
)]
fn satellite_seize_takeover_notifies_evicted_hub_consumer() {
    // phux-v45.13, L1 §9.1: a SEIZE takeover over a satellite terminal must
    // notify the evicted hub consumer — the satellite cannot, because every
    // hub consumer shares its link identity, so the lease change reads there
    // as a same-identity re-acquire. Mirrors the local takeover broadcast.
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = TerminalId::satellite("sat", sat_pane);
        let mut a = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let mut b = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        // Wait for the link (any relayed command succeeding proves it).
        let _ = get_screen_until_ok(&mut a, sat_pane).await;

        // A takes the wheel cooperatively over the satellite pane.
        let (result, _) = command_via_hub(
            &mut a,
            100,
            Command::AcquireInput {
                terminal_id: sat_id.clone(),
                mode: phux_protocol::wire::frame::InputMode::Cooperative,
                ttl_ms: 0,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok, "A's cooperative acquire wins");

        // B — a different hub consumer sharing the link identity — SEIZES
        // it. A takeover preempts (unlike a cooperative acquire, which would
        // refuse), so the hub records B as holder AND notifies the evicted A.
        let (result, _) = command_via_hub(
            &mut b,
            200,
            Command::AcquireInput {
                terminal_id: sat_id.clone(),
                mode: phux_protocol::wire::frame::InputMode::Seize,
                ttl_ms: 0,
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok, "B's SEIZE takeover preempts A");

        // A receives the hub-synthesized TerminalControl(Seized) eviction
        // event, re-tagged with the satellite id.
        let deadline = Instant::now() + STEP_DEADLINE;
        loop {
            assert!(
                Instant::now() < deadline,
                "evicted holder A never received a SEIZE eviction notification"
            );
            let (_, frame) = recv_typed(&mut a).await;
            if let FrameKind::Event { terminal, event } = frame {
                assert_eq!(
                    terminal.as_ref(),
                    Some(&sat_id),
                    "the eviction event must be re-tagged Satellite {{ host, id }}"
                );
                if let AgentEvent::TerminalControl {
                    action,
                    input_holder,
                    ..
                } = event
                {
                    assert_eq!(
                        action,
                        phux_protocol::wire::frame::ControlAction::Seized,
                        "the eviction event names the takeover action"
                    );
                    assert!(
                        input_holder.is_some(),
                        "the eviction event names the new holder"
                    );
                    break;
                }
            }
        }

        // And A has genuinely lost the wheel: its ROUTE_INPUT now refuses at
        // the hub lease gate (it was silently dropped before this fix).
        let (result, _) = command_via_hub(
            &mut a,
            101,
            Command::RouteInput {
                terminal_id: sat_id.clone(),
                event: phux_protocol::input::InputEvent::Focus(
                    phux_protocol::input::focus::FocusEvent::Gained,
                ),
            },
        )
        .await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::InputLeaseHeld,
                    ..
                }
            ),
            "the evicted holder's input must now be lease-gated, got {result:?}"
        );

        drop(sat_shutdown);
        drop(hub_shutdown);
        let _ = sat_task.await.unwrap();
        hub_task.await.unwrap().unwrap();
    });
}

#[test]
fn acquire_and_release_input_off_hub_are_unsupported_routes() {
    // phux-v45.11 finding 5: the per-handler satellite guards in
    // handle_acquire_input / handle_release_input were dead code — the
    // shared route interception owns that dispatch. This pins the wire
    // behavior their removal must preserve.
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (shutdown, task) = common::spawn_server(tmp.path().join("plain.sock"), Some("s"));
        let mut plain = wait_for_socket(&tmp.path().join("plain.sock"), STEP_DEADLINE).await;
        for (request_id, command) in [
            (
                1,
                Command::AcquireInput {
                    terminal_id: TerminalId::satellite("sat", 1),
                    mode: phux_protocol::wire::frame::InputMode::Cooperative,
                    ttl_ms: 0,
                },
            ),
            (
                2,
                Command::ReleaseInput {
                    terminal_id: TerminalId::satellite("sat", 1),
                },
            ),
        ] {
            let (result, _) = command_via_hub(&mut plain, request_id, command).await;
            assert!(
                matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::UnsupportedSatelliteRoute,
                        ..
                    }
                ),
                "satellite-tagged supervisory verbs must refuse off-hub, got {result:?}"
            );
        }
        drop(shutdown);
        task.await.unwrap().unwrap();
    });
}

#[test]
fn unknown_satellite_host_is_unsupported_route() {
    common::run_local(async {
        let tmp = TempDir::new().unwrap();
        // Hub with a registry for "sat" only: host "nowhere" has no route.
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", free_port())],
        );
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let result = get_screen_via_hub(&mut hub, 1, TerminalId::satellite("nowhere", 1)).await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::UnsupportedSatelliteRoute,
                    ..
                }
            ),
            "unknown host must be UnsupportedSatelliteRoute, got {result:?}"
        );
        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();

        // And a plain non-hub server refuses every satellite id the same
        // way — the pre-relay contract is unchanged off-hub.
        let (shutdown, task) = common::spawn_server(tmp.path().join("plain.sock"), Some("s"));
        let mut plain = wait_for_socket(&tmp.path().join("plain.sock"), STEP_DEADLINE).await;
        let result = get_screen_via_hub(&mut plain, 2, TerminalId::satellite("sat", 1)).await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::UnsupportedSatelliteRoute,
                    ..
                }
            ),
            "non-hub server must refuse satellite routes, got {result:?}"
        );
        drop(shutdown);
        task.await.unwrap().unwrap();
    });
}
