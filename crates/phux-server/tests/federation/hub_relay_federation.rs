//! Loopback federation (ADR-0007 §4): a satellite `ServerRuntime` listening
//! on loopback WebSocket, and a hub whose `[[satellites]]` registry points at
//! it. Consumers speak the ordinary wire protocol to the hub over UDS and
//! address satellite terminals as `ResourceId::Satellite { host: "sat", id }`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use phux_config::SatelliteConfigEntry;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::{ResourceId, SatelliteHost};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::RemoteListenerTransport;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, ErrorCode, FrameKind,
    InputMode, Scope, SpawnError, SpawnResource, SpawnResult, StateScope,
};
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use phux_server_testkit::{
    Spawn, ascii_key, bound_listener_addr, encode_frame, recv_typed, send_frame, spawn_resource,
    wait_for_raw_socket, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

mod conditional_kill;
mod consumer_filters;
mod detach_fence;
mod event_restamp;
mod link_gap;
mod list_directory;
mod live_reload;
mod path_query;
mod retain_on_exit;
mod satellite_spawn;

/// Per-step hang guard (the hub link dials with backoff).
const STEP_DEADLINE: Duration = Duration::from_secs(15);

/// Hang guard for satellite WebSocket readiness; `listen_ws` is a
/// `spawn_local` task that can sit behind the scheduler under load.
const WS_CONNECT_HANG_GUARD: Duration = Duration::from_secs(60);

type ServerTask = JoinHandle<Result<(), ServerError>>;

/// A loopback port held for the test's lifetime by a listener that accepts
/// and immediately drops every connection. Unlike a reserved-then-released
/// number, no neighbour (or a later satellite) can bind it and answer as the
/// "dead" host.
struct DeadEndpoint {
    port: u16,
    accept: JoinHandle<()>,
}

impl DeadEndpoint {
    async fn reserve() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::task::spawn_local(async move {
            while let Ok((conn, _)) = listener.accept().await {
                drop(conn);
            }
        });
        Self { port, accept }
    }

    const fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for DeadEndpoint {
    fn drop(&mut self) {
        self.accept.abort();
    }
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

/// Spawn a satellite with session `sat-session` (PTY-backed when `seed` is
/// given), listening on loopback WebSocket as well as its UDS. The WS
/// listener binds port 0 and the returned port is the one it reports bound,
/// so no neighbour can take it first.
async fn spawn_satellite_runtime(
    socket_path: PathBuf,
    seed: Option<CommandBuilder>,
) -> (u16, oneshot::Sender<()>, ServerTask) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path: socket_path.clone(),
        pre_seeded_session: Some("sat-session".to_owned()),
        seed_with_pty: seed.is_some(),
        seed_command: seed,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .listen_ws("127.0.0.1:0".parse().unwrap())
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    let ws_addr = bound_listener_addr(&socket_path, RemoteListenerTransport::Wss).await;
    (ws_addr.port(), tx, handle)
}

async fn spawn_satellite(socket_path: PathBuf) -> (u16, oneshot::Sender<()>, ServerTask) {
    spawn_satellite_runtime(socket_path, None).await
}

/// Spawn a hub dialing `satellites`, optionally pre-seeding a local session.
fn spawn_hub_with_session(
    socket_path: PathBuf,
    satellites: Vec<SatelliteConfigEntry>,
    pre_seeded_session: Option<&str>,
) -> (oneshot::Sender<()>, ServerTask) {
    spawn_hub_with_env(
        socket_path,
        satellites,
        pre_seeded_session,
        phux_server::ServerEnv::default(),
    )
}

fn spawn_hub_with_env(
    socket_path: PathBuf,
    satellites: Vec<SatelliteConfigEntry>,
    pre_seeded_session: Option<&str>,
    env: phux_server::ServerEnv,
) -> (oneshot::Sender<()>, ServerTask) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: pre_seeded_session.map(str::to_owned),
        seed_with_pty: false,
        seed_command: None,
        env,
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

fn spawn_hub(
    socket_path: PathBuf,
    satellites: Vec<SatelliteConfigEntry>,
) -> (oneshot::Sender<()>, ServerTask) {
    spawn_hub_with_session(socket_path, satellites, None)
}

async fn stop(shutdown: oneshot::Sender<()>, task: ServerTask) {
    drop(shutdown);
    tokio::time::timeout(STEP_DEADLINE, task)
        .await
        .expect("server stops")
        .unwrap()
        .unwrap();
}

/// Learn the satellite's seeded pane id over a direct WebSocket `GET_STATE`.
async fn discover_satellite_pane(ws_port: u16) -> u32 {
    let addr = format!("127.0.0.1:{ws_port}");
    let url = format!("ws://{addr}/");
    let started = Instant::now();
    let mut last_error = String::new();
    let mut ws = loop {
        assert!(
            started.elapsed() < WS_CONNECT_HANG_GUARD,
            "satellite WebSocket never became connectable at {addr}: {last_error}"
        );
        match TcpStream::connect(&addr).await {
            Ok(tcp) => match tokio_tungstenite::client_async(&url, tcp).await {
                Ok((ws, _)) => break ws,
                Err(err) => last_error = err.to_string(),
            },
            Err(err) => last_error = err.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let hello = FrameKind::Hello {
        client_name: "hub-relay-federation-test".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    };
    let get_state = FrameKind::Command {
        request_id: 900,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    };
    for frame in [hello, get_state] {
        ws.send(Message::Binary(encode_frame(&frame).to_vec().into()))
            .await
            .unwrap();
    }
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
                .focused_resource
                .local_id()
                .expect("seeded pane is local on the satellite");
        }
    }
}

/// A satellite and a hub linked to it, in one temp dir.
struct Fed {
    tmp: TempDir,
    seed: u32,
    satellite: Option<(oneshot::Sender<()>, ServerTask)>,
    hub: (oneshot::Sender<()>, ServerTask),
}

impl Fed {
    async fn boot() -> Self {
        Self::boot_with(None, None).await
    }

    /// Boot the satellite (PTY-seeded with `seed` if given) and a hub
    /// (pre-seeding local session `hub_session` if given).
    async fn boot_with(seed: Option<CommandBuilder>, hub_session: Option<&str>) -> Self {
        let tmp = TempDir::new().unwrap();
        let (ws_port, sat_shutdown, sat_task) =
            spawn_satellite_runtime(tmp.path().join("sat.sock"), seed).await;
        let hub = spawn_hub_with_session(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
            hub_session,
        );
        let seed = discover_satellite_pane(ws_port).await;
        Self {
            tmp,
            seed,
            satellite: Some((sat_shutdown, sat_task)),
            hub,
        }
    }

    fn sat_id(&self) -> ResourceId {
        ResourceId::satellite("sat", self.seed)
    }

    fn hub_path(&self) -> PathBuf {
        self.tmp.path().join("hub.sock")
    }

    async fn hub(&self) -> UnixStream {
        wait_for_socket(&self.hub_path(), STEP_DEADLINE).await
    }

    /// A hub client, returned once the hub's link to the satellite answers.
    async fn linked_hub(&self) -> UnixStream {
        let mut hub = self.hub().await;
        assert!(matches!(
            get_screen_until_ok(&mut hub, self.seed).await,
            CommandResult::OkWith(_)
        ));
        hub
    }

    /// A client connected directly to the satellite's UDS.
    async fn satellite(&self) -> UnixStream {
        wait_for_socket(&self.tmp.path().join("sat.sock"), STEP_DEADLINE).await
    }

    async fn kill_satellite(&mut self) {
        let (shutdown, task) = self.satellite.take().expect("satellite running");
        stop(shutdown, task).await;
    }

    async fn shutdown(mut self) {
        let (shutdown, task) = self.hub;
        stop(shutdown, task).await;
        if let Some((shutdown, task)) = self.satellite.take() {
            stop(shutdown, task).await;
        }
    }
}

/// Send `command` and await its correlated result, collecting every frame
/// that interleaves before it.
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
        match recv_typed(hub).await.1 {
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => return (result, interleaved),
            other => interleaved.push(other),
        }
    }
}

async fn get_screen_via_hub(
    hub: &mut UnixStream,
    request_id: u32,
    terminal_id: ResourceId,
) -> CommandResult {
    let get_screen = Command::GetScreen {
        terminal_id,
        request_scrollback: None,
        cells: false,
        format: 0,
    };
    command_via_hub(hub, request_id, get_screen).await.0
}

/// Retry `command` while the link reports `SatelliteUnreachable` (the dialer
/// backs off while the satellite boots).
async fn command_until_linked(
    hub: &mut UnixStream,
    first_request_id: u32,
    command: impl Fn() -> Command,
) -> (CommandResult, Vec<FrameKind>) {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut request_id = first_request_id;
    loop {
        let (result, frames) = command_via_hub(hub, request_id, command()).await;
        match result {
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            } if Instant::now() < deadline => {
                request_id += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            other => return (other, frames),
        }
    }
}

async fn get_screen_until_ok(hub: &mut UnixStream, sat_pane: u32) -> CommandResult {
    command_until_linked(hub, 1000, || Command::GetScreen {
        terminal_id: ResourceId::satellite("sat", sat_pane),
        request_scrollback: None,
        cells: false,
        format: 0,
    })
    .await
    .0
}

/// `GET_STATE { SERVER }` plus every un-correlated `ERROR` seen before the
/// reply (the per-satellite degradation notice, L1 §9.1).
async fn get_state_via_hub(
    hub: &mut UnixStream,
    request_id: u32,
) -> (CommandResult, Vec<(ErrorCode, String)>) {
    let get_state = Command::GetState {
        scope: StateScope::Server,
    };
    let (result, frames) = command_via_hub(hub, request_id, get_state).await;
    let errors = frames
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::Error {
                request_id: None,
                code,
                message,
            } => Some((code, message)),
            _ => None,
        })
        .collect();
    (result, errors)
}

fn on_satellite(host: &str, spawn: Spawn) -> Spawn {
    Spawn {
        satellite: Some(SatelliteHost::new(host)),
        ..spawn
    }
}

fn assert_error(result: &CommandResult, expected: ErrorCode) {
    assert!(
        matches!(result, CommandResult::Error { code, .. } if *code == expected),
        "expected {expected:?}, got {result:?}"
    );
}

const fn focus_gained(terminal_id: ResourceId) -> Command {
    Command::RouteInput {
        terminal_id,
        event: InputEvent::Focus(FocusEvent::Gained),
    }
}

/// Wait for the next `EVENT` accepted by `pred`.
async fn next_event(
    stream: &mut UnixStream,
    mut pred: impl FnMut(&FrameKind) -> bool,
) -> FrameKind {
    tokio::time::timeout(STEP_DEADLINE, async {
        loop {
            let frame = recv_typed(stream).await.1;
            if matches!(frame, FrameKind::Event { .. }) && pred(&frame) {
                return frame;
            }
        }
    })
    .await
    .expect("expected EVENT never arrived")
}

/// Outbound id rewrite, return-leg re-tagging, and teardown on satellite loss.
#[test]
fn command_round_trip_and_stream_retagging() {
    phux_server_testkit::run_local(async {
        let mut fed = Fed::boot().await;
        let sat_id = fed.sat_id();
        let mut hub = fed.hub().await;

        let result = get_screen_until_ok(&mut hub, fed.seed).await;
        let CommandResult::OkWith(CommandValue::Json(json)) = result else {
            panic!("GET_SCREEN through the hub must succeed, got {result:?}");
        };
        assert!(json.contains("\"cols\""), "{json}");
        let (result, _) = command_via_hub(&mut hub, 2000, focus_gained(sat_id.clone())).await;
        assert_eq!(result, CommandResult::Ok, "ROUTE_INPUT relays Ok");

        // A relayed REPORT_ASKED comes back as an EVENT re-tagged Satellite.
        send_frame(
            &mut hub,
            &FrameKind::SubscribeEvents {
                terminal: Some(sat_id.clone()),
                after_seq: None,
            },
        )
        .await;
        let ask = Command::ReportAsked {
            terminal_id: sat_id.clone(),
            id: "q-1".to_owned(),
            question: "proceed?".to_owned(),
            suggestions: vec!["yes".to_owned(), "no".to_owned()],
            elapsed_seconds: Some(3),
        };
        let (result, mut frames) = command_via_hub(&mut hub, 2001, ask).await;
        assert_eq!(result, CommandResult::Ok, "REPORT_ASKED relays Ok");
        let asked = |frame: &FrameKind| matches!(frame, FrameKind::Event { event: AgentEvent::Asked { id, .. }, .. } if id == "q-1");
        if !frames.iter().any(asked) {
            frames.push(next_event(&mut hub, asked).await);
        }
        let Some(FrameKind::Event { terminal, .. }) = frames.iter().find(|f| asked(f)) else {
            unreachable!()
        };
        assert_eq!(terminal.as_ref(), Some(&sat_id), "return leg re-tagged");

        // Satellite loss: subscribers get a typed teardown notice, and later
        // commands fail fast with the same code.
        fed.kill_satellite().await;
        tokio::time::timeout(STEP_DEADLINE, async {
            loop {
                if let FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::SatelliteUnreachable,
                    message,
                } = recv_typed(&mut hub).await.1
                {
                    assert!(message.contains("sat"), "{message}");
                    return;
                }
            }
        })
        .await
        .expect("no SatelliteUnreachable teardown notification");
        assert_error(
            &get_screen_via_hub(&mut hub, 3000, sat_id).await,
            ErrorCode::SatelliteUnreachable,
        );

        drop(hub);
        fed.shutdown().await;
    });
}

#[test]
fn satellite_targeted_spawn_round_trips_and_routes() {
    phux_server_testkit::run_local(async {
        let mut fed = Fed::boot().await;
        let mut hub = fed.linked_hub().await;

        let spawned = spawn_resource(
            &mut hub,
            5000,
            on_satellite("sat", Spawn::command(&["/bin/cat"])),
        )
        .await;
        let SpawnResult::Ok(spawned_id @ ResourceId::Satellite { .. }) = spawned else {
            panic!("satellite spawn must return a Satellite-tagged id, got {spawned:?}");
        };
        assert_eq!(spawned_id.host().map(SatelliteHost::as_str), Some("sat"));
        assert!(matches!(
            get_screen_via_hub(&mut hub, 6000, spawned_id.clone()).await,
            CommandResult::OkWith(CommandValue::Json(ref json)) if json.contains("\"cols\"")
        ));
        let (result, _) = get_state_via_hub(&mut hub, 6001).await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
            panic!("GET_STATE must succeed, got {result:?}");
        };
        assert!(snapshot.resources.iter().any(|p| p.id == spawned_id));

        let unknown =
            spawn_resource(&mut hub, 6002, on_satellite("nowhere", Spawn::default())).await;
        assert!(
            matches!(
                unknown,
                SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute)
            ),
            "{unknown:?}"
        );

        fed.kill_satellite().await;
        let deadline = Instant::now() + STEP_DEADLINE;
        loop {
            match spawn_resource(&mut hub, 6003, on_satellite("sat", Spawn::default())).await {
                SpawnResult::Err(SpawnError::SatelliteUnreachable(message)) => {
                    assert!(message.contains("sat"), "{message}");
                    break;
                }
                // The link may not have observed the disconnect yet.
                other => {
                    assert!(Instant::now() < deadline, "{other:?}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        drop(hub);
        fed.shutdown().await;
    });
}

/// Every satellite-addressed verb on a non-hub server refuses with
/// `UnsupportedSatelliteRoute` through the shared routing path.
#[test]
fn non_hub_server_refuses_every_satellite_route() {
    phux_server_testkit::run_local(async {
        let (_server, mut plain) = phux_server_testkit::spawn_server_connected(Some("s")).await;
        let spawned = spawn_resource(&mut plain, 1, on_satellite("sat", Spawn::default())).await;
        assert!(
            matches!(
                spawned,
                SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute)
            ),
            "{spawned:?}"
        );
        let sat = ResourceId::satellite("sat", 1);
        let commands = [
            Command::GetScreen {
                terminal_id: sat.clone(),
                request_scrollback: None,
                cells: false,
                format: 0,
            },
            Command::AcquireInput {
                terminal_id: sat.clone(),
                mode: InputMode::Cooperative,
                ttl_ms: 0,
            },
            Command::ReleaseInput { terminal_id: sat },
        ];
        for (request_id, command) in (2..).zip(commands) {
            let (result, _) = command_via_hub(&mut plain, request_id, command).await;
            assert_error(&result, ErrorCode::UnsupportedSatelliteRoute);
        }

        // SUBSCRIBE_METADATA has no reply, so its refusal is an uncorrelated
        // ERROR push; a pipelined GET_METADATA must answer after it, proving
        // both ordering and that the connection is not poisoned.
        let subscribe = FrameKind::SubscribeMetadata {
            scope: Scope::Resource(ResourceId::satellite("gpubox", 7)),
            key: "phux.agent/v1".to_owned(),
        };
        let barrier = FrameKind::GetMetadata {
            request_id: 9,
            scope: Scope::Global,
            key: "phux.test.barrier/v1".to_owned(),
        };
        send_frame(&mut plain, &subscribe).await;
        send_frame(&mut plain, &barrier).await;
        let FrameKind::Error {
            request_id: None,
            code: ErrorCode::UnsupportedSatelliteRoute,
            message,
        } = recv_typed(&mut plain).await.1
        else {
            panic!("the satellite subscribe must be refused before the barrier answers");
        };
        for needle in ["does not federate", "gpubox", "phux.agent/v1"] {
            assert!(message.contains(needle), "{message}");
        }
        assert!(matches!(
            recv_typed(&mut plain).await.1,
            FrameKind::MetadataValue {
                request_id: 9,
                value: None
            }
        ));
    });
}

/// A dead registry entry fails fast with `SatelliteUnreachable`; a host the
/// registry lacks is `UnsupportedSatelliteRoute`.
#[test]
fn dead_and_unknown_satellites_fail_fast_with_typed_errors() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let dead = DeadEndpoint::reserve().await;
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", dead.port())],
        );
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        let started = Instant::now();
        let result = get_screen_via_hub(&mut hub, 1, ResourceId::satellite("sat", 1)).await;
        assert_error(&result, ErrorCode::SatelliteUnreachable);
        assert!(started.elapsed() < STEP_DEADLINE, "fail-fast, not a hang");
        let result = get_screen_via_hub(&mut hub, 2, ResourceId::satellite("nowhere", 1)).await;
        assert_error(&result, ErrorCode::UnsupportedSatelliteRoute);

        drop(hub);
        stop(hub_shutdown, hub_task).await;
    });
}

/// The aggregated LIST merges local and satellite terminals, lists satellite
/// sessions per host, and degrades a dead satellite to an un-correlated error.
#[test]
fn aggregated_list_merges_local_and_satellite_terminals_and_degrades() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        // Reserve the dead port first so the live draw cannot collide with it.
        let down = DeadEndpoint::reserve().await;
        let (ws_port, sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock")).await;
        let (hub_shutdown, hub_task) = spawn_hub_with_session(
            tmp.path().join("hub.sock"),
            vec![
                satellite_entry("sat", ws_port),
                satellite_entry("down", down.port()),
            ],
            Some("hub-session"),
        );
        let sat_id = ResourceId::satellite("sat", discover_satellite_pane(ws_port).await);
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;

        let deadline = Instant::now() + STEP_DEADLINE;
        let mut request_id = 4000;
        let (snapshot, errors) = loop {
            let (result, errors) = get_state_via_hub(&mut hub, request_id).await;
            let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                panic!("aggregated GET_STATE must never fail, got {result:?}");
            };
            if snapshot.resources.iter().any(|p| p.id == sat_id) {
                break (snapshot, errors);
            }
            assert!(Instant::now() < deadline, "{snapshot:?}");
            request_id += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        assert!(
            snapshot
                .resources
                .iter()
                .any(|p| matches!(p.id, ResourceId::Local { .. }))
        );
        let sat_info = snapshot.resources.iter().find(|p| p.id == sat_id).unwrap();
        assert_eq!(
            (sat_info.cols, sat_info.rows),
            (80, 24),
            "dims relayed verbatim"
        );
        assert!(
            !snapshot
                .resources
                .iter()
                .any(|p| p.id.host().is_some_and(|h| h.as_str() == "down")),
            "dead satellite contributes no panes"
        );
        assert!(
            errors
                .iter()
                .any(|(code, message)| *code == ErrorCode::SatelliteUnreachable
                    && message.contains("down")),
            "{errors:?}"
        );
        // Satellite sessions do not merge; they are listed per host instead.
        let names: Vec<&str> = snapshot.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["hub-session"]);
        let hosts: Vec<&str> = snapshot.hosts().iter().map(|h| h.host.as_str()).collect();
        assert_eq!(hosts, ["down", "sat"], "one sorted row per satellite");
        let [down_row, sat_row] = snapshot.hosts() else {
            unreachable!()
        };
        assert!(!down_row.is_reachable() && down_row.sessions.is_empty());
        assert!(sat_row.is_reachable());
        let [sat_session] = sat_row.sessions.as_slice() else {
            panic!("{sat_row:?}");
        };
        assert_eq!(sat_session.name, "sat-session");
        assert_eq!((sat_session.window_count, sat_session.pane_count), (1, 1));
        assert_eq!(sat_session.active_resource, Some(sat_id));

        drop(hub);
        stop(sat_shutdown, sat_task).await;
        stop(hub_shutdown, hub_task).await;
    });
}

fn mkfifo(path: &std::path::Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo {} failed", path.display());
}

/// The `$PHUX_SSH` stub the hub spawns in place of `ssh`: it pipes its stdin
/// to the `c2s` FIFO and the `s2c` FIFO to its stdout, ignoring the argv (the
/// argv itself is asserted by `hub::link`'s unit tests). `exec 3<&0` because
/// a background job's stdin is reset to /dev/null.
fn write_ssh_stub(dir: &std::path::Path, c2s: &std::path::Path, s2c: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-ssh");
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

/// Play `phux stdio-bridge`: splice the stub's FIFOs onto the satellite UDS
/// until either direction ends.
async fn run_stub_bridge(c2s: PathBuf, s2c: PathBuf, sat_sock: PathBuf) {
    use tokio::net::unix::pipe;
    let mut from_hub = pipe::OpenOptions::new().open_receiver(&c2s).unwrap();
    // Keep a writer open so the receiver never sees EOF between stub children.
    let _hold_open = pipe::OpenOptions::new().open_sender(&c2s).unwrap();
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
    tokio::select! {
        _ = tokio::io::copy(&mut from_hub, &mut sat_wr) => {}
        _ = tokio::io::copy(&mut sat_rd, &mut to_hub) => {}
    }
}

/// The relay over the SSH-stdio dial path, with `PHUX_SSH` pointing at a
/// stub whose stdio the harness splices onto the satellite's UDS.
#[test]
fn ssh_stub_link_relays_commands_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let c2s = tmp.path().join("c2s.fifo");
    let s2c = tmp.path().join("s2c.fifo");
    mkfifo(&c2s);
    mkfifo(&s2c);
    let stub = write_ssh_stub(tmp.path(), &c2s, &s2c);

    phux_server_testkit::run_local(async move {
        let sat_sock = tmp.path().join("sat.sock");
        let (ws_port, sat_shutdown, sat_task) = spawn_satellite(sat_sock.clone()).await;
        let bridge = tokio::task::spawn_local(run_stub_bridge(c2s, s2c, sat_sock));
        let (hub_shutdown, hub_task) = spawn_hub_with_env(
            tmp.path().join("hub.sock"),
            vec![SatelliteConfigEntry {
                name: "sat".to_owned(),
                endpoint: "ssh://sat-host".to_owned(),
                enabled: true,
                token_file: None,
                cert_fingerprint: None,
            }],
            None,
            phux_server::ServerEnv {
                ssh_program: Some(stub.clone().into_os_string()),
                ..phux_server::ServerEnv::default()
            },
        );
        // Discovery uses the satellite's WS listener; only the hub leg rides ssh.
        let sat_pane = discover_satellite_pane(ws_port).await;
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let result = get_screen_until_ok(&mut hub, sat_pane).await;
        assert!(
            matches!(result, CommandResult::OkWith(CommandValue::Json(ref json)) if json.contains("\"cols\"")),
            "{result:?}"
        );

        // Remove the stub so redials fail at spawn, then kill the satellite:
        // the exited ssh child must read as an ordinary dropped link.
        std::fs::remove_file(&stub).unwrap();
        stop(sat_shutdown, sat_task).await;
        let deadline = Instant::now() + STEP_DEADLINE;
        for request_id in 5000.. {
            assert!(Instant::now() < deadline, "never SatelliteUnreachable");
            let result =
                get_screen_via_hub(&mut hub, request_id, ResourceId::satellite("sat", sat_pane))
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
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        drop(hub);
        stop(hub_shutdown, hub_task).await;
        let _ = bridge.await;
    });
}

/// Send one ASCII key + Enter (cooked-mode `cat` echoes on Enter).
async fn send_key_and_enter(hub: &mut UnixStream, sat_id: &ResourceId, c: char, key: PhysicalKey) {
    let mut enter = ascii_key('\r', PhysicalKey::Enter);
    enter.text = None;
    enter.unshifted_codepoint = None;
    for event in [ascii_key(c, key), enter] {
        send_frame(
            hub,
            &FrameKind::InputKey {
                terminal_id: sat_id.clone(),
                event,
            },
        )
        .await;
    }
}

/// Drain re-tagged `RESOURCE_OUTPUT` until `needle` appears; return the
/// stream/generation identity and last `seq`.
async fn await_satellite_echo(
    hub: &mut UnixStream,
    sat_id: &ResourceId,
    needle: u8,
) -> (
    phux_protocol::ids::StreamId,
    phux_protocol::ids::BootstrapId,
    u64,
) {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = Instant::now() + STEP_DEADLINE;
    while Instant::now() < deadline {
        if let FrameKind::ResourceOutput {
            terminal_id,
            seq,
            stream_id,
            bootstrap_id,
            bytes,
        } = recv_typed(hub).await.1
        {
            assert_eq!(&terminal_id, sat_id, "two-hop output re-tagged");
            acc.extend_from_slice(&bytes);
            if acc.contains(&needle) {
                return (stream_id, bootstrap_id, seq);
            }
        }
    }
    panic!(
        "echo {:?} never arrived; got {:?}",
        needle as char,
        String::from_utf8_lossy(&acc)
    );
}

async fn attach_terminal_until_ok(hub: &mut UnixStream, sat_id: &ResourceId) -> Vec<FrameKind> {
    let (result, frames) = command_until_linked(hub, 5000, || Command::AttachResource {
        terminal_id: sat_id.clone(),
        role_policy: None,
    })
    .await;
    assert_eq!(result, CommandResult::Ok, "ATTACH_RESOURCE through the hub");
    frames
}

/// Interactive attach over two hops: re-tagged snapshot before any output,
/// input echo, `FRAME_ACK` relay, and detach/re-attach.
#[test]
fn two_hop_attach_snapshot_output_input_ack_and_detach() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(Some(CommandBuilder::new("/bin/cat")), None).await;
        let sat_id = fed.sat_id();
        let mut hub = fed.hub().await;

        let frames = attach_terminal_until_ok(&mut hub, &sat_id).await;
        let snapshot_pos = frames
            .iter()
            .position(|f| matches!(f, FrameKind::BootstrapBegin { .. }))
            .expect("ATTACH_RESOURCE must deliver a snapshot");
        let FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            ..
        } = &frames[snapshot_pos]
        else {
            unreachable!()
        };
        assert_eq!(terminal_id, &sat_id, "snapshot re-tagged");
        assert!(*cols > 0 && *rows > 0);
        assert!(
            !frames[..snapshot_pos]
                .iter()
                .any(|f| matches!(f, FrameKind::ResourceOutput { .. })),
            "no output delta may precede the attach snapshot"
        );

        send_key_and_enter(&mut hub, &sat_id, 'a', PhysicalKey::A).await;
        let (stream_id, bootstrap_id, seq) = await_satellite_echo(&mut hub, &sat_id, b'a').await;
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
        send_key_and_enter(&mut hub, &sat_id, 'b', PhysicalKey::B).await;
        let _ = await_satellite_echo(&mut hub, &sat_id, b'b').await;

        let detach = Command::DetachResource {
            terminal_id: sat_id.clone(),
        };
        assert_eq!(
            command_via_hub(&mut hub, 7000, detach).await.0,
            CommandResult::Ok
        );
        let frames = attach_terminal_until_ok(&mut hub, &sat_id).await;
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, FrameKind::BootstrapBegin { .. })),
            "re-attach delivers a fresh snapshot"
        );

        drop(hub);
        fed.shutdown().await;
    });
}

/// Hub consumers share one link identity on the satellite, so the hub keeps
/// the input-lease ledger: B cannot acquire, route, or release A's lease, and
/// a SEIZE takeover notifies the evicted holder.
#[test]
fn satellite_input_lease_is_per_hub_consumer_and_seize_notifies() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let sat_id = fed.sat_id();
        let mut a = fed.linked_hub().await;
        let mut b = fed.hub().await;
        let acquire = |mode| Command::AcquireInput {
            terminal_id: sat_id.clone(),
            mode,
            ttl_ms: 0,
        };

        let (result, _) = command_via_hub(&mut a, 100, acquire(InputMode::Cooperative)).await;
        assert_eq!(result, CommandResult::Ok);
        let (result, _) = command_via_hub(&mut b, 200, acquire(InputMode::Cooperative)).await;
        assert_error(&result, ErrorCode::InputLeaseHeld);
        let (result, _) = command_via_hub(&mut b, 201, focus_gained(sat_id.clone())).await;
        assert_error(&result, ErrorCode::InputLeaseHeld);
        let release = Command::ReleaseInput {
            terminal_id: sat_id.clone(),
        };
        let (result, _) = command_via_hub(&mut b, 202, release).await;
        assert_eq!(result, CommandResult::Ok, "non-holder release is a no-op");
        let (result, _) = command_via_hub(&mut b, 203, acquire(InputMode::Cooperative)).await;
        assert_error(&result, ErrorCode::InputLeaseHeld);

        let (result, _) = command_via_hub(&mut b, 204, acquire(InputMode::Seize)).await;
        assert_eq!(result, CommandResult::Ok, "SEIZE preempts A");
        let evicted = next_event(&mut a, |frame| {
            matches!(
                frame,
                FrameKind::Event {
                    event: AgentEvent::TerminalControl { .. },
                    ..
                }
            )
        })
        .await;
        let FrameKind::Event {
            terminal,
            event:
                AgentEvent::TerminalControl {
                    action,
                    input_holder,
                    ..
                },
            ..
        } = evicted
        else {
            unreachable!()
        };
        assert_eq!(terminal.as_ref(), Some(&sat_id), "re-tagged");
        assert_eq!(action, ControlAction::Seized);
        assert!(input_holder.is_some(), "names the new holder");
        let (result, _) = command_via_hub(&mut a, 101, focus_gained(sat_id.clone())).await;
        assert_error(&result, ErrorCode::InputLeaseHeld);

        drop((a, b));
        fed.shutdown().await;
    });
}

/// The aggregate carries a satellite's `AgentSession` with both its id and
/// its `parent` re-tagged (ADR-0104 §6): re-tagging only the child would bind
/// it to whatever hub-local pane holds the same integer.
#[test]
fn hub_inventory_lists_a_satellites_agent_session_with_a_retagged_parent() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(None, Some("hub-session")).await;
        let mut sat = fed.satellite().await;
        let agent_session = Spawn {
            resource: Some(Box::new(SpawnResource::agent_session(
                ResourceId::local(fed.seed),
                "claude",
            ))),
            ..Spawn::default()
        };
        let SpawnResult::Ok(session) = spawn_resource(&mut sat, 1, agent_session).await else {
            panic!("the satellite must spawn its own session");
        };
        let sat_session = session.local_id().expect("local on the satellite");
        assert_ne!(sat_session, fed.seed);

        let hub_pane = fed.sat_id();
        let hub_session = ResourceId::satellite("sat", sat_session);
        let mut hub = fed.hub().await;
        let deadline = Instant::now() + STEP_DEADLINE;
        let mut request_id = 4000;
        let snapshot = loop {
            let (result, _) = get_state_via_hub(&mut hub, request_id).await;
            let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
                panic!("aggregated GET_STATE must never fail, got {result:?}");
            };
            if snapshot.resources.iter().any(|pane| pane.id == hub_session) {
                break snapshot;
            }
            assert!(Instant::now() < deadline, "{snapshot:?}");
            request_id += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let find = |id: &ResourceId| {
            snapshot
                .resources
                .iter()
                .find(|pane| &pane.id == id)
                .unwrap()
        };
        let listed = find(&hub_session);
        assert_eq!(listed.kind, phux_protocol::ids::ResourceKind::AgentSession);
        assert_eq!(listed.parent.as_ref(), Some(&hub_pane), "parent re-tagged");
        assert_eq!(
            listed.agent.as_ref().map(|facet| facet.provider.as_str()),
            Some("claude")
        );
        let parent = find(&hub_pane);
        assert_eq!(parent.kind, phux_protocol::ids::ResourceKind::Terminal);
        assert_eq!(parent.parent, None);
        assert!(
            snapshot
                .resources
                .iter()
                .any(|pane| matches!(pane.id, ResourceId::Local { .. }))
        );

        drop((sat, hub));
        fed.shutdown().await;
    });
}
