//! Real-QUIC acceptance for slow command isolation on the shipping client path.
//!
//! One production `Connection` drives one production `ServerRuntime`. A marker
//! handshake proves the transcriber is already running before the timed control
//! and input probes begin; no sleep is used as evidence that work started.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "ServerRuntime owns LocalSet actors")]

use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_client::attach::{CertTrust, QuicDial};
use phux_protocol::ids::{FileUploadId, InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SpawnResult, StateScope,
};
use phux_server::{DEFAULT_GROUP_ID, ServerConfig, ServerRuntime};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

const STEP_DEADLINE: Duration = Duration::from_secs(15);
const CONTROL_DEADLINE: Duration = Duration::from_secs(1);
const MINIMUM_TRANSCRIBE_HOLD: Duration = Duration::from_secs(2);

struct EnvGuard {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn install(cert: &Path, key: &Path, upload_dir: &Path) -> Self {
        phux_server::transport::tls::ensure_self_signed(cert, key).expect("provision QUIC cert");
        let previous = [
            "PHUX_WS_TLS_CERT",
            "PHUX_WS_TLS_KEY",
            "PHUX_UPLOAD_DIR",
            "PHUX_WS_SECURE",
            "PHUX_WORKLOAD_MTLS",
            "PHUX_TEST_WORKLOAD_FAILURE",
        ]
        .into_iter()
        .map(|name| (name, std::env::var_os(name)))
        .collect();
        unsafe {
            std::env::set_var("PHUX_WS_TLS_CERT", cert);
            std::env::set_var("PHUX_WS_TLS_KEY", key);
            std::env::set_var("PHUX_UPLOAD_DIR", upload_dir);
            std::env::remove_var("PHUX_WS_SECURE");
            std::env::remove_var("PHUX_WORKLOAD_MTLS");
            std::env::remove_var("PHUX_TEST_WORKLOAD_FAILURE");
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.previous {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

struct Server {
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<Result<(), phux_server::ServerError>>>,
}

impl Server {
    async fn stop(mut self) {
        self.shutdown.take().unwrap().send(()).ok();
        timeout(STEP_DEADLINE, self.handle.take().unwrap())
            .await
            .expect("server shutdown timed out")
            .expect("server task panicked")
            .expect("server shutdown failed");
    }
}

fn free_udp_addr() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("reserve UDP port");
    socket.local_addr().expect("read UDP port")
}

fn paste(bytes: &[u8]) -> InputEvent {
    InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: bytes.to_vec(),
    })
}

fn blocking_transcriber(started: &Path, release: &Path) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "label=$(cat \"$1\"); printf '%s %s\\n' \"$label\" \"$$\" >> \"$2\"; \
         while [ ! -e \"$3\" ]; do sleep 0.01; done; printf '%s\\n' \"$label\""
            .to_owned(),
        "sh".to_owned(),
        "{path}".to_owned(),
        started.display().to_string(),
        release.display().to_string(),
    ]
}

fn line_recorder(marker: &Path) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "stty -echo; while IFS= read -r line; do printf '%s\\n' \"$line\" >> \"$1\"; done"
            .to_owned(),
        "sh".to_owned(),
        marker.display().to_string(),
    ]
}

fn spawn_server(socket: PathBuf, quic_addr: SocketAddr, started: &Path, release: &Path) -> Server {
    let (shutdown, stopped) = oneshot::channel();
    let mut config = ServerConfig {
        socket_path: socket,
        pre_seeded_session: Some("quic-command-isolation".to_owned()),
        seed_with_pty: true,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    config.voice.transcriber = Some(blocking_transcriber(started, release));
    config.voice.timeout_secs = Some(15);
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(config)
            .listen_quic(quic_addr)
            .run_async(async move {
                let _ = stopped.await;
            })
            .await
    });
    Server {
        shutdown: Some(shutdown),
        handle: Some(handle),
    }
}

async fn dial(addr: SocketAddr) -> Connection {
    let dial = QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: None,
        trust: CertTrust::SkipVerify,
    };
    let deadline = Instant::now() + STEP_DEADLINE;
    loop {
        match Connection::connect_quic(&dial).await {
            Ok(connection) => return connection,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                sleep(Duration::from_millis(25)).await;
            }
            Err(error) => panic!("QUIC server did not become ready: {error}"),
        }
    }
}

fn state_resource(result: CommandResult) -> ResourceId {
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("GET_STATE failed: {result:?}");
    };
    snapshot.resources[0].id.clone()
}

async fn spawn_input_terminal(connection: &mut Connection, marker: &Path) -> ResourceId {
    let frame = FrameKind::SpawnResource {
        request_id: 2,
        group: DEFAULT_GROUP_ID,
        command: Some(line_recorder(marker)),
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: None,
    };
    let (result, _) = connection
        .request_spawn(&frame)
        .await
        .expect("SPAWN_RESOURCE transport")
        .into_parts();
    match result {
        Ok(SpawnResult::Ok(id)) => id,
        other => panic!("SPAWN_RESOURCE failed: {other:?}"),
    }
}

async fn upload(
    connection: &mut Connection,
    request_id: u32,
    upload_id: FileUploadId,
    terminal_id: &ResourceId,
    bytes: &[u8],
) {
    let (result, _) = connection
        .request(
            request_id,
            Command::PutFile {
                upload_id,
                terminal_id: terminal_id.clone(),
                extension: "wav".to_owned(),
                offset: 0,
                data: bytes.to_vec(),
                final_chunk: true,
                sha256: Some(Sha256::digest(bytes).into()),
            },
        )
        .await
        .expect("PUT_FILE transport")
        .into_parts();
    assert!(
        matches!(result, CommandResult::OkWith(CommandValue::FileUpload(_))),
        "PUT_FILE failed: {result:?}",
    );
}

async fn wait_for_lines(path: &Path, count: usize) -> Vec<String> {
    timeout(STEP_DEADLINE, async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await {
                let lines: Vec<_> = value.lines().map(str::to_owned).collect();
                if lines.len() >= count {
                    return lines;
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("marker {} never reached {count} lines", path.display()))
}

fn marker_pid(lines: &[String], label: &str) -> Option<u32> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(label)?.strip_prefix(' ')?.parse().ok())
}

fn transcript(result: CommandResult) -> String {
    let CommandResult::OkWith(CommandValue::Json(json)) = result else {
        panic!("TRANSCRIBE failed: {result:?}");
    };
    serde_json::from_str::<serde_json::Value>(&json).expect("TRANSCRIBE JSON")["text"]
        .as_str()
        .expect("transcript text")
        .to_owned()
}

async fn send_isolation_probes(connection: &mut Connection, terminal_id: ResourceId) {
    connection
        .send(&FrameKind::Ping { nonce: 0xc011_51a7 })
        .await
        .expect("send PING");
    for (request_id, command) in [
        (
            11,
            Command::GetState {
                scope: StateScope::Server,
            },
        ),
        (
            12,
            Command::ApplyInput {
                operation_id: InputOperationId::new([0x12; 16]).unwrap(),
                terminal_id: terminal_id.clone(),
                events: vec![paste(b"apply-first\n")],
            },
        ),
        (
            13,
            Command::RouteInput {
                terminal_id,
                event: paste(b"route-second\n"),
            },
        ),
    ] {
        connection
            .send(&FrameKind::Command {
                request_id,
                command,
            })
            .await
            .expect("send isolation probe");
    }
}

async fn await_isolation_probes(connection: &mut Connection) {
    let deadline = Instant::now() + CONTROL_DEADLINE;
    let (mut pong, mut state, mut apply, mut route) = (false, false, false, false);
    while !(pong && state && apply && route) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "QUIC control/input stalled behind TRANSCRIBE"
        );
        let frame = timeout(remaining, connection.recv())
            .await
            .expect("QUIC isolation deadline elapsed")
            .expect("QUIC receive failed");
        match frame {
            FrameKind::Pong { nonce: 0xc011_51a7 } => pong = true,
            FrameKind::CommandResult { request_id: 10, .. } => {
                panic!("TRANSCRIBE completed before marker release")
            }
            FrameKind::CommandResult {
                request_id: 11,
                result: CommandResult::OkWith(CommandValue::State(_)),
            } => state = true,
            FrameKind::CommandResult {
                request_id: 12,
                result: CommandResult::Ok,
            } => apply = true,
            FrameKind::CommandResult {
                request_id: 13,
                result: CommandResult::Ok,
            } => route = true,
            _ => {}
        }
    }
}

async fn next_command_result(connection: &mut Connection) -> (u32, CommandResult) {
    loop {
        if let FrameKind::CommandResult { request_id, result } =
            connection.recv().await.expect("receive COMMAND_RESULT")
        {
            return (request_id, result);
        }
    }
}

async fn prove_put_file_transcribe_fifo(connection: &mut Connection, terminal_id: ResourceId) {
    let upload_id = FileUploadId::new([0x72; 16]).unwrap();
    let bytes = b"quic fifo";
    connection
        .send(&FrameKind::Command {
            request_id: 20,
            command: Command::PutFile {
                upload_id,
                terminal_id: terminal_id.clone(),
                extension: "wav".to_owned(),
                offset: 0,
                data: bytes.to_vec(),
                final_chunk: true,
                sha256: Some(Sha256::digest(bytes).into()),
            },
        })
        .await
        .expect("send FIFO PUT_FILE");
    connection
        .send(&FrameKind::Command {
            request_id: 21,
            command: Command::Transcribe {
                upload_id,
                terminal_id,
            },
        })
        .await
        .expect("send FIFO TRANSCRIBE");
    let (put_id, put_result) = next_command_result(connection).await;
    assert_eq!(put_id, 20, "PUT_FILE reply correlation and FIFO");
    assert!(
        matches!(
            put_result,
            CommandResult::OkWith(CommandValue::FileUpload(_))
        ),
        "FIFO PUT_FILE failed: {put_result:?}",
    );
    let (transcribe_id, transcribe_result) = next_command_result(connection).await;
    assert_eq!(transcribe_id, 21, "TRANSCRIBE reply correlation");
    assert_eq!(transcript(transcribe_result), "quic fifo");
}

async fn prove_quic_isolation() {
    let tmp = TempDir::new().unwrap();
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");
    let started = tmp.path().join("transcriber-started");
    let release = tmp.path().join("transcriber-release");
    let input_marker = tmp.path().join("input-order");
    let _env = EnvGuard::install(&cert, &key, &tmp.path().join("uploads"));
    let quic_addr = free_udp_addr();
    let server = spawn_server(tmp.path().join("phux.sock"), quic_addr, &started, &release);
    let mut connection = dial(quic_addr).await;
    assert!(
        connection.multistream_enabled(),
        "QUIC_STREAMS must be negotiated"
    );

    let (state, _) = connection
        .request(
            1,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await
        .expect("GET_STATE transport")
        .into_parts();
    let transcribe_terminal = state_resource(state);
    let input_terminal = spawn_input_terminal(&mut connection, &input_marker).await;
    let upload_id = FileUploadId::new([0x71; 16]).unwrap();
    upload(
        &mut connection,
        3,
        upload_id,
        &transcribe_terminal,
        b"quic held",
    )
    .await;
    connection
        .send(&FrameKind::Command {
            request_id: 10,
            command: Command::Transcribe {
                upload_id,
                terminal_id: transcribe_terminal.clone(),
            },
        })
        .await
        .expect("send TRANSCRIBE");

    let starts = wait_for_lines(&started, 1).await;
    assert!(
        marker_pid(&starts, "quic held").is_some(),
        "complete start handshake"
    );
    let held_since = Instant::now();
    let probes_started = Instant::now();
    send_isolation_probes(&mut connection, input_terminal).await;
    await_isolation_probes(&mut connection).await;
    assert!(
        probes_started.elapsed() < CONTROL_DEADLINE,
        "timed PING/control/input probes exceeded the one-second acceptance bound",
    );
    assert_eq!(
        &wait_for_lines(&input_marker, 2).await[..2],
        ["apply-first", "route-second"]
    );
    if let Some(remaining) = MINIMUM_TRANSCRIBE_HOLD.checked_sub(held_since.elapsed()) {
        sleep(remaining).await;
    }
    tokio::fs::write(&release, b"go").await.unwrap();
    assert_eq!(
        transcript(next_command_result(&mut connection).await.1),
        "quic held"
    );

    prove_put_file_transcribe_fifo(&mut connection, transcribe_terminal).await;

    connection.shutdown().await;
    server.stop().await;
}

#[test]
fn real_quic_connection_keeps_control_and_terminal_input_live_during_transcribe() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        timeout(Duration::from_secs(30), prove_quic_isolation())
            .await
            .expect("real QUIC command-isolation acceptance timed out");
    });
}
